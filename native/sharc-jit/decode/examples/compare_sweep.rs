//! Verification tool #2 (not part of the library): decode a contiguous
//! sweep of every short-word PC in an image's VISA code range(s) -- most of
//! which are not real instruction starts, exactly the way a JIT may probe
//! anywhere -- and compare against a JSON-lines dump of Python's own
//! `tools/sharc_disasm.py::decode_confident_loaded(mem, pc)` (with
//! `tools/sharc_rsgen.py::split_compute()` applied, matching
//! `sharc_decode::Decoded::fields`'s documented contract).
//!
//! Each input line is `{"pc": N, "type_name": "...", "kind": "...",
//! "length_bytes": N|null, "fields": [["label", value], ...]}` -- see the
//! sibling Python dump script. This reuses the library's own JSON reader
//! (`src/json.rs`) rather than a dependency, via `#[path]`; that does not
//! change the library's public API (the module stays private in `lib.rs`).
//!
//! Firmware-derived inputs only, read by this binary alone (never by the
//! library): a `pack_image` blob and the matching sweep JSON-lines dump.
//!
//! Usage:
//!     cargo run --release --example compare_sweep -- IMAGE.pack SWEEP.jsonl

#[path = "../src/json.rs"]
mod json;

use std::env;
use std::fs;
use std::process::ExitCode;

use sharc_decode::{Decoded, Decoder, parse_pack_image};

fn parse_record(line: &str) -> Result<(u32, Decoded), String> {
    let v = json::parse(line).map_err(|e| e.to_string())?;
    let pc = v
        .get("pc")
        .ok_or("missing pc")?
        .as_int()
        .map_err(|e| e.to_string())? as u32;
    let type_name = v
        .get("type_name")
        .ok_or("missing type_name")?
        .as_str()
        .map_err(|e| e.to_string())?
        .to_string();
    let kind_str = v
        .get("kind")
        .ok_or("missing kind")?
        .as_str()
        .map_err(|e| e.to_string())?;
    let kind: &'static str = match kind_str {
        "confident" => "confident",
        "uncertain" => "uncertain",
        "unknown" => "unknown",
        other => return Err(format!("unknown kind {other:?}")),
    };
    let length_bytes = match v.get("length_bytes") {
        Some(json::Value::Int(n)) => Some(*n as u32),
        _ => None,
    };
    let mut fields = Vec::new();
    if let Some(arr) = v.get("fields") {
        for item in arr.as_arr().map_err(|e| e.to_string())? {
            let pair = item.as_arr().map_err(|e| e.to_string())?;
            let label = pair[0].as_str().map_err(|e| e.to_string())?.to_string();
            let value = pair[1].as_int().map_err(|e| e.to_string())?;
            fields.push((label, value));
        }
    }
    Ok((
        pc,
        Decoded {
            type_name,
            kind,
            length_bytes,
            fields,
        },
    ))
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: compare_sweep IMAGE.pack SWEEP.jsonl");
        return ExitCode::FAILURE;
    }
    let pack_bytes = fs::read(&args[1]).expect("read pack image");
    let image = parse_pack_image(&pack_bytes).expect("parse pack image");
    let decoder = Decoder::new();

    let text = fs::read_to_string(&args[2]).expect("read sweep jsonl");
    let mut total = 0u64;
    let mut mismatches = 0u64;
    let mut shown = 0;
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let (pc, want) = parse_record(line).expect("parse sweep record");
        let got = decoder.decode_at(&image, pc);
        total += 1;
        if got != want {
            mismatches += 1;
            if shown < 20 {
                println!("  pc={pc:#x} want={want:?} got={got:?}");
                shown += 1;
            }
        }
    }

    println!("compare_sweep: {total} PCs");
    println!("  exact matches: {}", total - mismatches);
    println!("  mismatches:    {mismatches}");
    if mismatches == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
