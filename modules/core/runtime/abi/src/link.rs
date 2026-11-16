//! The DSP link API: the types a writer uses (docs/plans/runtime-and-services.md, R6). The core owns the link (`link.rs` of the runtime crate); a
//! service reaches it through `CoreApi::link_*`.
//!
//! Two kinds of writes:
//! - **Live values** (effect knobs, send amounts): `link_set(slot, value)`. The core keeps the newest value of each slot with a version and a dirty bit and
//!   sends what is not confirmed. A fast knob turn sends the last value only; a write that failed is sent again.
//! - **Streams** with an owner: `link_submit(owner, stream)` returns a `Ticket`, and `link_result(ticket)` gives the result of that request to its owner
//!   only. `Bulk` is a run of bytes (a buffer the caller keeps valid until the result, or bytes the queue owns); `Sequence` is a list of word writes in
//!   order (the type or pc word last), with a rollback on failure.
//!
//! The windows of `core/dsp-link`: 0 is the area (byte addresses), 1 the voice rows, 2 the effect rows (word offsets). The field is 2 bits wide; window 3 is
//! reserved (the profiler's statistics, P8) and no writer may use it.

/// The most steps of one `Sequence`: an effect row takes 18.
pub const SEQ_MAX: usize = 18;
/// The most bytes of an `Owned` bulk.
pub const OWNED_MAX: usize = 28;
/// The first window number no writer may use.
pub const WIN_RESERVED: u8 = 3;

/// One word of a window: `word` is the word offset from the start of the window (window 0: the byte address in the area).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(C)]
pub struct Slot {
    pub win: u8,
    pub word: u32,
}

impl Slot {
    pub const fn new(win: u32, word: u32) -> Slot {
        Slot {
            win: win as u8,
            word,
        }
    }
}

/// Who a request belongs to. Each owner's requests are sent in the order they were submitted, and only that owner can take their results.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Owner {
    /// The DSP loader: the image of a module and its rows.
    Loader,
    /// The sample tracks' table writes.
    Tracks,
    /// The core's own clears (`write_word_blocking`, the quiesce of a module).
    Core,
}

/// The number of owners.
pub const OWNERS: usize = 3;

impl Owner {
    pub const fn index(self) -> usize {
        match self {
            Owner::Loader => 0,
            Owner::Tracks => 1,
            Owner::Core => 2,
        }
    }
}

/// The handle of one submitted request.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(C)]
pub struct Ticket {
    pub owner: Owner,
    pub id: u16,
}

/// What `link_result` says. A terminal result is given once: the request is gone after it is taken.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum LinkResult {
    /// Queued or being sent.
    Pending,
    /// Every byte or word was taken by the DSP.
    Done,
    /// A transfer failed. The words the sequence had written and that have an undo word were undone first.
    Failed,
    /// A transfer failed and the undo did not complete either.
    RollbackFailed,
    /// The owner drained the link before the request ended.
    Cancelled,
    /// No such request for this owner: never submitted, already taken, or another owner's.
    Gone,
}

/// One word write of a `Sequence`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(C)]
pub struct Step {
    pub win: u8,
    pub flags: u8,
    pub at: u32,
    pub value: u32,
    /// The word to write when the sequence fails at or after this step and `STEP_UNDO` is set.
    pub undo: u32,
}

/// The step has an undo word.
pub const STEP_UNDO: u8 = 1;
/// When the transfer of this step is accepted, the sequence's `mark` byte becomes 1 (from here on the row may hold words in the DSP).
pub const STEP_MARK: u8 = 2;

const STEP0: Step = Step {
    win: 0,
    flags: 0,
    at: 0,
    value: 0,
    undo: 0,
};

/// Word writes in order. The last step publishes (the row's type or pc word); the steps before it are not visible to the DSP until then.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Sequence {
    pub n: u8,
    pub mark: *mut u8,
    pub steps: [Step; SEQ_MAX],
}

