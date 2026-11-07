//! `coupled-run`: the desktop app's coupled run loop without Tauri or a webview.
//!
//! It restores the same DN2 ready session as the launcher (`tools/emulator/native_emu.py`
//! `--auto-coupled`), steps the emulator in the app's [`STEP_CHUNK`]-instruction
//! chunks as fast as it can with optional native playback (the PCM is also measured), and
//! plays a scenario of guest-clock-driven panel events. For each scenario it
//! reports exact per-thread CPU time, the DSP and ColdFire instruction split, the
//! per-period DSP wall histogram against the 0.667 ms deadline, link timing, and
//! the PCM hash, RMS and peak. Measurement adds no per-instruction work: a few
//! timer reads per 0.667 ms period and one hash/sum pass over the PCM.
//!
//! ```text
//! coupled-run --syx firmware/Digitone_II_OS1.12.syx \
//!   --dsp-image out/snapshots/dn2-audio-ready-cold-2026-10-07/digi-audio-dn2-image.bin \
//!   --dsp-state out/snapshots/dn2-audio-ready-cold-2026-10-07/digi-audio-m5.snap.dsp \
//!   --cf-snapshot out/snapshots/dn2-audio-ready-cold-2026-10-07/digi-audio-m5.snap \
//!   [--dsp-patch FILE] [--card-image FILE] [--scenarios FILE.json] [--scenario NAME]... [--seconds S] [--playback] [--audio-buffer S] [--no-pcm-tap] [--no-link-timing] [--profile-regions] [--profile-execution] \
//!   [--out report.json] [--list] \
//!   [--replay FILE.json] [--wav PATH] [--dump-dsp PATH] [--audio-mode rebuffer|stretch|gap]
//! ```
//!
//! `--dsp zero` runs without a DSP: the DSPI2 peer answers zeros, SSI is paced and the ColdFire
//! side is restored from `--cf-snapshot` alone (`--dsp-image` and `--dsp-state` are not needed).
//! No PCM is produced (PCM report fields are null, `missing_blocks` is 0) and `seconds` / `at_s`
//! count ColdFire instructions at `DIGI_CF_IPS` (default 132 MHz: `seconds * 132e6`; `at_instr` and `instructions` in scenario files are authored at 132e6 and scaled). `--trace` adds `trace` to each scenario's report: one entry per
//! chunk with the instruction count, PC, ready flag, framebuffer and LED hashes, main-loop hits,
//! CPU state and error. The report carries `"dsp": "zero"|"sharc"`.
//!
//! `--dsp-peek NAME=ADDR:LEN[:BLOCK]` (repeatable) puts the SHA-256 of a DSP memory range into each
//! scenario's report (`dsp_peek`), read after the scenario ends; `tools/firmware/l2canary.py` uses it.
//!
//! `--cold-base SHA256` (with `--dsp zero`) boots `--syx`, a patched build of the stock firmware
//! SHA256, from reset to the ready state instead of loading `--cf-snapshot`. Use it to run a build
//! that has no ready snapshot of its own. Without `--card-image` the runtime gives the firmware an
//! empty +Drive.
//!
//! `--span-log PATH` writes every span pass as a JSON line `{"span", "at", "len"}` (`at` is the
//! instruction count at the entry), with `-<name>` before the extension. Each span's report
//! also gives `worst`: the index and the entry count of its longest pass.
//!
//! `--call PC:ARG[,ARG...]` (repeatable) calls a guest function at the ready point, before the
//! scenario, in the given order (`Emulator::call_function`, gcc m68k ABI, interrupts masked, a
//! scratch stack 1 KiB below the guest's). ARG is a number, or `*ADDR+OFF`: the long at ADDR,
//! read at the call, plus OFF. The report lists each call's arguments, D0 and instructions.
//!
//! `--span-baseline STOCK.json` gates each scenario's spans against the same scenario and spans
//! in an earlier stock report (`--out` of a stock run with the same options). A span fails when
//! its `p50` or its `max` is more than its limit above stock: `--span-max NAME=N` (repeatable),
//! else 1,000 instructions. A span also fails when it has more passes over its period, or more
//! late entries, than stock. The result is `gates.span_gate`, and a failure fails the run (exit 2).
//! `tests/scenarios/dn2-load.json` holds the load scenarios for the gate. `--scenarios` is
//! repeatable, so one run can take `dn2.json` and `dn2-load.json`.
//!
//! `--span NAME=ENTRY:EXIT` (repeatable) records the ColdFire instructions from ENTRY to EXIT on each
//! pass and between two entries, from the start of the sample phase (`spans` in the report: count,
//! min, mean, p50, p99, max and `over_period`, a period being one DSPI2 frame at `DIGI_CF_IPS`). On
//! DN2 1.11 and 1.12 the vector-191 handler is `isr=0x40025e36:0x40027bc6` (its only `rte`), and
//! evaluator A is `evala=0x400272d4:0x400272da` (finding 04).
//!
//! `--replay FILE` runs only the scenario in FILE (a scenario object, or a desktop
//! diagnostics export carrying a `recorded_scenario`); it ignores `--scenario` and
//! the scenarios file. `--wav PATH` writes the emulator's own PCM to PATH (a float
//! WAV for `.wav`, else raw f32le); with several scenarios `-<name>` goes before the
//! extension. `--dump-dsp PATH` writes the DSP's canonical state blob (`SHRD`, all
//! memory written since the image; read it with `tools/emulator/dspdiff.py`) at the end of
//! each scenario to `PATH` with `-<name>` before the extension. `--audio-mode` is
//! the underrun recovery of `--playback`.
//!
//! Scenario file: `{"scenarios": [{"name", "description", "seconds" | "instructions",
//! "qa_script", "clip_last_chunk", "expect_sha256", "events": [...]}]}`. An event is `{"at_s" | "at_instr", "button",
//! "down"}` or `{"at_s" | "at_instr", "turn": encoder, "detents": n}`. Times count from
//! the restored ready point: `at_instr` in ColdFire instructions, `at_s` in audio
//! seconds (DSPI2 frames sent / 1500). Events fire at chunk boundaries, before the
//! chunk that follows, so a run is a function of its inputs; with `exact_events` the
//! chunk is shortened so an `at_instr` event fires at exactly that count. `qa_script` also plays
//! the shared QA sequence of `packages/emulator/boot/examples/common` (NO, NO, encoder A).
//! The default scenarios are `tests/scenarios/dn2.json`.

use digi_coupled::{
    AudioMode, AudioOptions, CoupledRuntime, ExecutionProfile, STEP_CHUNK, TierCounters,
    WorkerReport, common,
    scenario::{
        DEFAULT_SCENARIOS, Event, FRAMES_PER_SECOND, Scenario, When, cf_instructions_per_second,
        find_scenario, fire, parse_scenario, parse_scenarios,
    },
    span_gate::{self, Limits},
};
#[cfg(feature = "region-profile")]
use digi_coupled::{RegionCost, RegionProfile};
use elektron_native_boot::thread_cpu_ns;
use serde_json::{Value, json};
#[cfg(feature = "region-profile")]
use std::collections::BTreeMap;
use std::{
    fs,
    path::{Path, PathBuf},
    process::ExitCode,
    time::Instant,
};

/// The DSP period as wall time: a period slower than this cannot keep up in real time.
const DEADLINE_NS: u64 = 666_667;

