//! The sticky machine type assignment: the nine boots of the former `sticky-types` scenario, then the pieces.
//! Machine keys and map entries are `hash24 << 8` (| type for an entry), as in a `.dkm` and in the selection record.
extern crate std;
use super::*;
use crate::sel::Record;
use std::vec::Vec;

// Hashes of the machines the scenario used; any distinct 24-bit values do.
const NAN: u32 = 0x00aa_0001;
const SINE: u32 = 0x00bb_0002;
const PARAMS: u32 = 0x00cc_0003;

fn key(h: u32) -> u32 {
    h << 8
}
fn entry(h: u32, t: u32) -> u32 {
    h << 8 | t
}

struct Boot {
    ty: Vec<u32>,
    map: [u32; MAP_N],
    dirty: u32,
    events: Vec<Event>,
}

/// One boot: the map the card holds, the machine hashes in load order.
fn boot(card_map: [u32; MAP_N], hashes: &[u32]) -> Boot {
    let mut keys = [0u32; MAX_KEYS];
    for (i, h) in hashes.iter().enumerate() {
        keys[i] = key(*h);
    }
    let mut ty = [0u32; MAX_KEYS];
    let mut map = clean(&card_map);
    let mut events = Vec::new();
    let dirty = assign(&card_map, &mut map, &keys, &mut ty, hashes.len(), &mut |e: &Event| events.push(*e));
    Boot { ty: ty[..hashes.len()].to_vec(), map, dirty, events }
}

fn assigned(b: &Boot) -> Vec<[u32; 4]> {
    b.events.iter().filter(|e| e.kind == logk::MACHINE_TYPE_ASSIGNED).map(|e| [e.args[0], e.args[1], e.args[2], e.args[3]]).collect()
}

fn evicted(b: &Boot) -> Vec<(u32, u32, bool)> {
    b.events.iter().filter(|e| e.kind == logk::MACHINE_TYPE_EVICTED).map(|e| (e.args[0], e.args[1], e.error)).collect()
}

fn map_of(entries: &[u32]) -> [u32; MAP_N] {
    let mut m = [0; MAP_N];
    m[..entries.len()].copy_from_slice(entries);
    m
}

/// The eight entries of other machines (not in the boot), type 16 + i.
fn full_foreign() -> ([u32; MAP_N], [u32; MAP_N]) {
    let fakes: [u32; MAP_N] = core::array::from_fn(|i| 0x1111 + i as u32);
    (fakes, core::array::from_fn(|i| entry(fakes[i], 16 + i as u32)))
}

#[test]
fn fresh_card_gives_the_lowest_numbers_in_load_order_and_writes_the_map() {
    let b = boot([0; MAP_N], &[NAN, SINE]);
    assert_eq!(b.ty, [16, 17]);
    assert_eq!(b.map, map_of(&[entry(NAN, 16), entry(SINE, 17)]));
    assert_eq!(b.dirty, 1);
    assert_eq!(assigned(&b), [[NAN, 16, 0, 0], [SINE, 17, 0, 0]]);
    assert!(evicted(&b).is_empty());
}

#[test]
fn fresh_card_with_one_machine_gets_16_and_the_entry_is_written() {
    let b = boot([0; MAP_N], &[NAN]);
    assert_eq!(b.ty, [16]);
    assert_eq!(b.map, map_of(&[entry(NAN, 16)]));
    assert_eq!(b.dirty, 1);
    assert_eq!(assigned(&b), [[NAN, 16, 0, 0]]);
}

#[test]
fn a_machine_keeps_its_number_when_other_modules_are_absent_and_nothing_is_rewritten() {
    let m1 = boot([0; MAP_N], &[NAN, SINE]).map;
    let b = boot(m1, &[SINE]);
    assert_eq!(b.ty, [17]);
    assert_eq!(b.map, m1);
    assert_eq!(b.dirty, 0);
    assert_eq!(assigned(&b), [[SINE, 17, 1, 1]]);
}

#[test]
fn a_new_machine_does_not_take_the_number_of_an_absent_machine() {
    let m1 = boot([0; MAP_N], &[NAN, SINE]).map;
    let b = boot(m1, &[SINE, PARAMS]);
    assert_eq!(b.ty, [17, 18]);
    assert!(!b.ty.contains(&16));
    assert_eq!(b.dirty, 1);
    assert_eq!(b.map[..3], [entry(NAN, 16), entry(SINE, 17), entry(PARAMS, 18)]);
}

#[test]
fn load_order_does_not_change_the_numbers_the_map_remembers() {
    let m3 = boot(boot([0; MAP_N], &[NAN, SINE]).map, &[SINE, PARAMS]).map;
    let b = boot(m3, &[PARAMS, NAN, SINE]);
    assert_eq!(b.ty, [18, 16, 17]);
    assert_eq!(b.dirty, 0);
    assert_eq!(b.map, m3);
    assert!(assigned(&b).iter().all(|e| e[2] == 1));
}

#[test]
fn a_full_map_evicts_the_lowest_foreign_entry_and_logs_it_as_an_error() {
    // slot 2 is the sine machine's (in this boot); the other seven belong to no machine of the boot
    let (fakes, mut forged) = full_foreign();
    forged[2] = entry(SINE, 18);
    let b = boot(forged, &[NAN, SINE]);
    assert_eq!(b.ty, [16, 18]); // nan takes slot 0's number, sine keeps 18
    assert_eq!(evicted(&b), [(fakes[0], 16, true)]);
    let mut want = forged;
    want[0] = entry(NAN, 16);
    assert_eq!(b.map, want);
    assert_eq!(b.dirty, 1);
}

