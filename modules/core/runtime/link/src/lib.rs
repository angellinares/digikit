//! The DSP link of the core: one queue and one table of live values over the transport of `core/dsp-link` (docs/plans/runtime-and-services.md, R6).
//!
//! The wire protocol is unchanged: one transfer at a time (`dl_send`, `dl_send_win`, `dl_poll`), refused while the link is busy or sends its END frames.
//! What this module adds is who may use it and in what order:
//!
//! - **Live values**: `set(slot, value)` keeps the newest value of a word with a version and a dirty bit. `pump` sends a dirty word when the link is free.
//!   Several sets between two sends give one write of the newest value. The link reports DONE when the DSP has taken and copied the bytes, so the confirmed
//!   version is the version that was sent, kept here on the ColdFire side. A write that failed stays dirty and is sent again after a short hold. A DSP
//!   restart marks every word dirty again (`dsp_restarted`).
//! - **Requests with an owner**: `submit(owner, stream)` gives a `Ticket`; `result(ticket)` answers its owner only. A request is a bulk (bytes, sent in chunks
//!   of `CHUNK` so a live value can go between two chunks) or a sequence (word writes in order; the last word publishes). When a transfer of a sequence fails,
//!   the steps that have an undo word are undone in reverse order and the request fails. Requests are sent oldest first, so every owner's requests stay in order.
//! - **Priority**: an undo first, then a dirty live value, then the oldest request. A live edit therefore goes between bulk chunks and between sequence steps.
//! - `drain(owner)` cancels the owner's queued requests and waits for the transfer in flight.
//!
//! The pump runs in the UI task (`dkrt_tick`, and the blocking waits below). Nothing here runs in an interrupt. The transport is a trait so that host tests drive
//! the same code over a model of the link's states (`FakeLink`).
//!
//! If chains, not a `match`: dense arms become a PC-relative jump table, which is not ColdFire code.
#![cfg_attr(not(any(test, feature = "fake")), no_std)]
use dkrt_abi::link::*;

/// Requests the queue holds at once (the loader, the track writer, the core's clears and one spare).
pub const REQ_MAX: usize = 3;
/// Live words: 4 effect rows of 8 page words (7 knobs and MIX) and 4 send amounts.
pub const SLOT_MAX: usize = 48;
/// The bytes of one transfer of a bulk. A multiple of 4.
pub const CHUNK: u32 = 4096;
/// Pump passes a live word waits after a failed write before it is sent again.
const HOLD: u8 = 4;
/// Polls a blocking wait spins before it gives up (the same bound as the loader's one-word write had).
const SPIN: u32 = 4_000_000;

/// What the link answered to a send.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sent {
    Accepted,
    /// Busy or ending: try again later.
    Refused,
    /// Never: an area send with a bad destination, or while the loader is retained (the answer -2 of `dl_send`).
    Hard,
}

/// The state of the transfer in flight.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Poll {
    Busy,
    Done,
    /// The link's error code.
    Error(u32),
}

/// The bytes of `core/dsp-link` the queue needs, and the sink of its events.
pub trait Transport {
    /// Start a transfer into window `win` (0: the area, `dst` a byte address; 1 and 2: `dst` a word offset). `buf` is read in place until the poll says Done or Error.
    unsafe fn send(&mut self, win: u32, dst: u32, buf: *const u8, len: u32) -> Sent;
    unsafe fn poll(&mut self) -> Poll;
    /// One event of the log (`what` is an `EV_*`). Called on every event: a transport without a sink returns at once.
    fn event(&mut self, _what: u32, _a: [u32; 4]) {}
}

/// The events (`DSP_LINK` of the log): args are `[what, a, b, c, d]`.
/// SUBMIT `[owner, id, kind | steps << 8, len]`, START `[owner, id, 0, 0]`, DONE `[owner, id, 0, 0]`, ERROR `[owner, id, code, position]`,
/// RESEND `[slot index, version, attempt, 0]`, LIVE (a live word taken) `[slot index, version, win, word]`.
/// A live word has the id `0x8000 | slot index` and the owner 0xff.
pub const EV_SUBMIT: u32 = 1;
pub const EV_START: u32 = 2;
pub const EV_DONE: u32 = 3;
pub const EV_ERROR: u32 = 4;
pub const EV_RESEND: u32 = 5;
pub const EV_LIVE: u32 = 6;

const R_FREE: u8 = 0;
const R_QUEUED: u8 = 1;
const R_ACTIVE: u8 = 2;
const R_ROLLBACK: u8 = 3;
const R_DONE: u8 = 4;
const R_FAILED: u8 = 5;
const R_RBFAILED: u8 = 6;

const K_BULK: u8 = 0;
const K_OWNED: u8 = 1;
const K_SEQ: u8 = 2;

const F_NONE: u8 = 0;
const F_LIVE: u8 = 1;
const F_CHUNK: u8 = 2;
const F_STEP: u8 = 3;
const F_UNDO: u8 = 4;

#[derive(Clone, Copy)]
struct Live {
    used: bool,
    dirty: bool,
    has_conf: bool,
    failed: bool,
    win: u8,
    hold: u8,
    word: u32,
    value: u32,
    ver: u16,
    conf_ver: u16,
    conf_value: u32,
    tries: u8,
}
const LIVE0: Live = Live {
    used: false,
    dirty: false,
    has_conf: false,
    failed: false,
    win: 0,
    hold: 0,
    word: 0,
    value: 0,
    ver: 0,
    conf_ver: 0,
    conf_value: 0,
    tries: 0,
};

