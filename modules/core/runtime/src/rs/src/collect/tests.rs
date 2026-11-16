use super::*;
use std::boxed::Box;
use crate::test_lock::LOCK;

fn c(s: &'static [u8]) -> *const u8 {
    assert_eq!(s.last(), Some(&0));
    s.as_ptr()
}

const DT2: Profile = Profile {
    device: 1,
    pages: false,
    slot_base: 0,
    clone_of: 0,
    level: 10,
    own_dsp: false,
    tune_code: 0xca,
    tune_slot: 0,
    level_code: 0xd1,
    level_knob: 7,
    nslots: 6,
    slots: [1, 2, 4, 5, 6, 7, 0, 0],
    tpl: [0xcc, 0xd1, 0x47, 0x32, 0xca, 0xcb],
};
const DN2: Profile = Profile {
    device: 2,
    pages: true,
    slot_base: 25,
    clone_of: 0,
    level: 10,
    own_dsp: true,
    tune_code: 0,
    tune_slot: 0,
    level_code: 0,
    level_knob: 0,
    nslots: 8,
    slots: [1, 2, 4, 5, 6, 0, 3, 7],
    tpl: [0xd5, 0xd7, 0x47, 0xdd, 0xee, 0xd5],
};

const KEY: u32 = 0x00ab_cd00;
const FXID: u32 = 0x1234;

fn keys(mk: &[u32], fk: &[u32], rows: (u32, u32, u32)) -> KeysCopy {
    let mut k = KeysCopy::empty();
    k.machine_rows = rows.0;
    k.param_rows = rows.1;
    k.page_rows = rows.2;
    k.nm = mk.len();
    k.nf = fk.len();
    k.mk[..mk.len()].copy_from_slice(mk);
    k.fk[..fk.len()].copy_from_slice(fk);
    k
}

const FREE: Free = Free { machine_rows: 8, param_rows: 16, page_rows: 8 };

/// A machine with three params on one page.
fn analog() -> Box<Collect> {
    let mut b = Box::new(Collect::new());
    let m = b.add_machine(0, c(b"Analog\0"), c(b"ANLG\0"));
    let p = b.machine_add_page(m, c(b"Analog\0"), c(b"S\0"));
    b.page_add_param(p, c(b"Tone\0"), c(b"TONE\0"), 64);
    let decay = b.page_add_param(p, c(b"Decay\0"), c(b"DECAY\0"), 80);
    b.param_range(decay, 0, 100);
    b.param_latched(decay);
    let play = b.page_add_param(p, c(b"Play\0"), c(b"PLAY\0"), 0);
    b.param_range(play, 0, 3);
    b.param_glyph(play, 5);
    assert_eq!(b.err, 0);
    b
}

fn rd(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

#[test]
fn a_call_outside_init_returns_the_placeholder_and_changes_nothing() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        end();
        assert_eq!(dk_module_ctx_add_machine(core::ptr::null_mut(), 0, c(b"A\0"), c(b"A\0")), placeholder());
        assert_eq!(dk_machine_add_page(placeholder(), c(b"A\0"), c(b"S\0")), placeholder());
        assert_eq!(dk_page_add_param(placeholder(), c(b"A\0"), c(b"A\0"), 1), placeholder());
        dk_param_range(placeholder(), 1, 2);
        dk_param_latched(placeholder());
        dk_page_bind_local(placeholder(), 0, 1);
        // inside init a pointer that is not a live context is no builder either
        let mut b = Box::new(Collect::new());
        begin(&mut *b);
        assert_eq!(dk_module_ctx_add_machine(core::ptr::null_mut(), 0, c(b"A\0"), c(b"A\0")), placeholder());
        end();
        assert_eq!(b.nm, 0);
        assert_eq!(b.err, 0, "a call with a context that is not live records nothing");
    }
}

#[test]
fn a_ninth_knob_is_an_error_kept_for_the_commit() {
    let mut b = Box::new(Collect::new());
    let m = b.add_machine(0, c(b"A\0"), c(b"A\0"));
    let p = b.machine_add_page(m, c(b"A\0"), c(b"S\0"));
    for _ in 0..KNOBS {
        assert_ne!(b.page_add_param(p, c(b"P\0"), c(b"P\0"), 0), placeholder());
    }
    assert_eq!(b.err, 0);
    b.page_add_param(p, c(b"P\0"), c(b"P\0"), 0);
    assert_eq!(b.err, E_FORMAT);
}

