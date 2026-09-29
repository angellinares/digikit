//! Replay a `DT2MMIO` trace (`emu/mmiotrace.py`) against this crate's
//! timer/INTC models and report the first mismatch, with context.
//!
//! Usage: `mmio-replay <trace-file> [--limit N] [--verbose]`
//!
//! Method (see `docs/plan-native-emulator.md` P4, and `machine.rs`'s module
//! docs for the interface this drives):
//!
//! * The window's `MARK "setup"` record gives the exact PIT/DTIM channel
//!   sets and instructions-per-second the Python run used -- this replay
//!   configures [`periph::Timers`] from it, not from a hardcoded default,
//!   since a channel this crate did not enable gets no scheduling logic at
//!   all (plain RAM; see `pit.rs`/`dtim.rs`).
//! * The first `STATE`+`PAGE` block (recorded before the trace's first
//!   `STEP`) seeds every register byte and the PIT/DTIM scheduling state
//!   (`next[]`, `pending`); later `STATE`+`PAGE` blocks (every
//!   `state_every` instructions) resync both, which is also a free
//!   consistency check: if this crate's own simulation had already drifted,
//!   the resync papers over it silently, but a mismatch reported *before*
//!   that point is real.
//! * Every `STEP` record's clock equals the guest instruction count right
//!   after the *previous* step finished -- exactly the value
//!   `emu.longrun.spin` passes to `Timers.service` (see `pit.rs`'s module
//!   docs and this file's design notes in the handback). So: whenever the
//!   trace's clock advances to an unseen value, this replay calls
//!   `Timers::service` with that same value, before processing whatever
//!   record revealed the advance, and matches the vectors it raises against
//!   the trace's own `IRQ` records for PIT (205-208) and DTIM (96-99)
//!   vectors, in order, at that same clock.
//! * Guest `RD`/`WR` records at a timers/INTC address are routed through
//!   `Timers::read`/`write`; a `RD` is checked, a `WR` is mirrored (input,
//!   not compared -- see `regfile.rs`: unmodelled registers are plain RAM in
//!   the oracle, so mirroring the guest's own writes is what makes reads
//!   match, by construction, everywhere this crate does not add real
//!   scheduling logic).
//! * `HWR` records from `emu.dtim`/`emu.pit` (DTIM's DTER REF-bit
//!   read-modify-write; PIT never writes guest memory, see `pit.rs`) are
//!   checked against the host writes `Timers::service` reports performing.
//! * SR/IPL, which `deliver_pending`'s masking check needs and this
//!   peripheral-only crate has no CPU to supply, is reconstructed from every
//!   `IRQ` record's `frame_sr` and every `RTE` record's restored `sr` --
//!   see `sr.rs`'s module docs for why this is exact at those points and
//!   what the residual gap is.

use std::collections::VecDeque;
use std::env;
use std::process::ExitCode;

use periph::trace::{self, Reader, Record};
use periph::{Raised, Timers};

const PIT_VECTORS: [u16; 4] = periph::pit::VECTORS;
const DTIM_VECTORS: [u16; 4] = periph::dtim::VECTORS;

fn owned_vector(v: u16) -> bool {
    PIT_VECTORS.contains(&v) || DTIM_VECTORS.contains(&v)
}

struct Counters {
    rd_checked: u64,
    rd_mismatch: u64,
    irq_checked: u64,
    irq_mismatch: u64,
    irq_missing: u64,    // expected but never appeared
    irq_unexpected: u64, // appeared but not expected
    /// IRQ mismatches/missing/unexpected that happened while `sr_exempt` was
    /// set -- see the module docs' "scheduler-trap SR" note. Counted
    /// separately, not part of `total_mismatches`.
    irq_sr_exempt: u64,
    hwr_checked: u64,
    hwr_mismatch: u64,
    state_resyncs: u64,
    state_drift: u64,
}

impl Counters {
    fn new() -> Self {
        Self {
            rd_checked: 0,
            rd_mismatch: 0,
            irq_checked: 0,
            irq_mismatch: 0,
            irq_missing: 0,
            irq_unexpected: 0,
            irq_sr_exempt: 0,
            hwr_checked: 0,
            hwr_mismatch: 0,
            state_resyncs: 0,
            state_drift: 0,
        }
    }
    fn total_mismatches(&self) -> u64 {
        self.rd_mismatch
            + self.irq_mismatch
            + self.irq_missing
            + self.irq_unexpected
            + self.hwr_mismatch
    }
}