#[derive(Clone, Copy)]
struct Req {
    state: u8,
    owner: u8,
    kind: u8,
    cancel: bool,
    win: u8,
    id: u16,
    order: u32,
    dst: u32,
    src: *const u8,
    len: u32,
    off: u32,
    pos: u8,
    rb: i8,
    code: u32,
    bytes: [u8; OWNED_MAX],
    seq: Sequence,
}
const REQ0: Req = Req {
    state: R_FREE,
    owner: 0,
    kind: K_BULK,
    cancel: false,
    win: 0,
    id: 0,
    order: 0,
    dst: 0,
    src: core::ptr::null(),
    len: 0,
    off: 0,
    pos: 0,
    rb: 0,
    code: 0,
    bytes: [0; OWNED_MAX],
    seq: Sequence::new(),
};

/// The transfer in flight.
#[derive(Clone, Copy)]
struct Flight {
    kind: u8,
    /// The live slot or the request index.
    idx: u8,
    win: u8,
    ver: u16,
    /// The word of a live write.
    word: u32,
    /// The value of a live write; the bytes of a chunk.
    value: u32,
}
const FLIGHT0: Flight = Flight {
    kind: F_NONE,
    idx: 0,
    win: 0,
    ver: 0,
    word: 0,
    value: 0,
};

/// Counters of what the link did, for `DKRT_DSP`-style peeks and the tests.
#[derive(Clone, Copy, Default, Debug)]
pub struct Stats {
    pub live_sent: u32,
    pub live_errors: u32,
    pub req_done: u32,
    pub req_failed: u32,
}

pub struct Link<T: Transport> {
    pub t: T,
    live: [Live; SLOT_MAX],
    reqs: [Req; REQ_MAX],
    next_id: u16,
    next_order: u32,
    cursor: u8,
    flight: Flight,
    /// The bytes of the transfer in flight when they are not in the request (a word, an owned write).
    buf: [u8; OWNED_MAX],
    pub stats: Stats,
    /// Polls a blocking wait spins before it gives up (0: `SPIN`; a host test sets a small bound).
    pub spin: u32,
}

impl<T: Transport> Link<T> {
    pub const fn new(t: T) -> Self {
        Link {
            t,
            live: [LIVE0; SLOT_MAX],
            reqs: [REQ0; REQ_MAX],
            next_id: 0,
            next_order: 0,
            cursor: 0,
            flight: FLIGHT0,
            buf: [0; OWNED_MAX],
            stats: Stats {
                live_sent: 0,
                live_errors: 0,
                req_done: 0,
                req_failed: 0,
            },
            spin: 0,
        }
    }

    fn bound(&self) -> u32 {
        if self.spin == 0 {
            SPIN
        } else {
            self.spin
        }
    }

    // ---- live values -------------------------------------------------------------------------------------------------------

    fn find_live(&self, slot: Slot) -> Option<usize> {
        let mut i = 0;
        while i < SLOT_MAX {
            let e = &self.live[i];
            if e.used && e.win == slot.win && e.word == slot.word {
                return Some(i);
            }
            i += 1;
        }
        None
    }

    /// Set the live value of `slot`. The same value again changes nothing.
    pub fn set(&mut self, slot: Slot, value: u32) {
        if slot.win >= WIN_RESERVED {
            return;
        }
        if let Some(i) = self.find_live(slot) {
            let e = &mut self.live[i];
            if e.value != value {
                e.value = value;
                e.ver = e.ver.wrapping_add(1);
                e.dirty = true;
            }
            return;
        }
        let mut i = 0;
        while i < SLOT_MAX {
            if !self.live[i].used {
                self.live[i] = Live {
                    used: true,
                    dirty: true,
                    win: slot.win,
                    word: slot.word,
                    value,
                    ver: 1,
                    ..LIVE0
                };
                return;
            }
            i += 1;
        }
    }

    /// The value of `slot` the DSP has taken.
    pub fn confirmed(&self, slot: Slot) -> Option<u32> {
        match self.find_live(slot) {
            Some(i) if self.live[i].has_conf => Some(self.live[i].conf_value),
            _ => None,
        }
    }

    /// Whether `slot` has a value the DSP has not taken yet.
    pub fn dirty(&self, slot: Slot) -> bool {
        match self.find_live(slot) {
            Some(i) => self.live[i].dirty,
            None => false,
        }
    }

    pub fn errors(&self) -> u32 {
        self.stats.live_errors
    }

    /// Drop the live values of `count` words from `first` of window `win`.
    pub fn forget(&mut self, win: u32, first: u32, count: u32) {
        let mut i = 0;
        while i < SLOT_MAX {
            let e = &mut self.live[i];
            if e.used && e.win as u32 == win && e.word >= first && e.word - first < count {
                e.used = false;
            }
            i += 1;
        }
    }

    /// The DSP restarted: it holds none of the values. Every word is dirty again and none counts as confirmed.
    pub fn dsp_restarted(&mut self) {
        let mut i = 0;
        while i < SLOT_MAX {
            let e = &mut self.live[i];
            if e.used {
                e.dirty = true;
                e.has_conf = false;
                e.hold = 0;
            }
            i += 1;
        }
    }

    // ---- requests ----------------------------------------------------------------------------------------------------------

