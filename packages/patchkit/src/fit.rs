//! The fit rule for hosts: does a module (its `dkneeds`) fit the capacity of a device? The rule is `dkrt-fit` (modules/core/runtime/fit), the code
//! the DIGIKIT menu runs; the web app calls this through `pk_fit` (wasm) and the device tools through `patchkit fit --json -`. Only the wordings
//! for a person (`describe`, `describe_by`) are written here.
//!
//! Request: `{caps: {KIND: {limit, used, largest}}, needs: [{file_size?, ARENA?, ...}], empty?: bool}`. With `empty` the capacity first becomes
//! the one of a runtime that has loaded nothing (the next boot). Kinds that are not resources of a load are ignored; a kind the capacity lacks
//! is not judged.
//!
//! Response: `{each, set, line}`.
//! - `each`: per need, null or the short resource when that need alone does not fit the capacity.
//! - `set`: null when the needs fit in turn (the sum rule), else `{index, ...short}` of the first that does not.
//! - `line`: the load line (`DSP 52% CPU 1%`) of the capacity once every need that fits is taken, in order; a need that does not fit is skipped.
//!
//! A short resource is `{resource, need, free, text, brief, describe, describe_by}`: `text` and `brief` are the device's texts (popup and list
//! row), `describe` is `DSP memory 400 KiB needed, 254 KiB free` and `describe_by` is `DSP memory 146 KiB short`.

use crate::Result;
use dkrt_fit::{LINE_MAX, Needs, Short, empty, fits_now, fits_set, load_line, short_text, take};
use dkrt_sdk::DkrtCap;
use dkrt_sdk::proto::*;
use serde_json::{Map, Value, json};

/// The resources a fit looks at: the name in JSON and the CAPACITY kind.
const KINDS: [(&str, u8); 10] = [
    ("MODULES", CAPK_MODULES),
    ("SELECTION", CAPK_SELECTION),
    ("ARENA", CAPK_ARENA),
    ("STAGE", CAPK_STAGE),
    ("DSP_AREA", CAPK_DSP_AREA),
    ("MACHINE_ROWS", CAPK_MACHINE_ROWS),
    ("VOICE_ROWS", CAPK_VOICE_ROWS),
    ("PARAM_ROWS", CAPK_PARAM_ROWS),
    ("PAGE_ROWS", CAPK_PAGE_ROWS),
    ("FX_SLOTS", CAPK_FX_SLOTS),
];

/// What a resource is called to a person.
const LABEL: [(&str, &str); 10] = [
    ("STAGE", "file"),
    ("MODULES", "module records"),
    ("SELECTION", "selection paths"),
    ("DSP_AREA", "DSP memory"),
    ("ARENA", "CPU memory"),
    ("VOICE_ROWS", "voice rows"),
    ("MACHINE_ROWS", "machine rows"),
    ("PARAM_ROWS", "parameter rows"),
    ("PAGE_ROWS", "page rows"),
    ("FX_SLOTS", "effect slots"),
];

fn name_of(kind: u8) -> &'static str {
    KINDS.iter().find(|k| k.1 == kind).map_or("", |k| k.0)
}

fn label_of(name: &str) -> &str {
    LABEL.iter().find(|l| l.0 == name).map_or(name, |l| l.1)
}

fn u32_of(v: &Value, what: &str) -> Result<u32> {
    match v {
        Value::Null => Ok(0),
        _ => v
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| err!("fit: {what} is not a number of 32 bits")),
    }
}

fn caps_of(v: &Value) -> Result<Vec<DkrtCap>> {
    let Some(o) = v.as_object() else {
        return Err(err!("fit: caps is not an object"));
    };
    let mut out = vec![];
    for (name, c) in o {
        // an unknown name is ignored
        let Some(&(_, kind)) = KINDS.iter().find(|k| k.0 == name) else {
            continue;
        };
        out.push(DkrtCap {
            kind: kind as u32,
            limit: u32_of(&c["limit"], "limit")?,
            used: u32_of(&c["used"], "used")?,
            largest: u32_of(&c["largest"], "largest")?,
        });
    }
    Ok(out)
}