#[test]
fn the_collection_is_bounded_by_max_records_and_the_call_past_it_returns_the_placeholder() {
    let mut b = Box::new(Collect::new());
    let mut machines = std::vec::Vec::new();
    for _ in 0..MAX_M {
        machines.push(b.add_machine(0, c(b"A\0"), c(b"A\0")));
    }
    let mut first = core::ptr::null_mut();
    for i in 0..MAX_PG {
        let g = b.machine_add_page(machines[i % MAX_M], c(b"G\0"), c(b"S\0"));
        if i == 0 {
            first = g;
        }
    }
    for _ in 0..(MAX_RECORDS - MAX_M - MAX_PG) {
        assert_ne!(b.page_add_param(first, c(b"P\0"), c(b"P\0"), 0), placeholder());
    }
    assert_eq!(b.err, 0, "exactly MAX_RECORDS records fit");
    assert_eq!(b.records, MAX_RECORDS);
    let over = b.page_add_param(first, c(b"P\0"), c(b"P\0"), 0);
    assert_eq!(over, placeholder());
    assert_eq!(b.err, E_DECL_FULL);
    // setters on it and a call that takes it change nothing
    let before = (b.np, b.nparam, b.records);
    b.param_range(over, 5, 6);
    b.param_latched(over);
    b.param_on_click(over, 4, 8);
    b.page_canvas(over, 1, 4, 6, 8);
    b.page_bind_local(over, 0, 0);
    b.page_bind(over, over, 0);
    assert_eq!(b.page_add_param(over, c(b"X\0"), c(b"X\0"), 1), placeholder());
    assert_eq!((b.np, b.nparam, b.records), before);
    // a 17th page is the same
    assert_eq!(b.machine_add_page(machines[0], c(b"G\0"), c(b"S\0")), placeholder());
}

#[test]
fn a_bad_text_is_an_error_at_commit_not_a_failed_call() {
    let mut b = Box::new(Collect::new());
    let h = b.add_machine(0, c(b"A very long machine name that does not fit\0"), c(b"A\0"));
    assert_ne!(h, placeholder());
    assert_eq!(b.err, E_FORMAT);
    assert_eq!(check_commit(&b, &DT2, &keys(&[KEY], &[], (1, 0, 0)), &[], &FREE), E_FORMAT);
}

#[test]
fn an_effect_short_name_longer_than_its_tab_label_is_refused_not_cut() {
    let mut b = Box::new(Collect::new());
    b.add_fx(0, c(b"Freq shift\0"), c(b"FSHF\0"));
    assert_eq!(b.err, E_FORMAT);
    assert_eq!(check_commit(&b, &DT2, &keys(&[], &[FXID], (0, 0, 0)), &[], &FREE), E_FORMAT);
}

#[test]
fn commit_refuses_more_rows_than_uses_an_unknown_id_a_duplicate_fx_id_and_no_free_row() {
    let b = analog();
    let ok = keys(&[KEY], &[], (1, 3, 1));
    assert_eq!(check_commit(&b, &DN2, &ok, &[], &FREE), OK);
    // more rows than [uses]
    assert_eq!(check_commit(&b, &DN2, &keys(&[KEY], &[], (1, 2, 1)), &[], &FREE), E_DECL_USES);
    assert_eq!(check_commit(&b, &DN2, &keys(&[KEY], &[], (0, 3, 1)), &[], &FREE), E_DECL_USES);
    // a page device needs a page row for the machine; a machine-row device none
    assert_eq!(check_commit(&b, &DN2, &keys(&[KEY], &[], (1, 3, 0)), &[], &FREE), E_DECL_USES);
    assert_eq!(check_commit(&b, &DT2, &keys(&[KEY], &[], (1, 3, 0)), &[], &FREE), OK);
    // a machine index that is no row of the header's keys table
    assert_eq!(check_commit(&b, &DN2, &keys(&[], &[], (1, 3, 1)), &[], &FREE), E_DECL_ID);
    // no free row
    assert_eq!(check_commit(&b, &DN2, &ok, &[], &Free { machine_rows: 0, ..FREE }), E_TYPE_USED);
    assert_eq!(check_commit(&b, &DN2, &ok, &[], &Free { param_rows: 2, ..FREE }), E_PARAM_FULL);
    assert_eq!(check_commit(&b, &DN2, &ok, &[], &Free { page_rows: 0, ..FREE }), E_PAGE_FULL);
    // an effect: its id missing from the header, an id another loaded module has, an id twice in the header
    let mut f = Box::new(Collect::new());
    let fx = f.add_fx(0, c(b"Shift\0"), c(b"FSH\0"));
    let pg = f.fx_add_page(fx, c(b"Shift\0"), c(b"S\0"));
    f.page_add_fx_param(pg, c(b"Shift\0"), c(b"SHIFT\0"), DK_BIPOLAR, -64.0, 63.0, 0.0);
    assert_eq!(f.err, 0);
    assert_eq!(check_commit(&f, &DT2, &keys(&[], &[FXID], (0, 0, 0)), &[], &FREE), OK);
    assert_eq!(check_commit(&f, &DT2, &keys(&[], &[], (0, 0, 0)), &[], &FREE), E_DECL_ID);
    assert_eq!(check_commit(&f, &DT2, &keys(&[], &[FXID], (0, 0, 0)), &[7, FXID], &FREE), E_FX_ID_DUP);
    assert_eq!(check_commit(&f, &DT2, &keys(&[], &[FXID, FXID], (0, 0, 0)), &[], &FREE), E_FX_ID_DUP);
}