    /// Queue a request. `None` when the queue is full or the request is not valid.
    pub fn submit(&mut self, owner: Owner, stream: &Stream) -> Option<Ticket> {
        let mut r = REQ0;
        r.owner = owner.index() as u8;
        let (kind, steps, len) = if let &Stream::Bulk { dst, src, len } = stream {
            if src.is_null() || len == 0 || len & 3 != 0 || dst & 3 != 0 {
                return None;
            }
            r.kind = K_BULK;
            r.dst = dst;
            r.src = src;
            r.len = len;
            (K_BULK, 0, len)
        } else if let Stream::Owned {
            win,
            dst,
            len,
            bytes,
        } = stream
        {
            let (win, dst, len) = (*win, *dst, *len);
            if len == 0 || len & 3 != 0 || len as usize > OWNED_MAX || win >= WIN_RESERVED {
                return None;
            }
            r.kind = K_OWNED;
            r.win = win;
            r.dst = dst;
            r.len = len as u32;
            r.bytes = *bytes;
            (K_OWNED, 0, len as u32)
        } else if let Stream::Sequence(seq) = stream {
            if seq.n == 0 || seq.n as usize > SEQ_MAX {
                return None;
            }
            let mut i = 0;
            while i < seq.n as usize {
                if seq.steps[i].win >= WIN_RESERVED {
                    return None;
                }
                i += 1;
            }
            r.kind = K_SEQ;
            r.seq = *seq;
            (K_SEQ, seq.n as u32, seq.n as u32 * 4)
        } else {
            return None;
        };
        let mut i = 0;
        while i < REQ_MAX {
            if self.reqs[i].state == R_FREE {
                // ids start at 1 and skip 0; `next_id` is the last one given, so the whole queue is zero at start (it lives in .bss)
                let id = if self.next_id == u16::MAX {
                    1
                } else {
                    self.next_id + 1
                };
                self.next_id = id;
                r.id = id;
                r.order = self.next_order;
                self.next_order = self.next_order.wrapping_add(1);
                r.state = R_QUEUED;
                self.reqs[i] = r;
                self.t.event(
                    EV_SUBMIT,
                    [
                        owner.index() as u32,
                        id as u32,
                        kind as u32 | steps << 8,
                        len,
                    ],
                );
                return Some(Ticket { owner, id });
            }
            i += 1;
        }
        None
    }

    fn find_req(&self, t: Ticket) -> Option<usize> {
        let mut i = 0;
        while i < REQ_MAX {
            let r = &self.reqs[i];
            if r.state != R_FREE && r.id == t.id && r.owner == t.owner.index() as u8 {
                return Some(i);
            }
            i += 1;
        }
        None
    }

    /// The result of a request, for its owner only. A terminal result is given once.
    pub fn result(&mut self, t: Ticket) -> LinkResult {
        let i = match self.find_req(t) {
            Some(i) => i,
            None => return LinkResult::Gone,
        };
        let s = self.reqs[i].state;
        if s == R_QUEUED || s == R_ACTIVE || s == R_ROLLBACK {
            return LinkResult::Pending;
        }
        self.reqs[i].state = R_FREE;
        if s == R_DONE {
            LinkResult::Done
        } else if s == R_FAILED {
            LinkResult::Failed
        } else {
            LinkResult::RollbackFailed
        }
    }

    /// Cancel the owner's requests and wait for its transfer in flight. `false` when that did not end within the spin bound.
    pub unsafe fn drain(&mut self, owner: Owner) -> bool {
        let o = owner.index() as u8;
        let fl = self.flight;
        let mut i = 0;
        let mut waiting = false;
        while i < REQ_MAX {
            let r = &mut self.reqs[i];
            if r.state != R_FREE && r.owner == o {
                if fl.kind >= F_CHUNK && fl.idx as usize == i {
                    // its transfer is on the wire: settle it first, then drop the request
                    r.cancel = true;
                    waiting = true;
                } else {
                    r.state = R_FREE;
                }
            }
            i += 1;
        }
        let mut n = 0;
        while waiting && self.flight.kind != F_NONE && self.reqs[fl.idx as usize].cancel {
            match self.t.poll() {
                Poll::Busy => {}
                Poll::Done => self.settle(true, 0),
                Poll::Error(c) => self.settle(false, c),
            }
            n += 1;
            if n > self.bound() {
                return false;
            }
        }
        true
    }

    /// Give up a request nobody waits for: its slot is freed now, or when its transfer on the wire ends.
    fn abandon(&mut self, t: Ticket) {
        if let Some(i) = self.find_req(t) {
            if self.flight.kind >= F_CHUNK && self.flight.idx as usize == i {
                self.reqs[i].cancel = true;
            } else {
                self.reqs[i].state = R_FREE;
            }
        }
    }

    /// Send `seq` for `owner` and wait for the result.
    pub unsafe fn run(&mut self, owner: Owner, seq: &Sequence) -> bool {
        let stream = Stream::Sequence(*seq);
        let mut n = 0;
        let ticket = loop {
            if let Some(t) = self.submit(owner, &stream) {
                break t;
            }
            self.pump();
            n += 1;
            if n > self.bound() {
                return false;
            }
        };
        loop {
            self.pump();
            let r = self.result(ticket);
            if r == LinkResult::Done {
                return true;
            }
            if r != LinkResult::Pending {
                return false;
            }
            n += 1;
            if n > self.bound() {
                self.abandon(ticket);
                return false;
            }
        }
    }

