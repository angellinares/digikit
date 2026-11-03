//! The version 3 store on a card in RAM: the format, the table stream, free-space reuse, and what a power cut can leave.
extern crate std;
use super::*;
use crate::{E_BAD_STORE, E_NOT_FOUND};
use std::sync::Mutex;
use std::vec::Vec;

const FIRST: u32 = 5000;
const SECTORS: u32 = 4096;
const LAY: Layout = Layout { first: FIRST, sectors: SECTORS };

static CARD: Mutex<Vec<u8>> = Mutex::new(Vec::new());
static LOCK: Mutex<()> = Mutex::new(());

/// The tests share one card: one at a time (a failed test must not poison the rest).
fn lock() -> std::sync::MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}
/// Writes allowed before every further one fails (`u32::MAX`: no limit) and writes done.
static CUT: Mutex<(u32, u32)> = Mutex::new((u32::MAX, 0));

unsafe extern "C" fn rd(sector: u32, bytes: u32, buf: *mut u8) -> i32 {
    let c = CARD.lock().unwrap();
    let at = (sector - FIRST) as usize * 512;
    core::ptr::copy_nonoverlapping(c.as_ptr().add(at), buf, bytes as usize);
    0
}

unsafe extern "C" fn wr(sector: u32, bytes: u32, buf: *mut u8) -> i32 {
    {
        let mut k = CUT.lock().unwrap();
        if k.1 >= k.0 {
            return -1;
        }
        k.1 += 1;
    }
    assert!(LAY.write_allowed(sector, bytes), "a write outside the store: {sector} {bytes}");
    let mut c = CARD.lock().unwrap();
    let at = (sector - FIRST) as usize * 512;
    core::ptr::copy_nonoverlapping(buf, c.as_mut_ptr().add(at), bytes as usize);
    0
}

const IO: Io = Io { rd, wr };

#[repr(C, align(16))]
struct Buf<const N: usize>([u8; N]);

fn fresh() {
    *CARD.lock().unwrap() = std::vec![0u8; SECTORS as usize * 512];
    *CUT.lock().unwrap() = (u32::MAX, 0);
}

fn cstr(p: &str) -> Vec<u8> {
    let mut v = p.as_bytes().to_vec();
    v.push(0);
    v
}

fn entry(first: u32, size: u32, kind: u8) -> Entry {
    Entry { first, size, kind, sha: [(first & 0xff) as u8; 32] }
}

/// A scratch set: 16-byte aligned buffers.
struct Work {
    a: Box<Buf<4096>>,
    b: Box<Buf<4096>>,
    s: Box<Buf<512>>,
}

use std::boxed::Box;

fn work() -> Work {
    Work { a: Box::new(Buf([0; 4096])), b: Box::new(Buf([0; 4096])), s: Box::new(Buf([0; 512])) }
}

/// The state of the store as `open` sees it (no table: a fresh one).
fn state(w: &mut Work) -> Result<Table, i32> {
    unsafe { open(&IO, &LAY, w.s.0.as_mut_ptr(), w.a.0.as_mut_ptr(), 4096, true) }
}

fn current(w: &mut Work) -> Hdr {
    match state(w) {
        Ok(t) => t.h,
        Err(_) => Hdr::empty(&LAY),
    }
}

/// Commit an insert of `path` with data at `first` (`size` bytes).
fn insert(w: &mut Work, path: &str, first: u32, size: u32, extra: &[(u32, u32)]) -> Result<Committed, i32> {
    let cur = current(w);
    let mut e = [0u8; ENTRY];
    let p = cstr(path);
    unsafe { encode_entry(p.as_ptr(), &entry(first, size, KIND_FILE), e.as_mut_ptr()) };
    unsafe { commit(&IO, &LAY, &cur, w.a.0.as_mut_ptr(), w.b.0.as_mut_ptr(), 1024, w.s.0.as_mut_ptr(), Edit::Insert(e.as_ptr()), extra) }
}

fn remove(w: &mut Work, path: &str) -> Result<Committed, i32> {
    let cur = current(w);
    let p = cstr(path);
    unsafe { commit(&IO, &LAY, &cur, w.a.0.as_mut_ptr(), w.b.0.as_mut_ptr(), 1024, w.s.0.as_mut_ptr(), Edit::Remove(p.as_ptr()), &[]) }
}

fn paths(w: &mut Work) -> Vec<(std::string::String, u32)> {
    let h = current(w);
    let mut out = Vec::new();
    unsafe {
        scan(&IO, &LAY, &h, w.a.0.as_mut_ptr(), 4096, |_, e| {
            let n = path_len(e);
            out.push((std::string::String::from_utf8_lossy(core::slice::from_raw_parts(e, n)).into_owned(), be32(e.add(O_FIRST))));
            true
        });
    }
    out
}

#[test]
fn crc32_matches_the_zlib_check_value() {
    assert_eq!(crc32_of(b"123456789".as_ptr(), 9), 0xcbf4_3926);
}

#[test]
fn the_geometry_is_the_documented_one() {
    assert_eq!(TAIL, 35);
    assert_eq!(LAY.header_b(), SECTORS - 35);
    assert_eq!(LAY.limit(), SECTORS - 35);
    assert_eq!(LAY.set_a(), FIRST + SECTORS - 34);
    assert_eq!(LAY.set_b(), FIRST + SECTORS - 18);
    assert_eq!(LAY.set_b() + 16, LAY.sel_a());
    assert_eq!(LAY.sel_a(), FIRST + SECTORS - 2);
    assert_eq!(LAY.sel_b(), FIRST + SECTORS - 1);
    assert_eq!(table_sectors(0), 0);
    assert_eq!(table_sectors(1), 1);
    assert_eq!(table_sectors(2), 1);
    assert_eq!(table_sectors(3), 2);
    // the card writes of the runtime are allowed only inside the area
    assert!(LAY.write_allowed(FIRST, 512));
    assert!(LAY.write_allowed(FIRST + SECTORS - 1, 512));
    assert!(LAY.write_allowed(FIRST, SECTORS * 512));
    assert!(!LAY.write_allowed(FIRST - 1, 512));
    assert!(!LAY.write_allowed(FIRST + SECTORS, 512));
    assert!(!LAY.write_allowed(FIRST + SECTORS - 1, 1024));
    assert!(!LAY.write_allowed(FIRST, 0));
    assert!(!LAY.write_allowed(FIRST, 100));
    assert!(!LAY.write_allowed(u32::MAX, 512));
    assert!(!LAY.write_allowed(0, 512));
}

#[test]
fn an_empty_area_is_no_store_and_a_commit_makes_one() {
    let _g = lock();
    fresh();
    let mut w = work();
    assert_eq!(state(&mut w).err(), Some(E_NOT_FOUND));
    let c = insert(&mut w, "modules/a.dkm", 10, 1000, &[(10, 2)]).unwrap();
    assert_eq!((c.h.count, c.h.seq, c.h.table_sectors), (1, 1, 1));
    assert_eq!(c.h.used, 1 + 1 + 2);
    let t = state(&mut w).unwrap();
    assert_eq!((t.h, t.copy, t.agree), (c.h, 0, true));
    let p = cstr("modules/a.dkm");
    let f = unsafe { find(&IO, &LAY, &t.h, w.a.0.as_mut_ptr(), 4096, p.as_ptr()) }.unwrap().unwrap();
    assert_eq!((f.0, f.1.first, f.1.size), (0, 10, 1000));
    let q = cstr("store/zzz");
    assert!(unsafe { find(&IO, &LAY, &t.h, w.a.0.as_mut_ptr(), 4096, q.as_ptr()) }.unwrap().is_none());
}

#[test]
fn the_table_is_sorted_by_first_sector_then_path_and_a_path_is_replaced() {
    let _g = lock();
    fresh();
    let mut w = work();
    insert(&mut w, "b", 30, 512, &[]).unwrap();
    insert(&mut w, "a", 20, 512, &[]).unwrap();
    insert(&mut w, "zero2", 0, 0, &[]).unwrap();
    insert(&mut w, "zero1", 0, 0, &[]).unwrap();
    insert(&mut w, "c", 25, 512, &[]).unwrap();
    let got: Vec<_> = paths(&mut w).into_iter().map(|p| p.0).collect();
    assert_eq!(got, ["zero1", "zero2", "a", "c", "b"]);
    // the same path again replaces the entry and frees its extent
    let c = insert(&mut w, "a", 40, 1024, &[]).unwrap();
    assert_eq!(c.removed.unwrap().first, 20);
    let got: Vec<_> = paths(&mut w);
    assert_eq!(got.iter().map(|p| p.0.as_str()).collect::<Vec<_>>(), ["zero1", "zero2", "c", "b", "a"]);
    assert_eq!(current(&mut w).count, 5);
}

