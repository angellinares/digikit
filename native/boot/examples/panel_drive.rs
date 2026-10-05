//! Drive a firmware image headless, the way the web faceplate does: boot it
//! from reset (or resume a saved state), then run a script of panel input,
//! screen captures and memory reads and writes, and report what happened.
//!
//! usage: panel_drive SYX [--state IN] [--save-state OUT] [--after N]
//!                    [--max N] [--watch PC,...] [--count PC,...] [--out DIR]
//!                    [--hold N]
//!                    --steps STEP,STEP,...
//!   --state       resume a state written by --save-state (skips the boot)
//!   --save-state  after the boot (or the resume), before the steps, write the
//!                 state: the next run starts there in a moment
//!   --after       instructions to run past the main UI before the steps
//!                 (default 100,000,000)
//!   --max         give up the boot after this many (default 2,000,000,000)
//!   --watch       PCs to count; any execution stops the run as a fault
//!   --count       PCs to count only (reported with the watched ones)
//!   --regs-at     PCs at which to record D0-D7, A0-A7 and the four longwords at A7,
//!                 each time they run
//!                 (the first 4,096 executions, then a count)
//!   --out         where `frame:` writes (default .)
//!   --hold        how long `tap:` holds a key (default 10,000,000; the web UI's
//!                 40,000,000 runs into key repeat, so a DOWN tap moves twice)
//!
//! Steps (numbers are hex with 0x, or decimal; a trailing M is millions):
//!   wait:N               run N instructions
//!   press:CODE           hold a button (the firmware's own panel code)
//!   release:CODE         let it go
//!   tap:CODE             press, hold --hold, release, run --hold again
//!   turn:ENC:DETENTS     turn an encoder (1-based), negative for down
//!   frame:NAME           write the screen to OUT/NAME.pbm (128x64)
//!   peek:ADDR:LEN        read guest memory: reported as hex
//!   poke:ADDR:HEX        write guest memory
//!
//! Prints one JSON line: the outcome, every peek, the watched PCs and the
//! wall time. Exit status 0 done, 1 fault, 2 no UI, 3 usage.

use std::{env, fs, path::PathBuf, process::ExitCode, time::Instant};

use elektron_native_boot::Emulator;
use serde_json::{Value, json};

const CHUNK: u32 = 1_000_000;

fn number(text: &str) -> Option<u64> {
    let text = text.trim().replace('_', "");
    if let Some(millions) = text.strip_suffix(['M', 'm']) {
        return number(millions).map(|n| n * 1_000_000);
    }
    match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => text.parse().ok(),
    }
}

fn signed(text: &str) -> Option<i64> {
    match text.trim().strip_prefix('-') {
        Some(rest) => number(rest).map(|n| -(n as i64)),
        None => number(text).map(|n| n as i64),
    }
}

