//! Card safety (docs/findings/46-digikit-debug.md): the runtime writes the card in one place only, `crate::block_write`, and that place asks
//! this file first.
//!
//! - Range: a write outside the store area of this device (`[card.store]`, `store::layout()`) is refused (`E_RANGE`) and counted.
//! - Size: the card must reach the end of the area (the last sector reads); a smaller card has **no store** (`CARD_TOO_SMALL`): nothing is read or
//!   written there, uploads answer `E_NOSTORE`, the boot goes on.
//! - Ownership: before the first write the runtime has looked at its area on this card (`check`, at boot, or at the first write when no
//!   boot hook ran). The area is **ours** when store header A or B is a valid header, or a selection copy is valid. Otherwise the sectors that
//!   would mark the area (both headers and both selection copies) and the first `BLANK_SECTORS` of the area must read all 0x00 or all 0xFF
//!   (**blank**); the first write may then claim it. If not (**foreign**), or a read failed (**unreadable**), the card is read-only for the
//!   runtime: every write is refused (`E_CARD_RO`), the boot counter is not raised (so there is no safe mode by counter), uploads answer
//!   `E_INUSE`, and ABOUT says so. The area is up to 22 GiB: the rest of it is not scanned (**[O]**: finding 59).
//! - Claiming: a write that touches only the store's header sectors or the selection record is itself a mark of ours, so it needs nothing
//!   first. A write of data or a table into a blank range (an upload's staging) first writes an empty store header pair, so a power cut
//!   in the middle of an upload leaves a card the next boot calls ours, not a card with unexplained data in it.
//!
//! The check reads the start of the area only on a card where none of our structures is present (a fresh card): `BLANK_SECTORS` in 64 KiB reads,
//! stopping at the first sector that is not blank. A card that is ours costs five sector reads.
//!
//! The reads go through the staging buffer (`DKRT_STAGE`), so `check` must not run while a module file is staged: the boot hook runs it first.
use crate::dlog;
use crate::{block_read, rd32, DKRT_STAGE};
use core::ptr::{addr_of, addr_of_mut, read_volatile};
use dkrt_sdk::sel;
use dkrt_sdk::store::{self, Hdr};
use dkrt_sdk::*;

/// What the check found. Exported so the proof tools can peek it.
#[repr(C)]
pub struct CardInfo {
    pub state: u32,
    pub first_nonblank: u32,
    pub hash: u32,
    pub refused: u32,
    pub refused_range: u32,
    pub read_errors: u32,
    pub checked: u32,
    pub claims: u32,
    pub writes: u32,
}

#[no_mangle]
pub static mut DKRT_CARD: CardInfo = CardInfo {
    state: CARD_UNKNOWN,
    first_nonblank: u32::MAX,
    hash: 0,
    refused: 0,
    refused_range: 0,
    read_errors: 0,
    checked: 0,
    claims: 0,
    writes: 0,
};

#[repr(C, align(16))]
struct Sec([u8; 512]);
static mut CLAIM_BUF: Sec = Sec([0; 512]);

pub fn info() -> &'static mut CardInfo {
    unsafe { &mut *addr_of_mut!(DKRT_CARD) }
}

pub fn state() -> u32 {
    unsafe { read_volatile(addr_of!(DKRT_CARD.state)) }
}

/// May the runtime write the card at all (ours or blank)?
pub fn writable() -> bool {
    let s = state();
    s == CARD_OURS || s == CARD_BLANK || s == CARD_UNKNOWN
}

fn lay() -> store::Layout {
    crate::store::layout()
}

/// Sectors 128 at a time: 64 KiB, the most one block read takes.
const CHUNK_SECTORS: u32 = 128;
/// How much of the start of the area must read blank before the first write may claim it: 1 MiB (the headers, a table and the first files).
const BLANK_SECTORS: u32 = 2048;

unsafe fn stage() -> *mut u8 {
    addr_of_mut!(DKRT_STAGE) as *mut u8
}

/// Is the sector at `p` all 0x00 or all 0xFF?
pub(crate) unsafe fn blank(p: *const u8) -> bool {
    let v = *p;
    if v != 0x00 && v != 0xff {
        return false;
    }
    let mut i = 1;
    while i < 512 {
        if *p.add(i) != v {
            return false;
        }
        i += 1;
    }
    true
}

/// Is the sector at absolute `sector` one of ours (a valid store header, or one of another version: that store counts as empty; for the record sectors a valid selection copy)? `None`: the read failed.
unsafe fn ours_at(sector: u32, kind: u32) -> Option<bool> {
    let buf = stage();
    if block_read(sector, 512, buf) != 0 {
        return None;
    }
    Some(if kind == 0 {
        store::parse_header(buf).is_some() || store::old_header(buf)
    } else {
        sel::decode(&*(buf as *const [u8; 512])).is_some()
    })
}