/// Commit an insert of the folder entry `path`.
fn mkdir(w: &mut Work, path: &str) -> Result<Committed, i32> {
    let cur = current(w);
    let mut e = [0u8; ENTRY];
    let p = cstr(path);
    unsafe { encode_entry(p.as_ptr(), &entry(0, 0, KIND_DIR), e.as_mut_ptr()) };
    unsafe { commit(&IO, &LAY, &cur, w.a.0.as_mut_ptr(), w.b.0.as_mut_ptr(), 1024, w.s.0.as_mut_ptr(), Edit::Insert(e.as_ptr()), &[]) }
}

#[test]
fn a_folder_entry_goes_when_something_is_stored_inside_it() {
    let _g = lock();
    fresh();
    let mut w = work();
    mkdir(&mut w, "samples").unwrap();
    mkdir(&mut w, "samples/drums").unwrap();
    mkdir(&mut w, "other").unwrap();
    // a folder inside a folder makes the outer one implied: only the innermost folder entry is kept
    let p: Vec<_> = paths(&mut w).into_iter().map(|x| x.0).collect();
    assert_eq!(p, ["other", "samples/drums"]);
    // a file in samples/drums/: its folder entry goes, the unrelated one stays
    insert(&mut w, "samples/drums/kick.wav", 10, 1000, &[]).unwrap();
    let p: Vec<_> = paths(&mut w).into_iter().map(|x| x.0).collect();
    assert_eq!(p, ["other", "samples/drums/kick.wav"]);
    // a name that only starts like a folder is not inside it: "other" stays when "others/x" is stored
    insert(&mut w, "others/x", 20, 10, &[]).unwrap();
    assert_eq!(paths(&mut w).len(), 3);
    // the table is intact
    assert!(state(&mut w).is_ok());
    // deleting the last file leaves no folder entry: the folder was implied
    remove(&mut w, "samples/drums/kick.wav").unwrap();
    let p: Vec<_> = paths(&mut w).into_iter().map(|x| x.0).collect();
    assert_eq!(p, ["other", "others/x"]);
}

#[test]
fn a_remove_drops_the_entry_and_the_space_is_used_again() {
    let _g = lock();
    fresh();
    let mut w = work();
    // three files of 100 sectors from sector 10, 110, 210
    insert(&mut w, "f1", 10, 100 * 512, &[]).unwrap();
    insert(&mut w, "f2", 110, 100 * 512, &[]).unwrap();
    insert(&mut w, "f3", 210, 100 * 512, &[]).unwrap();
    let h = current(&mut w);
    let first_fit_then = |w: &mut Work, h: &Hdr, n: u32| unsafe { first_fit(&IO, &LAY, h, w.a.0.as_mut_ptr(), 4096, n, &[]) }.unwrap();
    // gaps: [1, 10) minus the table, the table's neighbours: a file of 150 sectors goes after f3
    let at = first_fit_then(&mut w, &h, 150).unwrap();
    assert!(at >= 310, "{at}");
    let c = remove(&mut w, "f2").unwrap();
    assert_eq!(c.removed.unwrap().first, 110);
    let h = current(&mut w);
    assert_eq!(h.count, 2);
    // the 100 sectors where f2 was are free: a file of 100 sectors goes there
    assert_eq!(first_fit_then(&mut w, &h, 100), Some(110));
    let big = unsafe { largest_gap(&IO, &LAY, &h, w.a.0.as_mut_ptr(), 4096, &[]) }.unwrap();
    assert_eq!(big, SECTORS - TAIL - 310);
    // an extent the caller reserves is not given out
    assert_eq!(unsafe { first_fit(&IO, &LAY, &h, w.a.0.as_mut_ptr(), 4096, 100, &[(110, 60)]) }.unwrap(), Some(310));
}

#[test]
fn the_table_goes_to_a_gap_and_never_over_the_one_in_use() {
    let _g = lock();
    fresh();
    let mut w = work();
    let mut at = Vec::new();
    for i in 0..6u32 {
        let c = insert(&mut w, &std::format!("f{i}"), 200 + i * 4, 4 * 512, &[]).unwrap();
        at.push((c.h.table_first, c.h.table_sectors));
    }
    // successive tables are written to different places (the old one stays valid until the headers move)
    for pair in at.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        assert!(a.0 + a.1 <= b.0 || b.0 + b.1 <= a.0, "{a:?} {b:?}");
    }
}

#[test]
fn many_entries_are_streamed_through_small_buffers() {
    let _g = lock();
    fresh();
    let mut w = work();
    // 600 files of one sector from sector 100: a table of 300 sectors, streamed through 1 KiB (4 entries at a time)
    for i in 0..600u32 {
        let name = std::format!("samples/s{i:04}");
        insert(&mut w, &name, 1000 + i, 512, &[]).unwrap_or_else(|e| panic!("entry {i}: error {e}"));
    }
    let h = current(&mut w);
    assert_eq!((h.count, h.table_sectors), (600, 300));
    assert_eq!(h.used, 1 + 300 + 600);
    let p = cstr("samples/s0599");
    let f = unsafe { find(&IO, &LAY, &h, w.a.0.as_mut_ptr(), 4096, p.as_ptr()) }.unwrap().unwrap();
    assert_eq!((f.0, f.1.first), (599, 1599));
    // the table check holds, and the free-space figure counts the room for one more entry
    assert_eq!(unsafe { table_crc(&IO, &LAY, h.table_first, h.table_sectors, w.a.0.as_mut_ptr(), 4096) }.unwrap(), h.table_crc);
    assert_eq!(free_sectors(&LAY, &h), SECTORS - TAIL - h.used - table_sectors(601));
}

#[test]
fn a_clear_leaves_an_empty_store() {
    let _g = lock();
    fresh();
    let mut w = work();
    insert(&mut w, "a", 10, 512, &[]).unwrap();
    insert(&mut w, "b", 11, 512, &[]).unwrap();
    let cur = current(&mut w);
    let c = unsafe { commit(&IO, &LAY, &cur, w.a.0.as_mut_ptr(), w.b.0.as_mut_ptr(), 1024, w.s.0.as_mut_ptr(), Edit::Clear, &[]) }.unwrap();
    assert_eq!((c.h.count, c.h.table_sectors, c.h.used, c.h.seq), (0, 0, 1, 3));
    let t = state(&mut w).unwrap();
    assert_eq!(t.h.count, 0);
    assert!(t.agree);
    assert!(paths(&mut w).is_empty());
}

#[test]
fn a_full_area_refuses_the_table_with_e_full() {
    let _g = lock();
    fresh();
    let mut w = work();
    insert(&mut w, "a", 10, 512, &[]).unwrap();
    // a reserved extent covers every free sector: no gap is left for the next table
    let cur = current(&mut w);
    let big = (1, SECTORS - 3 - 1);
    let c = insert(&mut w, "b", 11, 512, &[big]);
    assert_eq!(c.err(), Some(E_FULL));
    assert_eq!(current(&mut w), cur);
}

/// A power cut after any number of writes of a commit leaves the old store or the new one: both are readable and
/// the data of the old one is intact.
#[test]
fn a_power_cut_at_any_write_leaves_the_old_store_or_the_new_one() {
    let _g = lock();
    for case in 0..3 {
        for cut in 0..40u32 {
            fresh();
            let mut w = work();
            insert(&mut w, "a", 10, 512, &[]).unwrap();
            insert(&mut w, "b", 11, 512, &[]).unwrap();
            let old = paths(&mut w);
            *CUT.lock().unwrap() = (cut, 0);
            let r = match case {
                0 => insert(&mut w, "c", 12, 512, &[]).map(|_| ()),
                1 => remove(&mut w, "a").map(|_| ()),
                _ => {
                    let cur = current(&mut w);
                    unsafe { commit(&IO, &LAY, &cur, w.a.0.as_mut_ptr(), w.b.0.as_mut_ptr(), 1024, w.s.0.as_mut_ptr(), Edit::Clear, &[]) }.map(|_| ())
                }
            };
            *CUT.lock().unwrap() = (u32::MAX, 0);
            let now = paths(&mut w);
            let t = state(&mut w).unwrap_or_else(|e| panic!("case {case} cut {cut}: the store is unreadable ({e})"));
            if r.is_ok() {
                assert_ne!(now, old, "case {case} cut {cut}: the commit finished and changed nothing");
            } else {
                // failed: the old store, or (the cut was after header B) the new one
                let new_ok = match case {
                    0 => now.len() == 3,
                    1 => now.len() == 1,
                    _ => now.is_empty(),
                };
                assert!(now == old || new_ok, "case {case} cut {cut}: {now:?}");
            }
            // whichever it is, the table checks
            let ok = unsafe { table_crc(&IO, &LAY, t.h.table_first, t.h.table_sectors, w.a.0.as_mut_ptr(), 4096) }.unwrap();
            assert_eq!(ok, t.h.table_crc, "case {case} cut {cut}");
        }
    }
}