#[test]
fn the_effect_ids_of_loaded_modules_are_remembered_until_release() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let k = keys(&[], &[FXID, 0x55], (0, 0, 0));
        keep(3, 0, 0, &k);
        let mut out = [0u32; MAX_MODULES * MAX_RECORDS];
        assert_eq!(loaded_fx_ids(1, &mut out), 2);
        assert_eq!(&out[..2], &[FXID, 0x55]);
        assert_eq!(loaded_fx_ids(3, &mut out), 0, "a module does not clash with itself");
        assert_eq!(release(3), 0);
        assert_eq!(loaded_fx_ids(1, &mut out), 0);
    }
}

#[test]
fn a_digitakt_machine_lowers_to_the_records_the_packer_wrote() {
    let b = analog();
    let k = keys(&[KEY], &[], (1, 3, 0));
    let mut plan = Plan::new();
    let size = lower(&b, &DT2, &k, &[true; MAX_M], 0x1000, core::ptr::null_mut(), 0, &mut plan).unwrap();
    let mut buf = std::vec![0u8; size];
    assert_eq!(lower(&b, &DT2, &k, &[true; MAX_M], 0x1000, buf.as_mut_ptr(), size, &mut plan), Ok(size));
    assert_eq!(rd(&buf, 0), 0x444b_5247);
    assert_eq!(rd(&buf, 8), 1, "device");
    assert_eq!(rd(&buf, 12), 4, "3 params and 1 machine, no page row on a machine-row device");
    assert_eq!(size, 16 + 3 * 0x50 + 0x6c);
    // PARAM 1 (Decay): the encoder row 0xcc, slot = knob position of word 1 = 2, range 0..100, default 80, raw (x 256)
    let p1 = 16 + 0x50;
    assert_eq!((rd(&buf, p1), rd(&buf, p1 + 8)), (1, 1));
    assert_eq!((rd(&buf, p1 + 12), rd(&buf, p1 + 16)), (0xcc, 2));
    assert_eq!((rd(&buf, p1 + 20), rd(&buf, p1 + 24), rd(&buf, p1 + 28)), (0, 100 * 256, 80 * 256));
    // PARAM 2 (Play): the play glyph's stock row
    assert_eq!(rd(&buf, 16 + 2 * 0x50 + 12), 0xcb);
    // MACHINE: key, clone, position 255; knobs: tune on 0, the params on 1, 2 and 4 (B C E), LEV on 7
    let m = 16 + 3 * 0x50;
    assert_eq!((rd(&buf, m), rd(&buf, m + 8), rd(&buf, m + 16)), (2, KEY, 255), "kind, key, position");
    assert_eq!(rd(&buf, m + 12), 0, "clone_of");
    let knobs: std::vec::Vec<u32> = (0..8).map(|j| rd(&buf, m + 8 + 20 + 4 * j)).collect();
    assert_eq!(knobs, [0xca, KNOB_PARAM, KNOB_PARAM | 1, 0, KNOB_PARAM | 2, 0, 0, 0xd1]);
    // the voice row: the range of each knob position and the latch of word 1
    let (rng, latched) = voice_declared(&b, &plan, 0);
    assert_eq!(rng, [0, 127 * 256 << 16, 100 * 256 << 16, 0, 3 * 256 << 16, 0, 0, 0]);
    assert_eq!(latched, 0b10);
}

