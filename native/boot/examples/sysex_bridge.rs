//! A SysEx bridge to a firmware image: one request per line on stdin, its replies
//! on stdout, so a host tool's test suite can talk to the firmware's own message
//! handling with no MIDI or USB in between.
//!
//! usage: sysex_bridge SYX (--state IN | [--max N] [--after N])
//!                     --call-at PC --call-fn PC [--call-args N,...]
//!                     --capture PC:BUF:LEN [--settle N] [--limit N]
//!
//! Each request is handed to `--call-fn(buf, len, ARGS...)` (a SysEx router) from
//! `--call-at`, a PC the firmware's own task reaches. What reaches `--capture`
//! (a sender: the pointer in argument BUF, the length in argument LEN) is the
//! reply. The bridge runs until the call returns, then `--settle` more
//! instructions (default 20,000,000), and joins the captured pieces into whole
//! messages at each F7. `--limit` bounds one request (default 2,000,000,000).
//!
//! Input lines: a message as hex (spaces allowed), or blank, or `#` and a comment.
//! Output: one JSON line per request, in order:
//!   {"request": K, "replies": ["F0...F7", ...], "partial": "..."|null,
//!    "icount": N, "error": null}
//! `replies` is empty when the firmware answered nothing: an explicit no-reply.
//! `partial` holds bytes captured after the last F7, which is what a dropped
//! piece would look like. Nothing is repaired. The first line printed, before
//! any request, is {"ready": true, "icount": N}.
//!
//! Example (DN2 1.11): --call-at 0x4002e464 (the UI loop) --call-fn 0x4012166e
//! (the SysEx router) --call-args 2 (source: USB) --capture 0x401233f2:0:1 (the
//! SysEx sender).

use std::{
    env, fs,
    io::{self, BufRead, Write},
    process::ExitCode,
};

use elektron_native_boot::{Emulator, GuestCall};
use serde_json::json;

const CHUNK: u64 = 1_000_000;

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

fn hex_bytes(text: &str) -> Option<Vec<u8>> {
    let clean: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if clean.is_empty() || clean.len() % 2 != 0 {
        return None;
    }
    (0..clean.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&clean[i..i + 2], 16).ok())
        .collect()
}

fn usage(message: &str) -> ExitCode {
    eprintln!(
        "{message}\nusage: sysex_bridge SYX (--state IN | [--max N] [--after N]) --call-at PC \
         --call-fn PC [--call-args N,...] --capture PC:BUF:LEN [--settle N] [--limit N]"
    );
    ExitCode::from(3)
}

/// Runs the emulator in chunks; -> the error that stopped it, if any.
fn advance(emulator: &mut Emulator, icount: &mut u64, n: u64) -> Result<(), String> {
    let mut left = n;
    while left > 0 {
        let step = left.min(CHUNK) as u32;
        let snapshot = emulator.step_chunk(step);
        if let Some(error) = snapshot.status.error {
            return Err(error);
        }
        *icount = snapshot.status.icount;
        left -= u64::from(step);
    }
    Ok(())
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
    let num = |name: &str, default: u64| flag(name).and_then(|n| number(&n)).unwrap_or(default);
    let (Some(call_at), Some(call_fn)) = (
        flag("--call-at").and_then(|n| number(&n)),
        flag("--call-fn").and_then(|n| number(&n)),
    ) else {
        return usage("--call-at and --call-fn are required");
    };
    let call_args: Vec<u32> = flag("--call-args")
        .map(|l| l.split(',').filter_map(number).map(|v| v as u32).collect())
        .unwrap_or_default();
    let Some(capture) = flag("--capture").and_then(|c| {
        let f: Vec<_> = c.split(':').filter_map(number).collect();
        (f.len() == 3).then(|| (f[0] as u32, f[1] as usize, f[2] as usize))
    }) else {
        return usage("--capture PC:BUF:LEN is required");
    };
    let settle = num("--settle", 20_000_000);
    let limit = num("--limit", 2_000_000_000);

    let syx = match fs::read(syx_path) {
        Ok(bytes) => bytes,
        Err(error) => return usage(&format!("cannot read {syx_path}: {error}")),
    };
    let mut emulator = match Emulator::new(&syx, None) {
        Ok(emulator) => emulator,
        Err(error) => return usage(&format!("load: {error}")),
    };
    let mut icount = 0u64;
    if let Some(path) = flag("--state") {
        let state = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => return usage(&format!("cannot read {path}: {error}")),
        };
        if let Err(error) = emulator.load_state(&state) {
            return usage(&format!("state: {error}"));
        }
        icount = emulator.snapshot().status.icount;
    } else {
        let max = num("--max", 2_000_000_000);
        while !emulator.snapshot().status.main_ui_reached {
            if let Err(error) = advance(&mut emulator, &mut icount, CHUNK) {
                return usage(&format!("boot: {error}"));
            }
            if icount >= max {
                return usage("boot: no UI");
            }
        }
        if let Err(error) = advance(&mut emulator, &mut icount, num("--after", 100_000_000)) {
            return usage(&format!("boot: {error}"));
        }
    }
    emulator.capture_at(&[capture]);

    let stdout = io::stdout();
    let mut out = stdout.lock();
    let say = |out: &mut io::StdoutLock, value: serde_json::Value| {
        let _ = writeln!(out, "{value}");
        let _ = out.flush();
    };
    say(&mut out, json!({"ready": true, "icount": icount}));

    let mut request = 0u64;
    for line in io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let text = line.trim();
        if text.is_empty() || text.starts_with('#') {
            continue;
        }
        request += 1;
        let Some(bytes) = hex_bytes(text) else {
            say(&mut out, json!({"request": request, "replies": [], "partial": null,
                                 "icount": icount, "error": "not hex"}));
            continue;
        };
        emulator.take_captures();
        let len = bytes.len() as u32;
        let args = [vec![GuestCall::STACK, len], call_args.clone()].concat();
        emulator.queue_call(GuestCall {
            at: call_at as u32,
            func: call_fn as u32,
            args,
            data: vec![(GuestCall::STACK, bytes)],
        });
        let start = icount;
        let mut error = None;
        while emulator.calls_pending().0 != 0 {
            if icount - start >= limit {
                error = Some(format!("the call did not return in {limit} instructions"));
                break;
            }
            if let Err(e) = advance(&mut emulator, &mut icount, CHUNK) {
                error = Some(e);
                break;
            }
        }
        if error.is_none()
            && let Err(e) = advance(&mut emulator, &mut icount, settle)
        {
            error = Some(e);
        }
        let mut replies = Vec::new();
        let mut current: Vec<u8> = Vec::new();
        for piece in emulator.take_captures() {
            current.extend_from_slice(&piece.bytes);
            if current.last() == Some(&0xF7) {
                replies.push(current.iter().map(|b| format!("{b:02X}")).collect::<String>());
                current.clear();
            }
        }
        let partial = (!current.is_empty())
            .then(|| current.iter().map(|b| format!("{b:02X}")).collect::<String>());
        let fatal = error.is_some();
        say(&mut out, json!({"request": request, "replies": replies, "partial": partial,
                             "icount": icount, "error": error}));
        if fatal {
            return ExitCode::from(1);
        }
    }
    ExitCode::from(0)
}