#[test]
fn header_copies_fall_back_and_the_lagging_one_is_repaired() {
    let _g = lock();
    fresh();
    let mut w = work();
    insert(&mut w, "a", 10, 512, &[]).unwrap();
    insert(&mut w, "b", 11, 512, &[]).unwrap();
    // A torn: B is used
    CARD.lock().unwrap()[30] ^= 1;
    let t = state(&mut w).unwrap();
    assert_eq!((t.h.seq, t.copy, t.agree), (2, 1, false));
    assert!(unsafe { sync_headers(&IO, &LAY, &t, w.s.0.as_mut_ptr()) });
    let t = state(&mut w).unwrap();
    assert_eq!((t.h.seq, t.copy, t.agree), (2, 0, true));
    // B torn (a flipped bit under its CRC): A is used
    CARD.lock().unwrap()[LAY.header_b() as usize * 512 + 9] ^= 1;
    let t = state(&mut w).unwrap();
    assert_eq!((t.h.seq, t.copy, t.agree), (2, 0, false));
    // both torn
    CARD.lock().unwrap()[30] ^= 1;
    assert_eq!(state(&mut w).err(), Some(E_BAD_STORE));
}

#[test]
fn a_higher_seq_in_b_wins_and_a_table_that_does_not_check_is_skipped() {
    let _g = lock();
    fresh();
    let mut w = work();
    insert(&mut w, "a", 10, 512, &[]).unwrap();
    let c = insert(&mut w, "b", 11, 512, &[]).unwrap();
    // header A lags: seq 1 with the old table (as after a cut between B and A)
    let mut old = c.h;
    old.seq = 1;
    unsafe { encode_header(&old, w.s.0.as_mut_ptr()) };
    CARD.lock().unwrap()[..512].copy_from_slice(&w.s.0);
    let t = state(&mut w).unwrap();
    assert_eq!((t.h.seq, t.copy, t.agree), (2, 1, false));
    // B names a table that does not check: A (seq 1, pointing at the same table here) fails too, so the store is bad;
    // with a good table behind A, A is used
    let mut bad = c.h;
    bad.table_crc ^= 1;
    unsafe { encode_header(&bad, w.s.0.as_mut_ptr()) };
    let b_at = LAY.header_b() as usize * 512;
    CARD.lock().unwrap()[b_at..b_at + 512].copy_from_slice(&w.s.0);
    let good_a = Hdr { seq: 1, ..c.h };
    unsafe { encode_header(&good_a, w.s.0.as_mut_ptr()) };
    CARD.lock().unwrap()[..512].copy_from_slice(&w.s.0);
    let t = state(&mut w).unwrap();
    assert_eq!((t.h.seq, t.copy), (1, 0));
}

#[test]
fn a_store_made_for_another_area_is_not_this_one() {
    let _g = lock();
    fresh();
    let mut w = work();
    let c = insert(&mut w, "a", 10, 512, &[]).unwrap();
    let h = Hdr { area: SECTORS + 1, ..c.h };
    unsafe { encode_header(&h, w.s.0.as_mut_ptr()) };
    CARD.lock().unwrap()[..512].copy_from_slice(&w.s.0);
    let b_at = LAY.header_b() as usize * 512;
    CARD.lock().unwrap()[b_at..b_at + 512].copy_from_slice(&w.s.0);
    assert_eq!(state(&mut w).err(), Some(E_BAD_STORE));
}

#[test]
fn paths_and_their_relations() {
    unsafe {
        for (p, ok) in [
            ("a", true),
            ("modules/a.dkm", true),
            ("samples/drums/kick 1.wav", true),
            ("", false),
            ("/a", false),
            ("a//b", false),
            ("a/../b", false),
            ("./a", false),
            ("a/.", false),
            ("a\tb", false),
        ] {
            let c = cstr(p);
            assert_eq!(path_ok(c.as_ptr(), 160).is_some(), ok, "{p:?}");
        }
        let long = cstr(&"x".repeat(159));
        assert_eq!(path_ok(long.as_ptr(), 160), Some(159));
        let too = cstr(&"x".repeat(160));
        assert_eq!(path_ok(too.as_ptr(), 161), None);
        // relation(path, entry)
        let mk = |p: &str, kind: u8| {
            let mut e = [0u8; ENTRY];
            let c = cstr(p);
            encode_entry(c.as_ptr(), &entry(1, 1, kind), e.as_mut_ptr());
            e
        };
        let (file_a, dir_a) = (mk("a", KIND_FILE), mk("a", KIND_DIR));
        let inside = mk("a/b", KIND_FILE);
        let other = mk("ab", KIND_FILE);
        let pa = cstr("a");
        let pab = cstr("a/b");
        assert_eq!(relation(pa.as_ptr(), file_a.as_ptr()), 1);
        assert_eq!(relation(pa.as_ptr(), dir_a.as_ptr()), 1);
        assert_eq!(relation(pa.as_ptr(), inside.as_ptr()), 2);
        assert_eq!(relation(pab.as_ptr(), file_a.as_ptr()), 3);
        assert_eq!(relation(pab.as_ptr(), dir_a.as_ptr()), 0); // a folder is not a file: files may live in it
        assert_eq!(relation(pa.as_ptr(), other.as_ptr()), 0);
    }
}

// ---- a reader against a commit in another task ----

static HOOK: Mutex<Option<(u32, u32)>> = Mutex::new(None); // (reads so far, read number at which the commit runs)

unsafe extern "C" fn rd_hooked(sector: u32, bytes: u32, buf: *mut u8) -> i32 {
    let fire = {
        let mut g = HOOK.lock().unwrap();
        match g.as_mut() {
            Some(h) => {
                let n = h.0;
                h.0 += 1;
                n == h.1
            }
            None => false,
        }
    };
    if fire {
        // a commit in the middle of the read: a new file and its table
        let mut w = work();
        insert(&mut w, "late", 300, 512, &[]).unwrap();
    }
    rd(sector, bytes, buf)
}

#[test]
fn a_commit_during_a_stable_read_is_never_half_seen() {
    let _g = lock();
    let hooked = Io { rd: rd_hooked, wr };
    for at in 0..12u32 {
        fresh();
        let mut w = work();
        insert(&mut w, "a", 10, 512, &[]).unwrap();
        insert(&mut w, "b", 11, 512, &[]).unwrap();
        *HOOK.lock().unwrap() = Some((0, at));
        let mut count = 0;
        let mut buf = work();
        let r = unsafe {
            stable(&hooked, &LAY, buf.s.0.as_mut_ptr(), || {
                let t = open(&hooked, &LAY, buf.b.0.as_mut_ptr().add(0), buf.a.0.as_mut_ptr(), 4096, true)?;
                let mut n = 0;
                let r = scan(&hooked, &LAY, &t.h, buf.a.0.as_mut_ptr(), 4096, |_, _| {
                    n += 1;
                    true
                });
                if r != OK {
                    return Err(r);
                }
                count = n;
                Ok((t.h.count, n))
            })
        };
        *HOOK.lock().unwrap() = None;
        let (hc, n) = r.unwrap_or_else(|e| panic!("read {at}: error {e}"));
        assert_eq!(hc, n, "the header and the table agree (commit at read {at})");
        assert!(hc == 2 || hc == 3, "{hc}");
        assert_eq!(count, n);
    }
}

#[test]
fn a_store_that_reaches_into_the_tail_is_not_used() {
    // A store written before the settings area had a tail of 3 sectors: a file may lie where the settings copies and header B are now.
    let _g = lock();
    fresh();
    let mut w = work();
    let end = LAY.header_b();
    // a file that ends exactly at header B is fine (the allocator's own limit)
    insert(&mut w, "last", end - 4, 4 * 512, &[]).unwrap();
    let t = state(&mut w).unwrap();
    assert!(!unsafe { reaches_tail(&IO, &LAY, &t.h, w.a.0.as_mut_ptr(), 4096) }.unwrap());
    // one sector more, into the settings area: both header copies say so, and there is no usable copy (a reader never takes it as a store)
    insert(&mut w, "late", end - 2, 4 * 512, &[]).unwrap_or_else(|e| panic!("{e}"));
    let h = current(&mut w);
    assert_eq!(h.count, 0, "the store is not read: `current` falls back to the empty store");
    assert_eq!(state(&mut w).err(), Some(E_BAD_STORE));
    // a table in the tail
    fresh();
    let mut w = work();
    insert(&mut w, "a", 10, 512, &[]).unwrap();
    let t = state(&mut w).unwrap();
    let moved = Hdr { table_first: end, ..t.h };
    assert!(unsafe { reaches_tail(&IO, &LAY, &moved, w.a.0.as_mut_ptr(), 4096) }.unwrap());
}

// A deterministic digest seam: the runtime supplies its SHA-256 implementation.
fn snapshot_hash(bytes: &[u8]) -> [u8; 32] {
    let mut out = [0; 32];
    for (i, byte) in bytes.iter().enumerate() {
        out[i % 32] ^= byte;
    }
    out
}

