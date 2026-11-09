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
    is_blank(core::slice::from_raw_parts(p, 512))
}

/// Is the sector all 0x00 or all 0xFF?
fn is_blank(sec: &[u8]) -> bool {
    let v = sec[0];
    if v != 0x00 && v != 0xff {
        return false;
    }
    let mut i = 1;
    while i < sec.len() {
        if sec[i] != v {
            return false;
        }
        i += 1;
    }
    true
}

/// The first sector (index, in 512-byte units) of `buf` that is not blank.
fn first_nonblank(buf: &[u8]) -> Option<usize> {
    let mut i = 0;
    while (i + 1) * 512 <= buf.len() {
        if !is_blank(&buf[i * 512..(i + 1) * 512]) {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// The four sectors that show the area is ours, with their kind (0: store header, 1: selection copy).
fn probes(l: &store::Layout) -> [(u32, u32); 4] {
    [(l.first, 0), (l.first + l.header_b(), 0), (l.sel_a(), 1), (l.sel_b(), 1)]
}

/// The state the four probe results decide, `None` when the blank scan has to decide: ours when any probe is ours, unreadable when one
/// failed (and none is ours).
fn probe_state(results: &[Option<bool>; 4]) -> Option<u32> {
    let mut failed = false;
    let mut k = 0;
    while k < 4 {
        match results[k] {
            Some(true) => return Some(CARD_OURS),
            Some(false) => {}
            None => failed = true,
        }
        k += 1;
    }
    if failed {
        Some(CARD_UNREADABLE)
    } else {
        None
    }
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
    let p = probes(&l);
    let results = [ours_at(p[0].0, p[0].1), ours_at(p[1].0, p[1].1), ours_at(p[2].0, p[2].1), ours_at(p[3].0, p[3].1)];
    c.read_errors = 0;
    let mut k = 0;
    while k < 4 {
        if results[k].is_none() {
            c.read_errors += 1;
        }
        k += 1;
    }
    if let Some(s) = probe_state(&results) {
        c.state = s;
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
        let buf = core::slice::from_raw_parts(stage() as *const u8, n as usize * 512);
        if let Some(i) = first_nonblank(buf) {
            c.first_nonblank = sector + i as u32;
            c.hash = store::crc32_of(stage().add(i * 512), 512);
            found = true;
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
fn touches_marker(l: &store::Layout, sector: u32, sectors: u32) -> bool {
    let end = sector + sectors;
    let hit = |s: u32| s >= sector && s < end;
    hit(l.first) || hit(l.first + l.header_b()) || hit(l.sel_a()) || hit(l.sel_b())
}

/// What `admit` does for a write, decided from the layout and the card state alone.
#[derive(Debug, PartialEq)]
enum Gate {
    Allow,
    /// The write marks the area itself: the card is ours from now on.
    MarkOurs,
    /// A blank range: write the empty header pair first.
    Claim,
    /// The card was not checked yet: run `check`, then decide again.
    Check,
    /// Refuse with this reason (1 range, 2 foreign or unreadable, 4 too small).
    Refuse(u32),
}

fn gate(l: &store::Layout, state: u32, sector: u32, bytes: u32) -> Gate {
    if !l.write_allowed(sector, bytes) {
        return Gate::Refuse(1);
    }
    if state == CARD_UNKNOWN {
        Gate::Check
    } else if state == CARD_TOO_SMALL {
        Gate::Refuse(4)
    } else if state == CARD_OURS {
        Gate::Allow
    } else if state == CARD_BLANK {
        if touches_marker(l, sector, bytes / 512) {
            Gate::MarkOurs
        } else {
            Gate::Claim
        }
    } else {
        Gate::Refuse(2)
    }
}

/// The gate of `block_write`: `OK`, `E_RANGE` or `E_CARD_RO`. May claim a blank range.
pub unsafe fn admit(sector: u32, bytes: u32) -> i32 {
    let mut g = gate(&lay(), state(), sector, bytes);
    if g == Gate::Check {
        check();
        g = gate(&lay(), state(), sector, bytes);
    }
    if g == Gate::Allow {
        return OK;
    }
    if g == Gate::MarkOurs {
        info().state = CARD_OURS;
        return OK;
    }
    if g == Gate::Claim {
        if claim() {
            return OK;
        }
        count_refusal(sector, bytes, 3);
        return E_CARD_RO;
    }
    if g == Gate::Refuse(1) {
        count_refusal(sector, bytes, 1);
        return E_RANGE;
    }
    if let Gate::Refuse(r) = g {
        count_refusal(sector, bytes, r);
    }
    E_CARD_RO
}

/// `CMD_CARD_PROBE`: ask the one card-write function to write `bytes` at `sector` from the staging buffer; the answer is its result.
pub unsafe fn probe(arg: *const u8) -> i32 {
    crate::block_write(rd32(arg), rd32(arg.add(4)), addr_of!(DKRT_STAGE) as *const u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    const L: store::Layout = store::Layout { first: 1000, sectors: 4096 };

    #[test]
    fn blank_sector_is_all_zero_or_all_ff() {
        assert!(is_blank(&[0x00; 512]));
        assert!(is_blank(&[0xff; 512]));
        let mut s = [0u8; 512];
        s[511] = 1;
        assert!(!is_blank(&s));
        let mut s = [0xffu8; 512];
        s[0] = 0;
        assert!(!is_blank(&s));
        assert!(!is_blank(&[0x55; 512]));
    }

    #[test]
    fn scan_reports_the_first_nonblank_sector() {
        let mut buf = [0xffu8; 512 * 4];
        assert_eq!(first_nonblank(&buf), None);
        buf[512 * 2 + 7] = 3;
        buf[512 * 3] = 9;
        assert_eq!(first_nonblank(&buf), Some(2));
    }

    #[test]
    fn probes_are_both_headers_and_both_selection_copies() {
        assert_eq!(probes(&L), [(1000, 0), (1000 + L.header_b(), 0), (L.sel_a(), 1), (L.sel_b(), 1)]);
    }

    #[test]
    fn one_ours_probe_decides_ours_even_with_a_read_error() {
        assert_eq!(probe_state(&[Some(false), Some(true), None, Some(false)]), Some(CARD_OURS));
    }

    #[test]
    fn a_failed_probe_without_ours_is_unreadable() {
        assert_eq!(probe_state(&[Some(false), None, Some(false), Some(false)]), Some(CARD_UNREADABLE));
    }

    #[test]
    fn four_negative_probes_leave_the_decision_to_the_blank_scan() {
        assert_eq!(probe_state(&[Some(false); 4]), None);
    }

    #[test]
    fn writes_outside_the_area_are_refused_for_range_in_every_state() {
        for st in [CARD_UNKNOWN, CARD_OURS, CARD_BLANK, CARD_FOREIGN, CARD_UNREADABLE, CARD_TOO_SMALL] {
            assert_eq!(gate(&L, st, L.first - 1, 512), Gate::Refuse(1));
            assert_eq!(gate(&L, st, L.first + L.sectors, 512), Gate::Refuse(1));
            assert_eq!(gate(&L, st, L.first + L.sectors - 1, 1024), Gate::Refuse(1));
            assert_eq!(gate(&L, st, L.first, 0), Gate::Refuse(1));
            assert_eq!(gate(&L, st, L.first, 100), Gate::Refuse(1));
        }
    }

    #[test]
    fn an_unchecked_card_is_checked_before_a_decision() {
        assert_eq!(gate(&L, CARD_UNKNOWN, L.first + 10, 512), Gate::Check);
    }

    #[test]
    fn foreign_unreadable_and_too_small_cards_refuse_every_write_in_range() {
        for sector in [L.first, L.first + 10, L.sel_a(), L.sel_b()] {
            assert_eq!(gate(&L, CARD_FOREIGN, sector, 512), Gate::Refuse(2));
            assert_eq!(gate(&L, CARD_UNREADABLE, sector, 512), Gate::Refuse(2));
            assert_eq!(gate(&L, CARD_TOO_SMALL, sector, 512), Gate::Refuse(4));
        }
    }

    #[test]
    fn our_card_allows_every_write_in_range() {
        assert_eq!(gate(&L, CARD_OURS, L.first + 10, 512), Gate::Allow);
        assert_eq!(gate(&L, CARD_OURS, L.first, 512 * L.sectors), Gate::Allow);
    }

    #[test]
    fn blank_card_data_write_claims_first() {
        assert_eq!(gate(&L, CARD_BLANK, L.first + 10, 512 * 100), Gate::Claim);
    }

    #[test]
    fn blank_card_write_of_a_marker_sector_marks_ours_without_a_claim() {
        assert_eq!(gate(&L, CARD_BLANK, L.first, 512), Gate::MarkOurs);
        assert_eq!(gate(&L, CARD_BLANK, L.first + L.header_b(), 512), Gate::MarkOurs);
        assert_eq!(gate(&L, CARD_BLANK, L.sel_a(), 512), Gate::MarkOurs);
        assert_eq!(gate(&L, CARD_BLANK, L.sel_b(), 512), Gate::MarkOurs);
        // a range that covers a marker sector in its middle also marks
        assert_eq!(gate(&L, CARD_BLANK, L.first + L.header_b() - 3, 512 * 5), Gate::MarkOurs);
    }

    #[test]
    fn blank_card_write_next_to_a_marker_still_claims() {
        assert_eq!(gate(&L, CARD_BLANK, L.first + 1, 512 * 10), Gate::Claim);
        assert_eq!(gate(&L, CARD_BLANK, L.sel_a() - 1, 512), Gate::Claim);
    }
}