    /// A transfer of `owner` on the wire, for the loader's host tests: its request ends with whatever the transport answers to the next poll.
    #[cfg(any(test, feature = "fake"))]
    pub fn test_in_flight(&mut self, owner: Owner) -> Ticket {
        let t = self
            .submit(
                owner,
                &Stream::Sequence({
                    let mut s = Sequence::new();
                    s.push(1, 0, 0);
                    s
                }),
            )
            .unwrap();
        let i = self.find_req(t).unwrap();
        self.reqs[i].state = R_ACTIVE;
        self.flight = Flight {
            kind: F_STEP,
            idx: i as u8,
            win: 0,
            ver: 0,
            word: 0,
            value: 4,
        };
        t
    }

    // ---- the pump ----------------------------------------------------------------------------------------------------------

    /// Settle the transfer in flight, then start the next one when the link takes it. Once per UI pass.
    pub unsafe fn pump(&mut self) {
        if self.flight.kind != F_NONE {
            match self.t.poll() {
                Poll::Busy => return,
                Poll::Done => self.settle(true, 0),
                Poll::Error(c) => self.settle(false, c),
            }
        }
        self.start();
    }

    /// The transfer in flight ended.
    fn settle(&mut self, ok: bool, code: u32) {
        let f = self.flight;
        self.flight = FLIGHT0;
        let ix = f.idx as usize;
        if f.kind == F_LIVE {
            let e = &mut self.live[ix];
            if !e.used || e.win != f.win || e.word != f.word {
                return; // the word was forgotten while its write was on the wire
            }
            if ok {
                e.has_conf = true;
                e.conf_ver = f.ver;
                e.conf_value = f.value;
                if e.ver == f.ver {
                    e.dirty = false;
                }
                e.failed = false;
                e.tries = 0;
                self.stats.live_sent += 1;
                self.t
                    .event(EV_LIVE, [f.idx as u32, f.ver as u32, f.win as u32, f.word]);
            } else {
                e.hold = HOLD;
                e.failed = true;
                self.stats.live_errors += 1;
                self.t
                    .event(EV_ERROR, [0xff, 0x8000 | f.idx as u32, code, f.idx as u32]);
            }
            return;
        }
        let r = &mut self.reqs[ix];
        if r.cancel {
            r.state = R_FREE;
            return;
        }
        if f.kind == F_CHUNK {
            if ok {
                r.off += f.value;
                if r.off >= r.len {
                    r.state = R_DONE;
                }
            } else {
                r.state = R_FAILED;
                r.code = code;
            }
        } else if f.kind == F_STEP {
            if ok {
                r.pos += 1;
                if r.pos >= r.seq.n {
                    r.state = R_DONE;
                }
            } else {
                r.code = code;
                // undo what may have landed, from the failed step back
                r.rb = r.pos as i8;
                r.state = R_ROLLBACK;
                Self::next_undo(r);
            }
        } else if f.kind == F_UNDO {
            if ok {
                r.rb -= 1;
                Self::next_undo(r);
            } else {
                r.state = R_RBFAILED;
            }
        }
        let (st, owner, id, code, pos) =
            (r.state, r.owner as u32, r.id as u32, r.code, r.pos as u32);
        if st == R_DONE {
            self.stats.req_done += 1;
            self.t.event(EV_DONE, [owner, id, 0, 0]);
        } else if st == R_FAILED || st == R_RBFAILED {
            self.stats.req_failed += 1;
            self.t.event(EV_ERROR, [owner, id, code, pos]);
        }
    }

    /// Move `rb` to the next step with an undo word; with none left the rollback is complete and the request failed.
    fn next_undo(r: &mut Req) {
        while r.rb >= 0 && r.seq.steps[r.rb as usize].flags & STEP_UNDO == 0 {
            r.rb -= 1;
        }
        if r.rb < 0 {
            r.state = R_FAILED;
        }
    }