fn snapshot_file(w: &mut Work) {
    let data = [0x51; 513];
    let e = Entry { first: 100, size: data.len() as u32, kind: KIND_FILE, sha: snapshot_hash(&data) };
    let mut encoded = [0; ENTRY];
    unsafe { encode_entry(cstr("sample").as_ptr(), &e, encoded.as_mut_ptr()) };
    CARD.lock().unwrap()[100 * 512..100 * 512 + data.len()].copy_from_slice(&data);
    let h = current(w);
    unsafe { commit(&IO, &LAY, &h, w.a.0.as_mut_ptr(), w.b.0.as_mut_ptr(), 1024, w.s.0.as_mut_ptr(), Edit::Insert(encoded.as_ptr()), &[]) }.unwrap();
}

unsafe fn take_snapshot(io: &Io, w: &mut Work, dst: &mut Buf<1024>) -> Result<Entry, i32> {
    snapshot(io, &LAY, cstr("sample").as_ptr(), w.s.0.as_mut_ptr(), w.a.0.as_mut_ptr(), 4096, dst.0.as_mut_ptr(), 1024, 513, snapshot_hash)
}

#[test]
fn snapshot_owns_retained_bytes_and_rejects_corruption_or_read_failure() {
    let _g = lock();
    fresh();
    let mut w = work();
    snapshot_file(&mut w);
    let mut dst = Buf([0; 1024]);
    assert_eq!(unsafe { take_snapshot(&IO, &mut w, &mut dst) }.unwrap().size, 513);
    CARD.lock().unwrap()[100 * 512] ^= 1;
    assert_eq!(&dst.0[..513], &[0x51; 513]);
    assert_eq!(unsafe { take_snapshot(&IO, &mut w, &mut dst) }, Err(crate::E_SHA));
    unsafe extern "C" fn fail(_: u32, _: u32, _: *mut u8) -> i32 { -1 }
    assert_eq!(unsafe { take_snapshot(&Io { rd: fail, wr }, &mut w, &mut dst) }, Err(E_READ));
}

#[test]
fn snapshot_retries_commits_at_lookup_and_read_boundaries() {
    let _g = lock();
    for at in 0..18 {
        fresh();
        let mut w = work();
        snapshot_file(&mut w);
        *HOOK.lock().unwrap() = Some((0, at));
        let mut dst = Buf([0; 1024]);
        let r = unsafe { take_snapshot(&Io { rd: rd_hooked, wr }, &mut w, &mut dst) };
        *HOOK.lock().unwrap() = None;
        assert_eq!(r.unwrap().size, 513, "commit at read {at}");
        assert_eq!(&dst.0[..513], &[0x51; 513]);
    }
}

#[test]
fn snapshot_fails_closed_after_retry_exhaustion_and_after_missing_final_fingerprint() {
    let _g = lock();
    fresh();
    let mut w = work();
    snapshot_file(&mut w);
    // Change a header on each payload read. This simulates writers committing
    // continuously, including after each successful copy into private storage.
    unsafe extern "C" fn changing(sector: u32, bytes: u32, buf: *mut u8) -> i32 {
        if sector == FIRST + 100 {
            let mut w = work();
            insert(&mut w, "late", 300, 512, &[]).unwrap();
        }
        rd(sector, bytes, buf)
    }
    let mut dst = Buf([0; 1024]);
    assert_eq!(unsafe { take_snapshot(&Io { rd: changing, wr }, &mut w, &mut dst) }, Err(crate::E_BUSY));
    static READ_PAYLOAD: Mutex<bool> = Mutex::new(false);
    unsafe extern "C" fn missing_after(sector: u32, bytes: u32, buf: *mut u8) -> i32 {
        if *READ_PAYLOAD.lock().unwrap() { return -1; }
        if sector == FIRST + 100 { *READ_PAYLOAD.lock().unwrap() = true; }
        rd(sector, bytes, buf)
    }
    *READ_PAYLOAD.lock().unwrap() = false;
    assert_eq!(unsafe { take_snapshot(&Io { rd: missing_after, wr }, &mut w, &mut dst) }, Err(E_READ));
}

#[path = "../../../src/rs/src/sha256.rs"]
mod stream_sha;
use stream::FileStream;

fn stream_digest(data: &[u8]) -> [u8; 32] {
    let mut sha = stream_sha::Sha::new();
    let mut out = [0; 32];
    unsafe {
        sha.update(data.as_ptr(), data.len() as u32);
        sha.finish(out.as_mut_ptr());
    }
    out
}

fn stream_file(w: &mut Work, size: usize) -> Vec<u8> {
    let data: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
    let e = Entry {
        first: 100,
        size: size as u32,
        kind: KIND_FILE,
        sha: stream_digest(&data),
    };
    let mut encoded = [0; ENTRY];
    unsafe { encode_entry(cstr("sample").as_ptr(), &e, encoded.as_mut_ptr()) };
    let sectors = sectors_of(e.size) as usize;
    CARD.lock().unwrap()[100 * 512..(100 + sectors) * 512].fill(0xee);
    CARD.lock().unwrap()[100 * 512..100 * 512 + size].copy_from_slice(&data);
    let h = current(w);
    unsafe {
        commit(
            &IO,
            &LAY,
            &h,
            w.a.0.as_mut_ptr(),
            w.b.0.as_mut_ptr(),
            1024,
            w.s.0.as_mut_ptr(),
            Edit::Insert(encoded.as_ptr()),
            &[],
        )
    }
    .unwrap();
    data
}

unsafe fn stream_open(io: &Io, w: &mut Work, max: u32) -> Result<FileStream, i32> {
    FileStream::open(io, &LAY, cstr("sample").as_ptr(), w.s.0.as_mut_ptr(), w.a.0.as_mut_ptr(), 4096, max)
}

#[test]
fn stream_multichunk_exact_hash_excludes_padding_and_caps_io() {
    let _g = lock();
    fresh();
    let mut w = work();
    let data = stream_file(&mut w, 130 * 512 + 17);
    let mut cursor = unsafe { stream_open(&IO, &mut w, u32::MAX) }.unwrap();
    let mut dst = Box::new(Buf([0; 129 * 512]));
    let mut sha = stream_sha::Sha::new();
    let mut total = 0;
    for expected in [128 * 512, 2 * 512 + 17, 0] {
        let n = unsafe { cursor.read(w.s.0.as_mut_ptr(), dst.0.as_mut_ptr(), dst.0.len()) }.unwrap();
        assert_eq!(n, expected);
        assert_eq!(&dst.0[..n], &data[total..total + n]);
        unsafe {
            sha.update(dst.0.as_ptr(), n as u32);
        }
        total += n;
    }
    let mut digest = [0; 32];
    unsafe {
        sha.finish(digest.as_mut_ptr());
    }
    let e = unsafe { cursor.finish(w.s.0.as_mut_ptr(), digest) }.unwrap();
    assert_eq!(e.size as usize, data.len());
    assert_eq!(unsafe { cursor.finish(w.s.0.as_mut_ptr(), digest) }, Ok(e));
    assert_eq!(unsafe { cursor.read(w.s.0.as_mut_ptr(), dst.0.as_mut_ptr(), dst.0.len()) }, Ok(0));
}

#[test]
fn stream_replacement_before_between_and_during_chunks_is_sticky() {
    let _g = lock();
    unsafe extern "C" fn changing(sector: u32, bytes: u32, buf: *mut u8) -> i32 {
        let r = rd(sector, bytes, buf);
        if sector == FIRST + 100 {
            insert(&mut work(), "sample", 300, 1025, &[]).unwrap();
        }
        r
    }
    for timing in 0..3 {
        fresh();
        let mut w = work();
        stream_file(&mut w, 1025);
        let io = if timing == 2 { Io { rd: changing, wr } } else { IO };
        let mut c = unsafe { stream_open(&io, &mut w, 1025) }.unwrap();
        if timing == 1 {
            assert_eq!(unsafe { c.read(w.s.0.as_mut_ptr(), w.b.0.as_mut_ptr(), 512) }, Ok(512));
        }
        if timing != 2 {
            insert(&mut w, "sample", 300, 1025, &[]).unwrap();
        }
        assert_eq!(unsafe { c.read(w.s.0.as_mut_ptr(), w.b.0.as_mut_ptr(), 512) }, Err(crate::E_BUSY));
        assert_eq!(unsafe { c.read(w.s.0.as_mut_ptr(), w.b.0.as_mut_ptr(), 512) }, Err(crate::E_BUSY));
        assert_eq!(unsafe { c.finish(w.s.0.as_mut_ptr(), [0; 32]) }, Err(crate::E_BUSY));
    }
}

