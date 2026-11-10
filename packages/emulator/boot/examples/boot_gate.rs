//! Boot a firmware image from reset, headless, and say which of three things
//! happened: it **booted** (reached its main UI and kept running), it
//! **faulted** (one of the watched PCs, such as the firmware's own fault
//! reporter, ran; or the runtime stopped with an error, e.g.
//! `UnsupportedGuestPc`), or it reached **no UI** within the budget (a hang).
//!
//! usage: boot_gate SYX [--base SHA256] [--watch PC[,PC...]]... [--max INSTRUCTIONS]
//!                  [--after N] [--frame OUT.pbm]
//!   --base   the stock firmware SYX was made from, for an image with no digikit
//!            patch record (`Emulator::new_patched`); a stock image needs none, and
//!            nor does a build that keeps its release's build stamp (section 5)
//!   --watch  PCs to watch; any execution of one is a fault (e.g. a firmware's
//!            fault reporter). Hex with 0x, or decimal. Repeat the flag or give
//!            a comma list; a value that is not a PC is a usage error.
//!   --max    instructions to give up after (default 2,000,000,000)
//!   --after  instructions to keep running after the main UI (default
//!            100,000,000), so a fault just after boot is still caught
//!   --frame  write the last screen as a 128x64 PBM
//! An image with a DNFW area and no digikit patch record may run outside MAIN only
//! in the area's CODE chunks; a PC anywhere else is a fault (`UnsupportedGuestPc`).
//! `exec_ranges` in the report says which rule applied.
//! Prints one JSON line; the exit status is 0 booted, 1 fault, 2 no UI, 3 usage.
//! A watched PC's first execution is reported with its registers.

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
    // The runtime's frame: 1024 bytes, byte (7 - y/8) + 8*x, bit y%8.
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
        eprintln!(
            "usage: boot_gate SYX [--base SHA256] [--watch PC,...]... [--max N] [--after N] [--frame OUT.pbm]"
        );
        return ExitCode::from(3);
    };
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    // Every --watch counts: `--watch A --watch B` watches both, as `--watch A,B` does.
    let mut watch: Vec<u32> = Vec::new();
    for (i, _) in args.iter().enumerate().filter(|(_, a)| *a == "--watch") {
        let Some(list) = args.get(i + 1) else {
            eprintln!("--watch needs a PC list");
            return ExitCode::from(3);
        };
        for pc in list.split(',') {
            match number(pc).and_then(|v| u32::try_from(v).ok()) {
                Some(pc) => watch.push(pc),
                None => {
                    eprintln!("--watch: {pc:?} is not a 32-bit PC");
                    return ExitCode::from(3);
                }
            }
        }
    }
    let max = flag("--max")
        .and_then(|n| number(&n))
        .unwrap_or(2_000_000_000);
    let after = flag("--after")
        .and_then(|n| number(&n))
        .unwrap_or(100_000_000);
    let base = flag("--base");
    let syx = match fs::read(syx_path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("cannot read {syx_path}: {error}");
            return ExitCode::from(3);
        }
    };
    let started = Instant::now();
    let built = match &base {
        Some(sha) => Emulator::new_patched(&syx, None, sha),
        None => Emulator::new(&syx, None),
    };
    let mut emulator = match built {
        Ok(emulator) => emulator,
        Err(error) => {
            println!(
                "{}",
                serde_json::json!({"outcome": "fault", "stage": "load", "error": error})
            );
            return ExitCode::from(1);
        }
    };
    emulator.probe_pcs(&watch);
    emulator.record_regs_at(&watch);
    let mut ui_at: Option<u64> = None;
    let fired = |emulator: &Emulator| {
        emulator
            .probe_hits()
            .iter()
            .any(|&(pc, hits)| hits > 0 && watch.contains(&pc))
    };
    let (outcome, snapshot) = loop {
        let snapshot = emulator.step_chunk(CHUNK);
        let s = &snapshot.status;
        if fired(&emulator) || s.error.is_some() {
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
    let frame = snapshot
        .frame
        .clone()
        .or_else(|| emulator.current_frame().map(|(_, f)| f.to_vec()));
    if let (Some(path), Some(frame)) = (flag("--frame"), frame) {
        if frame.len() == 1024 {
            let _ = fs::write(path, pbm(&frame));
        }
    }
    let (log, _) = emulator.reg_log();
    let hits: Vec<_> = emulator
        .probe_hits()
        .iter()
        .filter(|(pc, _)| watch.contains(pc))
        .map(|&(pc, n)| {
            let first = log.iter().find(|h| h.pc == pc);
            serde_json::json!({
                "pc": format!("{pc:#010x}"), "hits": n,
                "first_icount": first.map(|h| h.icount),
                "first_regs": first.map(|h| serde_json::json!({
                    "d": h.d.iter().map(|v| format!("{v:#010x}")).collect::<Vec<_>>(),
                    "a": h.a.iter().map(|v| format!("{v:#010x}")).collect::<Vec<_>>(),
                    "stack": h.stack.iter().map(|v| format!("{v:#010x}")).collect::<Vec<_>>(),
                })),
            })
        })
        .collect();
    // Where the PC could run outside MAIN: "ram" (the board's RAM window), or the
    // CODE ranges of the image's DNFW area.
    let exec_ranges = match emulator.exec_ranges() {
        None => serde_json::json!("ram"),
        Some(ranges) => ranges
            .iter()
            .map(|(start, end)| format!("{start:#010x}..{end:#010x}"))
            .collect(),
    };
    println!(
        "{}",
        serde_json::json!({
            "outcome": outcome,
            "device": s.device, "version": s.version, "base": base,
            "icount": s.icount, "main_ui_at": ui_at, "phase": s.phase,
            "error": s.error, "pc": format!("{:#010x}", s.pc), "watched": hits,
            "exec_ranges": exec_ranges,
            "wall_seconds": started.elapsed().as_secs_f64(),
        })
    );
    ExitCode::from(match outcome {
        "booted" => 0,
        "fault" => 1,
        _ => 2,
    })
}
