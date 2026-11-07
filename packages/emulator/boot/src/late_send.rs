//! Late frame sends (#307 step 3, off by default). A host names the PC of the handler's send call
//! and a window of gaps in instructions. At each pass of that PC the runtime measures the gap from
//! the previous pass. A gap inside the window flags the send. On DN2 1.11 a frame sent 0.739 to
//! 0.772 ms after the previous one was lost on a unit (finding 04).
//!
//! With `drop` the flagged frame does not reach the DSP side: [`DropPeer`] gives the DSP the
//! previous frame again, and the ColdFire gets the DSP's reply to it. This is a model. How the unit
//! loses the frame is not known.

use std::{cell::Cell, rc::Rc};

use periph::dspi::Peer;
use serde_json::{Value, json};

/// Flagged sends kept for the report.
const KEEP: usize = 1000;

#[derive(Debug)]
pub struct LateSend {
    pc: u32,
    lo: u64,
    hi: u64,
    last: Option<u64>,
    sends: u64,
    flagged: u64,
    /// Each flagged send: the instruction count at the send, and its gap.
    kept: Vec<(u64, u64)>,
    /// Set at a flagged send when frames are dropped. The next DSPI2 exchange takes it.
    drop: Option<Rc<Cell<bool>>>,
    dropped: Rc<Cell<u64>>,
}

impl LateSend {
    pub fn new(pc: u32, lo: u64, hi: u64) -> Self {
        Self {
            pc,
            lo,
            hi,
            last: None,
            sends: 0,
            flagged: 0,
            kept: Vec::new(),
            drop: None,
            dropped: Rc::new(Cell::new(0)),
        }
    }

    pub fn pc(&self) -> u32 {
        self.pc
    }

    /// Drop the flagged frames: wrap `inner` so a flagged frame does not reach the DSP side.
    pub fn drop_peer(&mut self, inner: Box<dyn Peer>) -> Box<dyn Peer> {
        let flag = Rc::new(Cell::new(false));
        self.drop = Some(flag.clone());
        Box::new(DropPeer {
            inner,
            flag,
            dropped: self.dropped.clone(),
            last_tx: None,
        })
    }

    /// Called before the instruction at `pc` runs, with the instructions executed so far.
    pub fn observe(&mut self, pc: u32, icount: u64) {
        if pc != self.pc {
            return;
        }
        self.sends += 1;
        if let Some(last) = self.last {
            let gap = icount - last;
            if (self.lo..=self.hi).contains(&gap) {
                self.flagged += 1;
                if self.kept.len() < KEEP {
                    self.kept.push((icount, gap));
                }
                if let Some(flag) = &self.drop {
                    flag.set(true);
                }
            }
        }
        self.last = Some(icount);
    }

    pub fn report(&self) -> Value {
        json!({
            "pc": format!("{:#010x}", self.pc),
            "window": [self.lo, self.hi],
            "sends": self.sends,
            "flagged": self.flagged,
            "dropping": self.drop.is_some(),
            "dropped": self.dropped.get(),
            "at": self.kept.iter().map(|&(at, gap)| json!({"at": at, "gap": gap})).collect::<Vec<_>>(),
        })
    }
}

/// The DSP side of the link, wrapped: a flagged frame is replaced by the previous frame.
struct DropPeer {
    inner: Box<dyn Peer>,
    flag: Rc<Cell<bool>>,
    dropped: Rc<Cell<u64>>,
    last_tx: Option<Vec<u8>>,
}

impl Peer for DropPeer {
    fn exchange(&mut self, tx: &[u8]) -> Vec<u8> {
        if self.flag.replace(false)
            && let Some(previous) = &self.last_tx
            && previous.len() == tx.len()
        {
            self.dropped.set(self.dropped.get() + 1);
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
    use std::cell::RefCell;

    #[test]
    fn a_gap_in_the_window_flags_the_send() {
        let mut l = LateSend::new(0x100, 90, 110);
        for (pc, at) in [
            (0x100, 0),
            (0x104, 50),
            (0x100, 88),
            (0x100, 188),
            (0x100, 300),
        ] {
            l.observe(pc, at);
        }
        let r = l.report();
        assert_eq!(r["sends"], 4);
        assert_eq!(r["flagged"], 1);
        assert_eq!(r["at"][0]["at"], 188);
        assert_eq!(r["at"][0]["gap"], 100);
        assert_eq!(r["dropped"], 0);
    }

    struct Record(Rc<RefCell<Vec<Vec<u8>>>>);
    impl Peer for Record {
        fn exchange(&mut self, tx: &[u8]) -> Vec<u8> {
            self.0.borrow_mut().push(tx.to_vec());
            vec![0; tx.len()]
        }
    }

    #[test]
    fn a_dropped_frame_gives_the_dsp_the_previous_frame() {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let mut l = LateSend::new(0x100, 90, 110);
        let mut peer = l.drop_peer(Box::new(Record(seen.clone())));
        l.observe(0x100, 0);
        peer.exchange(&[1, 1]);
        l.observe(0x100, 88);
        peer.exchange(&[2, 2]);
        l.observe(0x100, 188); // late: frame 3 is dropped
        peer.exchange(&[3, 3]);
        l.observe(0x100, 276);
        peer.exchange(&[4, 4]);
        assert_eq!(*seen.borrow(), [[1, 1], [2, 2], [2, 2], [4, 4]]);
        assert_eq!(l.report()["dropped"], 1);
        assert_eq!(l.report()["flagged"], 1);
    }
}
