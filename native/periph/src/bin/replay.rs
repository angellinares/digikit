//! Replay a `DT2MMIO` trace (`emu/mmiotrace.py`) against this crate's
//! timer/INTC models and report the first mismatch, with context.
//!
//! Usage: `mmio-replay <trace-file> [--limit N] [--verbose] [--gpio-gate-only N]`
//!
//! `--gpio-gate-only N` is an explicit focused verdict: it requires exactly
//! N gate writes, N gate reads, and N GPIO read-hook writes, plus a clean END
//! record. It reports (but deliberately does not fail for) unrelated replay
//! mismatches such as the known vector-207 timer discrepancy. Without that
//! flag the normal full-trace verdict remains unchanged.
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
//!
//! ## DMA + SSI + DSPI lane (P4 stage 2b, `spilink::DmaLink`)
//!
//! Extends the same method to `EdmaBank`/`Dspi2Link`/`Fifo` (`spilink::
//! DmaLink`, see its module docs for the wiring and the SERQ multi-model
//! dispatch). Two things this lane needs that the timers lane did not:
//!
//! * **TX capture: HRD as the memory substitute.** A `TxChannel`/`Dspi2Link`
//!   TX capture needs source bytes from outside this crate's own register
//!   file (arbitrary guest DDR); a replay cannot read them any other way,
//!   so it takes them from the trace's own next `HRD` record instead --
//!   `DmaLink::write` returns a [`periph::spilink::SerqEffect`] saying one is
//!   owed, and this loop's `pending_capture` completes it from the very
//!   next record read (always that `HRD`, since the recorder's own writer
//!   emits it synchronously, with nothing interleaved -- see `dspi.rs`'s
//!   module docs). This is a genuinely different replay technique from the
//!   timers lane's boundary prediction: it replays the trace's own recorded
//!   *input*, then checks this crate's *transformation* of it (PUSHR-tag
//!   stripping, TCD write-back), not an independent re-derivation of memory
//!   this crate cannot see.
//! * **PC-triggered completions are not predicted.** Channel 35's
//!   completion vector (155) is delivered from a Python code hook at a
//!   fixed firmware PC (`emu/edma.py`'s `wait_loop`), not from a clock
//!   deadline a peripheral-only replay can compute -- unlike PIT/DTIM, an
//!   instruction-count boundary for it does not exist here to predict. This
//!   replay tracks `TxChannel::pending`/`consume_pending` and checks
//!   *ordering* (every vector-155 `IRQ` consumes a completion this crate's
//!   own `run()` tracking already queued) rather than the exact boundary,
//!   and reports it separately (`edma155`), not as a mismatch -- the same
//!   distinction `bin/replay.rs`'s `sr-exempt` window already draws between
//!   "wrong" and "outside what this model can predict".
//!
//! DSPI2's RX delivery (`_deliver`, multi-byte) and the FlexBus DSP FIFO's
//! read echo (`on_read`, always `READY` at `poll_delay=0` -- see `dsp.rs`)
//! are genuine `HWR` checks; SSI0 is not wired in here at all (see `ssi.rs`'s
//! module docs -- no trace ever installs it).

use std::collections::VecDeque;
use std::env;
use std::process::ExitCode;