    unsafe fn start(&mut self) {
        // 1. an undo: a half-published row must not stay visible
        let mut best = REQ_MAX;
        let mut i = 0;
        while i < REQ_MAX {
            if self.reqs[i].state == R_ROLLBACK
                && (best == REQ_MAX || self.reqs[i].order < self.reqs[best].order)
            {
                best = i;
            }
            i += 1;
        }
        if best != REQ_MAX {
            let s = self.reqs[best].seq.steps[self.reqs[best].rb as usize];
            self.buf[..4].copy_from_slice(&s.undo.to_le_bytes());
            let ok = self.t.send(s.win as u32, s.at, self.buf.as_ptr(), 4);
            if ok == Sent::Accepted {
                self.flight = Flight {
                    kind: F_UNDO,
                    idx: best as u8,
                    win: 0,
                    ver: 0,
                    word: 0,
                    value: 0,
                };
            } else if ok == Sent::Hard {
                self.reqs[best].state = R_RBFAILED;
            }
            return;
        }
        // 2. a dirty live word
        let mut i = 0;
        while i < SLOT_MAX {
            if self.live[i].hold > 0 {
                self.live[i].hold -= 1;
            }
            i += 1;
        }
        let mut n = 0;
        let mut j = self.cursor as usize;
        while n < SLOT_MAX {
            let e = self.live[j];
            if e.used && e.dirty && e.hold == 0 {
                self.buf[..4].copy_from_slice(&e.value.to_le_bytes());
                let ok = self.t.send(e.win as u32, e.word, self.buf.as_ptr(), 4);
                if ok == Sent::Accepted {
                    if e.failed {
                        self.t
                            .event(EV_RESEND, [j as u32, e.ver as u32, e.tries as u32 + 1, 0]);
                        self.live[j].tries = e.tries.saturating_add(1);
                    }
                    self.flight = Flight {
                        kind: F_LIVE,
                        idx: j as u8,
                        win: e.win,
                        ver: e.ver,
                        word: e.word,
                        value: e.value,
                    };
                    self.cursor = if j + 1 == SLOT_MAX { 0 } else { j as u8 + 1 };
                } else if ok == Sent::Hard {
                    self.live[j].hold = HOLD;
                    self.stats.live_errors += 1;
                }
                return;
            }
            j = if j + 1 == SLOT_MAX { 0 } else { j + 1 };
            n += 1;
        }
        // 3. the oldest request
        let mut best = REQ_MAX;
        let mut i = 0;
        while i < REQ_MAX {
            let s = self.reqs[i].state;
            if (s == R_QUEUED || s == R_ACTIVE)
                && (best == REQ_MAX || self.reqs[i].order < self.reqs[best].order)
            {
                best = i;
            }
            i += 1;
        }
        if best == REQ_MAX {
            return;
        }
        let r = self.reqs[best];
        let (owner, id) = (r.owner as u32, r.id as u32);
        let (win, dst, buf, len, kind, step);
        if r.kind == K_BULK {
            let n = if r.len - r.off > CHUNK {
                CHUNK
            } else {
                r.len - r.off
            };
            (win, dst, buf, len, kind, step) =
                (0, r.dst + r.off, r.src.add(r.off as usize), n, F_CHUNK, 0);
        } else if r.kind == K_OWNED {
            self.buf[..OWNED_MAX].copy_from_slice(&r.bytes);
            (win, dst, buf, len, kind, step) =
                (r.win as u32, r.dst, self.buf.as_ptr(), r.len, F_CHUNK, 0);
        } else {
            let s = r.seq.steps[r.pos as usize];
            self.buf[..4].copy_from_slice(&s.value.to_le_bytes());
            (win, dst, buf, len, kind, step) =
                (s.win as u32, s.at, self.buf.as_ptr(), 4, F_STEP, s.flags);
        }
        let ok = self.t.send(win, dst, buf, len);
        if ok == Sent::Accepted {
            if r.state == R_QUEUED {
                self.reqs[best].state = R_ACTIVE;
                self.t.event(EV_START, [owner, id, 0, 0]);
            }
            if step & STEP_MARK != 0 && !r.seq.mark.is_null() {
                *r.seq.mark = 1;
            }
            self.flight = Flight {
                kind,
                idx: best as u8,
                win: 0,
                ver: 0,
                word: 0,
                value: len,
            };
        } else if ok == Sent::Hard {
            self.reqs[best].state = R_FAILED;
            self.reqs[best].code = u32::MAX;
            self.stats.req_failed += 1;
            self.t.event(EV_ERROR, [owner, id, u32::MAX, 0]);
        }
    }
}

// ---- host tests: a model of the link's states ------------------------------------------------------------------------------

#[cfg(any(test, feature = "fake"))]
pub mod fake {
    use super::*;
    extern crate std;
    use std::collections::VecDeque;
    use std::vec::Vec;

    /// One transfer the model accepted.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Wire {
        pub win: u32,
        pub dst: u32,
        pub bytes: Vec<u8>,
    }

    /// `core/dsp-link` as the queue sees it: a send is refused while a transfer runs (BUSY) or the END frames follow it; the state turns DONE or ERROR after
    /// `TRANSFER` blocks and stays so until the next send. Every call of the model is one block.
    pub struct FakeLink {
        pub now: u32,
        busy_until: u32,
        end_until: u32,
        result: Option<Poll>,
        pub wire: Vec<Wire>,
        /// The answer for each accepted transfer, in order (`None`: DONE; `Some(code)`: ERROR); DONE when empty.
        pub script: VecDeque<Option<u32>>,
        pub refuse: bool,
        pub hard: bool,
        pub events: Vec<(u32, [u32; 4])>,
    }

    pub const TRANSFER: u32 = 3;
    pub const END_FRAMES: u32 = 2;

    impl FakeLink {
        pub fn new() -> Self {
            FakeLink {
                now: 0,
                busy_until: 0,
                end_until: 0,
                result: None,
                wire: Vec::new(),
                script: VecDeque::new(),
                refuse: false,
                hard: false,
                events: Vec::new(),
            }
        }
        pub fn words(&self) -> Vec<(u32, u32, u32)> {
            self.wire
                .iter()
                .map(|w| {
                    (
                        w.win,
                        w.dst,
                        u32::from_le_bytes([w.bytes[0], w.bytes[1], w.bytes[2], w.bytes[3]]),
                    )
                })
                .collect()
        }
    }

    impl Transport for FakeLink {
        unsafe fn send(&mut self, win: u32, dst: u32, buf: *const u8, len: u32) -> Sent {
            self.now += 1;
            if self.hard {
                return Sent::Hard;
            }
            if self.refuse || self.now < self.busy_until || self.now < self.end_until {
                return Sent::Refused;
            }
            self.wire.push(Wire {
                win,
                dst,
                bytes: core::slice::from_raw_parts(buf, len as usize).to_vec(),
            });
            self.busy_until = self.now + TRANSFER;
            self.end_until = self.busy_until + END_FRAMES;
            self.result = Some(match self.script.pop_front().flatten() {
                None => Poll::Done,
                Some(c) => Poll::Error(c),
            });
            Sent::Accepted
        }
        unsafe fn poll(&mut self) -> Poll {
            self.now += 1;
            if self.now < self.busy_until {
                return Poll::Busy;
            }
            self.result.unwrap_or(Poll::Busy)
        }
        fn event(&mut self, what: u32, a: [u32; 4]) {
            self.events.push((what, a));
        }
    }

    pub fn link() -> Link<FakeLink> {
        Link::new(FakeLink::new())
    }
}