struct Args {
    syx: PathBuf,
    image: PathBuf,
    state: PathBuf,
    snapshot: PathBuf,
    /// `--scenarios FILE` (repeatable): scenario files, read in order. None: the embedded `dn2.json`.
    scenarios: Vec<PathBuf>,
    names: Vec<String>,
    seconds: Option<f64>,
    playback: bool,
    audio_buffer: f64,
    pcm_tap: bool,
    link_timing: bool,
    profile_regions: bool,
    profile_execution: bool,
    out: Option<PathBuf>,
    list: bool,
    replay: Option<PathBuf>,
    wav: Option<PathBuf>,
    dump_dsp: Option<PathBuf>,
    dsp_patch: Option<PathBuf>,
    card: Option<PathBuf>,
    audio_mode: AudioMode,
    peeks: Vec<(String, Peek)>,
    /// `--dsp-peek NAME=ADDR:LEN`: DSP memory ranges hashed at the end of each scenario.
    dsp_peeks: Vec<(String, u32, usize, usize)>,
    /// `--span NAME=ENTRY:EXIT`: ColdFire instruction spans between two PCs.
    spans: Vec<(String, u32, u32)>,
    /// `--cold-base SHA256`: with `--dsp zero`, boot `--syx` from reset (a patched build of the
    /// stock firmware SHA256) to the ready state instead of loading `--cf-snapshot`.
    cold_base: Option<String>,
    /// `--call PC:ARG[,ARG...]`: guest functions called at the ready point, before the scenario.
    calls: Vec<(u32, Vec<CallArg>)>,
    /// `--span-log PATH`: every span pass as a JSON line, `-<scenario>` before the extension.
    span_log: Option<PathBuf>,
    /// `--span-baseline STOCK.json`: the stock report the spans are gated against.
    span_baseline: Option<Value>,
    /// `--span-max NAME=N`: the extra instructions over stock a span may add.
    span_limits: Limits,
    /// `--dsp zero`: the DSP side answers zeros (no SHARC engine, no PCM).
    zero: bool,
    /// `--trace`: one entry per chunk in each scenario's `trace`.
    trace: bool,
    /// `--guard FILE.json`: write-range guards (the `guard.set` shape), armed at scenario start.
    guard: Option<Value>,
}

/// First 8 bytes of the SHA-256 of `bytes` as 16 hex digits.
fn hex8(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A guest read taken at the end of a run: `ADDR:LEN` or `*ADDR+OFF:LEN`
/// (read the big-endian u32 at ADDR, add OFF).
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
struct Peek {
    addr: u32,
    hop: bool,
    off: u32,
    len: usize,
}

fn parse_num(t: &str) -> Result<u32, String> {
    let t = t.trim();
    let r = match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        Some(h) => u32::from_str_radix(h, 16),
        None => t.parse(),
    };
    r.map_err(|_| format!("bad number {t:?}"))
}

/// One `--call` argument: a number, or the long at an address plus an offset.
#[derive(Clone, Copy, Debug)]
enum CallArg {
    Value(u32),
    Pointer { addr: u32, off: u32 },
}

