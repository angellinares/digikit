//! Boot a firmware image from reset, headless, and say which of three things
//! happened: it **booted** (reached its main UI and kept running), it
//! **faulted** (one of the watched PCs, such as the firmware's own fault
//! reporter, ran; or the runtime stopped with an error, e.g.
//! `UnsupportedGuestPc`), or it reached **no UI** within the budget (a hang).
//! Stock and modified images alike (`docs/modified-firmware.md`).
//!
//! usage: boot_gate SYX [--watch PC[,PC...]] [--max INSTRUCTIONS] [--after N]
//!                  [--frame OUT.pbm] [--exec LO-HI[,...]]
//!   --watch  PCs to count; any execution of one is a fault (e.g. a firmware's
//!            fault reporter). Hex with 0x, or decimal.
//!   --max    instructions to give up after (default 2,000,000,000)
//!   --after  instructions to keep running after the main UI (default
//!            100,000,000), so a fault just after boot is still caught
//!   --frame  write the last screen as a 128x64 PBM
//!   --exec   extra executable ranges for the runaway check (as the faceplate)
//! Prints one JSON line; the exit status is 0 booted, 1 fault, 2 no UI, 3 usage.

use std::{env, fs, process::ExitCode, time::Instant};

use elektron_native_boot::Emulator;

const CHUNK: u32 = 1_000_000;

fn number(text: &str) -> Option<u64> {
    let text = text.trim().replace('_', "");
    match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => text.parse().ok(),
    }
}

fn pbm(frame: &[u8]) -> Vec<u8> {
    // The runtime's frame: 1024 bytes, column-major, eight rows a byte, the
    // top row group last (packages/web/src/runtime.ts drawFrame).
    let mut out = b"P1\n128 64\n".to_vec();
    for y in 0..64 {
        for x in 0..128 {
            let on = frame[(7 - y / 8) + 8 * x] & (1 << (y % 8)) != 0;
            out.extend_from_slice(if on { b"1 " } else { b"0 " });
        }
        out.push(b'\n');
    }
    out
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let Some(syx_path) = args.first() else {
        eprintln!("usage: boot_gate SYX [--watch PC,...] [--max N] [--after N] [--frame OUT.pbm] [--exec LO-HI,...]");
        return ExitCode::from(3);
    };
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let watch: Vec<u32> = flag("--watch")
        .map(|list| list.split(',').filter_map(|pc| number(pc).map(|v| v as u32)).collect())
        .unwrap_or_default();
    let max = flag("--max").and_then(|n| number(&n)).unwrap_or(2_000_000_000);
    let after = flag("--after").and_then(|n| number(&n)).unwrap_or(100_000_000);
    let syx = match fs::read(syx_path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("cannot read {syx_path}: {error}");
            return ExitCode::from(3);
        }
    };
    let started = Instant::now();
    let mut emulator = match Emulator::new(&syx, None) {
        Ok(emulator) => emulator,
        Err(error) => {
            println!("{}", serde_json::json!({"outcome": "fault", "stage": "load", "error": error}));
            return ExitCode::from(1);
        }
    };
    if let Some(ranges) = flag("--exec") {
        let parsed: Option<Vec<(u32, u32)>> = ranges
            .split(',')
            .map(|r| {
                let (lo, hi) = r.split_once('-')?;
                Some((number(lo)? as u32, number(hi)? as u32))
            })
            .collect();
        match parsed {
            Some(list) => emulator.set_exec_ranges(list),
            None => {
                eprintln!("--exec: not LO-HI pairs");
                return ExitCode::from(3);
            }
        }
    }
    emulator.watch_pcs(&watch);
    let mut ui_at: Option<u64> = None;
    let (outcome, snapshot) = loop {
        let snapshot = emulator.step_chunk(CHUNK);
        let s = &snapshot.status;
        if emulator.pc_hits().iter().any(|&(_, hits, _)| hits > 0) || s.error.is_some() {
            break ("fault", snapshot);
        }
        if s.main_ui_reached && ui_at.is_none() {
            ui_at = Some(s.icount);
        }
        if ui_at.is_some_and(|at| s.icount >= at + after) {
            break ("booted", snapshot);
        }
        if s.icount >= max {
            break ("no-ui", snapshot);
        }
    };
    let s = &snapshot.status;
    if let (Some(path), Some(frame)) = (flag("--frame"), snapshot.frame.as_ref().or(None)) {
        if frame.len() == 1024 {
            let _ = fs::write(path, pbm(frame));
        }
    }
    let hits: Vec<_> = emulator
        .pc_hits()
        .iter()
        .map(|&(pc, n, first)| serde_json::json!({"pc": format!("{pc:#010x}"), "hits": n, "first_icount": first}))
        .collect();
    println!(
        "{}",
        serde_json::json!({
            "outcome": outcome,
            "device": s.device, "version": s.version, "modified": s.modified,
            "icount": s.icount, "main_ui_at": ui_at, "phase": s.phase,
            "error": s.error, "pc": format!("{:#010x}", s.pc), "watched": hits,
            "wall_seconds": started.elapsed().as_secs_f64(),
        })
    );
    ExitCode::from(match outcome {
        "booted" => 0,
        "fault" => 1,
        _ => 2,
    })
}