fn needs_of(v: &Value) -> Result<Needs> {
    let g = |k: &str| u32_of(&v[k], k);
    Ok(Needs {
        file_size: g("file_size")?,
        arena: g("ARENA")?,
        arena_xfer: g("ARENA_XFER")?,
        dsp_area: g("DSP_AREA")?,
        dsp_align: g("DSP_ALIGN")?,
        machine_rows: g("MACHINE_ROWS")?,
        voice_rows: g("VOICE_ROWS")?,
        param_rows: g("PARAM_ROWS")?,
        page_rows: g("PAGE_ROWS")?,
        fx_slots: g("FX_SLOTS")?,
    })
}

/// `400 KiB` for a size (rounded up for what is needed, down for what is free); bytes below 1 KiB.
fn fmt_size(bytes: u32, up: bool) -> String {
    if bytes < 1024 {
        format!("{bytes} bytes")
    } else if up {
        format!("{} KiB", bytes.div_ceil(1024))
    } else {
        format!("{} KiB", bytes / 1024)
    }
}

/// One line for a person: `DSP memory 400 KiB needed, 254 KiB free`.
fn describe(name: &str, need: u32, free: u32) -> String {
    let label = label_of(name);
    match name {
        "STAGE" => format!(
            "{label} {} needed, {} at most",
            fmt_size(need, true),
            fmt_size(free, false)
        ),
        "DSP_AREA" | "ARENA" => format!(
            "{label} {} needed, {} free",
            fmt_size(need, true),
            fmt_size(free, false)
        ),
        _ => format!("{label} {need} needed, {free} free"),
    }
}

/// How much a short resource is over: `CPU memory 3 KiB short`, `parameter rows 2 short`, `file 5 KiB too large`.
fn describe_by(name: &str, need: u32, free: u32) -> String {
    let label = label_of(name);
    let over = need.saturating_sub(free);
    match name {
        "STAGE" => format!("{label} {} too large", fmt_size(over, true)),
        "DSP_AREA" | "ARENA" => format!("{label} {} short", fmt_size(over, true)),
        _ => format!("{label} {over} short"),
    }
}

