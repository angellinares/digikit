//! The selection record: two copies on the card (sequence number, CRC), the choice between them, and the writes that keep one copy valid
//! if a write is cut. The record is the only thing the runtime writes to the card here, through `block_write`.

use crate::dlog;
use crate::platform::{block_read, block_write};
use crate::card;
use core::ptr::addr_of_mut;
use dkrt_sdk::sel::{self, Record};
use dkrt_sdk::*;

#[repr(C, align(16))]
pub struct Sector([u8; 512]);
#[no_mangle]
pub static mut DKRT_SECBUF: Sector = Sector([0; 512]);

/// The record this boot started from (or an empty one) and the bookkeeping the writes need.
pub(crate) static mut REC: Record = Record::empty();
pub(crate) static mut REC_VALID: bool = false;
pub(crate) static mut REC_COPY: u32 = 0;

fn secbuf() -> *mut u8 {
    unsafe { addr_of_mut!(DKRT_SECBUF) as *mut u8 }
}

/// The selection index of the module being loaded by the boot hook (`u32::MAX`: none). Written by the boot, read by the type assignment and the loader.
pub static mut BOOT_IDX: u32 = u32::MAX;

/// One selection copy as read from the card.
#[derive(Clone, Copy)]
enum CopyRead {
    /// The read worked and the record is good.
    Valid(Record),
    /// The read worked and the record is bad.
    Invalid,
    /// The block read failed.
    ReadError,
}

/// The selection as the two copies give it.
#[derive(Clone, Copy)]
pub(crate) enum Choice {
    /// The valid copy with the higher sequence number (A when equal), the copy it is in, and whether the other copy is valid too.
    Use { record: Record, copy: u32, other_valid: bool },
    /// No valid copy and no read error: nothing was ever saved.
    Empty,
    /// No valid copy and at least one read error: the record cannot be known.
    Unreadable,
}

/// The choice from the two copies. A valid copy wins even when the other copy could not be read.
fn choose(a: CopyRead, b: CopyRead) -> Choice {
    match (a, b) {
        (CopyRead::Valid(x), CopyRead::Valid(y)) => {
            if y.seq > x.seq {
                Choice::Use { record: y, copy: 1, other_valid: true }
            } else {
                Choice::Use { record: x, copy: 0, other_valid: true }
            }
        }
        (CopyRead::Valid(x), _) => Choice::Use { record: x, copy: 0, other_valid: false },
        (_, CopyRead::Valid(y)) => Choice::Use { record: y, copy: 1, other_valid: false },
        (CopyRead::ReadError, _) | (_, CopyRead::ReadError) => Choice::Unreadable,
        _ => Choice::Empty,
    }
}

/// The copy `selection_set` writes first: the one that is not the only valid copy, so that a cut write leaves a valid copy.
fn first_write(c: &Choice) -> u32 {
    match c {
        Choice::Use { copy: 0, other_valid: false, .. } => 1,
        _ => 0,
    }
}

/// Read one copy (0 = A, 1 = B).
unsafe fn read_copy(i: u32) -> CopyRead {
    let sector = if i == 0 { crate::store::layout().sel_a() } else { crate::store::layout().sel_b() };
    if block_read(sector, 512, secbuf()) != 0 {
        return CopyRead::ReadError;
    }
    parse_copy(&*(secbuf() as *const [u8; 512]))
}

/// A copy sector that was read: valid, or bad (magic, version, CRC or count).
fn parse_copy(b: &[u8; 512]) -> CopyRead {
    match sel::decode(b) {
        Some(r) => CopyRead::Valid(r),
        None => CopyRead::Invalid,
    }
}

/// The record in use, read from both copies.
pub(crate) unsafe fn current() -> Choice {
    // the card's state first (reads only): a card too small has no store, which is an empty record and not a read failure
    card::check();
    current_on(card::state() == CARD_TOO_SMALL, |i| read_copy(i))
}