/// `PC:ARG[,ARG...]`, ARG a number or `*ADDR+OFF` (the long at ADDR, read at the call, plus OFF).
fn parse_call(spec: &str) -> Result<(u32, Vec<CallArg>), String> {
    let (pc, rest) = spec.split_once(':').unwrap_or((spec, ""));
    let args = rest
        .split(',')
        .filter(|a| !a.is_empty())
        .map(|a| match a.strip_prefix('*') {
            Some(p) => {
                let (addr, off) = p.split_once('+').unwrap_or((p, "0"));
                Ok(CallArg::Pointer {
                    addr: parse_num(addr)?,
                    off: parse_num(off)?,
                })
            }
            None => parse_num(a).map(CallArg::Value),
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok((parse_num(pc)?, args))
}

/// The address a call returns to: compared with the PC, never executed (as in `cf_call`).
const CALL_RETURN: u32 = 0x47fd_f000;
/// Bytes between the guest's stack pointer and the call's scratch stack.
const CALL_STACK_GAP: u32 = 0x400;

/// Run `--call`s in order with interrupts masked, on a scratch stack below the guest's.
fn do_calls(rt: &mut CoupledRuntime, calls: &[(u32, Vec<CallArg>)]) -> Result<Value, String> {
    let mut out = Vec::new();
    for (pc, args) in calls {
        let values = args
            .iter()
            .map(|arg| match *arg {
                CallArg::Value(v) => Ok(v),
                CallArg::Pointer { addr, off } => rt
                    .peek32(addr)
                    .map(|v| v.wrapping_add(off))
                    .ok_or(format!("--call: cannot read {addr:#010x}")),
            })
            .collect::<Result<Vec<u32>, String>>()?;
        let (_, a, _, _) = rt.cpu_regs();
        let spec = elektron_native_boot::CallSpec {
            pc: *pc,
            args: values.clone(),
            sp: (a[7] - CALL_STACK_GAP) & !3,
            ret_pc: CALL_RETURN,
            sr: 0x2700,
            max_insns: 5_000_000,
        };
        let r = rt.call_function(&spec)?;
        if !r.finished {
            return Err(format!(
                "--call {pc:#010x} did not return: pc {:#010x}, {:?}",
                r.pc, r.error
            ));
        }
        out.push(json!({
            "pc": format!("{pc:#010x}"),
            "args": values.iter().map(|v| format!("{v:#x}")).collect::<Vec<_>>(),
            "d0": format!("{:#x}", r.d[0]),
            "instructions": r.insns,
        }));
    }
    Ok(Value::Array(out))
}

/// `NAME=ENTRY:EXIT`: two ColdFire PCs.
fn parse_span(spec: &str) -> Result<(String, u32, u32), String> {
    let (name, rest) = spec.split_once('=').ok_or("--span needs NAME=ENTRY:EXIT")?;
    let (entry, exit) = rest.split_once(':').ok_or("--span needs ENTRY:EXIT")?;
    if name.is_empty() {
        return Err("--span needs a NAME".into());
    }
    Ok((name.to_owned(), parse_num(entry)?, parse_num(exit)?))
}

/// `NAME=ADDR:LEN[:BLOCK]`: BLOCK (a power of two, at most LEN) also lists a hash of every
/// BLOCK-byte block, to find which blocks changed.
fn parse_dsp_peek(spec: &str) -> Result<(String, u32, usize, usize), String> {
    let (name, rest) = spec
        .split_once('=')
        .ok_or("--dsp-peek needs NAME=ADDR:LEN[:BLOCK]")?;
    let mut parts = rest.split(':');
    let addr = parse_num(parts.next().unwrap_or(""))?;
    let len = parse_num(parts.next().ok_or("--dsp-peek needs ADDR:LEN")?)? as usize;
    let block = match parts.next() {
        Some(b) => parse_num(b)? as usize,
        None => 0,
    };
    if len == 0 || len > 0x40_0000 {
        return Err("--dsp-peek LEN must be 1..0x400000".into());
    }
    if block != 0 && (!block.is_power_of_two() || block > len) {
        return Err("--dsp-peek BLOCK must be a power of two, at most LEN".into());
    }
    Ok((name.to_string(), addr, len, block))
}

/// SHA-256 of DSP memory bytes (an unknown byte hashes as 0) and how many were unknown.
fn do_dsp_peeks(rt: &mut CoupledRuntime, peeks: &[(String, u32, usize, usize)]) -> Value {
    use sha2::{Digest, Sha256};
    let mut m = serde_json::Map::new();
    for (name, addr, len, block) in peeks {
        let value = match rt.peek_dsp(*addr, *len) {
            Ok(bytes) => {
                let flat: Vec<u8> = bytes.iter().map(|b| b.unwrap_or(0)).collect();
                let mut value = json!({
                    "sha256": format!("{:x}", Sha256::digest(&flat)),
                    "unknown": bytes.iter().filter(|b| b.is_none()).count(),
                    "head": flat.iter().take(16).map(|b| format!("{b:02x}")).collect::<String>(),
                });
                if *block != 0 {
                    // 16 hex digits of each block's SHA-256, and each block's unknown bytes.
                    value["blocks"] = flat
                        .chunks(*block)
                        .map(|c| format!("{:x}", Sha256::digest(c))[..16].to_string())
                        .collect();
                    value["block"] = (*block).into();
                    value["unknown_blocks"] = bytes
                        .chunks(*block)
                        .map(|c| c.iter().filter(|b| b.is_none()).count())
                        .collect();
                }
                value
            }
            Err(e) => json!({"error": e}),
        };
        m.insert(name.clone(), value);
    }
    Value::Object(m)
}

fn parse_peek(spec: &str) -> Result<(String, Peek), String> {
    let (name, rest) = spec.split_once('=').ok_or("--peek needs NAME=SPEC")?;
    let (loc, len) = rest.rsplit_once(':').ok_or("--peek SPEC needs :LEN")?;
    let len = parse_num(len)? as usize;
    if len == 0 || len > 4096 {
        return Err("--peek LEN must be 1..4096".into());
    }
    let (hop, loc) = match loc.strip_prefix('*') {
        Some(l) => (true, l),
        None => (false, loc),
    };
    let (addr, off) = match loc.split_once('+') {
        Some((a, o)) if hop => (parse_num(a)?, parse_num(o)?),
        Some(_) => return Err("+OFF needs a leading *".into()),
        None => (parse_num(loc)?, 0),
    };
    Ok((
        name.to_string(),
        Peek {
            addr,
            hop,
            off,
            len,
        },
    ))
}

fn read_bytes(rt: &mut CoupledRuntime, addr: u32, len: usize) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(len);
    let mut a = addr & !3;
    while out.len() < len + (addr - (addr & !3)) as usize {
        out.extend_from_slice(&rt.peek32(a)?.to_be_bytes());
        a = a.wrapping_add(4);
    }
    let skip = (addr & 3) as usize;
    Some(out[skip..skip + len].to_vec())
}

fn do_peeks(rt: &mut CoupledRuntime, peeks: &[(String, Peek)]) -> Value {
    let mut m = serde_json::Map::new();
    for (name, p) in peeks {
        let base = if p.hop {
            read_bytes(rt, p.addr, 4)
                .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]).wrapping_add(p.off))
        } else {
            Some(p.addr)
        };
        let hex = base
            .and_then(|a| read_bytes(rt, a, p.len))
            .map(|b| b.iter().map(|x| format!("{x:02x}")).collect::<String>());
        m.insert(name.clone(), hex.map_or(Value::Null, Value::from));
    }
    Value::Object(m)
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        syx: "firmware/Digitone_II_OS1.12.syx".into(),
        image: PathBuf::new(),
        state: PathBuf::new(),
        snapshot: PathBuf::new(),
        scenarios: Vec::new(),
        names: Vec::new(),
        seconds: None,
        playback: false,
        audio_buffer: 0.0,
        pcm_tap: true,
        link_timing: true,
        profile_regions: false,
        profile_execution: false,
        out: None,
        list: false,
        replay: None,
        wav: None,
        dump_dsp: None,
        audio_mode: AudioMode::default(),
        dsp_patch: None,
        card: None,
        peeks: Vec::new(),
        dsp_peeks: Vec::new(),
        spans: Vec::new(),
        cold_base: None,
        calls: Vec::new(),
        span_log: None,
        span_baseline: None,
        span_limits: Limits::default(),
        zero: false,
        trace: false,
        guard: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = |what: &str| it.next().ok_or(format!("{what} needs a value"));
        match flag.as_str() {
            "--syx" => a.syx = value("--syx")?.into(),
            "--dsp-image" => a.image = value("--dsp-image")?.into(),
            "--dsp-state" => a.state = value("--dsp-state")?.into(),
            "--cf-snapshot" => a.snapshot = value("--cf-snapshot")?.into(),
            "--dsp-patch" => a.dsp_patch = Some(value("--dsp-patch")?.into()),
            "--card-image" => a.card = Some(value("--card-image")?.into()),
            "--scenarios" => a.scenarios.push(value("--scenarios")?.into()),
            "--scenario" => a.names.push(value("--scenario")?),
            "--seconds" => {
                a.seconds = Some(value("--seconds")?.parse().map_err(|_| "bad --seconds")?)
            }
            "--playback" => a.playback = true,
            "--audio-buffer" => {
                a.audio_buffer = value("--audio-buffer")?
                    .parse()
                    .map_err(|_| "bad --audio-buffer")?
            }
            "--no-pcm-tap" => a.pcm_tap = false,
            "--no-link-timing" => a.link_timing = false,
            "--profile-regions" => a.profile_regions = true,
            "--profile-execution" => a.profile_execution = true,
            "--out" => a.out = Some(value("--out")?.into()),
            "--list" => a.list = true,
            "--replay" => a.replay = Some(value("--replay")?.into()),
            "--peek" => a.peeks.push(parse_peek(&value("--peek")?)?),
            "--dsp-peek" => a.dsp_peeks.push(parse_dsp_peek(&value("--dsp-peek")?)?),
            "--span" => a.spans.push(parse_span(&value("--span")?)?),
            "--cold-base" => a.cold_base = Some(value("--cold-base")?),
            "--call" => a.calls.push(parse_call(&value("--call")?)?),
            "--span-log" => a.span_log = Some(value("--span-log")?.into()),
            "--span-baseline" => {
                let path = value("--span-baseline")?;
                let text = fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
                a.span_baseline =
                    Some(serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))?);
            }
            "--span-max" => a
                .span_limits
                .per_span
                .push(span_gate::parse_limit(&value("--span-max")?)?),
            "--wav" => a.wav = Some(value("--wav")?.into()),
            "--dump-dsp" => a.dump_dsp = Some(value("--dump-dsp")?.into()),
            "--audio-mode" => a.audio_mode = AudioMode::parse(&value("--audio-mode")?)?,
            "--dsp" => {
                a.zero = match value("--dsp")?.as_str() {
                    "zero" => true,
                    "sharc" => false,
                    other => return Err(format!("unknown --dsp {other:?} (zero or sharc)")),
                }
            }
            "--trace" => a.trace = true,
            "--guard" => {
                let path = value("--guard")?;
                let text = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
                let spec: Value =
                    serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))?;
                elektron_native_boot::parse_guard_spec(&spec)
                    .map_err(|e| format!("{path}: {e}"))?;
                a.guard = Some(spec);
            }
            other => return Err(format!("unknown option {other}")),
        }
    }
    if a.span_baseline.is_some() && a.spans.is_empty() {
        return Err("--span-baseline needs at least one --span".into());
    }
    if !a.span_limits.per_span.is_empty() && a.span_baseline.is_none() {
        return Err("--span-max needs --span-baseline".into());
    }
    if a.zero {
        if a.playback || a.profile_regions || a.profile_execution {
            return Err(
                "--dsp zero has no DSP: --playback, --profile-regions and --profile-execution need the SHARC"
                    .into(),
            );
        }
        if !a.list && a.snapshot.as_os_str().is_empty() && a.cold_base.is_none() {
            return Err("--cf-snapshot or --cold-base is required".into());
        }
    } else if a.cold_base.is_some() {
        return Err("--cold-base needs --dsp zero".into());
    } else if !a.list
        && (a.image.as_os_str().is_empty()
            || a.state.as_os_str().is_empty()
            || a.snapshot.as_os_str().is_empty())
    {
        return Err("--dsp-image, --dsp-state and --cf-snapshot are required".into());
    }
    Ok(a)
}

fn percentile(sorted: &[u32], p: f64) -> u32 {
    if sorted.is_empty() {
        return 0;
    }
    sorted[(((sorted.len() - 1) as f64) * p).round() as usize]
}

