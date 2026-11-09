extern crate std;

use super::*;
use std::format;
use std::string::String;
use std::{vec, vec::Vec};

use serde_json::Value;

fn vectors() -> Value {
    serde_json::from_str(include_str!("../../../../../sdk/vectors/fit-vectors.json")).unwrap()
}

/// kind name of `[capacity.kinds]` -> CAPK number
fn kind(name: &str) -> u8 {
    match name {
        "MODULES" => CAPK_MODULES,
        "SELECTION" => CAPK_SELECTION,
        "ARENA" => CAPK_ARENA,
        "STAGE" => CAPK_STAGE,
        "DSP_AREA" => CAPK_DSP_AREA,
        "MACHINE_ROWS" => CAPK_MACHINE_ROWS,
        "VOICE_ROWS" => CAPK_VOICE_ROWS,
        "PARAM_ROWS" => CAPK_PARAM_ROWS,
        "PAGE_ROWS" => CAPK_PAGE_ROWS,
        "FX_SLOTS" => CAPK_FX_SLOTS,
        _ => panic!("kind {name}"),
    }
}

fn caps(base: &Value, over: Option<&Value>) -> Vec<DkrtCap> {
    let mut out: Vec<DkrtCap> = vec![];
    for (name, v) in base.as_object().unwrap() {
        let v = over.and_then(|o| o.get(name)).unwrap_or(v);
        if v.is_null() {
            continue; // the kind is left out
        }
        let a = v.as_array().unwrap();
        out.push(DkrtCap {
            kind: kind(name) as u32,
            limit: a[0].as_u64().unwrap() as u32,
            used: a[1].as_u64().unwrap() as u32,
            largest: a[2].as_u64().unwrap() as u32,
        });
    }
    out
}

fn needs(v: &Value) -> Needs {
    let g = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0) as u32;
    Needs {
        file_size: g("file_size"),
        arena: g("ARENA"),
        arena_xfer: g("ARENA_XFER"),
        dsp_area: g("DSP_AREA"),
        dsp_align: g("DSP_ALIGN"),
        machine_rows: g("MACHINE_ROWS"),
        voice_rows: g("VOICE_ROWS"),
        param_rows: g("PARAM_ROWS"),
        page_rows: g("PAGE_ROWS"),
        fx_slots: g("FX_SLOTS"),
    }
}

/// The vector's `want` as (resource name, need, free).
fn short_name(s: &Short) -> &'static str {
    [
        "MODULES",
        "SELECTION",
        "ARENA",
        "STAGE",
        "DSP_AREA",
        "MACHINE_ROWS",
        "VOICE_ROWS",
        "PARAM_ROWS",
        "PAGE_ROWS",
        "FX_SLOTS",
    ]
    .into_iter()
    .find(|n| kind(n) == s.kind)
    .unwrap()
}

#[test]
fn fits_now_on_the_shared_vectors() {
    let v = vectors();
    for c in v["cases"].as_array().unwrap() {
        let got = fits_now(&needs(&c["needs"]), &caps(&v["caps_base"], c.get("caps")));
        let want = &c["want"];
        match got {
            None => assert!(want.is_null(), "{}: fits, want {want}", c["name"]),
            Some(s) => {
                let w = want
                    .as_array()
                    .unwrap_or_else(|| panic!("{}: short {s:?}", c["name"]));
                assert_eq!(
                    (short_name(&s), s.need as u64, s.free as u64),
                    (
                        w[0].as_str().unwrap(),
                        w[1].as_u64().unwrap(),
                        w[2].as_u64().unwrap()
                    ),
                    "{}",
                    c["name"]
                );
            }
        }
    }
}

#[test]
fn a_set_loads_its_modules_in_turn() {
    let v = vectors();
    for s in v["sets"].as_array().unwrap() {
        // an entry with `times` stands for that many modules
        let mut set: Vec<Needs> = vec![];
        for n in s["needs"].as_array().unwrap() {
            for _ in 0..n.get("times").and_then(Value::as_u64).unwrap_or(1) {
                set.push(needs(n));
            }
        }
        let mut left = caps(&v["caps_base"], s.get("caps"));
        let want = &s["want"];
        match fits_set(&set, &mut left) {
            None => assert!(want.is_null(), "{}", s["name"]),
            Some((i, short)) => {
                let w = want
                    .as_array()
                    .unwrap_or_else(|| panic!("{}: short {short:?}", s["name"]));
                assert_eq!(
                    (
                        i as u64,
                        short_name(&short),
                        short.need as u64,
                        short.free as u64
                    ),
                    (
                        w[0].as_u64().unwrap(),
                        w[1].as_str().unwrap(),
                        w[2].as_u64().unwrap(),
                        w[3].as_u64().unwrap()
                    ),
                    "{}",
                    s["name"]
                );
            }
        }
    }
}