/// `current` with the card state and the copy reader given: no store means `Empty` with no read.
fn current_on(no_store: bool, mut read: impl FnMut(u32) -> CopyRead) -> Choice {
    if no_store {
        return Choice::Empty;
    }
    let a = read(0);
    let b = read(1);
    choose(a, b)
}

/// Write `r` to copy `i`. Only these two sectors are ever written.
unsafe fn write_copy(i: u32, r: &Record) -> i32 {
    let sector = if i == 0 { crate::store::layout().sel_a() } else { crate::store::layout().sel_b() };
    let b = &mut *(secbuf() as *mut [u8; 512]);
    sel::encode(r, b);
    let e = block_write(sector, 512, secbuf());
    if e == E_CARD_RO || e == E_RANGE {
        return e;
    }
    if e != 0 {
        return E_SEL_WRITE;
    }
    OK
}

unsafe fn path_ok(p: *const u8) -> bool {
    let mut n = 0;
    while n < SEL_PATH {
        let c = *p.add(n);
        if c == 0 {
            return n > 0;
        }
        if !(0x21..0x7f).contains(&c) {
            return false;
        }
        n += 1;
    }
    false
}

/// `dkrt_selection_get`: `E_READ` when no copy is valid and a copy could not be read.
pub unsafe fn selection_get(out: *mut DkrtSelection) -> i32 {
    let r = match current() {
        Choice::Use { record, .. } => record,
        Choice::Empty => Record::empty(),
        Choice::Unreadable => return E_READ,
    };
    let o = &mut *out;
    o.count = r.count;
    o.flags = r.flags;
    o.paths = r.paths;
    OK
}

/// `dkrt_selection_set`: both copies, A then B, with the next sequence number (B first when A is the only valid copy). The boot counter
/// carries over. `E_READ`, with no write, when no copy is valid and a copy could not be read.
pub unsafe fn selection_set(s: *const DkrtSelection) -> i32 {
    let s = &*s;
    let r = sel::check_count(s.count);
    if r != OK {
        return r;
    }
    let mut k = 0;
    while k < s.count as usize {
        if !path_ok(s.paths[k].as_ptr()) {
            return E_SEL_PATH;
        }
        k += 1;
    }
    let choice = current();
    let cur = match choice {
        Choice::Use { record, .. } => record,
        Choice::Empty => Record::empty(),
        Choice::Unreadable => return E_READ,
    };
    let r = sel::next_record(&cur, s.flags, s.count, &s.paths);
    dlog::ev(logk::MENU, dlog::INFO, [logk::MENU_SELECT, s.count, 0, 0, 0]);
    let first = first_write(&choice);
    let e = write_copy(first, &r);
    if e != OK {
        return e;
    }
    write_copy(1 - first, &r)
}

/// `CMD_SELECT_ONE`: write copy `i` only, with `seq + 1` (the state of a power cut between the two writes).
pub unsafe fn selection_set_one(i: u32, s: *const DkrtSelection) -> i32 {
    let s = &*s;
    let e = sel::check_one(s.count, i);
    if e != OK {
        return e;
    }
    let cur = match current() {
        Choice::Use { record, .. } => record,
        Choice::Empty => Record::empty(),
        Choice::Unreadable => return E_READ,
    };
    let r = sel::next_record(&cur, s.flags, s.count, &s.paths);
    write_copy(i, &r)
}

/// Write the record with a changed counter/flags into the copy that is not the current one (the current one stays valid if
/// the write is cut).
pub(crate) unsafe fn update(mut r: Record, current_copy: u32, valid: bool) -> i32 {
    r.seq = if valid { r.seq.wrapping_add(1) } else { 1 };
    let target = if valid { 1 - current_copy } else { 0 };
    let e = write_copy(target, &r);
    if e == OK {
        REC = r;
        REC_VALID = true;
        REC_COPY = target;
    }
    e
}

#[cfg(test)]
mod choice_tests {
    use super::*;

    #[derive(Clone, Copy)]
    enum In {
        Valid(u32),
        Invalid,
        ReadError,
    }