#[cfg(test)]
mod tests {
    use super::fake::*;
    use super::*;
    use std::vec;
    use std::vec::Vec;

    const S: Slot = Slot { win: 2, word: 53 };
    const S2: Slot = Slot { win: 2, word: 54 };

    unsafe fn settle_all(l: &mut Link<FakeLink>) {
        for _ in 0..200 {
            l.pump();
        }
    }

    fn seq(steps: &[(u32, u32, u32)]) -> Sequence {
        let mut s = Sequence::new();
        for &(w, a, v) in steps {
            s.push(w, a, v);
        }
        s
    }

    #[test]
    fn several_sets_before_a_send_give_one_write_of_the_newest_value() {
        let mut l = link();
        l.set(S, 1);
        l.set(S, 2);
        l.set(S, 3);
        unsafe { settle_all(&mut l) };
        assert_eq!(l.t.words(), vec![(2, 53, 3)]);
        assert_eq!(l.confirmed(S), Some(3));
        assert!(!l.dirty(S));
    }

    #[test]
    fn a_set_during_a_transfer_is_sent_after_it_and_skips_the_values_between() {
        let mut l = link();
        l.set(S, 1);
        unsafe {
            l.pump(); // 1 is on the wire
            l.set(S, 2);
            l.set(S, 3);
            settle_all(&mut l);
        }
        assert_eq!(l.t.words(), vec![(2, 53, 1), (2, 53, 3)]);
        assert_eq!(l.confirmed(S), Some(3));
    }

    #[test]
    fn the_same_value_again_sends_nothing() {
        let mut l = link();
        l.set(S, 7);
        unsafe { settle_all(&mut l) };
        l.set(S, 7);
        unsafe { settle_all(&mut l) };
        assert_eq!(l.t.wire.len(), 1);
    }

    #[test]
    fn a_refused_live_write_stays_dirty_and_goes_out_when_the_link_takes_it() {
        let mut l = link();
        l.t.refuse = true;
        l.set(S, 9);
        unsafe { settle_all(&mut l) };
        assert!(l.t.wire.is_empty());
        assert!(l.dirty(S));
        assert_eq!(l.confirmed(S), None);
        l.t.refuse = false;
        unsafe { settle_all(&mut l) };
        assert_eq!(l.t.words(), vec![(2, 53, 9)]);
        assert_eq!(l.confirmed(S), Some(9));
    }

    #[test]
    fn an_errored_live_write_is_sent_again_and_logged_as_a_resend() {
        let mut l = link();
        l.t.script.push_back(Some(92));
        l.set(S, 5);
        unsafe { settle_all(&mut l) };
        assert_eq!(l.t.words(), vec![(2, 53, 5), (2, 53, 5)]);
        assert_eq!(l.confirmed(S), Some(5));
        assert_eq!(l.errors(), 1);
        let kinds: Vec<u32> = l.t.events.iter().map(|e| e.0).collect();
        assert_eq!(kinds, vec![EV_ERROR, EV_RESEND, EV_LIVE]);
        assert_eq!(l.t.events[0].1[2], 92);
    }

    #[test]
    fn a_dsp_restart_marks_every_word_dirty_and_none_confirmed() {
        let mut l = link();
        l.set(S, 1);
        l.set(S2, 2);
        unsafe { settle_all(&mut l) };
        assert_eq!((l.confirmed(S), l.confirmed(S2)), (Some(1), Some(2)));
        l.dsp_restarted();
        assert_eq!((l.confirmed(S), l.confirmed(S2)), (None, None));
        assert!(l.dirty(S) && l.dirty(S2));
        unsafe { settle_all(&mut l) };
        assert_eq!(l.t.wire.len(), 4);
        assert_eq!((l.confirmed(S), l.confirmed(S2)), (Some(1), Some(2)));
    }

    #[test]
    fn a_forgotten_word_is_not_sent() {
        let mut l = link();
        l.set(S, 1);
        l.forget(2, 48, 24);
        unsafe { settle_all(&mut l) };
        assert!(l.t.wire.is_empty());
        assert_eq!(l.confirmed(S), None);
    }

    /// Finding 129: a writer that has finished but has not taken its result must not see the next writer's result.
    #[test]
    fn a_result_belongs_to_its_owner_and_never_to_another_writer() {
        let mut l = link();
        l.t.script.push_back(None); // A's word is taken
        l.t.script.push_back(Some(91)); // B's word fails
        let a = l
            .submit(Owner::Loader, &Stream::Sequence(seq(&[(1, 10, 1)])))
            .unwrap();
        let b = l
            .submit(Owner::Tracks, &Stream::owned(0, 0x100, &[7]).unwrap())
            .unwrap();
        unsafe { settle_all(&mut l) }; // both ended before A looks
        assert_eq!(
            l.result(Ticket {
                owner: Owner::Tracks,
                id: a.id
            }),
            LinkResult::Gone,
            "another owner cannot take A's result"
        );
        assert_eq!(l.result(a), LinkResult::Done);
        assert_eq!(l.result(a), LinkResult::Gone, "a result is given once");
        assert_eq!(l.result(b), LinkResult::Failed);
    }