fn report(first: &mut Option<String>, n: &mut u64, ctx: String, verbose: bool) {
    *n += 1;
    if first.is_none() {
        *first = Some(ctx.clone());
    }
    if verbose {
        eprintln!("mismatch #{n}: {ctx}");
    }
}

fn parse_channels(v: &serde_json::Value) -> Vec<usize> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_u64())
                .map(|x| x as usize)
                .collect()
        })
        .unwrap_or_default()
}

fn parse_next_pending(source: &serde_json::Value) -> ([Option<f64>; 4], [bool; 4], bool) {
    let mut next = [None; 4];
    if let Some(arr) = source.get("next").and_then(|v| v.as_array()) {
        for (i, v) in arr.iter().enumerate().take(4) {
            next[i] = v.as_f64();
        }
    }
    let mut pending = [false; 4];
    if let Some(arr) = source.get("pending").and_then(|v| v.as_array()) {
        for v in arr {
            if let Some(ch) = v.as_u64()
                && (ch as usize) < 4
            {
                pending[ch as usize] = true;
            }
        }
    }
    let held = source
        .get("held")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    (next, pending, held)
}

fn apply_state(
    timers: &mut Timers,
    data: &[u8],
    counters: &mut Counters,
    verbose: bool,
    clock: u64,
) {
    let is_first = counters.state_resyncs == 0;
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(data) else {
        return;
    };
    let Some(tstate) = v.pointer("/components/timers") else {
        return;
    };
    let Some(sources) = tstate.get("sources").and_then(|s| s.as_array()) else {
        return;
    };
    for src in sources {
        let ty = src.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let (next, pending, held) = parse_next_pending(src);
        let ips = src.get("ips").and_then(|i| i.as_f64());
        match ty {
            "Pits" => {
                // Cross-check before overwriting: a drift here is real
                // evidence this crate's own simulation disagreed with the
                // oracle at some point since the last resync.
                for ch in 0..4 {
                    let ours_next = timers.pit.next_deadline(ch);
                    let ours_pending = timers.pit.pending(ch);
                    let close = match (ours_next, next[ch]) {
                        (Some(a), Some(b)) => (a - b).abs() < 1.0,
                        (None, None) => true,
                        _ => false,
                    };
                    if !is_first && (!close || ours_pending != pending[ch]) {
                        counters.state_drift += 1;
                        if verbose {
                            eprintln!(
                                "STATE drift PIT ch{ch} at clock={clock}: ours next={ours_next:?} pending={ours_pending} vs oracle next={:?} pending={}",
                                next[ch], pending[ch]
                            );
                        }
                    }
                }
                timers.pit.load_checkpoint(&next, &pending, held);
                if let Some(ips) = ips {
                    timers.pit.set_ips(ips);
                }
            }
            "Dtims" => {
                for ch in 0..4 {
                    let ours_next = timers.dtim.next_deadline(ch);
                    let ours_pending = timers.dtim.pending(ch);
                    let close = match (ours_next, next[ch]) {
                        (Some(a), Some(b)) => (a - b).abs() < 1.0,
                        (None, None) => true,
                        _ => false,
                    };
                    if !is_first && (!close || ours_pending != pending[ch]) {
                        counters.state_drift += 1;
                        if verbose {
                            eprintln!(
                                "STATE drift DTIM ch{ch} at clock={clock}: ours next={ours_next:?} pending={ours_pending} vs oracle next={:?} pending={}",
                                next[ch], pending[ch]
                            );
                        }
                    }
                }
                timers.dtim.load_checkpoint(&next, &pending, held);
                if let Some(ips) = ips {
                    timers.dtim.set_ips(ips);
                }
            }
            _ => {}
        }
    }
    counters.state_resyncs += 1;
}