#[test]
fn stream_digest_premature_finish_and_buffer_bounds_fail_closed() {
    let _g = lock();
    fresh();
    let mut w = work();
    stream_file(&mut w, 513);
    let mut c = unsafe { stream_open(&IO, &mut w, 513) }.unwrap();
    assert_eq!(unsafe { c.finish(w.s.0.as_mut_ptr(), [0; 32]) }, Err(crate::E_FORMAT));
    assert_eq!(unsafe { c.read(w.s.0.as_mut_ptr(), w.b.0.as_mut_ptr(), 512) }, Err(crate::E_FORMAT));
    let mut c = unsafe { stream_open(&IO, &mut w, 513) }.unwrap();
    CARD.lock().unwrap()[100 * 512] ^= 1;
    assert_eq!(unsafe { c.read(w.s.0.as_mut_ptr(), w.b.0.as_mut_ptr(), 1024) }, Ok(513));
    // Hash what was actually read: corruption is discovered only at completion.
    let digest = stream_digest(&w.b.0[..513]);
    assert_eq!(unsafe { c.finish(w.s.0.as_mut_ptr(), digest) }, Err(crate::E_SHA));
    assert_eq!(unsafe { c.finish(w.s.0.as_mut_ptr(), digest) }, Err(crate::E_SHA));
    for cap in [0, 511, 513] {
        let mut c = unsafe { stream_open(&IO, &mut w, 513) }.unwrap();
        assert_eq!(unsafe { c.read(w.s.0.as_mut_ptr(), w.b.0.as_mut_ptr(), cap) }, Err(E_BAD_STORE));
    }
    assert_eq!(unsafe { stream_open(&IO, &mut w, 512) }.err(), Some(crate::E_TOO_BIG));
    let mut c = unsafe { stream_open(&IO, &mut w, 513) }.unwrap();
    assert_eq!(
        unsafe { c.read(w.s.0.as_mut_ptr(), w.b.0.as_mut_ptr().add(1), 512) },
        Err(E_BAD_STORE)
    );
}

#[test]
fn stream_failed_fingerprint_or_payload_read_is_sticky() {
    let _g = lock();
    fresh();
    let mut w = work();
    stream_file(&mut w, 513);
    static FAIL_AT: Mutex<Option<u32>> = Mutex::new(None);
    unsafe extern "C" fn failing(sector: u32, bytes: u32, buf: *mut u8) -> i32 {
        if *FAIL_AT.lock().unwrap() == Some(sector) {
            return -1;
        }
        rd(sector, bytes, buf)
    }
    let io = Io { rd: failing, wr };
    for sector in [FIRST, FIRST + LAY.header_b(), FIRST + 100] {
        *FAIL_AT.lock().unwrap() = None;
        let mut c = unsafe { stream_open(&io, &mut w, 513) }.unwrap();
        *FAIL_AT.lock().unwrap() = Some(sector);
        assert_eq!(unsafe { c.read(w.s.0.as_mut_ptr(), w.b.0.as_mut_ptr(), 1024) }, Err(E_READ));
        *FAIL_AT.lock().unwrap() = None;
        assert_eq!(unsafe { c.finish(w.s.0.as_mut_ptr(), [0; 32]) }, Err(E_READ));
    }
}

#[test]
fn stream_lookup_rejects_generation_changes_and_failed_headers() {
    let _g = lock();
    // Every read boundary of the complete initial lookup (fingerprints, headers,
    // CRC, extent verification, and find) must fail instead of retrying.
    for at in 1..9 {
        fresh();
        let mut w = work();
        stream_file(&mut w, 513);
        *HOOK.lock().unwrap() = Some((0, at));
        let result = unsafe { stream_open(&Io { rd: rd_hooked, wr }, &mut w, 513) };
        *HOOK.lock().unwrap() = None;
        assert_eq!(result.err(), Some(crate::E_BUSY), "commit at lookup read {at}");
    }
    unsafe extern "C" fn fail(_: u32, _: u32, _: *mut u8) -> i32 {
        -1
    }
    let mut w = work();
    assert_eq!(unsafe { stream_open(&Io { rd: fail, wr }, &mut w, 513) }.err(), Some(E_READ));
    fresh();
    stream_file(&mut w, 0);
    assert_eq!(unsafe { stream_open(&IO, &mut w, u32::MAX) }.err(), Some(crate::E_TOO_BIG));
    // Verified table contents still need a valid data extent.
    fresh();
    insert(&mut w, "sample", 0, 513, &[]).unwrap();
    assert_eq!(unsafe { stream_open(&IO, &mut w, 513) }.err(), Some(E_BAD_STORE));
    fresh();
    insert(&mut w, "sample", 100, u32::MAX, &[]).unwrap();
    assert_eq!(unsafe { stream_open(&IO, &mut w, u32::MAX) }.err(), Some(E_BAD_STORE));
}

#[test]
fn stream_final_check_and_post_read_fingerprint_fail_closed() {
    let _g = lock();
    fresh();
    let mut w = work();
    let data = stream_file(&mut w, 513);
    let mut c = unsafe { stream_open(&IO, &mut w, 513) }.unwrap();
    assert_eq!(unsafe { c.read(w.s.0.as_mut_ptr(), w.b.0.as_mut_ptr(), 1024) }, Ok(513));
    insert(&mut w, "sample", 300, 513, &[]).unwrap();
    assert_eq!(unsafe { c.finish(w.s.0.as_mut_ptr(), stream_digest(&data)) }, Err(crate::E_BUSY));
    static PAYLOAD: Mutex<bool> = Mutex::new(false);
    unsafe extern "C" fn missing_after(sector: u32, bytes: u32, buf: *mut u8) -> i32 {
        if *PAYLOAD.lock().unwrap() {
            return -1;
        }
        if sector == FIRST + 100 {
            *PAYLOAD.lock().unwrap() = true;
        }
        rd(sector, bytes, buf)
    }
    fresh();
    stream_file(&mut w, 513);
    *PAYLOAD.lock().unwrap() = false;
    let mut c = unsafe { stream_open(&Io { rd: missing_after, wr }, &mut w, 513) }.unwrap();
    assert_eq!(unsafe { c.read(w.s.0.as_mut_ptr(), w.b.0.as_mut_ptr(), 1024) }, Err(E_READ));
    *PAYLOAD.lock().unwrap() = false;
    assert_eq!(unsafe { c.read(w.s.0.as_mut_ptr(), w.b.0.as_mut_ptr(), 1024) }, Err(E_READ));
}
use catalog::{CatalogCursor, CatalogEntry};
const CATALOG_EMPTY: CatalogEntry = CatalogEntry { path: [0; PATH_BYTES], entry: Entry { first: 0, size: 0, kind: 0, sha: [0; 32] } };

/// Install a real CRC-checked table directly, including a maximum-length path.
fn catalog_table(count: u32) -> Vec<CatalogEntry> {
    let mut expected = Vec::new();
    let mut card = CARD.lock().unwrap();
    for i in 0..count {
        let name = if i == 1 { "x".repeat(PATH_MAX) } else { std::format!("folder/item-{i}") };
        let mut value = CATALOG_EMPTY;
        value.path[..name.len()].copy_from_slice(name.as_bytes());
        value.entry = if i & 1 == 0 { entry(1000 + i, i + 1, KIND_FILE) } else { entry(0, 0, KIND_DIR) };
        unsafe {
            encode_entry(value.path.as_ptr(), &value.entry, card.as_mut_ptr().add(10 * 512 + i as usize * ENTRY));
        }
        expected.push(value);
    }
    let mut h =
        Hdr { count, table_first: 10, table_sectors: table_sectors(count), table_crc: 0, seq: 7, used: 1 + table_sectors(count) + count, area: SECTORS };
    h.table_crc = crc32_of(card[10 * 512..].as_ptr(), h.table_sectors as usize * 512);
    unsafe {
        encode_header(&h, card.as_mut_ptr());
        encode_header(&h, card.as_mut_ptr().add(LAY.header_b() as usize * 512));
    }
    expected
}

unsafe fn catalog_open(io: &Io, w: &mut Work) -> Result<CatalogCursor, i32> {
    CatalogCursor::open(io, &LAY, w.s.0.as_mut_ptr(), w.a.0.as_mut_ptr(), 4096)
}

fn catalog_change() {
    let mut card = CARD.lock().unwrap();
    for at in [0, LAY.header_b() as usize * 512] {
        unsafe {
            let p = card.as_mut_ptr().add(at);
            let mut h = parse_header(p).unwrap();
            h.seq += 1;
            encode_header(&h, p);
        }
    }
}