#[test]
fn a_digitone_machine_with_canvas_pages_lowers_pages_and_native_records() {
    let mut b = Box::new(Collect::new());
    let m = b.add_machine(0, c(b"Shot\0"), c(b"SHOT\0"));
    let knobs = b.machine_add_page(m, c(b"SHOT\0"), c(b"S\0"));
    let smp = b.page_add_param(knobs, c(b"Sample\0"), c(b"SMP\0"), 0);
    let start = b.page_add_param(knobs, c(b"Start\0"), c(b"START\0"), 0);
    b.param_on_click(smp, 0x1100, 0x77);
    let ctl = b.machine_add_page(m, c(b"SHOT\0"), c(b"S\0"));
    b.page_canvas(ctl, 1, 0x1200, 0x1300, 0x88);
    b.page_bind(ctl, start, 0);
    let wave = b.machine_add_page(m, c(b"SHOT WAVE\0"), c(b"S\0"));
    b.page_canvas(wave, 2, 0x1400, 0x1500, 0x99);
    b.page_bind(wave, start, 4);
    b.page_bind_local(wave, 0, 6);
    assert_eq!(b.err, 0);
    let k = keys(&[KEY], &[], (1, 2, 1));
    let mut plan = Plan::new();
    let size = lower(&b, &DN2, &k, &[true; MAX_M], 0x1000, core::ptr::null_mut(), 0, &mut plan).unwrap();
    let mut buf = std::vec![0u8; size];
    lower(&b, &DN2, &k, &[true; MAX_M], 0x1000, buf.as_mut_ptr(), size, &mut plan).unwrap();
    // 2 params, 1 machine, 1 page, 1 action, 2 native pages (v2)
    assert_eq!(rd(&buf, 12), 7);
    assert_eq!(size, 16 + 2 * 0x50 + 0x6c + 0x60 + 0x10 + 2 * 0x98);
    // slot = 25 + knob position: word 0 at position 1, word 1 at position 2
    assert_eq!(rd(&buf, 16 + 8 + 8), 25 + 1);
    assert_eq!(rd(&buf, 16 + 12), 0xd5, "the Digitone II template of the default glyph");
    assert_eq!(rd(&buf, 16 + 0x50 + 8 + 8), 25 + 2);
    let page = 16 + 2 * 0x50 + 0x6c;
    assert_eq!(rd(&buf, page), 3);
    let act = page + 0x60;
    assert_eq!((rd(&buf, act), rd(&buf, act + 8), rd(&buf, act + 12)), (5, 0, 0x100), "action: param 0, image offset");
    let n0 = act + 0x10;
    assert_eq!((rd(&buf, n0), rd(&buf, n0 + 8), rd(&buf, n0 + 16), rd(&buf, n0 + 20)), (7, 2, 0x200, 0x300));
    assert_eq!((rd(&buf, n0 + 8 + 40), rd(&buf, n0 + 8 + 44)), (0, 1), "page 0, overlay flag");
    let b0 = n0 + 8 + 48;
    assert_eq!((rd(&buf, b0), rd(&buf, b0 + 4), rd(&buf, b0 + 8)), (1, 1, 0), "param file index 1 on encoder 0");
    let n1 = n0 + 0x98;
    assert_eq!((rd(&buf, n1 + 8 + 40), rd(&buf, n1 + 8 + 44)), (1, 2), "page 1, wide flag");
    let b1 = n1 + 8 + 48;
    assert_eq!((rd(&buf, b1 + 12), rd(&buf, b1 + 16), rd(&buf, b1 + 20)), (2, 0, 6), "local 0 on encoder 6");
    let args = apply_args(&b, &plan);
    assert_eq!((args.click[0], args.native[0], args.native[1]), (0x77, 0x88, 0x99));
}

#[test]
fn one_canvas_page_without_flags_is_a_v1_page() {
    let mut b = analog();
    let m = &mut b.machines[0] as *mut CMachine as *mut u8;
    let g = b.machine_add_page(m, c(b"W\0"), c(b"S\0"));
    b.page_canvas(g, 0, 0x1400, 0x1500, 0);
    b.page_bind_local(g, 3, 5);
    let k = keys(&[KEY], &[], (1, 3, 1));
    let mut plan = Plan::new();
    let size = lower(&b, &DN2, &k, &[true; MAX_M], 0, core::ptr::null_mut(), 0, &mut plan).unwrap();
    let mut buf = std::vec![0u8; size];
    lower(&b, &DN2, &k, &[true; MAX_M], 0, buf.as_mut_ptr(), size, &mut plan).unwrap();
    let n = size - 0x90;
    assert_eq!((rd(&buf, n), rd(&buf, n + 8)), (6, 1), "kind 6, version 1");
    assert_eq!((rd(&buf, n + 8 + 40), rd(&buf, n + 8 + 44), rd(&buf, n + 8 + 48)), (2, 3, 5));
}