impl Sequence {
    pub const fn new() -> Sequence {
        Sequence {
            n: 0,
            mark: core::ptr::null_mut(),
            steps: [STEP0; SEQ_MAX],
        }
    }
    /// Append a write; false when the sequence is full.
    pub fn push(&mut self, win: u32, word: u32, value: u32) -> bool {
        self.push_flags(win, word, value, 0, 0)
    }
    /// Append a write with an undo word.
    pub fn push_undo(&mut self, win: u32, word: u32, value: u32, undo: u32) -> bool {
        self.push_flags(win, word, value, STEP_UNDO, undo)
    }
    fn push_flags(&mut self, win: u32, word: u32, value: u32, flags: u8, undo: u32) -> bool {
        if self.n as usize >= SEQ_MAX {
            return false;
        }
        self.steps[self.n as usize] = Step {
            win: win as u8,
            flags,
            at: word,
            value,
            undo,
        };
        self.n += 1;
        true
    }
    /// The steps from index `from` on set `*mark` when they are accepted.
    pub fn set_mark(&mut self, mark: *mut u8, from: usize) {
        self.mark = mark;
        let mut i = from;
        while i < SEQ_MAX {
            self.steps[i].flags |= STEP_MARK;
            i += 1;
        }
    }
}

/// What a request sends.
#[derive(Clone, Copy)]
#[repr(C, u8)]
pub enum Stream {
    /// `len` bytes at `src` to byte address `dst` of the area (window 0), 4-aligned. The core reads `src` in place: it must stay valid until the result is
    /// taken or `link_drain` of the owner has returned.
    Bulk { dst: u32, src: *const u8, len: u32 },
    /// The bytes are copied into the queue (at most `OWNED_MAX`, a multiple of 4), little-endian DSP words, to `win` at word offset (window 0: byte address) `dst`.
    Owned {
        win: u8,
        dst: u32,
        len: u8,
        bytes: [u8; OWNED_MAX],
    },
    /// Word writes in order.
    Sequence(Sequence),
}

impl Stream {
    /// An owned write of `words` (CF values; they travel as little-endian DSP words) to `win` at `dst`. `None` when there are too many.
    pub fn owned(win: u32, dst: u32, words: &[u32]) -> Option<Stream> {
        if words.is_empty() || words.len() * 4 > OWNED_MAX {
            return None;
        }
        let mut bytes = [0u8; OWNED_MAX];
        let mut i = 0;
        while i < words.len() {
            let b = words[i].to_le_bytes();
            bytes[4 * i] = b[0];
            bytes[4 * i + 1] = b[1];
            bytes[4 * i + 2] = b[2];
            bytes[4 * i + 3] = b[3];
            i += 1;
        }
        Some(Stream::Owned {
            win: win as u8,
            dst,
            len: (words.len() * 4) as u8,
            bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sequence_holds_an_effect_row_and_no_more() {
        let mut s = Sequence::new();
        for i in 0..SEQ_MAX as u32 {
            assert!(s.push(2, i, i));
        }
        assert!(!s.push(2, 99, 0));
        assert_eq!(s.n as usize, SEQ_MAX);
    }

    #[test]
    fn mark_covers_the_steps_from_the_given_one() {
        let mut s = Sequence::new();
        s.push(1, 0, 1);
        s.push_undo(1, 1, 2, 3);
        let mut flag = 0u8;
        s.set_mark(&mut flag, 1);
        assert_eq!(s.steps[0].flags & STEP_MARK, 0);
        assert_eq!(s.steps[1].flags, STEP_UNDO | STEP_MARK);
    }

    #[test]
    fn owned_bytes_are_little_endian_words() {
        match Stream::owned(0, 0x100, &[0x0102_0304, 5]).unwrap() {
            Stream::Owned { len, bytes, .. } => {
                assert_eq!(len, 8);
                assert_eq!(&bytes[..8], &[4, 3, 2, 1, 5, 0, 0, 0]);
            }
            _ => panic!(),
        }
        assert!(Stream::owned(0, 0, &[0; 8]).is_none());
    }
}