fn period_histogram(periods: &[u32]) -> Value {
    let mut sorted = periods.to_vec();
    sorted.sort_unstable();
    let over = periods
        .iter()
        .filter(|&&n| u64::from(n) > DEADLINE_NS)
        .count();
    let ms = |n: u32| f64::from(n) / 1e6;
    let mean = periods.iter().map(|&n| f64::from(n)).sum::<f64>() / periods.len().max(1) as f64;
    json!({
        "deadline_ms": DEADLINE_NS as f64 / 1e6,
        "periods": periods.len(),
        "over_deadline": over,
        "over_deadline_fraction": over as f64 / periods.len().max(1) as f64,
        "mean_ms": mean / 1e6,
        "p50_ms": ms(percentile(&sorted, 0.50)),
        "p90_ms": ms(percentile(&sorted, 0.90)),
        "p99_ms": ms(percentile(&sorted, 0.99)),
        "max_ms": ms(sorted.last().copied().unwrap_or(0)),
        "total_ms": periods.iter().map(|&n| u64::from(n)).sum::<u64>() as f64 / 1e6,
    })
}

/// Difference of two numeric JSON objects, field by field.
fn json_delta(after: &Value, before: &Value) -> Value {
    let (Some(a), Some(b)) = (after.as_object(), before.as_object()) else {
        return Value::Null;
    };
    let mut out = serde_json::Map::new();
    for (k, v) in a {
        if let (Some(x), Some(y)) = (v.as_u64(), b.get(k).and_then(Value::as_u64)) {
            out.insert(k.clone(), x.saturating_sub(y).into());
        }
    }
    out.into()
}

fn dsp_split(a: &TierCounters, b: &TierCounters) -> Value {
    let total = a.icount.saturating_sub(b.icount);
    let idle = a.idle_skipped.saturating_sub(b.idle_skipped);
    let interp = a.interpreted.saturating_sub(b.interpreted);
    let blocks = a.block_instructions.saturating_sub(b.block_instructions);
    let fast = a.fast_insns.saturating_sub(b.fast_insns);
    // The fast tier's instructions are counted in block_instructions too.
    let aot = blocks.saturating_sub(fast);
    let other = total.saturating_sub(idle + interp + aot + fast);
    let pct = |n: u64| 100.0 * n as f64 / total.max(1) as f64;
    json!({
        "instructions": total,
        "idle_skip": idle, "interpreter": interp, "aot_block": aot, "fast_tier": fast, "other": other,
        "idle_skip_pct": pct(idle), "interpreter_pct": pct(interp), "aot_block_pct": pct(aot),
        "fast_tier_pct": pct(fast), "other_pct": pct(other),
        "block_entries": a.block_entries.saturating_sub(b.block_entries),
        "block_traps": a.block_traps.saturating_sub(b.block_traps),
        "idle_skips": a.idle_skips.saturating_sub(b.idle_skips),
        "fast_present": a.fast_present,
        "fast_runs": a.fast_runs.saturating_sub(b.fast_runs),
        "fast_calls": a.fast_calls.saturating_sub(b.fast_calls),
        "fast_built_total": a.fast_built, "fast_refused_total": a.fast_refused,
        "fast_armed_total": a.fast_armed,
        "fast_compile_ms_total": a.fast_compile_ns as f64 / 1e6,
        "fast_build_ms_total": a.fast_build_ns as f64 / 1e6,
    })
}

const EXECUTION_PROFILE_MAPS: [(&str, &str); 5] = [
    ("coverage", "pc MODE1 known count"),
    (
        "entries",
        "pc count (interpreter sequence entries, not AOT dispatch attempts)",
    ),
    ("transitions", "from_pc to_pc count"),
    (
        "exits",
        "block_pc exit_kind at_pc count; traps are exit rows",
    ),
    (
        "entry_bails",
        "pc MODE1 unknown_register_bitset pending count; non-trap zero-progress entry bails",
    ),
];

/// Write the Engine's existing option-26 maps only after the timed end probe.
/// Empty TSV maps remain valid evidence that this fresh scenario made no rows
/// of a given kind.
fn write_execution_profile(
    out: &Path,
    scenario: &str,
    profile: &ExecutionProfile,
    end: &WorkerReport,
    start: &WorkerReport,
) -> Result<Value, String> {
    let stem = out
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| format!("--out path has no UTF-8 file stem: {}", out.display()))?;
    let dir = out.with_file_name(format!("{stem}-execution-profile"));
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let scenario_name = scenario
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    let mut maps = Vec::with_capacity(EXECUTION_PROFILE_MAPS.len());
    for (kind, ((name, columns), text)) in
        EXECUTION_PROFILE_MAPS.iter().zip(&profile.maps).enumerate()
    {
        let path = dir.join(format!("{scenario_name}.{name}.tsv"));
        fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        maps.push(json!({
            "kind": kind,
            "name": name,
            "columns": columns,
            "rows": text.lines().count(),
            "path": path,
        }));
    }
    let model_stats = model_stats_delta(end, start)?;
    Ok(json!({
        "metadata": {
            "source": "Engine.set_option(26) and Engine.profile_text(0..4)",
            "measurement": "frequency counts and aggregate model-gate counters only; no CPU or benchmark attribution",
            "start": "enabled by ordered worker acknowledgement after ready and before the start probe",
            "end": "requested by a dedicated worker message after the end timing probe",
            "maps_scope": "fresh Engine for this scenario; maps are cumulative from enable to the post-run request",
            "model_stats_scope": "checked end-minus-start worker-boundary delta; excludes work before the start probe",
            "empty_maps_valid": true,
        },
        "maps": maps,
        "model_stats": model_stats,
    }))
}

fn model_stats_delta(after: &WorkerReport, before: &WorkerReport) -> Result<Value, String> {
    let after = after
        .model_stats
        .ok_or("execution profile requires DSP model stats at end")?;
    let before = before
        .model_stats
        .ok_or("execution profile requires DSP model stats at start")?;
    let delta = |name: &str, end: u64, start: u64| {
        end.checked_sub(start)
            .ok_or_else(|| format!("execution profile model counter moved backwards: {name}"))
    };
    Ok(json!({
        "gated": delta("gated", after.gated, before.gated)?,
        "unsafe_block": delta("unsafe_block", after.unsafe_block, before.unsafe_block)?,
        "irq_deferred": delta("irq_deferred", after.irq_deferred, before.irq_deferred)?,
        "timer_gated": delta("timer_gated", after.timer_gated, before.timer_gated)?,
        "mmr_exits": delta("mmr_exits", after.mmr_exits, before.mmr_exits)?,
        "code_mismatch": delta("code_mismatch", after.code_mismatch, before.code_mismatch)?,
    }))
}

#[cfg(feature = "region-profile")]
fn counter_delta(after: u64, before: u64, name: &str) -> Result<u64, String> {
    after
        .checked_sub(before)
        .ok_or_else(|| format!("region profile counter moved backwards: {name}"))
}

#[cfg(feature = "region-profile")]
fn cost_delta(after: RegionCost, before: RegionCost, name: &str) -> Result<RegionCost, String> {
    Ok(RegionCost {
        calls: counter_delta(after.calls, before.calls, &format!("{name}.calls"))?,
        zero_progress_calls: counter_delta(
            after.zero_progress_calls,
            before.zero_progress_calls,
            &format!("{name}.zero_progress_calls"),
        )?,
        instructions: counter_delta(
            after.instructions,
            before.instructions,
            &format!("{name}.instructions"),
        )?,
        wall_ns: counter_delta(after.wall_ns, before.wall_ns, &format!("{name}.wall_ns"))?,
    })
}

#[cfg(feature = "region-profile")]
fn profile_costs(profile: &RegionProfile) -> BTreeMap<(u8, u32), RegionCost> {
    let mut costs = BTreeMap::new();
    for (pc, entry) in profile.entries() {
        if entry.fast.calls != 0 {
            costs.insert((0, pc), entry.fast);
        }
        if entry.aot.calls != 0 {
            costs.insert((1, pc), entry.aot);
        }
    }
    costs
}