#[test]
fn catalog_pages_keep_full_paths_metadata_and_bound_each_read() {
    let _g = lock();
    fresh();
    let expected = catalog_table(263);
    static READS: Mutex<Vec<(u32, u32)>> = Mutex::new(Vec::new());
    unsafe extern "C" fn counted(sector: u32, bytes: u32, buf: *mut u8) -> i32 {
        READS.lock().unwrap().push((sector, bytes));
        rd(sector, bytes, buf)
    }
    let mut w = work();
    let mut c = unsafe { catalog_open(&Io { rd: counted, wr }, &mut w) }.unwrap();
    let mut scratch = Box::new(Buf([0; 129 * 512]));
    let mut out = [CATALOG_EMPTY; 300];
    let mut got = Vec::new();
    // Odd output boundaries reread only the shared sector, never rescan.
    for (capacity, n) in [(3, 3), (300, 255), (3, 3), (300, 2), (300, 0)] {
        READS.lock().unwrap().clear();
        assert_eq!(unsafe { c.read_page(w.s.0.as_mut_ptr(), scratch.0.as_mut_ptr(), scratch.0.len(), &mut out[..capacity]) }, Ok(n));
        got.extend_from_slice(&out[..n]);
        let reads = READS.lock().unwrap();
        assert_eq!(reads.len(), if n == 0 { 4 } else { 5 });
        assert!(reads.iter().all(|(_, bytes)| *bytes <= MAX_IO_SECTORS * 512));
        if n > 0 {
            assert_eq!(reads[2].1 as usize, (n + ((got.len() - n) & 1) + 1) / 2 * 512);
        }
    }
    assert_eq!(got, expected);
    catalog_change();
    assert_eq!(unsafe { c.read_page(w.s.0.as_mut_ptr(), w.a.0.as_mut_ptr(), 512, &mut out) }, Err(crate::E_BUSY));
}

#[test]
fn catalog_empty_eof_and_preferred_corrupt_table_fallback() {
    let _g = lock();
    fresh();
    catalog_table(0);
    let mut w = work();
    let mut out = [CATALOG_EMPTY; 3];
    let mut c = unsafe { catalog_open(&IO, &mut w) }.unwrap();
    for _ in 0..2 {
        assert_eq!(unsafe { c.read_page(w.s.0.as_mut_ptr(), w.a.0.as_mut_ptr(), 512, &mut out) }, Ok(0));
    }
    let expected = catalog_table(3);
    {
        let mut card = CARD.lock().unwrap();
        let mut h = unsafe { parse_header(card.as_ptr()) }.unwrap();
        h.seq += 1;
        h.table_first = 20; // higher generation, bad CRC
        unsafe {
            encode_header(&h, card.as_mut_ptr());
        }
    }
    let mut c = unsafe { catalog_open(&IO, &mut w) }.unwrap();
    assert_eq!(unsafe { c.read_page(w.s.0.as_mut_ptr(), w.a.0.as_mut_ptr(), 1024, &mut out) }, Ok(3));
    assert_eq!(out.as_slice(), expected.as_slice());
}

#[test]
fn catalog_mutations_before_between_during_pages_and_open_fail_closed() {
    let _g = lock();
    static TRIGGER: Mutex<bool> = Mutex::new(false);
    unsafe extern "C" fn changing(sector: u32, bytes: u32, buf: *mut u8) -> i32 {
        let r = rd(sector, bytes, buf);
        if sector == FIRST + 10 && *TRIGGER.lock().unwrap() {
            catalog_change();
        }
        r
    }
    for timing in 0..4 {
        fresh();
        catalog_table(5);
        *TRIGGER.lock().unwrap() = timing == 3;
        let mut w = work();
        let mut out = [CATALOG_EMPTY; 1];
        let result = unsafe { catalog_open(&Io { rd: changing, wr }, &mut w) };
        if timing == 3 {
            assert_eq!(result.err(), Some(crate::E_BUSY));
            continue;
        }
        let mut c = result.unwrap();
        if timing == 1 {
            assert_eq!(unsafe { c.read_page(w.s.0.as_mut_ptr(), w.a.0.as_mut_ptr(), 512, &mut out) }, Ok(1));
        }
        if timing == 2 {
            *TRIGGER.lock().unwrap() = true;
        } else {
            catalog_change();
        }
        assert_eq!(unsafe { c.read_page(w.s.0.as_mut_ptr(), w.a.0.as_mut_ptr(), 512, &mut out) }, Err(crate::E_BUSY));
        *TRIGGER.lock().unwrap() = false;
        for _ in 0..2 {
            assert_eq!(unsafe { c.read_page(w.s.0.as_mut_ptr(), w.a.0.as_mut_ptr(), 512, &mut out) }, Err(crate::E_BUSY));
        }
    }
}

#[test]
fn catalog_io_failures_and_bad_buffers_stick() {
    let _g = lock();
    fresh();
    catalog_table(5);
    static FAIL: Mutex<Option<u32>> = Mutex::new(None);
    unsafe extern "C" fn failing(sector: u32, bytes: u32, buf: *mut u8) -> i32 {
        if *FAIL.lock().unwrap() == Some(sector) {
            return -1;
        }
        rd(sector, bytes, buf)
    }
    let mut w = work();
    let mut out = [CATALOG_EMPTY; 2];
    for at in [FIRST, FIRST + LAY.header_b(), FIRST + 10] {
        *FAIL.lock().unwrap() = None;
        let mut c = unsafe { catalog_open(&Io { rd: failing, wr }, &mut w) }.unwrap();
        *FAIL.lock().unwrap() = Some(at);
        assert_eq!(unsafe { c.read_page(w.s.0.as_mut_ptr(), w.a.0.as_mut_ptr(), 512, &mut out) }, Err(E_READ));
        *FAIL.lock().unwrap() = None;
        assert_eq!(unsafe { c.read_page(w.s.0.as_mut_ptr(), w.a.0.as_mut_ptr(), 512, &mut out) }, Err(E_READ));
    }
    for bad in 0..8 {
        let mut c = unsafe { catalog_open(&IO, &mut w) }.unwrap();
        let fp = if bad == 0 {
            core::ptr::null_mut()
        } else if bad == 6 {
            unsafe { w.s.0.as_mut_ptr().add(1) }
        } else {
            w.s.0.as_mut_ptr()
        };
        let scratch = if bad == 1 {
            core::ptr::null_mut()
        } else if bad == 2 {
            unsafe { w.a.0.as_mut_ptr().add(1) }
        } else {
            w.a.0.as_mut_ptr()
        };
        let cap = if bad == 3 {
            0
        } else if bad == 4 {
            513
        } else if bad == 7 {
            usize::MAX - 511
        } else {
            512
        };
        let output = if bad == 5 { &mut out[..0] } else { &mut out[..] };
        assert_eq!(unsafe { c.read_page(fp, scratch, cap, output) }, Err(E_BAD_STORE));
        assert_eq!(unsafe { c.read_page(w.s.0.as_mut_ptr(), w.a.0.as_mut_ptr(), 512, &mut out) }, Err(E_BAD_STORE));
    }
    for lay in [Layout { first: u32::MAX, sectors: SECTORS }, Layout { first: FIRST, sectors: TAIL }] {
        assert_eq!(unsafe { CatalogCursor::open(&IO, &lay, w.s.0.as_mut_ptr(), w.a.0.as_mut_ptr(), 512) }.err(), Some(E_BAD_STORE));
    }
    assert_eq!(unsafe { CatalogCursor::open(&IO, &LAY, w.s.0.as_mut_ptr(), core::ptr::null_mut(), 512) }.err(), Some(E_BAD_STORE));
}

#[test]
fn catalog_crc_verified_but_malformed_entries_are_rejected() {
    let _g = lock();
    for bad_kind in [false, true] {
        fresh();
        catalog_table(3);
        {
            let mut card = CARD.lock().unwrap();
            if bad_kind {
                card[10 * 512 + O_KIND] = 2;
            } else {
                card[10 * 512..10 * 512 + PATH_BYTES].fill(b'x');
            }
            let mut h = unsafe { parse_header(card.as_ptr()) }.unwrap();
            h.table_crc = crc32_of(card[10 * 512..].as_ptr(), h.table_sectors as usize * 512);
            unsafe {
                encode_header(&h, card.as_mut_ptr());
                encode_header(&h, card.as_mut_ptr().add(LAY.header_b() as usize * 512));
            }
        }
        let mut w = work();
        let mut out = [CATALOG_EMPTY; 3];
        let mut c = unsafe { catalog_open(&IO, &mut w) }.unwrap();
        assert_eq!(unsafe { c.read_page(w.s.0.as_mut_ptr(), w.a.0.as_mut_ptr(), 1024, &mut out) }, Err(E_BAD_STORE));
        assert_eq!(unsafe { c.read_page(w.s.0.as_mut_ptr(), w.a.0.as_mut_ptr(), 1024, &mut out) }, Err(E_BAD_STORE));
    }
}

