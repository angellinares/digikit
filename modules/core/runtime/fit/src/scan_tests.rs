extern crate std;

use super::*;
use std::vec::Vec;

fn text(b: &[u8]) -> &str {
    let n = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    core::str::from_utf8(&b[..n]).unwrap()
}

fn cap(kind: u8, limit: u32) -> DkrtCap {
    DkrtCap {
        kind: kind as u32,
        limit,
        used: 0,
        largest: limit,
    }
}

fn caps() -> Vec<DkrtCap> {
    std::vec![
        cap(CAPK_MODULES, 8),
        cap(CAPK_SELECTION, 8),
        cap(CAPK_ARENA, 524288),
        cap(CAPK_DSP_AREA, 1_000_000)
    ]
}

fn entry(flags: u32, arena: u32, dsp: u32) -> DkrtScan {
    // SAFETY: plain integers
    let mut e: DkrtScan = unsafe { core::mem::zeroed() };
    e.flags = flags;
    e.need_arena = arena;
    e.need_dsp_area = dsp;
    e
}

fn out() -> std::boxed::Box<DkrtFitOut> {
    // SAFETY: plain integers
    std::boxed::Box::new(unsafe { core::mem::zeroed() })
}

#[test]
fn sizes_read_as_the_menu_shows_them() {
    for (n, want) in [
        (0, "<1K"),
        (1023, "<1K"),
        (1024, "1K"),
        (265_000, "259K"),
        (1_300_000, "1.2M"),
    ] {
        let mut b = [0u8; 8];
        size_k(&mut b, n);
        assert_eq!(text(&b), want, "{n}");
    }
}

#[test]
fn short_texts_are_the_popup_and_the_row_forms() {
    let s = Short {
        kind: CAPK_DSP_AREA,
        need: 400 * 1024,
        free: 254 * 1024 + 5,
    };
    let mut b = [0u8; 40];
    short_text(&mut b, &s, false);
    assert_eq!(text(&b), "DSP 400K > 254K FREE");
    short_text(&mut b, &s, true);
    assert_eq!(text(&b), "DSP 400K>254K");
    let s = Short {
        kind: CAPK_MACHINE_ROWS,
        need: 5,
        free: 2,
    };
    short_text(&mut b, &s, false);
    assert_eq!(text(&b), "MACHINES 5 > 2 FREE");
    let s = Short {
        kind: CAPK_STAGE,
        need: 70_000,
        free: 65_536,
    };
    short_text(&mut b, &s, false);
    assert_eq!(text(&b), "FILE 69K > 64K MAX");
}

#[test]
fn a_ticked_set_is_taken_first_and_the_rest_is_judged_against_it() {
    let both = SCAN_COMPAT | SCAN_NEEDS;
    // two modules of 600 KB DSP each: either fits alone, not both
    let e = [
        entry(both, 1024, 600_000),
        entry(both, 1024, 600_000),
        entry(SCAN_COMPAT, 0, 0),
        entry(SCAN_NEEDS, 0, 0),
    ];
    let mut o = out();
    fit_set(&mut caps(), &e, &[0, 0, 0, 0], &mut o);
    assert_eq!(o.count, 4);
    assert_eq!((o.rows[0].wont, o.rows[1].wont), (0, 0));
    assert_eq!(text(&o.line), "");
    assert_eq!(text(&o.rows[0].mem), "587K");
    fit_set(&mut caps(), &e, &[1, 0, 0, 0], &mut o);
    assert_eq!((o.rows[0].wont, o.rows[1].wont), (0, 1));
    assert_eq!(text(&o.rows[1].reason), "DSP 586K > 390K FREE");
    assert_eq!(text(&o.rows[1].brief), "DSP 586K>390K");
    assert_eq!(text(&o.line), "DSP 60% CPU 1%");
    // a ticked one that does not fit is flagged and not taken
    fit_set(&mut caps(), &e, &[1, 1, 0, 0], &mut o);
    assert_eq!((o.rows[0].wont, o.rows[1].wont), (0, 1));
    // no verdict without needs or without a compatible header
    assert_eq!((o.rows[2].wont, o.rows[3].wont), (0, 0));
    assert_eq!(text(&o.rows[2].mem), "");
}