#[test]
fn the_short_texts_on_the_vectors() {
    // the popup form and the compact list form; the host wordings (describe, describe_by) are tested in patchkit
    let v = vectors();
    for case in v["shorts"].as_array().unwrap() {
        let s = Short {
            kind: kind(case["resource"].as_str().unwrap()),
            need: case["need"].as_u64().unwrap() as u32,
            free: case["free"].as_u64().unwrap() as u32,
        };
        let mut buf = [0u8; 40];
        short_text(&mut buf, &s, false);
        assert_eq!(text(&buf), case["text"].as_str().unwrap());
        short_text(&mut buf, &s, true);
        assert_eq!(text(&buf), case["brief"].as_str().unwrap());
    }
}

fn text(b: &[u8]) -> &str {
    let n = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    core::str::from_utf8(&b[..n]).unwrap()
}

#[test]
fn take_leaves_the_capacity_it_says() {
    let v = vectors();
    let mut c = caps(&v["caps_base"], None);
    let n = Needs {
        arena: 32,
        arena_xfer: 1392,
        dsp_area: 265600,
        dsp_align: 64,
        voice_rows: 1,
        ..Needs::default()
    };
    take(&n, &mut c);
    let get = |k: u8| *c.iter().find(|e| e.kind == k as u32).unwrap();
    assert_eq!(
        (get(CAPK_ARENA).used, get(CAPK_ARENA).largest),
        (1424, 524272 - 1424)
    );
    assert_eq!(get(CAPK_DSP_AREA).used, 265656); // the alignment room is taken too
    assert_eq!((get(CAPK_MODULES).used, get(CAPK_VOICE_ROWS).used), (1, 1));
}

#[test]
fn empty_is_the_capacity_of_a_runtime_that_has_loaded_nothing() {
    let v = vectors();
    let mut c = caps(&v["empty"]["from"], None);
    empty(&mut c);
    let want = caps(&v["empty"]["want"], None);
    assert_eq!(c.len(), want.len());
    for (a, b) in c.iter().zip(&want) {
        assert_eq!(
            (a.kind, a.limit, a.used, a.largest),
            (b.kind, b.limit, b.used, b.largest)
        );
    }
}

fn line(caps: &[DkrtCap]) -> String {
    let mut out = [0u8; LINE_MAX];
    let n = load_line(caps, &mut out);
    String::from_utf8(out[..n].to_vec()).unwrap()
}

#[test]
fn the_load_line_on_the_vectors() {
    let v = vectors();
    for case in v["load_lines"].as_array().unwrap() {
        let c = caps(&case["caps"], None);
        assert_eq!(
            line(&c),
            case["want"].as_str().unwrap(),
            "{}",
            case["name"].as_str().unwrap()
        );
        assert!(line(&c).len() <= LINE_MAX);
    }
}

#[test]
fn the_load_line_of_a_selection_is_the_empty_capacity_with_the_ticked_modules_taken() {
    // the same path as the menu's `refit`: limits, nothing used, then each ticked module taken in turn
    let v = vectors();
    let mut c = caps(&v["caps_base"], None);
    empty(&mut c);
    assert!(!line(&c).is_empty());
    let n = Needs {
        arena: 32,
        arena_xfer: 1392,
        dsp_area: 265600,
        dsp_align: 64,
        voice_rows: 1,
        machine_rows: 1,
        ..Needs::default()
    };
    take(&n, &mut c);
    let dsp = c.iter().find(|e| e.kind == CAPK_DSP_AREA as u32).unwrap();
    // 265656 of the limit, rounded up
    let want = (dsp.used * 100).div_ceil(dsp.limit);
    assert!(
        line(&c).starts_with(&format!("DSP {want}%")),
        "{}",
        line(&c)
    );
}