#[test]
fn catalog_post_read_header_failure_invalidates_populated_output() {
    let _g = lock();
    fresh();
    catalog_table(3);
    static AFTER: Mutex<bool> = Mutex::new(false);
    static ENABLED: Mutex<bool> = Mutex::new(false);
    unsafe extern "C" fn missing_after(sector: u32, bytes: u32, buf: *mut u8) -> i32 {
        if *AFTER.lock().unwrap() {
            return -1;
        }
        let r = rd(sector, bytes, buf);
        if sector == FIRST + 10 && *ENABLED.lock().unwrap() {
            *AFTER.lock().unwrap() = true;
        }
        r
    }
    *AFTER.lock().unwrap() = false;
    *ENABLED.lock().unwrap() = false;
    let io = Io { rd: missing_after, wr };
    let mut w = work();
    let mut out = [CATALOG_EMPTY; 3];
    let mut c = unsafe { catalog_open(&io, &mut w) }.unwrap();
    *ENABLED.lock().unwrap() = true;
    assert_eq!(unsafe { c.read_page(w.s.0.as_mut_ptr(), w.a.0.as_mut_ptr(), 1024, &mut out) }, Err(E_READ));
    assert_ne!(out[0], CATALOG_EMPTY); // Filled staging must still be discarded.
    *AFTER.lock().unwrap() = false;
    assert_eq!(unsafe { c.read_page(w.s.0.as_mut_ptr(), w.a.0.as_mut_ptr(), 1024, &mut out) }, Err(E_READ));
    *AFTER.lock().unwrap() = true;
    assert_eq!(unsafe { catalog_open(&io, &mut w) }.err(), Some(E_READ));
}
use catalog::CatalogOpen;
unsafe fn catalog_step(open: &mut CatalogOpen, w: &mut Work, cap: usize) -> Result<bool, i32> {
    open.step(w.s.0.as_mut_ptr(), w.a.0.as_mut_ptr(), cap)
}

#[test]
fn incremental_catalog_bounds_each_step_and_rewinds_without_reverification() {
    let _g = lock();
    fresh();
    let expected = catalog_table(263);
    static READS: Mutex<Vec<(u32, u32)>> = Mutex::new(Vec::new());
    unsafe extern "C" fn counted(sector: u32, bytes: u32, buf: *mut u8) -> i32 {
        READS.lock().unwrap().push((sector, bytes));
        rd(sector, bytes, buf)
    }
    READS.lock().unwrap().clear();
    let mut opener = CatalogOpen::begin(&Io { rd: counted, wr }, &LAY).unwrap();
    assert!(READS.lock().unwrap().is_empty());
    let mut w = work();
    assert_eq!(unsafe { catalog_step(&mut opener, &mut w, 512) }, Ok(false));
    assert_eq!(READS.lock().unwrap().len(), 6);
    let mut big = Box::new(Buf([0; 129 * 512]));
    for (done, table_bytes) in [(false, 128 * 512), (true, 4 * 512)] {
        READS.lock().unwrap().clear();
        assert_eq!(unsafe { opener.step(w.s.0.as_mut_ptr(), big.0.as_mut_ptr(), big.0.len()) }, Ok(done));
        let reads = READS.lock().unwrap();
        assert_eq!(reads.len(), 5);
        assert_eq!(reads[2].1, table_bytes);
        assert!(reads.iter().all(|(_, bytes)| *bytes <= MAX_IO_SECTORS * 512));
    }
    let mut cursor = opener.take().unwrap();
    let mut out = [CATALOG_EMPTY; 300];
    let mut actual = Vec::new();
    loop {
        let n = unsafe { cursor.read_page(w.s.0.as_mut_ptr(), big.0.as_mut_ptr(), big.0.len(), &mut out) }.unwrap();
        if n == 0 {
            break;
        }
        actual.extend_from_slice(&out[..n]);
    }
    assert_eq!(actual, expected);
    READS.lock().unwrap().clear();
    unsafe { cursor.rewind(w.s.0.as_mut_ptr()) }.unwrap();
    assert_eq!(READS.lock().unwrap().as_slice(), [(FIRST, 512), (FIRST + LAY.header_b(), 512)]);
    assert_eq!(unsafe { cursor.read_page(w.s.0.as_mut_ptr(), w.a.0.as_mut_ptr(), 512, &mut out) }, Ok(2));
    assert_eq!(&out[..2], &expected[..2]);
    catalog_change();
    assert_eq!(unsafe { cursor.rewind(w.s.0.as_mut_ptr()) }, Err(crate::E_BUSY));
    assert_eq!(unsafe { cursor.read_page(w.s.0.as_mut_ptr(), w.a.0.as_mut_ptr(), 512, &mut out) }, Err(crate::E_BUSY));
}

#[test]
fn incremental_catalog_falls_back_for_crc_tail_read_failure_and_identical_copies() {
    let _g = lock();
    static FAIL: Mutex<Option<u32>> = Mutex::new(None);
    unsafe extern "C" fn once(sector: u32, bytes: u32, buf: *mut u8) -> i32 {
        let mut fail = FAIL.lock().unwrap();
        if *fail == Some(sector) {
            *fail = None;
            return -1;
        }
        drop(fail);
        rd(sector, bytes, buf)
    }
    for bad in 0..4 {
        fresh();
        let expected = catalog_table(3);
        *FAIL.lock().unwrap() = None;
        if bad != 3 {
            let mut card = CARD.lock().unwrap();
            let table = card[10 * 512..12 * 512].to_vec();
            card[200 * 512..202 * 512].copy_from_slice(&table);
            let mut h = unsafe { parse_header(card.as_ptr()) }.unwrap();
            h.table_first = 200;
            h.seq += 1;
            if bad == 0 {
                h.table_crc ^= 1;
            }
            if bad == 1 {
                unsafe {
                    put32(card.as_mut_ptr().add(200 * 512 + O_FIRST), LAY.limit());
                }
                h.table_crc = crc32_of(card[200 * 512..].as_ptr(), 1024);
            }
            unsafe {
                encode_header(&h, card.as_mut_ptr());
            }
        }
        let mut w = work();
        let mut opener = CatalogOpen::begin(&Io { rd: once, wr }, &LAY).unwrap();
        assert_eq!(unsafe { catalog_step(&mut opener, &mut w, 4096) }, Ok(false));
        if bad >= 2 {
            *FAIL.lock().unwrap() = Some(FIRST + if bad == 2 { 200 } else { 10 });
        }
        assert_eq!(unsafe { catalog_step(&mut opener, &mut w, 4096) }, Ok(false));
        assert_eq!(unsafe { catalog_step(&mut opener, &mut w, 4096) }, Ok(true));
        let mut c = opener.take().unwrap();
        let mut out = [CATALOG_EMPTY; 3];
        assert_eq!(unsafe { c.read_page(w.s.0.as_mut_ptr(), w.a.0.as_mut_ptr(), 1024, &mut out) }, Ok(3));
        assert_eq!(out.as_slice(), expected.as_slice());
    }
}

#[test]
fn incremental_catalog_empty_absent_and_invalid_headers_keep_acceptance_rules() {
    let _g = lock();
    let mut w = work();
    for scenario in 0..5 {
        fresh();
        if scenario != 0 {
            catalog_table(0);
        }
        if scenario >= 2 {
            let mut card = CARD.lock().unwrap();
            if scenario == 2 {
                card[508] ^= 1;
            } // invalid A CRC, usable B
            if scenario == 3 {
                card[508] ^= 1;
                card[LAY.header_b() as usize * 512 + 508] ^= 1;
            }
            if scenario == 4 {
                let mut h = unsafe { parse_header(card.as_ptr()) }.unwrap();
                h.table_crc = 1;
                unsafe {
                    encode_header(&h, card.as_mut_ptr());
                    encode_header(&h, card.as_mut_ptr().add(LAY.header_b() as usize * 512));
                }
            }
        }
        let mut o = CatalogOpen::begin(&IO, &LAY).unwrap();
        let captured = unsafe { catalog_step(&mut o, &mut w, 512) };
        if scenario == 0 || scenario == 3 {
            let error = if scenario == 0 { E_NOT_FOUND } else { E_BAD_STORE };
            assert_eq!(captured, Err(error));
            assert_eq!(o.take().err(), Some(error));
        } else {
            assert_eq!(captured, Ok(false));
            if scenario == 4 {
                assert_eq!(unsafe { catalog_step(&mut o, &mut w, 512) }, Ok(false));
                assert_eq!(unsafe { catalog_step(&mut o, &mut w, 512) }, Err(E_BAD_STORE));
                assert_eq!(o.take().err(), Some(E_BAD_STORE));
            } else {
                assert_eq!(unsafe { catalog_step(&mut o, &mut w, 512) }, Ok(true));
                let mut c = o.take().unwrap();
                let mut out = [CATALOG_EMPTY; 1];
                assert_eq!(unsafe { c.read_page(w.s.0.as_mut_ptr(), w.a.0.as_mut_ptr(), 512, &mut out) }, Ok(0));
            }
        }
    }
}