use periph::esdhc::{CardPort, Esdhc, RegisterPolicy};
use periph::gpio::{PPDSDR_C, SdGate};
use periph::spilink::{DmaLink, SerqEffect};
use periph::trace::{self, Reader, Record};
use periph::{Raised, Timers, dsp};

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
    /// DmaLink (eDMA/DSPI2/DSP FIFO) `RD` checks -- see the module docs.
    dma_rd_checked: u64,
    dma_rd_mismatch: u64,
    /// DSPI2 `_deliver` and DSP FIFO `on_read` `HWR` checks.
    dma_hwr_checked: u64,
    dma_hwr_mismatch: u64,
    /// A capture's expected follow-up `HRD` (see the module docs,
    /// "TX capture") was not the next record, or its length did not cover
    /// what the TCD asked for.
    dma_capture_mismatch: u64,
    /// Vector-155 (channel 35, UART8 TX) ordering only -- see the module
    /// docs, "PC-triggered completions are not predicted". Not a mismatch.
    edma155_seen: u64,
    edma155_unordered: u64,
    /// DSPI2 TX frames captured, and how many carried a nonzero TRIG mask
    /// at frame offset 0x22..0x24 (`docs/findings`'s "the DSPI2 TX frame
    /// carrying the TRIG mask" -- the gate's byte-exactness check, done
    /// generically: a window presses TRIG at most once, so this should be
    /// 0 or 1, not hardcoded to one trace's exact clock).
    dspi2_frames: u64,
    dspi2_trig_frames: u64,
    /// SD continuity-gate guest writes, reads, and read-hook writes.
    gpio_wr_checked: u64,
    gpio_rd_checked: u64,
    gpio_rd_mismatch: u64,
    gpio_hwr_checked: u64,
    gpio_hwr_mismatch: u64,
    /// eSDHC accesses evaluated by the explicit early-only gate.
    esdhc_events: u64,
    esdhc_rd_checked: u64,
    esdhc_rd_mismatch: u64,
    /// eSDHC has no host-memory writes in this early register-only scope.
    esdhc_hwr_checked: u64,
    esdhc_unmodeled: u64,
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
            dma_rd_checked: 0,
            dma_rd_mismatch: 0,
            dma_hwr_checked: 0,
            dma_hwr_mismatch: 0,
            dma_capture_mismatch: 0,
            edma155_seen: 0,
            edma155_unordered: 0,
            dspi2_frames: 0,
            dspi2_trig_frames: 0,
            gpio_wr_checked: 0,
            gpio_rd_checked: 0,
            gpio_rd_mismatch: 0,
            gpio_hwr_checked: 0,
            gpio_hwr_mismatch: 0,
            esdhc_events: 0,
            esdhc_rd_checked: 0,
            esdhc_rd_mismatch: 0,
            esdhc_hwr_checked: 0,
            esdhc_unmodeled: 0,
        }
    }
    fn total_mismatches(&self) -> u64 {
        self.rd_mismatch
            + self.irq_mismatch
            + self.irq_missing
            + self.irq_unexpected
            + self.hwr_mismatch
            + self.dma_rd_mismatch
            + self.dma_hwr_mismatch
            + self.dma_capture_mismatch
            + self.gpio_rd_mismatch
            + self.gpio_hwr_mismatch
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EndStatus {
    Missing,
    Invalid(String),
    Complete {
        errors: u64,
        first_error: Option<String>,
    },
}

fn end_status(data: &[u8]) -> EndStatus {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(data) else {
        return EndStatus::Invalid("END is not valid JSON".to_string());
    };
    let Some(errors) = value.get("errors").and_then(serde_json::Value::as_u64) else {
        return EndStatus::Invalid("END has no integer errors field".to_string());
    };
    let first_error = value
        .get("first_error")
        .and_then(|v| (!v.is_null()).then(|| v.to_string()));
    EndStatus::Complete {
        errors,
        first_error,
    }
}

fn end_failure(end: &EndStatus) -> Option<String> {
    match end {
        EndStatus::Missing => Some("missing END record".to_string()),
        EndStatus::Invalid(reason) => Some(format!("invalid END record: {reason}")),
        EndStatus::Complete {
            errors,
            first_error,
        } if *errors > 0 => Some(format!(
            "recorder END errors={errors}, first_error={}",
            first_error.as_deref().unwrap_or("null")
        )),
        EndStatus::Complete { .. } => None,
    }
}

fn esdhc_gate_failure(counters: &Counters, expected: u64, end: &EndStatus) -> Option<String> {
    if expected == 0 {
        return Some("eSDHC gate expected count must be positive".to_string());
    }
    if let Some(reason) = end_failure(end) {
        return Some(reason);
    }
    if counters.esdhc_events != expected {
        return Some(format!(
            "eSDHC gate event count mismatch: expected {expected}, got {}",
            counters.esdhc_events
        ));
    }
    if counters.esdhc_rd_checked == 0 {
        return Some("eSDHC gate saw no read values to check".to_string());
    }
    if counters.esdhc_rd_mismatch != 0 || counters.esdhc_unmodeled != 0 {
        return Some(format!(
            "eSDHC gate failures: RD mismatches={}, unmodeled operations={}",
            counters.esdhc_rd_mismatch, counters.esdhc_unmodeled
        ));
    }
    None
}

/// Replay-only generic eMMC port matching `emu.esdhc.Card`'s public early
/// command contract. It contains no firmware data or storage image.
#[derive(Default)]
struct EarlyCard {
    rca: u16,
}
impl CardPort for EarlyCard {
    fn command(&mut self, idx: u8, arg: u32) -> [u32; 4] {
        match idx {
            0 => [0; 4],
            1 => [0xC0FF_8080, 0, 0, 0],
            2 | 10 => [0, 0x4530_0000, 0x3030_3447, 0x0011_0000],
            9 => [0, 0xAFC0_0380, 0x0000_0A03, 0],
            3 => {
                self.rca = (arg >> 16) as u16;
                [0x900, 0, 0, 0]
            }
            _ => [0x900, 0, 0, 0],
        }
    }
    fn read_word(&mut self, idx: u8, pattern: u32) -> u32 {
        if idx == 14 { !pattern } else { 0 }
    }
}

fn gpio_gate_failure(counters: &Counters, expected: u64, end: &EndStatus) -> Option<String> {
    if expected == 0 {
        return Some("GPIO gate expected count must be positive".to_string());
    }
    if let Some(reason) = end_failure(end) {
        return Some(reason);
    }
    if counters.gpio_wr_checked != expected
        || counters.gpio_rd_checked != expected
        || counters.gpio_hwr_checked != expected
    {
        return Some(format!(
            "GPIO gate count mismatch: expected WR/RD/HWR={expected}/{expected}/{expected}, got {}/{}/{}",
            counters.gpio_wr_checked, counters.gpio_rd_checked, counters.gpio_hwr_checked
        ));
    }
    if counters.gpio_rd_mismatch != 0 || counters.gpio_hwr_mismatch != 0 {
        return Some(format!(
            "GPIO gate mismatches: RD={}, HWR={}",
            counters.gpio_rd_mismatch, counters.gpio_hwr_mismatch
        ));
    }
    None
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
    dma: &mut DmaLink,
    gpio: &mut SdGate,
    data: &[u8],
    counters: &mut Counters,
    verbose: bool,
    clock: u64,
) {
    let is_first = counters.state_resyncs == 0;
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(data) else {
        return;
    };
    // Every `STATE` record carries the live forced-MMIO table
    // (`emu.harness.Machine.mmio`, `emu/mmiotrace.py`'s `machine_state`) --
    // see `spilink.rs`'s module docs. Applied on every resync, not just the
    // first, since a run can change it after restore (`emu/longrun.py`'s
    // `dspi2_peer` re-apply, see its own comment).
    if let Some(mmio) = v.get("mmio").and_then(|m| m.as_object()) {
        for (k, val) in mmio {
            if let (Some(addr), Some(value)) = (
                k.strip_prefix("0x")
                    .and_then(|h| u32::from_str_radix(h, 16).ok()),
                val.as_u64(),
            ) {
                dma.set_forced(addr, value as u32);
            }
        }
    }
    if let Some(driven) = v
        .pointer("/models/sdgate/driven")
        .and_then(serde_json::Value::as_u64)
    {
        gpio.load_state(driven != 0);
    }
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

fn apply_page(
    timers: &mut Timers,
    dma: &mut DmaLink,
    gpio: &mut SdGate,
    esdhc: &mut Esdhc<EarlyCard>,
    base: u32,
    data: &[u8],
) {
    if timers.pit.load_page(base, data) {
        return;
    }
    if timers.dtim.load_page(base, data) {
        return;
    }
    if timers.intc.load_page(base, data) {
        return;
    }
    if gpio.load_page(base, data) {
        return;
    }
    if esdhc.load_page(base, data) {
        return;
    }
    let _ = dma.load_page(base, data);
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!(
            "usage: {} <trace-file> [--limit N] [--verbose] [--gpio-gate-only N] [--esdhc-early-only N]",
            args[0]
        );
        return ExitCode::FAILURE;
    }
    let path = &args[1];
    let mut limit: Option<u64> = None;
    let mut verbose = false;
    let mut gpio_gate_only: Option<u64> = None;
    let mut esdhc_early_only: Option<u64> = None;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--limit" => {
                i += 1;
                limit = args.get(i).and_then(|s| s.parse().ok());
            }
            "--verbose" => verbose = true,
            "--gpio-gate-only" => {
                i += 1;
                gpio_gate_only = args.get(i).and_then(|s| s.parse().ok());
                if gpio_gate_only.is_none_or(|n| n == 0) {
                    eprintln!("--gpio-gate-only requires a positive count");
                    return ExitCode::FAILURE;
                }
            }
            "--esdhc-early-only" => {
                i += 1;
                esdhc_early_only = args.get(i).and_then(|s| s.parse().ok());
                if esdhc_early_only.is_none_or(|n| n == 0) {
                    eprintln!("--esdhc-early-only requires a positive count");
                    return ExitCode::FAILURE;
                }
            }
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
    let mut dma = DmaLink::default();
    let mut gpio = SdGate::default();
    let mut esdhc = Esdhc::with_policy(EarlyCard::default(), RegisterPolicy::Oracle);
    let mut counters = Counters::new();
    let mut first_mismatch: Option<String> = None;

    let mut last_boundary: Option<u64> = None;
    let mut initial_deadline_applied = false;
    let mut expected_irqs: VecDeque<Raised> = VecDeque::new();
    let mut expected_hwr: VecDeque<(u32, u8)> = VecDeque::new();
    // DSPI2's RX `_deliver` (multi-byte, unlike PIT/DTIM's single-byte
    // `expected_hwr`) -- see the module docs.
    let mut expected_dma_hwr: VecDeque<(u32, Vec<u8>)> = VecDeque::new();
    // A DMA capture this replay owes the DmaLink from the trace's very next
    // record (see the module docs, "TX capture: HRD as the memory
    // substitute").
    let mut pending_capture: Option<SerqEffect> = None;
    let mut n_records: u64 = 0;
    let mut end = EndStatus::Missing;
    // HWR hooks run before the guest WR record that triggers a command, so
    // compare these after the stream against the controller's ordered effects.
    let mut esdhc_hwr: Vec<(u32, Vec<u8>)> = Vec::new();

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
            trace::HRD if pending_capture.is_some() => {
                // A capture owed by an earlier WR (see the module docs, "TX
                // capture: HRD as the memory substitute"). Python's own
                // field-accessor helpers (`_u16`/`_u32`/`_s16` for TCD
                // fields this crate already reads from its own register
                // file) are recorded as their OWN, separate, small `HRD`
                // records ahead of the one big merged block read this
                // replay actually needs -- so every `HRD` seen while a
                // capture is owed is checked by source name, not blindly
                // taken as the next record; a non-matching one (or one with
                // no still-known source, e.g. the DSP FIFO's echo has none
                // to read at all) is simply not this replay's concern and
                // falls through with no effect, same as any other `HRD`.
                let src_id = rec.u16(0);
                let name = reader
                    .sources
                    .get(&src_id)
                    .map(String::as_str)
                    .unwrap_or("");
                let target = match pending_capture.unwrap() {
                    SerqEffect::Tx35Capture => name.ends_with(".run"),
                    SerqEffect::Dspi2Capture => name.ends_with("._capture"),
                    SerqEffect::None => false,
                };
                if target {
                    match pending_capture.take().unwrap() {
                        SerqEffect::Tx35Capture => dma.finish_tx35_capture(&rec.data),
                        SerqEffect::Dspi2Capture => {
                            let frame = dma.finish_dspi2_capture(&rec.data);
                            counters.dspi2_frames += 1;
                            // The gate's byte-exact TRIG-mask check: offset
                            // 0x22..0x24 of the logical TX frame (`docs/
                            // findings`'s "The ColdFire tells the SHARC
                            // through a periodic DSPI2 frame"). A window
                            // presses TRIG at most once, so this should end
                            // at 0 or 1, not a hardcoded clock.
                            if frame.len() >= 0x24 && frame[0x22..0x24] != [0, 0] {
                                counters.dspi2_trig_frames += 1;
                                if verbose {
                                    eprintln!(
                                        "DSPI2 TX frame #{} at clock={clock}: TRIG mask={:02x}{:02x}",
                                        counters.dspi2_frames, frame[0x22], frame[0x23]
                                    );
                                }
                            }
                        }
                        SerqEffect::None => {}
                    }
                }
            }
            trace::WR => {
                let addr = rec.u32(0);
                let value = rec.u32(1);
                let size = rec.u8(3);
                if Timers::owns(addr) {
                    timers.write(addr, size, value);
                } else if Esdhc::<EarlyCard>::owns(addr) {
                    counters.esdhc_events += 1;
                    if !esdhc.write(addr, size, value) {
                        counters.esdhc_unmodeled += 1;
                    }
                } else if SdGate::owns(addr) {
                    gpio.write(addr, size, value);
                    if matches!(addr, periph::gpio::PPDSDR_D | periph::gpio::PCLRR_D) {
                        counters.gpio_wr_checked += 1;
                    }
                } else if DmaLink::owns(addr) {
                    let (_, effect, hw) = dma.write(addr, size, value);
                    if effect != SerqEffect::None {
                        pending_capture = Some(effect);
                    }
                    if let Some((a, data)) = hw {
                        expected_dma_hwr.push_back((a, data));
                    }
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
                } else if Esdhc::<EarlyCard>::owns(addr) {
                    counters.esdhc_events += 1;
                    match esdhc.read(addr, size) {
                        Some(expected) => {
                            counters.esdhc_rd_checked += 1;
                            if expected != value {
                                counters.esdhc_rd_mismatch += 1;
                                if verbose {
                                    eprintln!(
                                        "eSDHC RD mismatch addr={addr:#010x} size={size} pc={pc:#010x} clock={clock}: trace={value:#x} model={expected:#x}"
                                    );
                                }
                            }
                        }
                        None => counters.esdhc_unmodeled += 1,
                    }
                } else if let Some(expected) = gpio.read(addr, size) {
                    counters.gpio_rd_checked += 1;
                    if expected != value {
                        report(
                            &mut first_mismatch,
                            &mut counters.gpio_rd_mismatch,
                            format!(
                                "GPIO RD mismatch addr={addr:#010x} size={size} pc={pc:#010x} clock={clock}: trace={value:#x} model={expected:#x}"
                            ),
                            verbose,
                        );
                    }
                } else if let Some(expected) = dma.read(addr, size) {
                    counters.dma_rd_checked += 1;
                    if expected != value {
                        report(
                            &mut first_mismatch,
                            &mut counters.dma_rd_mismatch,
                            format!(
                                "DMA RD mismatch addr={addr:#010x} size={size} pc={pc:#010x} clock={clock}: trace={value:#x} model={expected:#x}"
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
                    if vector == 155 {
                        // Ordering only, not boundary prediction -- see the
                        // module docs, "PC-triggered completions".
                        counters.edma155_seen += 1;
                        if !dma.tx35.consume_pending() {
                            counters.edma155_unordered += 1;
                        }
                    }
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
                if Esdhc::<EarlyCard>::owns(addr) {
                    esdhc_hwr.push((addr, rec.data.clone()));
                } else if name.starts_with("emu.gpio.SdGate")
                    && addr == PPDSDR_C
                    && rec.data.len() == 1
                {
                    counters.gpio_hwr_checked += 1;
                    let expected = gpio.sense();
                    if rec.data[0] != expected {
                        report(
                            &mut first_mismatch,
                            &mut counters.gpio_hwr_mismatch,
                            format!(
                                "GPIO HWR mismatch addr={addr:#010x} clock={clock} source={name}: trace byte={:#04x} model={expected:#04x}",
                                rec.data[0]
                            ),
                            verbose,
                        );
                    }
                } else if (name.starts_with("emu.dtim") || name.starts_with("emu.pit"))
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
                } else if name.starts_with("emu.dspi2.Dspi2Link._deliver") {
                    // DSPI2 RX: queued by `write`'s SERQ handling (RX arms
                    // second -- see `dspi.rs`'s module docs), so this is
                    // always the very next such record; checked against it
                    // directly rather than through `expected_dma_hwr`'s
                    // queue-by-address search (there is at most one entry).
                    counters.dma_hwr_checked += 1;
                    match expected_dma_hwr.pop_front() {
                        Some((a, d)) if a == addr && d == rec.data => {}
                        Some((a, d)) => report(
                            &mut first_mismatch,
                            &mut counters.dma_hwr_mismatch,
                            format!(
                                "DSPI2 deliver mismatch clock={clock}: trace addr={addr:#010x} len={} vs model addr={a:#010x} len={}",
                                rec.data.len(),
                                d.len()
                            ),
                            verbose,
                        ),
                        None => report(
                            &mut first_mismatch,
                            &mut counters.dma_hwr_mismatch,
                            format!(
                                "unexpected DSPI2 deliver addr={addr:#010x} len={} clock={clock}: model predicted none",
                                rec.data.len()
                            ),
                            verbose,
                        ),
                    }
                } else if name.starts_with("emu.dsp.Fifo") {
                    // The FlexBus DSP FIFO's read echo: at this crate's
                    // (and the oracle's) `poll_delay=0`, always exactly
                    // READY (`dsp.rs`'s module docs) -- checked as a
                    // constant rather than through a same-order queue,
                    // because in Python this write happens INSIDE the read
                    // hook, before the matching `RD` record even exists (the
                    // opposite order from every other HWR this replay
                    // checks), so there is nothing yet to queue against.
                    counters.dma_hwr_checked += 1;
                    let expected = dsp::READY.to_be_bytes();
                    if rec.data != expected {
                        report(
                            &mut first_mismatch,
                            &mut counters.dma_hwr_mismatch,
                            format!(
                                "DSP FIFO echo mismatch clock={clock}: trace={:02x?} expected={:02x?}",
                                rec.data, expected
                            ),
                            verbose,
                        );
                    }
                } else {
                    // Any other host write into one of this link's slots
                    // (e.g. `emu.panelin.feed` advancing TCD34's DADDR --
                    // owned by a different P4 lane, see `spilink.rs`'s
                    // `mirror_write` docs) still has to land in the register
                    // bytes a later `RD` checks against, even though this
                    // replay does not model its source.
                    dma.mirror_write(addr, &rec.data);
                }
            }
            trace::STATE => {
                apply_state(
                    &mut timers,
                    &mut dma,
                    &mut gpio,
                    &rec.data,
                    &mut counters,
                    verbose,
                    clock,
                );
            }
            trace::PAGE => {
                let base = rec.u32(0);
                apply_page(
                    &mut timers,
                    &mut dma,
                    &mut gpio,
                    &mut esdhc,
                    base,
                    &rec.data,
                );
            }
            trace::MARK => {
                apply_mark(&mut timers, &rec.data);
            }
            trace::RATE => {
                let ips = rec.fields[0] as f64;
                timers.rescale(clock as f64, ips);
            }
            trace::END => {
                end = end_status(&rec.data);
            }
            _ => {}
        }
    }
    // HWR hooks precede their triggering guest write in this trace format;
    // compare their real byte values once all command effects are available.
    for (addr, data) in esdhc_hwr {
        if data.len() % 4 != 0 {
            counters.esdhc_unmodeled += 1;
            continue;
        }
        for (word, got) in data.chunks_exact(4).enumerate() {
            counters.esdhc_hwr_checked += 1;
            match esdhc.take_host_write() {
                Some((expected_addr, expected))
                    if expected_addr == addr + (word as u32 * 4) && expected == got => {}
                _ => counters.esdhc_unmodeled += 1,
            }
        }
    }
    while esdhc.take_host_write().is_some() {
        counters.esdhc_unmodeled += 1;
    }

    // Drain any leftover expectation at end of stream.
    check_boundary_carryover!(last_boundary.unwrap_or(0));
    if let Some(effect) = pending_capture {
        report(
            &mut first_mismatch,
            &mut counters.dma_capture_mismatch,
            format!("a DMA capture ({effect:?}) was never completed by end of stream"),
            verbose,
        );
    }
    for (addr, data) in expected_dma_hwr.drain(..) {
        report(
            &mut first_mismatch,
            &mut counters.dma_hwr_mismatch,
            format!(
                "expected DSPI2 deliver addr={addr:#010x} len={} never appeared before end of stream",
                data.len()
            ),
            verbose,
        );
    }

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
    println!(
        "  GPIO gate WR checked={} RD checked={} (mismatch {}), HWR checked={} (mismatch {})",
        counters.gpio_wr_checked,
        counters.gpio_rd_checked,
        counters.gpio_rd_mismatch,
        counters.gpio_hwr_checked,
        counters.gpio_hwr_mismatch,
    );
    println!(
        "  eSDHC early events={} RD checked={} (mismatch {}), HWR checked={} (register-only prediction), unmodeled operations={}",
        counters.esdhc_events,
        counters.esdhc_rd_checked,
        counters.esdhc_rd_mismatch,
        counters.esdhc_hwr_checked,
        counters.esdhc_unmodeled,
    );
    println!(
        "  DMA RD checked={} (mismatch {}), DMA HWR checked={} (mismatch {}), capture mismatches={}, edma vec155 seen={} (unordered {}), DSPI2 TX frames={} (trig-mask {})",
        counters.dma_rd_checked,
        counters.dma_rd_mismatch,
        counters.dma_hwr_checked,
        counters.dma_hwr_mismatch,
        counters.dma_capture_mismatch,
        counters.edma155_seen,
        counters.edma155_unordered,
        counters.dspi2_frames,
        counters.dspi2_trig_frames,
    );
    if let Some(ctx) = &first_mismatch {
        println!("FIRST MISMATCH: {ctx}");
    }
    if let Some(expected) = esdhc_early_only {
        println!(
            "eSDHC EARLY-ONLY verdict (explicit): expects exactly {expected} eSDHC RD/WR events; unrelated full-replay mismatches remain reported but do not decide this mode"
        );
        if let Some(reason) = esdhc_gate_failure(&counters, expected, &end) {
            println!("eSDHC EARLY-ONLY FAIL: {reason}");
            ExitCode::FAILURE
        } else {
            println!(
                "eSDHC EARLY-ONLY PASS: clean END, matching read values, and no unmodeled eSDHC operations; ignored unrelated full-replay mismatches={total}"
            );
            ExitCode::SUCCESS
        }
    } else if let Some(expected) = gpio_gate_only {
        println!(
            "GPIO GATE-ONLY verdict (explicit): expects WR/RD/HWR={expected}/{expected}/{expected}; unrelated full-replay mismatches remain reported but do not decide this mode"
        );
        if let Some(reason) = gpio_gate_failure(&counters, expected, &end) {
            println!("GPIO GATE-ONLY FAIL: {reason}");
            ExitCode::FAILURE
        } else {
            println!(
                "GPIO GATE-ONLY PASS: clean END and matching GPIO counts/mismatches; ignored unrelated full-replay mismatches={total}"
            );
            ExitCode::SUCCESS
        }
    } else if let Some(reason) = end_failure(&end) {
        println!("END VALIDATION FAIL: {reason}");
        ExitCode::FAILURE
    } else if total == 0 {
        println!("0 mismatches ({} sr-exempt)", counters.irq_sr_exempt);
        ExitCode::SUCCESS
    } else {
        println!("{total} mismatches");
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod verdict_tests {
    use super::*;

    fn clean_end() -> EndStatus {
        end_status(br#"{"errors": 0, "first_error": null}"#)
    }

    #[test]
    fn gpio_gate_only_rejects_absent_counts() {
        let counters = Counters::new();
        assert!(
            gpio_gate_failure(&counters, 20, &clean_end())
                .unwrap()
                .contains("count mismatch")
        );
        assert!(gpio_gate_failure(&counters, 0, &clean_end()).is_some());
    }

    #[test]
    fn gpio_gate_only_rejects_missing_end() {
        let mut counters = Counters::new();
        counters.gpio_wr_checked = 20;
        counters.gpio_rd_checked = 20;
        counters.gpio_hwr_checked = 20;
        assert_eq!(
            gpio_gate_failure(&counters, 20, &EndStatus::Missing),
            Some("missing END record".to_string())
        );
    }

    #[test]
    fn end_validation_rejects_recorder_errors() {
        let end = end_status(br#"{"errors": 1, "first_error": "dropped event"}"#);
        assert_eq!(
            end_failure(&end),
            Some("recorder END errors=1, first_error=\"dropped event\"".to_string())
        );
    }
}