/// Decide the state of the card's area (once). The result is logged.
pub unsafe fn check() {
    let c = info();
    if c.checked != 0 {
        return;
    }
    c.checked = 1;
    let l = lay();
    // the card must reach the end of the area: the firmware's block read refuses a start at or past the card's capacity
    if block_read(l.first + l.sectors - 1, 512, stage()) != 0 {
        c.state = CARD_TOO_SMALL;
        report();
        return;
    }
    let mut errors = 0;
    let mut ours = false;
    let probes: [(u32, u32); 4] = [
        (l.first, 0),
        (l.first + l.header_b(), 0),
        (l.sel_a(), 1),
        (l.sel_b(), 1),
    ];
    let mut k = 0;
    while k < 4 {
        match ours_at(probes[k].0, probes[k].1) {
            Some(true) => ours = true,
            Some(false) => {}
            None => errors += 1,
        }
        k += 1;
    }
    c.read_errors = errors;
    if ours {
        c.state = CARD_OURS;
        report();
        return;
    }
    if errors != 0 {
        c.state = CARD_UNREADABLE;
        report();
        return;
    }
    // nothing of ours: is the start of the area blank?
    let mut sector = 0u32;
    let mut found = false;
    while sector < BLANK_SECTORS && !found {
        let n = if BLANK_SECTORS - sector > CHUNK_SECTORS { CHUNK_SECTORS } else { BLANK_SECTORS - sector };
        if block_read(l.first + sector, n * 512, stage()) != 0 {
            c.read_errors += 1;
            c.state = CARD_UNREADABLE;
            report();
            return;
        }
        let mut i = 0;
        while i < n {
            let p = stage().add(i as usize * 512);
            if !blank(p) {
                c.first_nonblank = sector + i;
                c.hash = store::crc32_of(p, 512);
                found = true;
                break;
            }
            i += 1;
        }
        sector += n;
    }
    c.state = if found { CARD_FOREIGN } else { CARD_BLANK };
    report();
}

unsafe fn report() {
    let c = info();
    let bad = c.state != CARD_OURS && c.state != CARD_BLANK;
    dlog::ev(
        logk::CARD_CHECK,
        if bad { logk::F_ERROR } else { dlog::INFO },
        [c.state, c.first_nonblank, c.hash, c.read_errors, 0],
    );
}

fn count_refusal(sector: u32, bytes: u32, reason: u32) {
    let c = info();
    c.refused = c.refused.wrapping_add(1);
    if reason == 1 {
        c.refused_range = c.refused_range.wrapping_add(1);
    }
    unsafe { dlog::ev(logk::CARD_REFUSED, logk::F_ERROR, [sector, bytes, reason, c.state, 0]) }
}

/// Write the empty store header pair (both copies, `seq` 1): the runtime's mark on a blank range.
unsafe fn claim() -> bool {
    let l = lay();
    let h = Hdr { seq: 1, ..Hdr::empty(&l) };
    let buf = addr_of_mut!(CLAIM_BUF) as *mut u8;
    store::encode_header(&h, buf);
    // set first: a second task that writes meanwhile sees a claimed card
    info().state = CARD_OURS;
    info().claims += 1;
    let b = crate::raw_block_write(l.first + l.header_b(), 512, buf) == 0;
    let a = crate::raw_block_write(l.first, 512, buf) == 0;
    a && b
}

/// Does a write of `sectors` from `sector` touch a sector that marks the area as ours (a store header or a selection record)?
fn touches_marker(sector: u32, sectors: u32) -> bool {
    let end = sector + sectors;
    let hit = |s: u32| s >= sector && s < end;
    let l = lay();
    hit(l.first) || hit(l.first + l.header_b()) || hit(l.sel_a()) || hit(l.sel_b())
}

/// The gate of `block_write`: `OK`, `E_RANGE` or `E_CARD_RO`. May claim a blank range.
pub unsafe fn admit(sector: u32, bytes: u32) -> i32 {
    if !lay().write_allowed(sector, bytes) {
        count_refusal(sector, bytes, 1);
        return E_RANGE;
    }
    if state() == CARD_UNKNOWN {
        check();
    }
    let s = state();
    if s == CARD_TOO_SMALL {
        count_refusal(sector, bytes, 4);
        return E_CARD_RO;
    }
    if s == CARD_OURS {
        return OK;
    }
    if s == CARD_BLANK {
        if touches_marker(sector, bytes / 512) {
            info().state = CARD_OURS;
            return OK;
        }
        if claim() {
            return OK;
        }
        count_refusal(sector, bytes, 3);
        return E_CARD_RO;
    }
    count_refusal(sector, bytes, 2);
    E_CARD_RO
}

/// `CMD_CARD_PROBE`: ask the one card-write function to write `bytes` at `sector` from the staging buffer; the answer is its result.
pub unsafe fn probe(arg: *const u8) -> i32 {
    crate::block_write(rd32(arg), rd32(arg.add(4)), addr_of!(DKRT_STAGE) as *const u8)
}