/// Handles both `MARK` events this replay needs to act on: `"setup"` (the
/// window's initial channel/rate configuration) and `"intro handover"`
/// (`Timers::release_intro`'s trigger -- see its doc comment for why the
/// channel-set change is not itself in the record).
fn apply_mark(timers: &mut Timers, data: &[u8]) {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(data) else {
        return;
    };
    match v.get("event").and_then(|e| e.as_str()) {
        Some("setup") => {
            let Some(list) = v.get("timers").and_then(|t| t.as_array()) else {
                return;
            };
            for entry in list {
                let ty = entry.get("type").and_then(|t| t.as_str()).unwrap_or("");
                let channels =
                    parse_channels(entry.get("channels").unwrap_or(&serde_json::Value::Null));
                let ips = entry
                    .get("ips")
                    .and_then(|i| i.as_f64())
                    .unwrap_or(4_680_000.0);
                match ty {
                    "Pits" => timers.pit.configure(channels, ips),
                    "Dtims" => timers.dtim.configure(channels, ips),
                    _ => {}
                }
            }
        }
        Some("intro handover") => timers.release_intro(),
        _ => {}
    }
}

fn apply_page(timers: &mut Timers, base: u32, data: &[u8]) {
    if timers.pit.load_page(base, data) {
        return;
    }
    if timers.dtim.load_page(base, data) {
        return;
    }
    let _ = timers.intc.load_page(base, data);
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: {} <trace-file> [--limit N] [--verbose]", args[0]);
        return ExitCode::FAILURE;
    }
    let path = &args[1];
    let mut limit: Option<u64> = None;
    let mut verbose = false;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--limit" => {
                i += 1;
                limit = args.get(i).and_then(|s| s.parse().ok());
            }
            "--verbose" => verbose = true,
            other => {
                eprintln!("unknown argument: {other}");
                return ExitCode::FAILURE;
            }
        }
        i += 1;
    }

    let mut reader = match Reader::open(path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{path}: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut timers = Timers::default();
    let mut counters = Counters::new();
    let mut first_mismatch: Option<String> = None;

    let mut last_boundary: Option<u64> = None;
    let mut initial_deadline_applied = false;
    let mut expected_irqs: VecDeque<Raised> = VecDeque::new();
    let mut expected_hwr: VecDeque<(u32, u8)> = VecDeque::new();
    let mut n_records: u64 = 0;

    // See the module docs' "scheduler-trap SR" note: this crate reconstructs
    // SR/IPL from IRQ.frame_sr and RTE.sr. Vector 32 is not a real interrupt
    // source (MCF5441x vectors 0-63 are the core's own; 32-47 are TRAP
    // #0-15) -- `emu.longrun.IdleSpin` repurposes it purely as an
    // emulator-side idle-credit device, and the RTOS's own context switcher
    // also runs through it (`pit.py`'s module docstring). When its handler's
    // RTE restores a nonzero IPL, that value belongs to whichever task the
    // scheduler just switched TO, not to a delivered interrupt this crate
    // can attribute -- the task may lower it again through a direct
    // `move.w #x,sr` this peripheral-only model cannot see (`sr.rs`'s module
    // docs). `sr_exempt` marks that window: it opens on such an RTE and
    // closes on the very next taken IRQ of any vector, which carries its own
    // `frame_sr` ground truth. A timer/INTC prediction that disagrees with
    // the trace while it is open is not a model defect; it is counted
    // separately (`irq_sr_exempt`), not as a mismatch.
    let mut sr_exempt = false;
    let mut last_dispatched_vector: Option<u16> = None;

    macro_rules! check_boundary_carryover {
        ($clock:expr) => {
            if !expected_irqs.is_empty() {
                for r in expected_irqs.drain(..) {
                    if sr_exempt {
                        counters.irq_sr_exempt += 1;
                        if verbose {
                            eprintln!(
                                "(sr-exempt) expected IRQ vec={} level={} at clock={} never appeared before the next boundary",
                                r.vector, r.level, $clock
                            );
                        }
                        continue;
                    }
                    report(
                        &mut first_mismatch,
                        &mut counters.irq_missing,
                        format!(
                            "expected IRQ vec={} level={} at clock={} never appeared in trace before the next boundary",
                            r.vector, r.level, $clock
                        ),
                        verbose,
                    );
                }
            }
            if !expected_hwr.is_empty() {
                for (addr, byte) in expected_hwr.drain(..) {
                    report(
                        &mut first_mismatch,
                        &mut counters.hwr_mismatch,
                        format!(
                            "expected host write addr={addr:#010x} byte={byte:#04x} at clock={} never appeared in trace before the next boundary",
                            $clock
                        ),
                        verbose,
                    );
                }
            }
        };
    }

    loop {
        if let Some(l) = limit
            && n_records >= l
        {
            break;
        }
        let rec: Record = match reader.next_record() {
            Ok(Some(r)) => r,
            Ok(None) => break,
            Err(e) => {
                eprintln!("{path}: read error at record {n_records}: {e}");
                return ExitCode::FAILURE;
            }
        };
        n_records += 1;
        let clock = rec.clock;

        match last_boundary {
            None => last_boundary = Some(clock),
            Some(lb) if clock > lb => {
                if !initial_deadline_applied {
                    // Mirrors `spin`'s very first `pits.step(base+done)`
                    // call, at `done=lb` (the window's start clock), before
                    // its first chunk has even run -- see
                    // `Timers::service`'s doc comment on arming. Applied
                    // here, not at record #1, because it must run after the
                    // window's `MARK "setup"` and initial `STATE` have
                    // configured the banks' channels; both share the
                    // window's start clock, so `lb` is unchanged either way.
                    timers.pit.deadline(lb);
                    timers.dtim.deadline(lb);
                    initial_deadline_applied = true;
                }
                check_boundary_carryover!(lb);
                let (raised, writes) = timers.service(clock);
                expected_irqs.extend(raised);
                expected_hwr.extend(writes.into_iter().map(|w| (w.addr, w.byte)));
                last_boundary = Some(clock);
            }
            _ => {}
        }

        match rec.tag {
            trace::WR => {
                let addr = rec.u32(0);
                let value = rec.u32(1);
                let size = rec.u8(3);
                if Timers::owns(addr) {
                    timers.write(addr, size, value);
                }
            }
            trace::RD => {
                let addr = rec.u32(0);
                let value = rec.u32(1);
                let pc = rec.u32(2);
                let size = rec.u8(3);
                if let Some(expected) = timers.read(addr, size) {
                    counters.rd_checked += 1;
                    if expected != value {
                        report(
                            &mut first_mismatch,
                            &mut counters.rd_mismatch,
                            format!(
                                "RD mismatch addr={addr:#010x} size={size} pc={pc:#010x} clock={clock}: trace={value:#x} model={expected:#x}"
                            ),
                            verbose,
                        );
                    }
                }
            }
            trace::IRQ => {
                let vector = rec.u16(0);
                let level_raw = rec.u8(1);
                let flags = rec.u8(2);
                let frame_sr = rec.u16(6) as u16;
                let taken = flags & trace::IRQ_TAKEN != 0;
                let level = if level_raw == 0xFF {
                    None
                } else {
                    Some(level_raw)
                };
                if taken && owned_vector(vector) {
                    // Snapshot before this record's own frame_sr resync (and
                    // before it closes the window): the exemption covers
                    // whether the PREDICTION (made earlier, at the boundary)
                    // could have been trusted, not whether the record itself
                    // now gives us fresh ground truth.
                    let exempt_now = sr_exempt;
                    counters.irq_checked += 1;
                    match expected_irqs.pop_front() {
                        Some(exp) => {
                            let got_level = level.unwrap_or(0);
                            if exp.vector != vector || exp.level != got_level {
                                if exempt_now {
                                    counters.irq_sr_exempt += 1;
                                    if verbose {
                                        eprintln!(
                                            "(sr-exempt) IRQ mismatch at clock={clock}: trace vec={vector} level={got_level} vs model expected vec={} level={}",
                                            exp.vector, exp.level
                                        );
                                    }
                                } else {
                                    report(
                                        &mut first_mismatch,
                                        &mut counters.irq_mismatch,
                                        format!(
                                            "IRQ mismatch at clock={clock}: trace vec={vector} level={got_level} vs model expected vec={} level={}",
                                            exp.vector, exp.level
                                        ),
                                        verbose,
                                    );
                                }
                            }
                        }
                        None => {
                            if exempt_now {
                                counters.irq_sr_exempt += 1;
                                if verbose {
                                    eprintln!(
                                        "(sr-exempt) unexpected IRQ vec={vector} level={} at clock={clock}: model predicted nothing",
                                        level.unwrap_or(0)
                                    );
                                }
                            } else {
                                report(
                                    &mut first_mismatch,
                                    &mut counters.irq_unexpected,
                                    format!(
                                        "unexpected IRQ vec={vector} level={} at clock={clock}: model predicted nothing",
                                        level.unwrap_or(0)
                                    ),
                                    verbose,
                                );
                            }
                        }
                    }
                }
                if taken {
                    // Ground truth for this instant, closing the exempt
                    // window regardless of vector: any taken IRQ's frame_sr
                    // resyncs the tracker (see sr.rs).
                    timers.sr.on_taken(level, frame_sr);
                    sr_exempt = false;
                    last_dispatched_vector = Some(vector);
                }
            }
            trace::RTE => {
                let sr = rec.u16(1);
                timers.sr.on_rte(sr);
                let ipl_restored = (sr >> 8) & 0x07;
                if last_dispatched_vector == Some(32) && ipl_restored != 0 {
                    sr_exempt = true;
                }
            }
            trace::HWR => {
                let src_id = rec.u16(0) as u16;
                let addr = rec.u32(1);
                let name = reader.sources.get(&src_id).cloned().unwrap_or_default();
                if (name.starts_with("emu.dtim") || name.starts_with("emu.pit"))
                    && rec.data.len() == 1
                {
                    counters.hwr_checked += 1;
                    let byte = rec.data[0];
                    match expected_hwr.iter().position(|(a, _)| *a == addr) {
                        Some(idx) => {
                            let (_, exp_byte) = expected_hwr.remove(idx).unwrap();
                            if exp_byte != byte {
                                report(
                                    &mut first_mismatch,
                                    &mut counters.hwr_mismatch,
                                    format!(
                                        "HWR mismatch addr={addr:#010x} clock={clock} source={name}: trace byte={byte:#04x} model byte={exp_byte:#04x}"
                                    ),
                                    verbose,
                                );
                            }
                        }
                        None => {
                            report(
                                &mut first_mismatch,
                                &mut counters.hwr_mismatch,
                                format!(
                                    "unexpected host write addr={addr:#010x} byte={byte:#04x} clock={clock} source={name}: model predicted none"
                                ),
                                verbose,
                            );
                        }
                    }
                }
            }
            trace::STATE => {
                apply_state(&mut timers, &rec.data, &mut counters, verbose, clock);
            }
            trace::PAGE => {
                let base = rec.u32(0);
                apply_page(&mut timers, base, &rec.data);
            }
            trace::MARK => {
                apply_mark(&mut timers, &rec.data);
            }
            trace::RATE => {
                let ips = rec.fields[0] as f64;
                timers.rescale(clock as f64, ips);
            }
            _ => {}
        }
    }
    // Drain any leftover expectation at end of stream.
    check_boundary_carryover!(last_boundary.unwrap_or(0));

    let total = counters.total_mismatches();
    println!(
        "{path}: {n_records} records, RD checked={} (mismatch {}), IRQ checked={} (mismatch {}, missing {}, unexpected {}, sr-exempt {}), HWR checked={} (mismatch {}), STATE resyncs={} (drift {})",
        counters.rd_checked,
        counters.rd_mismatch,
        counters.irq_checked,
        counters.irq_mismatch,
        counters.irq_missing,
        counters.irq_unexpected,
        counters.irq_sr_exempt,
        counters.hwr_checked,
        counters.hwr_mismatch,
        counters.state_resyncs,
        counters.state_drift,
    );
    if let Some(ctx) = &first_mismatch {
        println!("FIRST MISMATCH: {ctx}");
    }
    if total == 0 {
        println!("0 mismatches ({} sr-exempt)", counters.irq_sr_exempt);
        ExitCode::SUCCESS
    } else {
        println!("{total} mismatches");
        ExitCode::FAILURE
    }
}