fn text(buf: &[u8]) -> String {
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

/// The fields of a short resource (see the module docs).
pub fn short_json(s: &Short) -> Map<String, Value> {
    let name = name_of(s.kind);
    let mut buf = [0u8; 48];
    short_text(&mut buf, s, false);
    let long = text(&buf);
    short_text(&mut buf, s, true);
    let mut o = Map::new();
    o.insert("resource".into(), name.into());
    o.insert("need".into(), s.need.into());
    o.insert("free".into(), s.free.into());
    o.insert("text".into(), long.into());
    o.insert("brief".into(), text(&buf).into());
    o.insert("describe".into(), describe(name, s.need, s.free).into());
    o.insert(
        "describe_by".into(),
        describe_by(name, s.need, s.free).into(),
    );
    o
}

/// Answer one request (see the module docs).
pub fn fit_json(req: &[u8]) -> Result<Value> {
    let req: Value = serde_json::from_slice(req)?;
    let mut caps = caps_of(&req["caps"])?;
    if req["empty"].as_bool().unwrap_or(false) {
        empty(&mut caps);
    }
    let set: Vec<Needs> = match req["needs"].as_array() {
        Some(a) => a.iter().map(needs_of).collect::<Result<_>>()?,
        None => return Err(err!("fit: needs is not an array")),
    };
    let each: Vec<Value> = set
        .iter()
        .map(|n| fits_now(n, &caps).map_or(Value::Null, |s| Value::Object(short_json(&s))))
        .collect();
    let mut left = caps.clone();
    let in_turn = match fits_set(&set, &mut left) {
        None => Value::Null,
        Some((index, s)) => {
            let mut o = short_json(&s);
            o.insert("index".into(), index.into());
            Value::Object(o)
        }
    };
    let mut taken = caps;
    for n in &set {
        if fits_now(n, &taken).is_none() {
            take(n, &mut taken);
        }
    }
    let mut buf = [0u8; LINE_MAX];
    let len = load_line(&taken, &mut buf);
    Ok(json!({ "each": each, "set": in_turn, "line": text(&buf[..len]) }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vectors() -> Value {
        serde_json::from_str(include_str!("../../../sdk/vectors/fit-vectors.json")).unwrap()
    }

    /// A vector capacity (`[limit, used, largest]` per kind, null: left out) as a request's `caps`.
    fn caps(base: &Value, over: Option<&Value>) -> Value {
        let mut out = Map::new();
        for (k, v) in base.as_object().unwrap() {
            let v = over.and_then(|o| o.get(k)).unwrap_or(v);
            if !v.is_null() {
                out.insert(
                    k.clone(),
                    json!({"limit": v[0], "used": v[1], "largest": v[2]}),
                );
            }
        }
        Value::Object(out)
    }

    fn ask(req: Value) -> Value {
        fit_json(req.to_string().as_bytes()).unwrap()
    }

    #[test]
    fn a_need_that_does_not_fit_is_named_with_its_texts() {
        let v = vectors();
        let case = v["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "one byte over the largest free range")
            .unwrap();
        let got =
            ask(json!({"caps": caps(&v["caps_base"], case.get("caps")), "needs": [case["needs"]]}));
        let s = &got["each"][0];
        assert_eq!(
            (s["resource"].clone(), s["need"].clone(), s["free"].clone()),
            (json!("DSP_AREA"), json!(524288), json!(524284))
        );
        assert_eq!(s["text"], "DSP 512K > 511K FREE");
        assert_eq!(got["set"]["index"], 0);
    }

    #[test]
    fn a_set_that_fits_in_turn_and_its_load_line() {
        let v = vectors();
        let n = json!({"file_size": 2308, "ARENA": 32, "ARENA_XFER": 1392, "DSP_AREA": 265600, "DSP_ALIGN": 64, "MACHINE_ROWS": 1});
        let got = ask(json!({"caps": caps(&v["caps_base"], None), "needs": [n], "empty": true}));
        assert_eq!(got["each"][0], Value::Null);
        assert_eq!(got["set"], Value::Null);
        assert_eq!(got["line"], "DSP 51% CPU 1%");
    }

    #[test]
    fn the_second_pluck_finds_half_the_dsp_area() {
        let v = vectors();
        let set = v["sets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"].as_str().unwrap().starts_with("two pluck-like"))
            .unwrap();
        let got = ask(json!({"caps": caps(&v["caps_base"], None), "needs": set["needs"]}));
        assert_eq!(got["each"], json!([null, null]));
        assert_eq!(got["set"]["index"], 1);
        assert_eq!(got["set"]["resource"], "DSP_AREA");
    }

    #[test]
    fn the_host_wordings_on_the_vectors() {
        let v = vectors();
        for c in v["shorts"].as_array().unwrap() {
            let s = Short {
                kind: KINDS.iter().find(|k| k.0 == c["resource"]).unwrap().1,
                need: c["need"].as_u64().unwrap() as u32,
                free: c["free"].as_u64().unwrap() as u32,
            };
            let got = short_json(&s);
            for k in ["text", "brief", "describe", "describe_by"] {
                assert_eq!(got[k], c[k], "{} {k}", c["resource"]);
            }
        }
    }

    #[test]
    fn unknown_kinds_are_ignored_and_bad_requests_are_refused() {
        let got = ask(
            json!({"caps": {"NOPE": {"limit": 1, "used": 0, "largest": 1}}, "needs": [{"ARENA": 9}]}),
        );
        assert_eq!(got["each"], json!([null]));
        assert!(fit_json(b"{}").is_err());
        assert!(fit_json(b"[").is_err());
    }
}