    #[test]
    fn the_owners_requests_go_out_in_the_order_they_were_submitted() {
        let mut l = link();
        let a1 = l
            .submit(
                Owner::Loader,
                &Stream::Sequence(seq(&[(1, 1, 1), (1, 2, 2)])),
            )
            .unwrap();
        let b1 = l
            .submit(Owner::Tracks, &Stream::owned(2, 5, &[5]).unwrap())
            .unwrap();
        let a2 = l
            .submit(Owner::Loader, &Stream::Sequence(seq(&[(2, 3, 3)])))
            .unwrap();
        unsafe { settle_all(&mut l) };
        assert_eq!(
            l.t.words(),
            vec![(1, 1, 1), (1, 2, 2), (2, 5, 5), (2, 3, 3)]
        );
        assert_eq!(
            (l.result(a1), l.result(b1), l.result(a2)),
            (LinkResult::Done, LinkResult::Done, LinkResult::Done)
        );
    }

    #[test]
    fn a_sequence_sends_its_steps_in_order_and_the_publishing_word_last() {
        let mut l = link();
        let mut flag = 0u8;
        let mut s = Sequence::new();
        for w in 0..5 {
            s.push(1, 24 * 3 + 2 + w, w);
        }
        s.push_undo(1, 24 * 3, 17, 0xffff_ffff); // the type word, last
        s.set_mark(&mut flag, 5);
        let t = l.submit(Owner::Loader, &Stream::Sequence(s)).unwrap();
        unsafe { settle_all(&mut l) };
        let words = l.t.words();
        assert_eq!(words.len(), 6);
        assert_eq!(words[5], (1, 72, 17));
        assert_eq!(l.result(t), LinkResult::Done);
        assert_eq!(
            flag, 1,
            "the mark is set when the publishing word is accepted"
        );
    }

    #[test]
    fn a_failed_publishing_word_is_undone_and_the_request_fails() {
        let mut l = link();
        let mut s = Sequence::new();
        s.push(2, 100, 1);
        s.push(2, 101, 2);
        s.push_undo(2, 102, 3, 0xffff_ffff);
        for _ in 0..2 {
            l.t.script.push_back(None);
        }
        l.t.script.push_back(Some(92)); // the last word errors
        let t = l.submit(Owner::Loader, &Stream::Sequence(s)).unwrap();
        unsafe { settle_all(&mut l) };
        assert_eq!(
            l.t.words(),
            vec![(2, 100, 1), (2, 101, 2), (2, 102, 3), (2, 102, 0xffff_ffff)]
        );
        assert_eq!(l.result(t), LinkResult::Failed);
    }

    #[test]
    fn the_undo_words_run_in_reverse_order_of_the_steps() {
        let mut l = link();
        let mut s = Sequence::new();
        s.push_undo(2, 10, 1, 100);
        s.push_undo(2, 11, 2, 101);
        s.push_undo(2, 12, 3, 102);
        s.push(2, 13, 4);
        l.t.script.extend([None, None, None, Some(1)]);
        let t = l.submit(Owner::Loader, &Stream::Sequence(s)).unwrap();
        unsafe { settle_all(&mut l) };
        let w = l.t.words();
        assert_eq!(&w[4..], &[(2, 12, 102), (2, 11, 101), (2, 10, 100)]);
        assert_eq!(l.result(t), LinkResult::Failed);
    }

    #[test]
    fn a_failed_step_without_undo_words_fails_at_once() {
        let mut l = link();
        l.t.script.push_back(Some(3));
        let t = l
            .submit(
                Owner::Loader,
                &Stream::Sequence(seq(&[(2, 1, 1), (2, 2, 2)])),
            )
            .unwrap();
        unsafe { settle_all(&mut l) };
        assert_eq!(l.t.wire.len(), 1, "the second step is not sent");
        assert_eq!(l.result(t), LinkResult::Failed);
    }

    #[test]
    fn a_failed_undo_is_reported_as_a_rollback_that_did_not_complete() {
        let mut l = link();
        let mut s = Sequence::new();
        s.push_undo(2, 10, 1, 0);
        l.t.script.extend([Some(1), Some(2)]);
        let t = l.submit(Owner::Loader, &Stream::Sequence(s)).unwrap();
        unsafe { settle_all(&mut l) };
        assert_eq!(l.result(t), LinkResult::RollbackFailed);
    }

    #[test]
    fn a_live_edit_goes_between_two_chunks_of_a_bulk() {
        let mut l = link();
        let data = vec![0xabu8; (2 * CHUNK + 8) as usize];
        let t = l
            .submit(
                Owner::Loader,
                &Stream::Bulk {
                    dst: 0x2000_0000,
                    src: data.as_ptr(),
                    len: data.len() as u32,
                },
            )
            .unwrap();
        unsafe {
            l.pump(); // chunk 1 is on the wire
            l.set(S, 42);
            settle_all(&mut l);
        }
        let w = &l.t.wire;
        assert_eq!(w.len(), 4);
        assert_eq!((w[0].win, w[0].bytes.len()), (0, CHUNK as usize));
        assert_eq!(
            (w[1].win, w[1].dst),
            (2, 53),
            "the live word goes before chunk 2"
        );
        assert_eq!(
            (w[2].dst, w[2].bytes.len()),
            (0x2000_0000 + CHUNK, CHUNK as usize)
        );
        assert_eq!((w[3].dst, w[3].bytes.len()), (0x2000_0000 + 2 * CHUNK, 8));
        assert_eq!(l.result(t), LinkResult::Done);
    }

