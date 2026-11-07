//! Late frame sends (#307 step 3, off by default). The runtime wraps the DSPI2 peer, so it sees
//! each frame at the exchange, when the frame goes to the DSP side. At each exchange it measures
//! the gap from the previous one in instructions. A gap inside the window flags the frame. On DN2
//! 1.11 a frame sent 0.739 to 0.772 ms after the previous one was lost on a unit (finding 04).
//!
//! The exchange is the transfer itself, so a build that moves the send call, or holds a frame in a
//! guard, cannot hide a transfer from the count.
//!
//! With `drop` the flagged frame does not reach the DSP side: the DSP gets the previous frame
//! again, and the ColdFire gets the DSP's reply to it. This is a model. How a unit loses a frame
//! is not known.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use periph::dspi::Peer;
use serde_json::{Value, json};

/// Flagged frames kept for the report.
const KEEP: usize = 1000;

#[derive(Debug, Default)]
struct State {
    lo: u64,
    hi: u64,
    drop: bool,
    last: Option<u64>,
    sends: u64,
    flagged: u64,
    dropped: u64,
    /// Each flagged frame: the instruction count at its exchange, and its gap.
    kept: Vec<(u64, u64)>,
}

/// The runtime's handle: it sets `clock` before each instruction and reads the report.
#[derive(Debug)]
pub struct LateSend {
    clock: Rc<Cell<u64>>,
    state: Rc<RefCell<State>>,
}

impl LateSend {
    /// Wrap `inner`, the DSP side of the link. Flag each exchange whose gap from the previous
    /// exchange is `lo..=hi` instructions, and with `drop` replace the flagged frame.
    pub fn wrap(inner: Box<dyn Peer>, lo: u64, hi: u64, drop: bool) -> (Self, Box<dyn Peer>) {
        let clock = Rc::new(Cell::new(0));
        let state = Rc::new(RefCell::new(State {
            lo,
            hi,
            drop,
            ..State::default()
        }));
        let peer = Box::new(TransferPeer {
            inner,
            clock: clock.clone(),
            state: state.clone(),
            last_tx: None,
        });
        (Self { clock, state }, peer)
    }

    /// The instruction count the next exchange sees.
    #[inline]
    pub fn set_icount(&self, icount: u64) {
        self.clock.set(icount);
    }

    pub fn report(&self) -> Value {
        let s = self.state.borrow();
        json!({
            "window": [s.lo, s.hi],
            "sends": s.sends,
            "flagged": s.flagged,
            "dropping": s.drop,
            "dropped": s.dropped,
            "at": s.kept.iter().map(|&(at, gap)| json!({"at": at, "gap": gap})).collect::<Vec<_>>(),
        })
    }
}

/// The DSP side of the link, wrapped: it times each exchange and can replace a late frame.
struct TransferPeer {
    inner: Box<dyn Peer>,
    clock: Rc<Cell<u64>>,
    state: Rc<RefCell<State>>,
    last_tx: Option<Vec<u8>>,
}

impl Peer for TransferPeer {
    fn exchange(&mut self, tx: &[u8]) -> Vec<u8> {
        let now = self.clock.get();
        let replace = {
            let mut s = self.state.borrow_mut();
            s.sends += 1;
            let gap = s.last.map(|last| now.saturating_sub(last));
            s.last = Some(now);
            let late = gap.filter(|g| (s.lo..=s.hi).contains(g));
            if let Some(gap) = late {
                s.flagged += 1;
                if s.kept.len() < KEEP {
                    s.kept.push((now, gap));
                }
            }
            let replace = late.is_some()
                && s.drop
                && self.last_tx.as_ref().is_some_and(|p| p.len() == tx.len());
            if replace {
                s.dropped += 1;
            }
            replace
        };
        if replace && let Some(previous) = &self.last_tx {
            return self.inner.exchange(previous);
        }
        self.last_tx = Some(tx.to_vec());
        self.inner.exchange(tx)
    }

    fn link_bytes(&mut self, bytes: &[u8]) {
        self.inner.link_bytes(bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Record(Rc<RefCell<Vec<Vec<u8>>>>);
    impl Peer for Record {
        fn exchange(&mut self, tx: &[u8]) -> Vec<u8> {
            self.0.borrow_mut().push(tx.to_vec());
            vec![0; tx.len()]
        }
    }

    fn run(drop: bool) -> (LateSend, Vec<Vec<u8>>) {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let (late, mut peer) = LateSend::wrap(Box::new(Record(seen.clone())), 90, 110, drop);
        for (at, frame) in [(0, 1), (88, 2), (188, 3), (276, 4)] {
            late.set_icount(at);
            peer.exchange(&[frame, frame]);
        }
        let frames = seen.borrow().clone();
        (late, frames)
    }

    #[test]
    fn a_gap_in_the_window_flags_the_frame() {
        let (late, frames) = run(false);
        let r = late.report();
        assert_eq!(r["sends"], 4);
        assert_eq!(r["flagged"], 1);
        assert_eq!(r["at"][0]["at"], 188);
        assert_eq!(r["at"][0]["gap"], 100);
        assert_eq!(r["dropped"], 0);
        assert_eq!(frames, [[1, 1], [2, 2], [3, 3], [4, 4]]);
    }

    #[test]
    fn a_dropped_frame_gives_the_dsp_the_previous_frame() {
        let (late, frames) = run(true);
        assert_eq!(frames, [[1, 1], [2, 2], [2, 2], [4, 4]]);
        assert_eq!(late.report()["dropped"], 1);
        assert_eq!(late.report()["flagged"], 1);
    }
}