fn hex_bytes(text: &str) -> Option<Vec<u8>> {
    let text = text.trim();
    if text.len() % 2 != 0 {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
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

struct Run {
    emulator: Emulator,
    frame: Option<Vec<u8>>,
    icount: u64,
    /// The watched PCs (a fault when one runs); the counted ones are not.
    faults: Vec<u32>,
}

impl Run {
    /// Steps `n` instructions; Err with the reason on a fault.
    fn advance(&mut self, n: u64) -> Result<(), String> {
        let target = self.icount + n;
        while self.icount < target {
            let budget = (target - self.icount).min(u64::from(CHUNK)) as u32;
            let snapshot = self.emulator.step_chunk(budget.max(1));
            if let Some(frame) = snapshot.frame {
                self.frame = Some(frame);
            }
            let status = &snapshot.status;
            if status.icount <= self.icount && status.error.is_none() {
                return Err(format!("no progress at {:#010x}", status.pc));
            }
            self.icount = status.icount;
            if let Some(error) = &status.error {
                return Err(error.clone());
            }
            let faults = &self.faults;
            if let Some(&(pc, _, _)) = self
                .emulator
                .pc_hits()
                .iter()
                .find(|&&(pc, hits, _)| hits > 0 && faults.contains(&pc))
            {
                return Err(format!("watched pc {pc:#010x} ran"));
            }
        }
        Ok(())
    }
}

fn usage(message: &str) -> ExitCode {
    eprintln!(
        "{message}\nusage: panel_drive SYX [--state IN] [--save-state OUT] [--after N] [--max N] \
               [--watch PC,...] [--count PC,...] [--out DIR] [--hold N] --steps STEP,..."
    );
    ExitCode::from(3)
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let Some(syx_path) = args.first() else {
        return usage("no image");
    };
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let after = flag("--after")
        .and_then(|n| number(&n))
        .unwrap_or(100_000_000);
    let max = flag("--max")
        .and_then(|n| number(&n))
        .unwrap_or(2_000_000_000);
    let hold = flag("--hold")
        .and_then(|n| number(&n))
        .unwrap_or(10_000_000);
    let out = PathBuf::from(flag("--out").unwrap_or_else(|| ".".into()));
    let pcs = |name: &str| -> Vec<u32> {
        flag(name)
            .map(|list| {
                list.split(',')
                    .filter_map(|pc| number(pc).map(|v| v as u32))
                    .collect()
            })
            .unwrap_or_default()
    };
    let watch = pcs("--watch");
    let counted = pcs("--count");
    let regs_at = pcs("--regs-at");
    let steps: Vec<String> = flag("--steps")
        .map(|s| {
            s.split(',')
                .map(|step| step.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();

    let syx = match fs::read(syx_path) {
        Ok(bytes) => bytes,
        Err(error) => return usage(&format!("cannot read {syx_path}: {error}")),
    };
    let started = Instant::now();
    let emulator = match Emulator::new(&syx, None) {
        Ok(emulator) => emulator,
        Err(error) => {
            println!(
                "{}",
                json!({"outcome": "fault", "stage": "load", "error": error})
            );
            return ExitCode::from(1);
        }
    };
    let mut run = Run {
        emulator,
        frame: None,
        icount: 0,
        faults: watch.clone(),
    };

    // The start: a saved state, or a boot from reset to the main UI and past it.
    let mut boot = json!(null);
    if let Some(path) = flag("--state") {
        let state = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => return usage(&format!("cannot read {path}: {error}")),
        };
        if let Err(error) = run.emulator.load_state(&state) {
            println!(
                "{}",
                json!({"outcome": "fault", "stage": "state", "error": error})
            );
            return ExitCode::from(1);
        }
        let snapshot = run.emulator.snapshot();
        run.icount = snapshot.status.icount;
        run.frame = snapshot.frame;
    }
    run.emulator
        .watch_pcs(&[watch.as_slice(), counted.as_slice()].concat());
    run.emulator.record_regs_at(&regs_at);
    if flag("--state").is_none() {
        let mut ui_at = None;
        while ui_at.is_none() {
            if let Err(error) = run.advance(u64::from(CHUNK)) {
                println!(
                    "{}",
                    json!({"outcome": "fault", "stage": "boot", "error": error, "icount": run.icount})
                );
                return ExitCode::from(1);
            }
            if run.emulator.snapshot().status.main_ui_reached {
                ui_at = Some(run.icount);
            } else if run.icount >= max {
                println!("{}", json!({"outcome": "no-ui", "icount": run.icount}));
                return ExitCode::from(2);
            }
        }
        if let Err(error) = run.advance(after) {
            println!(
                "{}",
                json!({"outcome": "fault", "stage": "boot", "error": error, "icount": run.icount})
            );
            return ExitCode::from(1);
        }
        boot = json!({"main_ui_at": ui_at, "seconds": started.elapsed().as_secs_f64()});
    }
    if let Some(path) = flag("--save-state") {
        match run.emulator.save_state() {
            Ok(state) => {
                if let Err(error) = fs::write(&path, state) {
                    return usage(&format!("cannot write {path}: {error}"));
                }
            }
            Err(error) => {
                println!(
                    "{}",
                    json!({"outcome": "fault", "stage": "save-state", "error": error})
                );
                return ExitCode::from(1);
            }
        }
    }

    // The script.
    let mut results: Vec<Value> = Vec::new();
    let mut fault = None;
    for step in &steps {
        let parts: Vec<&str> = step.split(':').collect();
        let outcome: Result<(), String> = (|| {
            let bad = || format!("step {step:?}: not understood");
            let code = |i: usize| {
                parts
                    .get(i)
                    .and_then(|p| number(p))
                    .map(|n| n as u8)
                    .ok_or_else(bad)
            };
            match parts[0] {
                "wait" => run.advance(parts.get(1).and_then(|p| number(p)).ok_or_else(bad)?),
                "press" => run.emulator.button(code(1)?, true),
                "release" => run.emulator.button(code(1)?, false),
                "tap" => {
                    let key = code(1)?;
                    run.emulator.button(key, true)?;
                    run.advance(hold)?;
                    run.emulator.button(key, false)?;
                    run.advance(hold)
                }
                "turn" => {
                    let detents = parts.get(2).and_then(|p| signed(p)).ok_or_else(bad)?;
                    run.emulator.turn(code(1)?, detents as i32)
                }
                "frame" => {
                    let name = parts.get(1).ok_or_else(bad)?;
                    let frame = run
                        .frame
                        .as_ref()
                        .filter(|f| f.len() == 1024)
                        .ok_or("no frame yet")?;
                    let path = out.join(format!("{name}.pbm"));
                    fs::write(&path, pbm(frame)).map_err(|e| format!("{}: {e}", path.display()))?;
                    results
                        .push(json!({"frame": path.display().to_string(), "icount": run.icount}));
                    Ok(())
                }
                "peek" => {
                    let addr = parts.get(1).and_then(|p| number(p)).ok_or_else(bad)? as u32;
                    let len = parts.get(2).and_then(|p| number(p)).ok_or_else(bad)? as usize;
                    let bytes = run.emulator.peek(addr, len)?;
                    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
                    results.push(
                        json!({"peek": format!("{addr:#010x}"), "hex": hex, "icount": run.icount}),
                    );
                    Ok(())
                }
                "poke" => {
                    let addr = parts.get(1).and_then(|p| number(p)).ok_or_else(bad)? as u32;
                    let bytes = parts.get(2).and_then(|p| hex_bytes(p)).ok_or_else(bad)?;
                    run.emulator.poke(addr, &bytes)
                }
                _ => Err(bad()),
            }
        })();
        if let Err(error) = outcome {
            fault = Some(json!({"step": step, "error": error}));
            break;
        }
    }

    let hits: Vec<_> = run
        .emulator
        .pc_hits()
        .iter()
        .map(|&(pc, n, first)| json!({"pc": format!("{pc:#010x}"), "hits": n, "first_icount": first}))
        .collect();
    let (log, regs_dropped) = run.emulator.reg_log();
    let hex8 = |v: &[u32]| v.iter().map(|x| format!("{x:#010x}")).collect::<Vec<_>>();
    let regs: Vec<_> = log
        .iter()
        .map(|h| json!({"pc": format!("{:#010x}", h.pc), "icount": h.icount, "d": hex8(&h.d), "a": hex8(&h.a), "stack": hex8(&h.stack)}))
        .collect();
    println!(
        "{}",
        json!({
            "outcome": if fault.is_some() { "fault" } else { "done" },
            "fault": fault, "boot": boot, "results": results, "watched": hits,
            "regs": regs, "regs_dropped": regs_dropped,
            "icount": run.icount, "wall_seconds": started.elapsed().as_secs_f64(),
        })
    );
    ExitCode::from(if fault.is_some() { 1 } else { 0 })
}