#[test]
fn a_flagged_canvas_page_alone_is_an_incomplete_v2_pair_that_the_registry_refuses() {
    let mut b = analog();
    let m = &mut b.machines[0] as *mut CMachine as *mut u8;
    let g = b.machine_add_page(m, c(b"W\0"), c(b"S\0"));
    b.page_canvas(g, 2, 0x1400, 0x1500, 0);
    b.page_bind_local(g, 0, 0);
    let k = keys(&[KEY], &[], (1, 3, 1));
    let mut plan = Plan::new();
    let size = lower(&b, &DN2, &k, &[true; MAX_M], 0, core::ptr::null_mut(), 0, &mut plan).unwrap();
    let mut buf = std::vec![0u8; size];
    lower(&b, &DN2, &k, &[true; MAX_M], 0, buf.as_mut_ptr(), size, &mut plan).unwrap();
    let n = size - 0x98;
    assert_eq!(rd(&buf, n), 7);
    let mut set = crate::handlers::exports::PageSet::<8>::new();
    assert!(set.claim(7, KEY, rd(&buf, n + 8 + 40)));
    assert!(!set.complete());
}

#[test]
fn an_effect_with_a_page_lowers_to_an_effect_record_in_keys_order() {
    let mut f = Box::new(Collect::new());
    let fx = f.add_fx(1, c(b"Freq shift\0"), c(b"FSH\0"));
    f.fx_routes(fx, ROUTE_SEND | ROUTE_MASTER, ROUTE_SEND);
    let pg = f.fx_add_page(fx, c(b"Freq shift\0"), c(b"FSH\0"));
    f.page_add_fx_param(pg, c(b"Freq Shift\0"), c(b"SHIFT\0"), DK_BIPOLAR, -64.0, 63.0, 0.0);
    let k = keys(&[], &[0x77, FXID], (0, 0, 0));
    let mut plan = Plan::new();
    let size = lower(&f, &DT2, &k, &[true; MAX_M], 0, core::ptr::null_mut(), 0, &mut plan).unwrap();
    assert_eq!(size, 16 + 0x1a4);
    let mut buf = std::vec![0u8; size];
    lower(&f, &DT2, &k, &[true; MAX_M], 0, buf.as_mut_ptr(), size, &mut plan).unwrap();
    let e = 16;
    assert_eq!((rd(&buf, e), rd(&buf, e + 8), rd(&buf, e + 12), rd(&buf, e + 16), rd(&buf, e + 20)), (4, 1, 127, 1, FXID));
    assert_eq!(&buf[e + 24..e + 27], b"FSH");
    assert_eq!(fx_flags(ROUTE_SEND | ROUTE_MASTER, ROUTE_SEND), 0b101);
    assert_eq!(fx_flags(ROUTE_SEND, ROUTE_SEND), 0b111);
    assert_eq!(f32::from_bits(fx_defaults(&f, 0)[0]), 0.0);
    // the display of a bipolar knob is 1
    assert_eq!(rd(&buf, e + 8 + 48 + 4), 1);
}

#[test]
fn dk_fx_mix_sets_the_mix_default_of_the_effect_record_and_127_without_it() {
    let mut f = Box::new(Collect::new());
    let fx = f.add_fx(1, c(b"Freq shift\0"), c(b"FSH\0"));
    let pg = f.fx_add_page(fx, c(b"Freq shift\0"), c(b"FSH\0"));
    f.page_add_fx_param(pg, c(b"Freq Shift\0"), c(b"SHIFT\0"), DK_BIPOLAR, -64.0, 63.0, 0.0);
    let k = keys(&[], &[0x77, FXID], (0, 0, 0));
    let mix_of = |f: &Collect| {
        let mut plan = Plan::new();
        let size = lower(f, &DT2, &k, &[true; MAX_M], 0, core::ptr::null_mut(), 0, &mut plan).unwrap();
        let mut buf = std::vec![0u8; size];
        lower(f, &DT2, &k, &[true; MAX_M], 0, buf.as_mut_ptr(), size, &mut plan).unwrap();
        rd(&buf, 16 + 12)
    };
    assert_eq!(mix_of(&f), 127);
    f.fx_mix(fx, 64);
    assert_eq!(mix_of(&f), 64);
}