#[test]
fn an_entry_of_a_machine_in_the_boot_is_never_the_one_evicted_even_in_slot_0() {
    // the entry in slot 0 belongs to sine, which loads in this boot: it is protected, so the next lowest entry (slot 1) goes
    let (fakes, mut forged) = full_foreign();
    forged[0] = entry(SINE, 16);
    forged[2] = entry(fakes[2], 18);
    let b = boot(forged, &[NAN, SINE]);
    assert_eq!(b.ty[1], 16); // sine
    assert_eq!(b.ty[0], 17); // nan takes the evicted number
    assert_eq!(evicted(&b), [(fakes[1], 17, true)]);
    assert_eq!(b.map[0], entry(SINE, 16));
    assert_eq!(b.map[1], entry(NAN, 17));
    assert_eq!(b.map[2..], forged[2..]);
}

#[test]
fn a_full_map_of_machines_that_are_all_in_the_boot_evicts_nothing_and_assigns_no_number() {
    let hs: [u32; 8] = core::array::from_fn(|i| 0x2000 + i as u32);
    let map: [u32; MAP_N] = core::array::from_fn(|i| entry(hs[i], 16 + i as u32));
    let mut all = hs.to_vec();
    all.push(NAN);
    let b = boot(map, &all);
    assert_eq!(b.ty[8], 0);
    assert!(evicted(&b).is_empty());
    assert_eq!(b.map, map);
}

#[test]
fn another_machines_entry_holds_16_so_the_new_machine_takes_17_and_that_entry_stays() {
    let b = boot(map_of(&[entry(SINE, 16)]), &[NAN]);
    assert_eq!(b.ty, [17]);
    assert_eq!(assigned(&b), [[NAN, 17, 0, 0]]);
    assert_eq!(b.map[..2], [entry(SINE, 16), entry(NAN, 17)]);
}

#[test]
fn a_machines_own_entry_beats_the_lowest_free_number_and_nothing_is_rewritten() {
    let m = map_of(&[entry(NAN, 20)]);
    let b = boot(m, &[NAN]);
    assert_eq!(b.ty, [20]);
    assert_eq!(assigned(&b), [[NAN, 20, 1, 1]]);
    assert_eq!(b.map, m);
    assert_eq!(b.dirty, 0);
}

#[test]
fn a_selection_keeps_the_map_bytes() {
    let cur = Record { seq: 4, boot_counter: 9, flags: 0, count: 1, stable_blocks: 77, map: map_of(&[entry(NAN, 16), entry(SINE, 17)]), ..Record::empty() };
    let mut paths = [[0u8; crate::SEL_PATH]; SEL_MAX];
    paths[0][..3].copy_from_slice(b"/a\0");
    let r = crate::sel::next_record(&cur, 0xffff_ffff, 1, &paths);
    assert_eq!(r.map, cur.map);
    assert_eq!((r.seq, r.boot_counter, r.stable_blocks), (5, 9, 77));
    assert_eq!(r.flags, crate::sel::FLAG_SAFE_ONCE);
    assert_eq!((r.count, r.paths[0][..3].to_vec()), (1, b"/a\0".to_vec()));
    let (mut a, mut b) = ([0u8; 512], [0u8; 512]);
    crate::sel::encode(&cur, &mut a);
    crate::sel::encode(&r, &mut b);
    for m in 0..MAP_N {
        let at = crate::sel::map_at(m);
        assert_eq!(a[at..at + 4], b[at..at + 4]);
    }
}

#[test]
fn a_pinned_key_is_left_alone_and_takes_no_number() {
    let mut keys = [0u32; MAX_KEYS];
    keys[0] = 0x12; // hash 0: a pinned number
    keys[1] = key(NAN);
    let mut ty = [9u32; MAX_KEYS];
    let mut map = [0; MAP_N];
    let mut n_events = 0;
    let dirty = assign(&[0; MAP_N], &mut map, &keys, &mut ty, 2, &mut |_| n_events += 1);
    assert_eq!((ty[0], ty[1], dirty, n_events), (0, 16, 1, 1));
}

#[test]
fn a_repeated_key_takes_a_fresh_number_and_is_not_recorded() {
    let b = boot([0; MAP_N], &[NAN, NAN]);
    assert_eq!(b.ty, [16, 17]);
    assert_eq!(b.map, map_of(&[entry(NAN, 16)]));
}

#[test]
fn clean_drops_entries_that_cannot_be_right() {
    let m = map_of(&[
        entry(NAN, 16),
        entry(NAN, 17),   // same hash as an earlier entry
        entry(SINE, 16),  // same number as an earlier entry
        entry(PARAMS, 24), // outside the run-time range
        entry(0, 18),     // no hash
        entry(0x77, 23),
    ]);
    assert_eq!(clean(&m), map_of(&[entry(NAN, 16), 0, 0, 0, 0, entry(0x77, 23)]));
}

#[test]
fn lowest_free_skips_taken_numbers_and_gives_0_when_none_is_left() {
    assert_eq!(lowest_free(0), 16);
    assert_eq!(lowest_free(0b0000_0111), 19);
    assert_eq!(lowest_free(0xff), 0);
}
