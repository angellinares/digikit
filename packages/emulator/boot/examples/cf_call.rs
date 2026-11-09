//! Call functions on the ColdFire core (the patchkit unit-test harness).
//!
//! usage: cf_call SYX REQUEST.json [--state FILE] [--base-sha HEX]
//!
//! `--base-sha` names the stock firmware SYX was made from (a patched image), as `Emulator::new_patched`.
//!
//! Boots SYX to ready (or loads `--state`, a save_state blob; written when
//! missing), then runs the request:
//!   {"pokes":[{"addr":"0x47e8b000","hex":"..."}],
//!    "calls":[{"pc":"0x47e8b000","args":["0x47e8c000","16"],
//!              "reads":[{"addr":"0x47e8c000","len":16}],
//!              "sp":"0x47fe0000","ret":"0x47fdf000","max":1000000,"sr":"0x2700"}]}
//! Numbers are strings (hex with 0x or decimal). Prints one JSON line per
//! call: {"ok","d":[8],"a":[8],"pc","insns","reads":[hex],"error"}.
//! Defaults: sr 0x2700 (interrupts masked), max 5,000,000 instructions.

use elektron_native_boot::{CallSpec, Emulator};
use serde_json::{Value, json};
use std::{env, fs};

fn num(v: &Value) -> u32 {
    let s = v
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| v.to_string());
    match s.trim().strip_prefix("0x") {
        Some(h) => u32::from_str_radix(h, 16).unwrap(),
        None => s.trim().parse::<i64>().unwrap() as u32,
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: cf_call SYX REQUEST.json [--state FILE]");
        std::process::exit(2);
    }
    let syx = fs::read(&args[1]).unwrap();
    let req: Value = serde_json::from_slice(&fs::read(&args[2]).unwrap()).unwrap();
    let state = args
        .iter()
        .position(|a| a == "--state")
        .map(|i| args[i + 1].clone());
    let base_sha = args
        .iter()
        .position(|a| a == "--base-sha")
        .map(|i| args[i + 1].clone());
    let mut emu = match &base_sha {
        Some(b) => Emulator::new_patched(&syx, None, b).unwrap(),
        None => Emulator::new(&syx, None).unwrap(),
    };
    let loaded = match &state {
        Some(p) if fs::metadata(p).is_ok() => {
            emu.load_state(&fs::read(p).unwrap()).unwrap();
            true
        }
        _ => false,
    };
    if !loaded {
        loop {
            let snap = emu.step_chunk(250_000);
            let s = &snap.status;
            if s.ready {
                break;
            }
            if s.error.is_some() || s.icount >= 1_500_000_000 {
                eprintln!("boot failed: {:?} at {}", s.error, s.icount);
                std::process::exit(3);
            }
        }
        if let Some(p) = &state {
            fs::write(p, emu.save_state().unwrap()).unwrap();
        }
    }
    for p in req["pokes"].as_array().into_iter().flatten() {
        let bytes: Vec<u8> = p["hex"]
            .as_str()
            .unwrap()
            .as_bytes()
            .chunks(2)
            .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap())
            .collect();
        emu.poke_bytes(num(&p["addr"]), &bytes).unwrap();
    }
    for c in req["calls"].as_array().into_iter().flatten() {
        let spec = CallSpec {
            pc: num(&c["pc"]),
            args: c["args"]
                .as_array()
                .into_iter()
                .flatten()
                .map(num)
                .collect(),
            sp: c.get("sp").map_or(0x47fe_0000, num),
            ret_pc: c.get("ret").map_or(0x47fd_f000, num),
            sr: c.get("sr").map_or(0x2700, num) as u16,
            max_insns: c.get("max").map_or(5_000_000, num) as u64,
        };
        let r = emu.call_function(&spec).unwrap();
        let reads: Vec<String> = c["reads"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|rd| {
                let b = emu
                    .peek_bytes(num(&rd["addr"]), num(&rd["len"]) as usize)
                    .unwrap();
                b.iter().map(|x| format!("{x:02x}")).collect()
            })
            .collect();
        println!(
            "{}",
            json!({"ok": r.finished && r.error.is_none(), "d": r.d, "a": r.a, "d_in": r.d_in, "a_in": r.a_in, "sp0": spec.sp - 4 * (spec.args.len() as u32 + 1), "pc": r.pc,
                   "insns": r.insns, "reads": reads, "error": r.error})
        );
    }
}