#[cfg(feature = "region-profile")]
fn region_profile_delta(after: &WorkerReport, before: &WorkerReport) -> Result<Value, String> {
    let after_profile = after
        .region_profile
        .as_ref()
        .ok_or("region profile was not enabled in the DSP worker")?;
    let before_profile = before
        .region_profile
        .as_ref()
        .ok_or("region profile was not enabled at scenario start")?;
    let after_costs = profile_costs(after_profile);
    let before_costs = profile_costs(before_profile);
    let mut totals = [RegionCost::default(); 2];
    let mut rows = Vec::new();
    for (&(tier, pc), &after_cost) in &after_costs {
        let before_cost = before_costs.get(&(tier, pc)).copied().unwrap_or_default();
        let cost = cost_delta(after_cost, before_cost, &format!("{tier}:{pc:06x}"))?;
        if cost.calls == 0 {
            continue;
        }
        let total = &mut totals[tier as usize];
        total.calls = total
            .calls
            .checked_add(cost.calls)
            .ok_or("region profile calls overflow")?;
        total.zero_progress_calls = total
            .zero_progress_calls
            .checked_add(cost.zero_progress_calls)
            .ok_or("region profile zero-progress calls overflow")?;
        total.instructions = total
            .instructions
            .checked_add(cost.instructions)
            .ok_or("region profile instructions overflow")?;
        total.wall_ns = total
            .wall_ns
            .checked_add(cost.wall_ns)
            .ok_or("region profile wall time overflow")?;
        rows.push((tier, pc, cost));
    }
    for key in before_costs.keys() {
        if !after_costs.contains_key(key) {
            return Err(format!(
                "region profile entry disappeared: {}:{:06x}",
                key.0, key.1
            ));
        }
    }
    rows.sort_unstable_by(|a, b| b.2.wall_ns.cmp(&a.2.wall_ns));
    let (Some(after_tiers), Some(before_tiers)) = (&after.tiers, &before.tiers) else {
        return Err("region profile requires DSP tier counters".into());
    };
    let blocks = counter_delta(
        after_tiers.block_instructions,
        before_tiers.block_instructions,
        "block_instructions",
    )?;
    let fast = counter_delta(
        after_tiers.fast_insns,
        before_tiers.fast_insns,
        "fast_insns",
    )?;
    let aot = blocks
        .checked_sub(fast)
        .ok_or("fast instruction delta exceeds block instruction delta")?;
    if totals[0].instructions != fast || totals[1].instructions != aot {
        // Chained fast-tier runs are not all timed: report, do not fail.
        eprintln!(
            "coupled-run: region profile does not reconcile: fast {} != {fast}, aot {} != {aot}",
            totals[0].instructions, totals[1].instructions
        );
    }
    if false {
        return Err(format!(
            "region profile instruction reconciliation failed: fast {} != {fast}, aot {} != {aot}",
            totals[0].instructions, totals[1].instructions
        ));
    }
    let cost_json = |cost: RegionCost| {
        json!({
            "calls": cost.calls,
            "zero_progress_calls": cost.zero_progress_calls,
            "retired_instructions": cost.instructions,
            "wall_ns": cost.wall_ns,
        })
    };
    Ok(json!({
        "metadata": {
            "clock": "std::time::Instant elapsed nanoseconds",
            "method": "times each fast.run attempt and generated entry.f invocation; instruction counts are icount deltas",
            "fast_wall_ns": "includes cold build, rebuild, and compiler work performed inside fast.run; do not treat it as steady-state execution time",
            "chain_attribution": "an AOT invocation includes any generated chain calls and is attributed to its dispatch entry PC",
            "unmeasured": "dispatcher, model and bank housekeeping, interpreter instructions, and idle visits are not attributed",
            "overhead": "opt-in profiling adds timer reads and sparse-page counter updates at measured dispatches",
        },
        "tiers": { "fast": cost_json(totals[0]), "aot": cost_json(totals[1]) },
        "rows": rows.into_iter().map(|(tier, pc, cost)| json!({
            "tier": if tier == 0 { "fast" } else { "aot" },
            "entry_pc": format!("{pc:06x}"),
            "entry_pc_u32": pc,
            "calls": cost.calls,
            "zero_progress_calls": cost.zero_progress_calls,
            "retired_instructions": cost.instructions,
            "wall_ns": cost.wall_ns,
        })).collect::<Vec<_>>(),
        "reconciliation": { "fast_instructions": fast, "aot_instructions": aot, "ok": true },
    }))
}

struct Probe {
    wall: Instant,
    cf_cpu_ns: u64,
    worker: WorkerReport,
    status: Value,
    link: Value,
    frames: u64,
}

fn probe(rt: &mut CoupledRuntime) -> Result<Probe, String> {
    // snapshot() synchronises the DSP: every frame sent is rendered and drained.
    let snap = rt.snapshot();
    let (worker, report) = if rt.has_audio() {
        (
            rt.worker_report().ok_or("DSP worker stopped")?,
            rt.audio_report().ok_or("no native audio session")?,
        )
    } else {
        // Zero DSP: no worker and no audio session.
        (WorkerReport::default(), Value::Null)
    };
    Ok(Probe {
        wall: Instant::now(),
        cf_cpu_ns: thread_cpu_ns(),
        worker,
        status: serde_json::to_value(&snap.status).map_err(|e| e.to_string())?,
        link: report["link_timing"].clone(),
        frames: rt.frames_sent(),
    })
}

/// Most ColdFire instructions a `--cold-base` boot may take to reach the ready state.
const COLD_BOOT_LIMIT: u64 = 4_000_000_000;

/// `--cold-base`: a patched build booted from reset with the zero DSP peer, stepped to the
/// ready state. Its scenario clock starts there, as after a snapshot restore.
fn cold_zero_runtime(
    syx: &[u8],
    card: Option<emmc_card::Card>,
    base: &str,
) -> Result<CoupledRuntime, String> {
    let started = Instant::now();
    let mut rt = CoupledRuntime::new_patched(syx, card, base)?;
    rt.enable_zero_dsp_timing()?;
    loop {
        let status = rt.step_chunk(STEP_CHUNK).status;
        if let Some(error) = status.error {
            return Err(format!("cold boot stopped at {:#010x}: {error}", status.pc));
        }
        if status.ready {
            break;
        }
        if status.icount > COLD_BOOT_LIMIT {
            return Err(format!(
                "cold boot: not ready after {} instructions (phase {})",
                status.icount, status.phase
            ));
        }
    }
    rt.rebase_clock();
    eprintln!(
        "cold boot ready at {} instructions in {:.1}s",
        rt.icount(),
        started.elapsed().as_secs_f64()
    );
    Ok(rt)
}

/// `base` with `-<name>` before its extension.
fn wav_path_for(base: &Path, name: &str) -> PathBuf {
    let stem = base.file_stem().and_then(|s| s.to_str()).unwrap_or("out");
    let file = match base.extension().and_then(|e| e.to_str()) {
        Some(ext) => format!("{stem}-{name}.{ext}"),
        None => format!("{stem}-{name}"),
    };
    base.with_file_name(file)
}