#[test]
fn incremental_catalog_header_read_errors_are_strict_and_sticky() {
    let _g = lock();
    fresh();
    catalog_table(3);
    static CALL: Mutex<(u32, u32)> = Mutex::new((0, 0));
    unsafe extern "C" fn failing(sector: u32, bytes: u32, buf: *mut u8) -> i32 {
        let mut call = CALL.lock().unwrap();
        call.0 += 1;
        let fail = call.0 == call.1;
        drop(call);
        if fail {
            -1
        } else {
            rd(sector, bytes, buf)
        }
    }
    let mut w = work();
    // Includes B failure only inside capture, not just surrounding fingerprints.
    for at in [1, 2, 3, 4, 5, 6, 7, 8, 10, 11] {
        *CALL.lock().unwrap() = (0, at);
        let mut o = CatalogOpen::begin(&Io { rd: failing, wr }, &LAY).unwrap();
        let result = unsafe { catalog_step(&mut o, &mut w, 512) };
        let result = if result.is_ok() { unsafe { catalog_step(&mut o, &mut w, 512) } } else { result };
        assert_eq!(result, Err(E_READ), "header read {at}");
        assert_eq!(unsafe { catalog_step(&mut o, &mut w, 512) }, Err(E_READ));
        assert_eq!(o.take().err(), Some(E_READ));
    }
}

#[test]
fn incremental_catalog_generation_changes_before_between_and_during_steps_stick() {
    let _g = lock();
    static AT: Mutex<(u32, u32)> = Mutex::new((0, 0));
    unsafe extern "C" fn changing(sector: u32, bytes: u32, buf: *mut u8) -> i32 {
        let r = rd(sector, bytes, buf);
        let mut at = AT.lock().unwrap();
        at.0 += 1;
        let change = at.0 == at.1;
        drop(at);
        if change {
            catalog_change();
        }
        r
    }
    for at in 1..=11 {
        fresh();
        catalog_table(9);
        *AT.lock().unwrap() = (0, at);
        let mut w = work();
        let mut o = CatalogOpen::begin(&Io { rd: changing, wr }, &LAY).unwrap();
        let result = unsafe { catalog_step(&mut o, &mut w, 512) };
        let result = if result.is_ok() { unsafe { catalog_step(&mut o, &mut w, 512) } } else { result };
        // A commit after the last fingerprint byte is observable next step,
        // not an immutable lease on the just-returned fingerprint.
        let result = if result.is_ok() { unsafe { catalog_step(&mut o, &mut w, 512) } } else { result };
        assert_eq!(result, Err(crate::E_BUSY), "commit at read {at}");
        assert_eq!(unsafe { catalog_step(&mut o, &mut w, 512) }, Err(crate::E_BUSY));
        assert_eq!(o.take().err(), Some(crate::E_BUSY));
    }
    fresh();
    catalog_table(9);
    let mut w = work();
    let mut o = CatalogOpen::begin(&IO, &LAY).unwrap();
    assert_eq!(unsafe { catalog_step(&mut o, &mut w, 512) }, Ok(false));
    catalog_change();
    assert_eq!(unsafe { catalog_step(&mut o, &mut w, 512) }, Err(crate::E_BUSY));
}

#[test]
fn incremental_catalog_bounds_premature_take_and_rewind_fail_closed() {
    let _g = lock();
    fresh();
    catalog_table(3);
    let mut w = work();
    assert_eq!(CatalogOpen::begin(&IO, &LAY).unwrap().take().err(), Some(crate::E_BUSY));
    let mut o = CatalogOpen::begin(&IO, &LAY).unwrap();
    unsafe { catalog_step(&mut o, &mut w, 512) }.unwrap();
    assert_eq!(o.take().err(), Some(crate::E_BUSY));
    for bad in 0..6 {
        let mut o = CatalogOpen::begin(&IO, &LAY).unwrap();
        let fp = if bad == 0 {
            core::ptr::null_mut()
        } else if bad == 1 {
            unsafe { w.s.0.as_mut_ptr().add(1) }
        } else {
            w.s.0.as_mut_ptr()
        };
        let scratch = if bad == 2 {
            core::ptr::null_mut()
        } else if bad == 3 {
            unsafe { w.a.0.as_mut_ptr().add(1) }
        } else {
            w.a.0.as_mut_ptr()
        };
        let cap = if bad == 4 {
            511
        } else if bad == 5 {
            usize::MAX - 511
        } else {
            512
        };
        assert_eq!(unsafe { o.step(fp, scratch, cap) }, Err(E_BAD_STORE));
        assert_eq!(unsafe { catalog_step(&mut o, &mut w, 512) }, Err(E_BAD_STORE));
        assert_eq!(o.take().err(), Some(E_BAD_STORE));
    }
    for lay in [Layout { first: FIRST, sectors: TAIL }, Layout { first: u32::MAX, sectors: SECTORS }] {
        assert_eq!(CatalogOpen::begin(&IO, &lay).err(), Some(E_BAD_STORE));
    }
    let mut c = unsafe { catalog_open(&IO, &mut w) }.unwrap();
    assert_eq!(unsafe { c.rewind(core::ptr::null_mut()) }, Err(E_BAD_STORE));
    assert_eq!(unsafe { c.rewind(w.s.0.as_mut_ptr()) }, Err(E_BAD_STORE));
}

#[test]
fn catalog_rewind_header_failure_is_sticky_and_incremental_padding_is_not_an_entry() {
    let _g = lock();
    fresh();
    let expected = catalog_table(3);
    // Padding is CRC-covered but not an entry: legacy verified open ignores it.
    {
        let mut card = CARD.lock().unwrap();
        let padding = 10 * 512 + 3 * ENTRY;
        unsafe {
            put32(card.as_mut_ptr().add(padding + O_FIRST), LAY.limit());
            put32(card.as_mut_ptr().add(padding + O_SIZE), 512);
        }
        let mut h = unsafe { parse_header(card.as_ptr()) }.unwrap();
        h.table_crc = crc32_of(card[10 * 512..].as_ptr(), 1024);
        unsafe {
            encode_header(&h, card.as_mut_ptr());
            encode_header(&h, card.as_mut_ptr().add(LAY.header_b() as usize * 512));
        }
    }
    static FAIL: Mutex<bool> = Mutex::new(false);
    unsafe extern "C" fn failing(sector: u32, bytes: u32, buf: *mut u8) -> i32 {
        if *FAIL.lock().unwrap() {
            return -1;
        }
        rd(sector, bytes, buf)
    }
    *FAIL.lock().unwrap() = false;
    let mut w = work();
    let mut o = CatalogOpen::begin(&Io { rd: failing, wr }, &LAY).unwrap();
    while !unsafe { catalog_step(&mut o, &mut w, 512) }.unwrap() {}
    let mut c = o.take().unwrap();
    let mut out = [CATALOG_EMPTY; 3];
    assert_eq!(unsafe { c.read_page(w.s.0.as_mut_ptr(), w.a.0.as_mut_ptr(), 1024, &mut out) }, Ok(3));
    assert_eq!(out.as_slice(), expected.as_slice());
    *FAIL.lock().unwrap() = true;
    assert_eq!(unsafe { c.rewind(w.s.0.as_mut_ptr()) }, Err(E_READ));
    *FAIL.lock().unwrap() = false;
    assert_eq!(unsafe { c.rewind(w.s.0.as_mut_ptr()) }, Err(E_READ));
    assert_eq!(unsafe { c.read_page(w.s.0.as_mut_ptr(), w.a.0.as_mut_ptr(), 1024, &mut out) }, Err(E_READ));
}

#[test]
fn a_file_range_stays_inside_the_file_rounded_up_to_sectors() {
    // 1,025 bytes are 3 sectors
    assert_eq!(file_span(100, 1025, 0, 3), Ok(100));
    assert_eq!(file_span(100, 1025, 2, 1), Ok(102));
    assert_eq!(file_span(100, 1024, 1, 1), Ok(101));
    assert_eq!(file_span(100, 1025, 2, 2), Err(crate::E_RANGE));
    assert_eq!(file_span(100, 1024, 2, 1), Err(crate::E_RANGE), "1,024 bytes are 2 sectors");
    assert_eq!(file_span(100, 1025, 3, 1), Err(crate::E_RANGE));
    assert_eq!(file_span(100, 0, 0, 1), Err(crate::E_RANGE), "an empty file has no sector");
    assert_eq!(file_span(100, 1025, 0, 0), Err(crate::E_RANGE));
    // one call reads at most what the block read takes
    assert_eq!(file_span(7, u32::MAX, 0, MAX_IO_SECTORS), Ok(7));
    assert_eq!(file_span(7, u32::MAX, 0, MAX_IO_SECTORS + 1), Err(crate::E_RANGE));
    // the end of a 4 GiB file does not overflow
    assert_eq!(file_span(7, u32::MAX, 8_388_607, 1), Ok(7 + 8_388_607));
    assert_eq!(file_span(7, u32::MAX, u32::MAX, 1), Err(crate::E_RANGE));
    assert_eq!(file_span(u32::MAX, u32::MAX, 5, 1), Err(crate::E_RANGE));
}