#[test]
fn a_param_beyond_the_devices_knob_positions_or_outside_its_glyph_range_is_refused() {
    let mut b = Box::new(Collect::new());
    let m = b.add_machine(0, c(b"A\0"), c(b"A\0"));
    let p = b.machine_add_page(m, c(b"A\0"), c(b"S\0"));
    for _ in 0..7 {
        b.page_add_param(p, c(b"P\0"), c(b"P\0"), 0);
    }
    let k = keys(&[KEY], &[], (1, 7, 1));
    let mut plan = Plan::new();
    // the Digitakt has six knob positions for words, the Digitone eight
    assert_eq!(lower(&b, &DT2, &k, &[true; MAX_M], 0, core::ptr::null_mut(), 0, &mut plan), Err(E_FORMAT));
    assert!(lower(&b, &DN2, &k, &[true; MAX_M], 0, core::ptr::null_mut(), 0, &mut plan).is_ok());
    let mut b = analog();
    b.params[0].glyph = 3; // on/off wants 0 to 1, the param has 0 to 127
    assert_eq!(lower(&b, &DN2, &keys(&[KEY], &[], (1, 3, 1)), &[true; MAX_M], 0, core::ptr::null_mut(), 0, &mut plan), Err(E_FORMAT));
}

#[test]
fn a_machine_with_two_knob_pages_is_refused_on_either_device() {
    // the shape of the parked fixture `test/dyn-dn2-pages` (back with run-time multi-page machines, RS-C): one machine, page A with three parameters, page B with two (the runtime has one knob page per machine)
    let mut b = Box::new(Collect::new());
    let m = b.add_machine(0, c(b"A\0"), c(b"A\0"));
    for n in [3, 2] {
        let p = b.machine_add_page(m, c(b"A\0"), c(b"S\0"));
        for _ in 0..n {
            b.page_add_param(p, c(b"P\0"), c(b"P\0"), 0);
        }
    }
    let k = keys(&[KEY], &[], (1, 5, 2));
    let mut plan = Plan::new();
    assert_eq!(lower(&b, &DN2, &k, &[true; MAX_M], 0, core::ptr::null_mut(), 0, &mut plan), Err(E_FORMAT));
    assert_eq!(lower(&b, &DT2, &k, &[true; MAX_M], 0, core::ptr::null_mut(), 0, &mut plan), Err(E_FORMAT));
}

#[test]
fn a_dsp_section_must_match_the_keys_table() {
    let k = keys(&[KEY], &[FXID, 0x77], (1, 0, 0));
    assert_eq!(check_dsp_keys(&k, 1, |_| KEY, 2), OK);
    assert_eq!(check_dsp_keys(&k, 1, |_| KEY ^ 1, 2), E_DSP_TYPE, "a voice of a machine that is not in the table");
    assert_eq!(check_dsp_keys(&k, 1, |_| KEY, 1), E_DECL_ID, "an effect record fewer than the table's effects");
    assert_eq!(check_dsp_keys(&k, 0, |_| 0, 3), E_DECL_ID, "an effect record more than the table's effects");
}

#[test]
fn a_digitone_knob_page_has_the_short_name_and_title_it_was_given() {
    let mut b = Box::new(Collect::new());
    let m = b.add_machine(0, c(b"Sine\0"), c(b"SIN\0"));
    let pg = b.machine_add_page(m, c(b"Sine\0"), c(b"SNE\0"));
    b.page_add_param(pg, c(b"Level\0"), c(b"LEVEL\0"), 127);
    let k = keys(&[KEY], &[], (1, 1, 1));
    let mut plan = Plan::new();
    let size = lower(&b, &DN2, &k, &[true; MAX_M], 0, core::ptr::null_mut(), 0, &mut plan).unwrap();
    let mut buf = std::vec![0u8; size];
    lower(&b, &DN2, &k, &[true; MAX_M], 0, buf.as_mut_ptr(), size, &mut plan).unwrap();
    let page = 16 + 0x50 + 0x6c;
    assert_eq!(rd(&buf, page), 3);
    assert_eq!(&buf[page + 8 + 4 * 10..][..5], b"SNE\0\0");
    assert_eq!(&buf[page + 8 + 4 * 10 + 24..][..5], b"Sine\0");
}

#[test]
fn an_effect_without_routes_runs_on_the_master_only_and_one_with_routes_follows_them() {
    let mut f = Box::new(Collect::new());
    let a = f.add_fx(0, c(b"A\0"), c(b"A\0"));
    let b = f.add_fx(1, c(b"B\0"), c(b"B\0"));
    let d = f.add_fx(2, c(b"D\0"), c(b"D\0"));
    f.fx_routes(b, ROUTE_SEND | ROUTE_MASTER, ROUTE_SEND);
    f.fx_routes(d, ROUTE_SEND, ROUTE_SEND);
    let _ = a;
    // no flag bit: master only, default master (the committed row keeps the old fixture's rule)
    assert_eq!(fx_flags_of_decl(&f.fxs[0]), 0);
    assert_eq!(fx_flags_of_decl(&f.fxs[1]), 0b101);
    assert_eq!(fx_flags_of_decl(&f.fxs[2]), 0b111);
}