fn run_scenario(
    args: &Args,
    syx: &[u8],
    sc: &Scenario,
    wav: Option<&Path>,
) -> Result<Value, String> {
    let setup = Instant::now();
    let options = AudioOptions {
        image: args.image.clone(),
        state: args.state.clone(),
        snapshot: args.snapshot.clone(),
        buffer_seconds: args.audio_buffer,
        playback: args.playback,
        automatic: false,
        expected_syx: None,
        link_timing: args.link_timing,
        audio_mode: args.audio_mode,
        dsp_patch: args.dsp_patch.clone(),
        zero: args.zero,
    };
    let card = args
        .card
        .as_deref()
        .map(elektron_native_boot::card_file::card_from_image)
        .transpose()?;
    let mut rt = match &args.cold_base {
        Some(base) => cold_zero_runtime(syx, card, base)?,
        None => CoupledRuntime::new(syx, card, Some(&options))?,
    };
    if let Some(spec) = &args.guard {
        let (protect, allow_pc) = elektron_native_boot::parse_guard_spec(spec)?;
        rt.set_write_guards(protect, allow_pc);
    }
    if let Some(path) = wav {
        rt.capture_pcm(path)?;
    }
    if args.pcm_tap {
        rt.observe_pcm(false);
    }
    let setup_s = setup.elapsed().as_secs_f64();
    let initial = rt.snapshot().status;
    if !initial.ready {
        return Err("the restored snapshot is not at ready".into());
    }
    #[cfg(feature = "region-profile")]
    if args.profile_regions && !rt.set_region_profile(true) {
        return Err("DSP worker stopped while enabling region profile".into());
    }
    if args.profile_execution && !rt.set_execution_profile(true) {
        return Err("DSP worker did not enable execution profile".into());
    }
    let mut script = sc.qa_script.then(common::Script::from_env);
    if let Some(script) = &mut script {
        script.poll(&mut rt, initial.icount, true);
    }
    let seconds = args.seconds.or(sc.seconds);
    let target_frames = seconds.map(|s| (s * FRAMES_PER_SECOND).ceil() as u64);
    let until_instr = if args.zero {
        // No DSP frame clock: seconds count ColdFire instructions.
        match seconds {
            Some(s) => Some((s * cf_instructions_per_second()).round() as u64),
            None => sc.instructions,
        }
    } else if args.seconds.is_some() {
        None
    } else {
        sc.instructions
    };

    let calls = do_calls(&mut rt, &args.calls)?;
    #[cfg(feature = "cf-mix")]
    rt.cf_mix_reset();
    for (name, entry, exit) in &args.spans {
        rt.add_span(name, *entry, *exit);
    }
    let start = probe(&mut rt)?;
    let initial_audio_health = rt.audio_report().unwrap_or(Value::Null);
    let start_health = json!({
        "elapsed_wall_seconds": 0.0,
        "source_pcm_frames": initial_audio_health["source_pcm_frames"],
        "health": initial_audio_health,
    });
    eprintln!("coupled-run sample phase started");
    let (mut next, mut icount, mut step_error) = (0usize, initial.icount, None);
    let mut playback_observations = Vec::new();
    let mut next_playback_observation = 0.1_f64;
    let mut events: Vec<Event> = sc.events.clone();
    if args.zero {
        for e in &mut events {
            if let Some(s) = e.at_s {
                e.when = When::Instr((s * cf_instructions_per_second()).round() as u64);
            }
        }
    }
    events.sort_by_key(|e| match e.when {
        When::Instr(n) => (0, n),
        When::Frames(n) => (1, n),
    });
    let mut chunks = 0u64;
    let mut trace = Vec::new();
    let (mut last_fb, mut last_leds, mut last_rgb) = (String::new(), String::new(), String::new());
    loop {
        while let Some(e) = events.get(next) {
            let due = match e.when {
                When::Instr(n) => icount - initial.icount >= n,
                When::Frames(n) => rt.frames_sent() >= n,
            };
            if !due {
                break;
            }
            fire(&mut rt, e.action)?;
            next += 1;
        }
        let elapsed = icount - initial.icount;
        let done = until_instr.map_or_else(
            || target_frames.is_some_and(|n| rt.frames_sent() >= n),
            |n| elapsed >= n,
        );
        if done {
            break;
        }
        let mut budget = match until_instr {
            Some(n) if sc.clip_last_chunk => (n - elapsed).min(u64::from(STEP_CHUNK)) as u32,
            _ => STEP_CHUNK,
        };
        if sc.exact_events
            && let Some(e) = events.get(next)
            && let When::Instr(n) = e.when
            && n > elapsed
        {
            budget = budget.min((n - elapsed).min(u64::from(u32::MAX)) as u32);
        }
        let snap = rt.step_chunk(budget);
        let status = snap.status;
        chunks += 1;
        if args.trace {
            if let Some(f) = &snap.frame {
                last_fb = hex8(f);
            }
            if let Some(l) = &snap.leds {
                last_leds.clone_from(l);
            }
            if let Some(l) = &snap.rgb {
                last_rgb = hex8(l.as_bytes());
            }
            trace.push(json!({
                "el": status.icount - initial.icount, "pc": status.pc, "ready": status.ready,
                "fb": last_fb, "frev": status.frame_revision, "leds": last_leds,
                "rgb": last_rgb, "ledrev": status.led_revision,
                "mainloop_hits": status.mainloop_hits, "cpu_state": status.cpu_state,
                "err": status.error,
            }));
        }
        if args.playback && start.wall.elapsed().as_secs_f64() >= next_playback_observation {
            let elapsed = start.wall.elapsed().as_secs_f64();
            let health = rt.audio_report().unwrap_or(Value::Null);
            playback_observations.push(json!({
                "elapsed_wall_seconds": elapsed,
                "source_pcm_frames": health["source_pcm_frames"],
                "health": health,
            }));
            next_playback_observation = elapsed + 0.1;
        }
        icount = status.icount;
        if let Some(error) = status.error {
            step_error = Some(error);
            break;
        }
        if let Some(script) = &mut script {
            script.poll(&mut rt, icount, status.ready);
        }
    }
    let end = probe(&mut rt)?;
    #[cfg(feature = "cf-mix")]
    if let Ok(dir) = std::env::var("DIGI_CF_MIX_OUT") {
        let mut mix = rt.cf_mix();
        let st = |p: &Probe, k: &str| p.status[k].as_u64().unwrap_or(0);
        mix["scenario"] = json!(sc.name);
        mix["guest_instructions"] = json!(st(&end, "icount") - st(&start, "icount"));
        mix["interpreted"] =
            json!(st(&end, "interpreted_instructions") - st(&start, "interpreted_instructions"));
        mix["idle_fast_forward"] = json!(
            st(&end, "idle_fast_forwarded_instructions")
                - st(&start, "idle_fast_forwarded_instructions")
        );
        mix["ram_clear"] = json!(
            st(&end, "ram_clear_fast_forwarded_instructions")
                - st(&start, "ram_clear_fast_forwarded_instructions")
        );
        mix["softfloat_hle_calls"] =
            json!(st(&end, "softfloat_hle_calls") - st(&start, "softfloat_hle_calls"));
        mix["cf_ips"] = json!(elektron_native_boot::clock::cf_ips());
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = Path::new(&dir).join(format!("{}.json", sc.name.replace('/', "_")));
        std::fs::write(&path, serde_json::to_vec(&mix).map_err(|e| e.to_string())?)
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    let peek = do_peeks(&mut rt, &args.peeks);
    let dsp_peek = do_dsp_peeks(&mut rt, &args.dsp_peeks);
    let spans = rt.spans_report((cf_instructions_per_second() / FRAMES_PER_SECOND).round() as u64);
    if let Some(base) = &args.span_log {
        let path = wav_path_for(base, &sc.name);
        let mut text = String::new();
        for (name, at, len) in rt.span_passes() {
            text.push_str(&json!({"span": name, "at": at, "len": len}).to_string());
            text.push('\n');
        }
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    let dsp_dump = match &args.dump_dsp {
        Some(base) => {
            let path = wav_path_for(base, &sc.name);
            let (blob, dsp_icount) = rt.dsp_export().ok_or("--dump-dsp: no DSP engine")?;
            std::fs::write(&path, &blob).map_err(|e| format!("{}: {e}", path.display()))?;
            json!({"path": path, "bytes": blob.len(), "dsp_icount": dsp_icount})
        }
        None => Value::Null,
    };
    let execution_profile = if args.profile_execution {
        let profile = rt
            .execution_profile()
            .ok_or("DSP worker did not return execution profile maps")?;
        let out = args
            .out
            .as_deref()
            .ok_or("--profile-execution requires --out")?;
        Some(write_execution_profile(
            out,
            &sc.name,
            &profile,
            &end.worker,
            &start.worker,
        )?)
    } else {
        None
    };
    let wall = end.wall.duration_since(start.wall).as_secs_f64();
    let cf_cpu = (end.cf_cpu_ns - start.cf_cpu_ns) as f64 / 1e9;
    let dsp_cpu = (end.worker.cpu_ns - start.worker.cpu_ns) as f64 / 1e9;
    let dsp_cycles = end.worker.cycles.saturating_sub(start.worker.cycles);
    let dsp_insns = end.worker.insns.saturating_sub(start.worker.insns);
    let tap = rt.pcm_tap();
    let audio_s = match tap {
        Some(t) => t.seconds(),
        None => (end.frames - start.frames) as f64 / FRAMES_PER_SECOND,
    };
    let per = |x: f64| if audio_s > 0.0 { x / audio_s } else { 0.0 };

    let periods = end
        .worker
        .period_ns
        .get(start.worker.period_ns.len()..)
        .unwrap_or(&[]);
    let st = |p: &Probe, k: &str| p.status[k].as_u64().unwrap_or(0);
    let cf_total = st(&end, "icount") - st(&start, "icount");
    let cf_interp = st(&end, "interpreted_instructions") - st(&start, "interpreted_instructions");
    let cf_idle = st(&end, "idle_fast_forwarded_instructions")
        - st(&start, "idle_fast_forwarded_instructions");
    let cf_clear = st(&end, "ram_clear_fast_forwarded_instructions")
        - st(&start, "ram_clear_fast_forwarded_instructions");
    let health = rt.audio_report().unwrap_or(Value::Null);
    let missing = if args.zero {
        0
    } else {
        health["missing_blocks"].as_u64().unwrap_or(u64::MAX)
    };
    let halted = health["halted"].clone();
    let pcm = tap.map_or(Value::Null, |t| {
        json!({
            "values": t.count, "seconds": t.seconds(), "sha256": t.sha256_hex(), "rms": t.rms(),
            "peak": t.peak, "nonzero": t.nonzero, "non_finite": t.non_finite,
            "active_windows_100ms": t.active_windows, "windows_100ms": t.windows,
            "window_rms_100ms": t.window_rms.iter().map(|&r| (f64::from(r) * 1e4).round() / 1e4).collect::<Vec<_>>(),
        })
    });
    let hash_match = match (&sc.expect_sha256, tap) {
        (Some(want), Some(t)) => Some(*want == t.sha256_hex()),
        (Some(_), None) if !args.zero => Some(false),
        _ => None,
    };
    #[cfg(feature = "region-profile")]
    let region_profile = if args.profile_regions {
        region_profile_delta(&end.worker, &start.worker)?
    } else {
        Value::Null
    };
    #[cfg(not(feature = "region-profile"))]
    let region_profile = Value::Null;
    let guard = rt.guard_report();
    let guard_ok = guard.total == 0;
    let playback_ok = !args.playback
        || (health["sink_connected"].as_bool() == Some(true)
            && health["sink"]["stream_started"].as_bool() == Some(true)
            && health["sink"]["callback_max_frames"].as_u64().unwrap_or(0) > 0
            && health["device_error"].is_null()
            && health["sink"]["error"].is_null());
    let gates = json!({
        "missing_blocks": missing, "halted": halted, "step_error": step_error,
        "pcm_non_finite": tap.map_or(0, |t| t.non_finite),
        "pcm_hash_expected": sc.expect_sha256, "pcm_hash_match": hash_match,
        "ok": missing == 0 && halted.is_null() && step_error.is_none()
            && tap.is_none_or(|t| t.non_finite == 0) && hash_match != Some(false) && playback_ok
            && guard_ok,
        "playback_health_ok": playback_ok,
        "guard_ok": guard_ok,
    });
    Ok(json!({
        "name": sc.name, "description": sc.description,
        "audio_seconds": audio_s, "wall_seconds": wall, "real_time_factor": per(wall).recip(),
        "setup_wall_seconds": setup_s, "chunks": chunks,
        "audio_config": { "playback": args.playback, "audio_buffer_seconds": args.audio_buffer, "audio_mode": args.audio_mode.name() },
        "audio_health_start": start_health,
        "audio_health_end": { "elapsed_wall_seconds": wall, "source_pcm_frames": health["source_pcm_frames"], "health": health },
        "playback_observations": playback_observations,
        "guest_instructions": cf_total, "frames": end.frames - start.frames,
        "cf_ips": elektron_native_boot::clock::cf_ips(),
        "softfloat_hle_calls": st(&end, "softfloat_hle_calls") - st(&start, "softfloat_hle_calls"),
        "cpu": {
            "dsp_thread_s": dsp_cpu, "cf_thread_s": cf_cpu,
            "dsp_thread_per_audio_s": per(dsp_cpu), "cf_thread_per_audio_s": per(cf_cpu),
            "dsp_thread_cycles_per_audio_s": per(dsp_cycles as f64),
            "dsp_thread_insns_per_audio_s": per(dsp_insns as f64),
            "dsp_thread_busy_of_wall": dsp_cpu / wall.max(1e-9),
            "cf_thread_busy_of_wall": cf_cpu / wall.max(1e-9),
            "clock": "CLOCK_THREAD_CPUTIME_ID",
        },
        "link_timing": json_delta(&end.link, &start.link),
        "dsp_instructions": match (&end.worker.tiers, &start.worker.tiers) {
            (Some(a), Some(b)) => dsp_split(a, b),
            _ => Value::Null,
        },
        "region_profile": region_profile,
        "execution_profile": execution_profile,
        "cf_instructions": {
            "total": cf_total, "interpreted": cf_interp, "idle_fast_forward": cf_idle,
            "ram_clear": cf_clear,
            "interpreted_pct": 100.0 * cf_interp as f64 / cf_total.max(1) as f64,
            "idle_fast_forward_pct": 100.0 * cf_idle as f64 / cf_total.max(1) as f64,
        },
        "dsp_period_wall": period_histogram(periods),
        "pcm": pcm,
        "peek": peek,
        "dsp_peek": dsp_peek,
        "spans": spans,
        "calls": calls,
        "dsp_dump": dsp_dump,
        "gates": gates,
        "guard": guard,
        "trace": if args.trace { Value::Array(trace) } else { Value::Null },
    }))
}

fn summary_line(r: &Value) -> String {
    let f = |v: &Value| v.as_f64().unwrap_or(f64::NAN);
    format!(
        "{:<14} audio {:>6.2}s wall {:>7.2}s rtf {:>5.2} dsp_cpu/s {:>5.2} cf_cpu/s {:>5.2} over_deadline {:>5.1}% rms {:.4} ok={}",
        r["name"].as_str().unwrap_or("?"),
        f(&r["audio_seconds"]),
        f(&r["wall_seconds"]),
        f(&r["real_time_factor"]),
        f(&r["cpu"]["dsp_thread_per_audio_s"]),
        f(&r["cpu"]["cf_thread_per_audio_s"]),
        100.0 * f(&r["dsp_period_wall"]["over_deadline_fraction"]),
        f(&r["pcm"]["rms"]),
        r["gates"]["ok"],
    )
}

fn real_main() -> Result<bool, String> {
    let args = parse_args()?;
    if args.profile_regions && !cfg!(feature = "region-profile") {
        return Err(
            "--profile-regions needs a diagnostic rebuild with --features coupled-audio,region-profile"
                .into(),
        );
    }
    if args.profile_execution && args.out.is_none() {
        return Err("--profile-execution requires --out".into());
    }
    let all = if let Some(path) = &args.replay {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let root: Value =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let found =
            find_scenario(&root).ok_or_else(|| format!("{}: no scenario found", path.display()))?;
        vec![parse_scenario(found)?]
    } else {
        let mut all: Vec<Scenario> = Vec::new();
        if args.scenarios.is_empty() {
            all = parse_scenarios(DEFAULT_SCENARIOS)?;
        }
        for path in &args.scenarios {
            let text =
                std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
            for sc in parse_scenarios(&text)? {
                if all.iter().any(|s| s.name == sc.name) {
                    return Err(format!(
                        "{}: scenario {} is named twice",
                        path.display(),
                        sc.name
                    ));
                }
                all.push(sc);
            }
        }
        all
    };
    if args.list {
        for s in &all {
            println!("{:<14} {}", s.name, s.description);
        }
        return Ok(true);
    }
    if args.replay.is_none() {
        for n in &args.names {
            if !all.iter().any(|s| &s.name == n) {
                return Err(format!("no scenario named {n}"));
            }
        }
    }
    let chosen: Vec<&Scenario> = all
        .iter()
        .filter(|s| args.replay.is_some() || args.names.is_empty() || args.names.contains(&s.name))
        .collect();
    let syx = std::fs::read(&args.syx).map_err(|e| format!("{}: {e}", args.syx.display()))?;
    let mut results = Vec::new();
    let several = chosen.len() > 1;
    for sc in chosen {
        eprintln!("scenario {} ...", sc.name);
        let wav = args.wav.as_deref().map(|base| {
            if several {
                wav_path_for(base, &sc.name)
            } else {
                base.to_path_buf()
            }
        });
        let mut r = run_scenario(&args, &syx, sc, wav.as_deref())?;
        if let Some(stock) = &args.span_baseline {
            let dsp = if args.zero { "zero" } else { "sharc" };
            let gate = span_gate::check(&r, dsp, stock, &args.span_limits);
            let ok = r["gates"]["ok"] == true && gate["ok"] == true;
            r["gates"]["span_gate"] = gate;
            r["gates"]["ok"] = json!(ok);
        }
        println!("{}", summary_line(&r));
        for line in span_gate::summary(&r["gates"]["span_gate"]) {
            println!("{line}");
        }
        results.push(r);
    }
    let ok = results.iter().all(|r| r["gates"]["ok"] == true);
    let report = json!({
        "schema_version": 1,
        "step_chunk": STEP_CHUNK,
        "env": {
            "SHARC_FAST_REGIONS": std::env::var("SHARC_FAST_REGIONS").ok(),
            "SHARC_FAST_AUTO": std::env::var("SHARC_FAST_AUTO").ok(),
            "SHARC_GEN_DIR": std::env::var("SHARC_GEN_DIR").ok(),
            "DSP_IDLE_SKIP": std::env::var("DSP_IDLE_SKIP").ok(),
        },
        "dsp": if args.zero { "zero" } else { "sharc" },
        "audio_mode": args.audio_mode.name(),
        "pcm_tap": args.pcm_tap,
        "link_timing": args.link_timing,
        "profile_regions": args.profile_regions,
        "profile_execution": args.profile_execution,
        "scenarios": results,
    });
    if let Some(out) = &args.out {
        if let Some(dir) = out.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(
            out,
            serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("{}: {e}", out.display()))?;
    }
    Ok(ok)
}

fn main() -> ExitCode {
    match real_main() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => {
            eprintln!("coupled-run: a gate failed (see \"gates\" in the report)");
            ExitCode::from(2)
        }
        Err(e) => {
            eprintln!("coupled-run: {e}");
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use digi_coupled::scenario::Action;

    #[test]
    fn default_scenarios_parse_and_have_the_documented_names() {
        let all = parse_scenarios(DEFAULT_SCENARIOS).unwrap();
        let names: Vec<_> = all.iter().map(|s| s.name.as_str()).collect();
        for n in [
            "idle",
            "trig1-hold",
            "kb-chord16",
            "exactness-100m",
            "trig1-noqa-40m",
        ] {
            assert!(names.contains(&n), "{n}");
        }
        let hold = all.iter().find(|s| s.name == "trig1-hold").unwrap();
        assert!(hold.qa_script && hold.seconds == Some(10.0));
    }

    #[test]
    fn load_scenarios_parse_and_have_the_documented_names() {
        let all =
            parse_scenarios(include_str!("../../../../../tests/scenarios/dn2-load.json")).unwrap();
        let names: Vec<_> = all.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "load-seq16",
                "load-turns",
                "load-ptn-switch",
                "load-kb-roll"
            ]
        );
        let defaults = parse_scenarios(DEFAULT_SCENARIOS).unwrap();
        assert!(
            all.iter()
                .all(|s| defaults.iter().all(|d| d.name != s.name))
        );
    }

    #[test]
    fn scenarios_need_exactly_one_end_and_a_valid_event() {
        let bad = |text: &str| parse_scenarios(text).unwrap_err();
        assert!(bad(r#"{"scenarios":[{"name":"x"}]}"#).contains("exactly one"));
        assert!(
            bad(r#"{"scenarios":[{"name":"x","seconds":1,"instructions":2}]}"#)
                .contains("exactly one")
        );
        assert!(
            bad(r#"{"scenarios":[{"name":"x","seconds":1,"events":[{"button":1}]}]}"#)
                .contains("at_s")
        );
        assert!(
            bad(r#"{"scenarios":[{"name":"x","seconds":1,"events":[{"at_s":0}]}]}"#)
                .contains("button")
        );
        let ok = parse_scenarios(
            r#"{"scenarios":[{"name":"x","seconds":2,"events":[{"at_s":1.5,"button":25},{"at_instr":7,"turn":1,"detents":-2}]}]}"#,
        )
        .unwrap();
        assert!(matches!(ok[0].events[0].when, When::Frames(2250)));
        assert!(matches!(
            ok[0].events[1].action,
            Action::Turn {
                encoder: 1,
                detents: -2
            }
        ));
    }

    #[test]
    fn peek_specs_parse() {
        let (n, p) = parse_peek("br=*0x800052a0+0x106:2").unwrap();
        assert_eq!(n, "br");
        assert_eq!(
            p,
            Peek {
                addr: 0x8000_52a0,
                hop: true,
                off: 0x106,
                len: 2
            }
        );
        let (_, p) = parse_peek("x=2147500000:16").unwrap();
        assert_eq!(
            p,
            Peek {
                addr: 2_147_500_000,
                hop: false,
                off: 0,
                len: 16
            }
        );
        assert!(parse_peek("x=0x10+4:2").is_err());
        assert!(parse_peek("x=0x10").is_err());
        assert!(parse_peek("x=0x10:0").is_err());
    }

    #[test]
    fn percentiles_and_deadline_count() {
        let periods: Vec<u32> = (1..=100).map(|n| n * 10_000).collect();
        let h = period_histogram(&periods);
        assert_eq!(h["periods"], 100);
        // Periods above 666,667 ns: 67..=100.
        assert_eq!(h["over_deadline"], 34);
        assert!((h["max_ms"].as_f64().unwrap() - 1.0).abs() < 1e-9);
        assert!((h["p50_ms"].as_f64().unwrap() - 0.51).abs() < 0.011);
        assert_eq!(period_histogram(&[])["over_deadline"], 0);
    }

    #[test]
    fn tier_split_counts_fast_instructions_once() {
        let before = TierCounters::default();
        let after = TierCounters {
            icount: 1000,
            idle_skipped: 400,
            interpreted: 10,
            block_instructions: 590, // includes the 200 the fast tier ran
            fast_insns: 200,
            fast_present: true,
            ..TierCounters::default()
        };
        let split = dsp_split(&after, &before);
        assert_eq!(split["aot_block"], 390);
        assert_eq!(split["fast_tier"], 200);
        assert_eq!(split["other"], 0);
        assert_eq!(split["idle_skip"], 400);
    }
}