    #[test]
    fn a_failed_chunk_fails_the_bulk_and_sends_no_more_of_it() {
        let mut l = link();
        l.t.script.push_back(Some(100));
        let data = vec![0u8; (CHUNK + 4) as usize];
        let t = l
            .submit(
                Owner::Loader,
                &Stream::Bulk {
                    dst: 0x2000_0000,
                    src: data.as_ptr(),
                    len: data.len() as u32,
                },
            )
            .unwrap();
        unsafe { settle_all(&mut l) };
        assert_eq!(l.t.wire.len(), 1);
        assert_eq!(l.result(t), LinkResult::Failed);
    }

    #[test]
    fn drain_cancels_queued_requests_waits_for_the_transfer_in_flight_and_keeps_other_owners() {
        let mut l = link();
        let data = vec![1u8; 2 * CHUNK as usize];
        let a1 = l
            .submit(
                Owner::Loader,
                &Stream::Bulk {
                    dst: 0x2000_0000,
                    src: data.as_ptr(),
                    len: data.len() as u32,
                },
            )
            .unwrap();
        let a2 = l
            .submit(Owner::Loader, &Stream::Sequence(seq(&[(1, 1, 1)])))
            .unwrap();
        let b = l
            .submit(Owner::Tracks, &Stream::owned(2, 5, &[5]).unwrap())
            .unwrap();
        unsafe {
            l.pump(); // chunk 1 of A's bulk is on the wire
            assert!(l.drain(Owner::Loader));
            assert_eq!(l.t.wire.len(), 1, "nothing more of A's bulk was sent");
            let before = l.t.wire.len();
            settle_all(&mut l);
            // only B's word follows; A's chunk 2 and sequence are gone
            assert_eq!(l.t.words()[before..], [(2, 5, 5)]);
        }
        assert_eq!(
            (l.result(a1), l.result(a2)),
            (LinkResult::Gone, LinkResult::Gone)
        );
        assert_eq!(l.result(b), LinkResult::Done);
    }

    #[test]
    fn a_blocking_run_returns_the_outcome_of_the_sequence() {
        let mut l = link();
        unsafe {
            assert!(l.run(Owner::Core, &seq(&[(1, 0, 0xffff_ffff), (1, 1, 2)])));
            l.t.script.push_back(Some(5));
            assert!(!l.run(Owner::Core, &seq(&[(1, 3, 3)])));
        }
    }

    #[test]
    fn a_full_queue_refuses_a_submit_until_a_result_is_taken() {
        let mut l = link();
        let one = Stream::Sequence(seq(&[(1, 1, 1)]));
        let mut ts = Vec::new();
        for _ in 0..REQ_MAX {
            ts.push(l.submit(Owner::Loader, &one).unwrap());
        }
        assert!(l.submit(Owner::Loader, &one).is_none());
        unsafe { settle_all(&mut l) };
        assert_eq!(l.result(ts[0]), LinkResult::Done);
        assert!(l.submit(Owner::Loader, &one).is_some());
    }

    #[test]
    fn a_request_for_the_reserved_window_or_an_odd_size_is_refused() {
        let mut l = link();
        assert!(l
            .submit(Owner::Core, &Stream::Sequence(seq(&[(3, 0, 0)])))
            .is_none());
        assert!(l
            .submit(Owner::Core, &Stream::Sequence(Sequence::new()))
            .is_none());
        assert!(l
            .submit(
                Owner::Core,
                &Stream::Bulk {
                    dst: 0x2000_0000,
                    src: [0u8; 6].as_ptr(),
                    len: 6
                }
            )
            .is_none());
        l.set(Slot { win: 3, word: 0 }, 1);
        unsafe { settle_all(&mut l) };
        assert!(l.t.wire.is_empty());
    }

    #[test]
    fn a_hard_refusal_fails_the_request_instead_of_waiting_for_ever() {
        let mut l = link();
        l.t.hard = true;
        let t = l
            .submit(Owner::Loader, &Stream::Sequence(seq(&[(1, 1, 1)])))
            .unwrap();
        unsafe { settle_all(&mut l) };
        assert_eq!(l.result(t), LinkResult::Failed);
    }

    #[test]
    fn requests_log_submit_start_done_and_error_events() {
        let mut l = link();
        l.t.script.push_back(None);
        l.t.script.push_back(Some(7));
        let a = l
            .submit(Owner::Loader, &Stream::Sequence(seq(&[(1, 1, 1)])))
            .unwrap();
        let b = l
            .submit(Owner::Tracks, &Stream::owned(2, 5, &[5]).unwrap())
            .unwrap();
        unsafe { settle_all(&mut l) };
        l.result(a);
        l.result(b);
        let kinds: Vec<u32> = l.t.events.iter().map(|e| e.0).collect();
        assert_eq!(
            kinds,
            vec![EV_SUBMIT, EV_SUBMIT, EV_START, EV_DONE, EV_START, EV_ERROR]
        );
    }
}