/// A `.dkm` file of `size` bytes whose header points at a keys table with these machine keys.
fn dkm_file(mk: &[u32], size: usize) -> std::vec::Vec<u8> {
    let mut f = std::vec![0u8; size];
    f[0..4].copy_from_slice(&dkm::MAGIC.to_be_bytes());
    f[dkm::H_SIZE..][..4].copy_from_slice(&(size as u32).to_be_bytes());
    let off = dkm::HEADER as usize;
    let n = dkneeds::keys::write_keys((1, 1, 1), 0, mk, &[], &mut f[off..]).unwrap();
    f[dkm::H_REG_OFF..][..4].copy_from_slice(&(off as u32).to_be_bytes());
    f[dkm::H_REG_SIZE..][..4].copy_from_slice(&(n as u32).to_be_bytes());
    f
}

#[test]
fn machine_keys_reads_the_keys_table_of_a_file() {
    let mut out = [0u32; 8];
    let f = dkm_file(&[0x1234_5600, 0x0000_0007], 0xa0 + 64);
    assert_eq!(machine_keys(&f, &mut out), 2);
    assert_eq!(out[..2], [0x1234_5600, 7]);
}

#[test]
fn machine_keys_of_a_torn_file_is_none() {
    let mut out = [0u32; 8];
    let f = dkm_file(&[0x1234_5600], 0xa0 + 64);
    assert_eq!(machine_keys(&f[..0x40], &mut out), 0); // shorter than the header
    assert_eq!(machine_keys(&f[..f.len() - 1], &mut out), 0); // size field does not match
    let mut bad = f.clone();
    bad[0] ^= 1; // not a .dkm
    assert_eq!(machine_keys(&bad, &mut out), 0);
    let mut past = f.clone();
    past[dkm::H_REG_SIZE..][..4].copy_from_slice(&1000u32.to_be_bytes()); // table runs past the end
    assert_eq!(machine_keys(&past, &mut out), 0);
}

#[test]
fn a_generic_record_of_an_unknown_or_a_typed_kind_is_refused_and_the_error_is_kept() {
    let body = [0u8; 0x100];
    // no service owns kind 999
    let mut a = Collect::new();
    assert_eq!(a.add_record(999, body.as_ptr(), 8), E_FORMAT);
    assert_eq!(a.err, E_FORMAT);
    assert_eq!(a.ng, 0);
    // a typed kind has its builders: even with the right length it is not taken as a generic record
    let mut b = Collect::new();
    assert_eq!(b.add_record(KIND_PARAM, body.as_ptr(), 0x50 - 8), E_FORMAT);
    assert_eq!(b.ng, 0);
    // without an active collection the import refuses and changes nothing
    let _g = LOCK.lock().unwrap();
    unsafe {
        end();
        assert_eq!(dk_record_add(core::ptr::null_mut(), KIND_PARAM, body.as_ptr(), 8), E_CONTEXT);
    }
}

#[test]
fn a_header_table_of_runtime_0_7_or_none_is_a_contract_error_and_a_damaged_one_a_format_error() {
    // no table (the old "no machine, no effect" file), and the old table that begins with the row counts
    assert_eq!(KeysCopy::from_bytes(&[]).err(), Some(E_CONTRACT));
    let old = [0u8, 2, 0, 3, 0, 4, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1];
    assert_eq!(KeysCopy::from_bytes(&old).err(), Some(E_CONTRACT));
    // the current table: rows from USES, keys from KEYS
    let mut t = [0u8; 128];
    let n = dkneeds::keys::write_keys((2, 5, 1), 0, &[0x100, 0x200], &[7], &mut t).unwrap();
    let k = KeysCopy::from_bytes(&t[..n]).unwrap();
    assert_eq!((k.machine_rows, k.param_rows, k.page_rows, k.nm, k.nf), (2, 5, 1, 2, 1));
    assert_eq!((k.mk[1], k.fk[0]), (0x200, 7));
    // a damaged table (cut) is a format error, not a contract error
    assert_eq!(KeysCopy::from_bytes(&t[..n - 4]).err(), Some(E_FORMAT));
}