    fn read(i: In) -> CopyRead {
        match i {
            In::Valid(seq) => {
                let mut r = Record::empty();
                r.seq = seq;
                CopyRead::Valid(r)
            }
            In::Invalid => CopyRead::Invalid,
            In::ReadError => CopyRead::ReadError,
        }
    }

    /// (kind, seq, copy, other_valid)
    fn summary(c: &Choice) -> (&'static str, u32, u32, bool) {
        match c {
            Choice::Use { record, copy, other_valid } => ("use", record.seq, *copy, *other_valid),
            Choice::Empty => ("empty", 0, 0, false),
            Choice::Unreadable => ("unreadable", 0, 0, false),
        }
    }

    #[test]
    fn the_choice_over_every_pair_of_copy_states() {
        use In::*;
        let table = [
            (Valid(5), Valid(9), ("use", 9, 1, true)),
            (Valid(9), Valid(5), ("use", 9, 0, true)),
            (Valid(7), Valid(7), ("use", 7, 0, true)),
            (Valid(5), Invalid, ("use", 5, 0, false)),
            (Valid(5), ReadError, ("use", 5, 0, false)),
            (Invalid, Valid(5), ("use", 5, 1, false)),
            (Invalid, Invalid, ("empty", 0, 0, false)),
            (Invalid, ReadError, ("unreadable", 0, 0, false)),
            (ReadError, Valid(5), ("use", 5, 1, false)),
            (ReadError, Invalid, ("unreadable", 0, 0, false)),
            (ReadError, ReadError, ("unreadable", 0, 0, false)),
        ];
        for (a, b, want) in table {
            assert_eq!(summary(&choose(read(a), read(b))), want);
        }
    }

    /// The boot's choice over two copy sectors as a power cut leaves them.
    #[test]
    fn a_torn_copy_is_bad_and_the_boot_takes_the_valid_one_with_the_higher_seq() {
        let sector = |seq: u32, count: u32| {
            let mut r = Record::empty();
            r.seq = seq;
            r.count = count;
            let mut b = [0u8; 512];
            sel::encode(&r, &mut b);
            b
        };
        let new = sector(2, 0);
        let old = sector(1, 0);
        let mut torn = new;
        torn[100] ^= 0xff;
        let mut cut = new; // a cut inside the sector write: the second half still zero
        cut[256..].fill(0);
        // (copy A, copy B, what the boot uses: kind, seq, copy), the four cases of the device test
        let cases = [
            (new, old, ("use", 2, 0, true)),
            (torn, old, ("use", 1, 1, false)),
            (old, new, ("use", 2, 1, true)),
            (torn, torn, ("empty", 0, 0, false)),
            (cut, old, ("use", 1, 1, false)),
        ];
        for (a, b, want) in cases {
            assert_eq!(summary(&choose(parse_copy(&a), parse_copy(&b))), want);
        }
    }

    #[test]
    fn a_card_without_a_store_is_empty_and_is_not_read() {
        let c = current_on(true, |_| panic!("no read on a card without a store"));
        assert_eq!(summary(&c), ("empty", 0, 0, false));
        assert_eq!(first_write(&c), 0);
        // with a store, read errors with no valid copy are still unreadable
        let c = current_on(false, |_| CopyRead::ReadError);
        assert_eq!(summary(&c), ("unreadable", 0, 0, false));
    }

    #[test]
    fn selection_set_never_writes_first_to_the_only_valid_copy() {
        use In::*;
        let first = |a, b| first_write(&choose(read(a), read(b)));
        assert_eq!(first(Valid(5), ReadError), 1);
        assert_eq!(first(Valid(5), Invalid), 1);
        assert_eq!(first(Invalid, Valid(5)), 0);
        assert_eq!(first(ReadError, Valid(5)), 0);
        assert_eq!(first(Valid(5), Valid(9)), 0);
        assert_eq!(first(Invalid, Invalid), 0);
    }
}