#[test]
fn what_init_declared_must_be_what_the_header_table_carries() {
    use dkneeds::keys::{write_table, KindDecl, Pair, Slot, Table};
    // a header table with one kind (kind 90, 24 bytes, handler at image offset 0x40), one slot (entry 0x60) and one export
    let t = Table {
        kinds: &[KindDecl { kind: 90, len: 24, entry: 0x40 }],
        slots: &[Slot { id: 3, flags: 1, entry: 0x60 }],
        exports: &[Pair { a: 0xaabb, b: 0x50 }],
        // a slot with an entry needs a version 2 table, which PROVIDES makes
        provides: &[Pair { a: 0x5e, b: 0 }],
        ..Table::default()
    };
    let mut b = [0u8; 128];
    let n = write_table(&t, &mut b).unwrap();
    let keys = KeysCopy::from_bytes(&b[..n]).unwrap();
    let base = 0x1000;
    let _g = LOCK.lock().unwrap();
    // nothing declared: the table's entries are missing
    assert!(!decl_matches(&Collect::new(), &keys, base));
    // the same items, with the addresses of the loaded image
    let declared = || {
        let mut c = Collect::new();
        c.decl.add(Declare::KIND, 90, 24, base + 0x40);
        c.decl.add(Declare::SLOT, 3, 1, base + 0x60);
        c.decl.add(Declare::EXPORT, 0xaabb, base + 0x50, 0);
        c
    };
    assert!(decl_matches(&declared(), &keys, base));
    assert_eq!(decl_diff(&declared(), &keys, base), None);
    // the diff names the first item that is not in both: the export the table has and `init` did not declare (the 0x100 bit), or the hook `init` declared more
    let mut d = declared();
    d.decl.n -= 1;
    assert_eq!(decl_diff(&d, &keys, base), Some((Declare::EXPORT as u32 | 0x100, 0xaabb)));
    let mut d = declared();
    d.decl.add(Declare::HOOK, 1, base, 0);
    assert_eq!(decl_diff(&d, &keys, base), Some((Declare::HOOK as u32, 1)));
    // another handler, another length, an item more
    let mut d = declared();
    d.decl.items[0].c = base + 0x44;
    assert!(!decl_matches(&d, &keys, base));
    let mut d = declared();
    d.decl.items[0].b = 28;
    assert!(!decl_matches(&d, &keys, base));
    // the slot's entry is compared too
    let mut d = declared();
    d.decl.items[1].c = base + 0x64;
    assert!(!decl_matches(&d, &keys, base));
    let mut d = declared();
    d.decl.add(Declare::HOOK, 1, base, 0);
    assert!(!decl_matches(&d, &keys, base));
    // a module with no table entries and no declarations passes
    let none = KeysCopy::empty();
    assert!(decl_matches(&Collect::new(), &none, base));
    // the row of a service file is no entry of the table: it does not count against it
    let mut d = declared();
    d.decl.row(&ROW);
    assert!(decl_matches(&d, &keys, base));
    // outside init the imports refuse
    unsafe {
        end();
        assert_eq!(dk_service_declare_slot(core::ptr::null_mut(), 1, 0, core::ptr::null()), E_CONTEXT);
    }
}

static ROW: dkrt_abi::service::ServiceRow = dkrt_abi::service::ServiceRow::of::<Nothing, crate::core_api::RealCore>("nothing", &[]);
struct Nothing;
impl<C: dkrt_abi::core_api::CoreApi> dkrt_abi::service::Service<C> for Nothing {}

#[test]
fn a_service_file_declares_one_row_in_init_and_only_there() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        // outside init
        end();
        assert_eq!(dk_service_declare_row(core::ptr::null_mut(), &ROW), E_CONTEXT);
        // inside init the context must be live; a null row and a second row are refused and kept as the load's error
        let mut b = Box::new(Collect::new());
        begin(&mut *b);
        let ctx = crate::entry::bind(0) as *mut u8;
        assert_eq!(dk_service_declare_row(ctx, core::ptr::null()), E_DECL_KIND);
        assert_eq!(b.err, E_DECL_KIND);
        b.err = 0;
        assert_eq!(dk_service_declare_row(core::ptr::null_mut(), &ROW), E_CONTEXT);
        assert_eq!(dk_service_declare_row(ctx, &ROW), OK);
        assert!(core::ptr::eq(b.decl.row_ptr().unwrap(), &ROW));
        assert_eq!(dk_service_declare_row(ctx, &ROW), E_DECL_KIND);
        end();
        crate::entry::unbind(0);
        assert_eq!(b.decl.n, 1);
    }
}
