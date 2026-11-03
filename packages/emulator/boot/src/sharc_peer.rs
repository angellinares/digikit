//! Opt-in live coupling of the ColdFire DSPI2 link to the native SHARC+
//! engine (cargo feature `sharc`).
//!
//! One ColdFire DSPI2 frame arrives every 88,000 ColdFire instructions
//! (1.5 kHz, one 32-sample 48 kHz audio block). For each frame the peer
//! 1. delivers it through the engine's SPI2 DMA model and takes the reply,
//! 2. runs the DSP for `period` instructions (the DSP clock / 1.5 kHz,
//!    666,667 at an assumed 1 GHz), noting how many instructions ran before
//!    the RTOS idle loop was reached,
//! 3. completes one SPORT4 audio block and appends its 32 stereo samples to
//!    a shared PCM buffer.
//!
//! The engine is generic over [`DspEngine`] so the plumbing is testable
//! without firmware.

use std::{
    cell::RefCell,
    collections::BTreeMap,
    fs::File,
    io::{BufWriter, Write},
    rc::Rc,
    sync::{mpsc, Arc, Mutex},
    thread,
    time::Instant,
};

use crate::ddr_monitor::{DdrMonitorCommand, DdrMonitorStatus};
use periph::dspi::Peer;

/// DN2 RTOS idle loop, short-word PCs (inclusive start, exclusive end).
pub const DN2_IDLE_RANGE: (u32, u32) = (0xb8_8a49, 0xb8_8abb);
/// Head of the idle task's spin loop (a linked call to `0xb88a49`).
pub const DN2_IDLE_HEAD: u32 = 0xb8_8aab;
/// DT2 1.16 RTOS idle loop: the same code as DN2's (function hashes match
/// `0xb88a49` -> `0xb8636d`, `0xb88aa6` -> `0xb863ca`), moved by -0x26dc.
pub const DT2_IDLE_RANGE: (u32, u32) = (0xb8_636d, 0xb8_63df);
/// Head of the DT2 idle task's spin loop (a linked call to `0xb8636d`).
pub const DT2_IDLE_HEAD: u32 = 0xb8_63cf;

/// Where a firmware's RTOS idle task spins (from the device profile).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IdleLoop {
    /// Short-word PCs, inclusive start, exclusive end.
    pub range: (u32, u32),
    /// First instruction of the spin loop.
    pub head: u32,
}

pub const DN2_IDLE: IdleLoop = IdleLoop {
    range: DN2_IDLE_RANGE,
    head: DN2_IDLE_HEAD,
};
pub const DT2_IDLE: IdleLoop = IdleLoop {
    range: DT2_IDLE_RANGE,
    head: DT2_IDLE_HEAD,
};

/// 1 GHz DSP clock / 1.5 kHz frame rate.
pub const DEFAULT_PERIOD: u32 = 666_667;
/// DSP instructions per DSPI2 frame: `DIGI_DSP_PERIOD` (an experiment
/// switch for the DSP clock assumption; 666,667 = 1 GHz at 1.5 kHz), else
/// [`DEFAULT_PERIOD`].
pub fn period_from_env() -> u32 {
    std::env::var("DIGI_DSP_PERIOD")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|&p| p >= 1000)
        .unwrap_or(DEFAULT_PERIOD)
}
const CHUNK: u32 = 1024;
/// Words in one SPORT4A block: 32 stereo samples, L even / R odd.
pub const BLOCK_WORDS: usize = 64;

pub fn fnv1a(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ b as u64).wrapping_mul(0x100_0000_01b3)
    })
}

/// What the peer needs from a DSP engine.
pub trait DspEngine {
    fn spi2_exchange(&mut self, frame: &[u8]) -> Result<Vec<u8>, String>;
    /// Whether link port 0's receive DMA is waiting for a work unit.
    fn lp0_ready(&self) -> bool {
        false
    }
    /// Deliver one link port 0 work unit (the sample-load path of findings 15).
    fn lp0_transfer(&mut self, _bytes: &[u8]) -> Result<(), String> {
        Err("link port 0 is not modelled by this engine".into())
    }
    /// Run up to `n` instructions; fewer than `n` means the engine stopped.
    fn step(&mut self, n: u32) -> u32;
    /// Like `step(n)`, but also end at the first instruction boundary whose
    /// PC lies in `range` (start inclusive, end exclusive). None: not
    /// supported (the peer then steps in chunks). Results never depend on
    /// step sizes, so this only saves the chunk boundaries' cost.
    fn step_until_in(&mut self, _n: u32, _range: (u32, u32)) -> Option<u32> {
        None
    }
    /// `step_until_in` that stops only at a boundary whose instruction count
    /// is a positive multiple of `chunk` past `base_icount`, which is an
    /// `icount()` value. None: not supported (the peer steps in chunks).
    fn step_until_in_aligned(
        &mut self,
        _n: u32,
        _range: (u32, u32),
        _chunk: u32,
        _base_icount: u64,
    ) -> Option<u32> {
        None
    }
    /// One SPORT4 block (zero input); `Ok(None)` while the SPORTs are off.
    fn sport_block(&mut self) -> Result<Option<Vec<u8>>, String>;
    /// Short-word PC.
    fn pc(&self) -> u32;
    /// Halt reason, once stopped.
    fn halt_reason(&self) -> Option<String>;
    /// Canonical state blob (hosts that snapshot); empty when unsupported.
    fn export(&self) -> Vec<u8> {
        Vec::new()
    }
    /// Executed-instruction counter (hosts that snapshot).
    fn icount(&self) -> u64 {
        0
    }
    /// Instruction counts by execution tier, where the engine keeps them.
    fn tier_counters(&self) -> Option<sharc_native::TierCounters> {
        None
    }
    /// Aggregate model-gate counters, copied into ordinary worker reports.
    fn model_stats(&self) -> Option<sharc_native::ModelStats> {
        None
    }
    /// Overlay a DSP patch onto the running engine's memory between frames.
    fn apply_dsp_patch(&mut self, _patch: &crate::dsp_patch::DspPatch) -> Result<(), String> {
        Err("this DSP engine cannot be patched".into())
    }
    /// Enable or clear opt-in per-entry dispatch profiling.
    fn set_region_profile(&mut self, _enabled: bool) {}
    /// Cumulative opt-in per-entry dispatch profile.
    fn region_profile(&self) -> Option<sharc_native::RegionProfile> {
        None
    }
    /// Enable the existing exact execution maps. The profile is frequency
    /// data only; it adds no per-instruction timer reads.
    fn set_execution_profile(&mut self, _enabled: bool) -> bool {
        false
    }
    /// Exact execution maps, when enabled.
    fn execution_profile(&self) -> Option<ExecutionProfile> {
        None
    }
    /// Memory bytes from `addr` (`None`: unknown to the engine), read through
    /// the aligned words of `peek_word`.
    fn peek_bytes(&self, addr: u32, n: usize) -> Vec<Option<u8>> {
        let mut word: Option<(u32, Option<u32>)> = None;
        (0..n as u32)
            .map(|k| {
                let a = addr.wrapping_add(k);
                let base = a & !3;
                let w = match word {
                    Some((b, w)) if b == base => w,
                    _ => {
                        let w = self.peek_word(base);
                        word = Some((base, w));
                        w
                    }
                };
                w.map(|w| (w >> (8 * (a & 3))) as u8)
            })
            .collect()
    }
    /// Ordered host diagnostic, unsupported by engines without a monitor.
    fn ddr_monitor(&mut self, _command: DdrMonitorCommand) -> Result<DdrMonitorStatus, String> {
        Err("this DSP engine does not support the DDR monitor".into())
    }
    /// One 32-bit word at the exact DSP data address, including compact MMR
    /// addresses that need not be byte-aligned. None means unknown,
    /// unmodeled, or unsupported by the engine.
    fn peek_word(&self, _addr: u32) -> Option<u32> {
        None
    }
}

/// Opt-in diagnostics of the DSP worker (and the synchronous peer):
/// `DIGI_DSPI_LOG=path` appends every DSPI frame to a binary log, and
/// `DIGI_DSP_WATCH=addr[:n],...` with `DIGI_DSP_WATCH_OUT=path.csv` writes
/// watched DSP words after every frame's render.
///
/// Log format: magic `DSPILOG1`, then per frame `u64 BE index`, `u8
/// attached`, `u32 BE tx length`, tx bytes, `u32 BE reply length`, reply bytes.
struct Diag {
    log: Option<BufWriter<File>>,
    log_index: u64,
    watch: Option<Watch>,
    model: Option<ModelLog>,
}

/// `DIGI_DSP_MODEL_LOG=path.csv`: one row per frame of counter deltas.
struct ModelLog {
    out: BufWriter<File>,
    index: u64,
    prev_tiers: sharc_native::TierCounters,
    prev_model: sharc_native::ModelStats,
}

struct Watch {
    out: BufWriter<File>,
    addrs: Vec<u32>,
    index: u64,
}

/// Parse `addr[:n],...` (hex, `0x` optional, `n` decimal words, default 1,
/// max 256) into the byte address of every watched word. Addresses are the
/// byte addresses `Engine::peek` takes (it reads with `normal_word` false, so
/// no word-to-byte conversion happens); consecutive words are 4 apart.
fn parse_watch(spec: &str) -> Result<Vec<u32>, String> {
    let mut addrs = Vec::new();
    for item in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let (a, n) = match item.split_once(':') {
            Some((a, n)) => (
                a,
                n.parse::<u32>()
                    .map_err(|_| format!("bad count in {item:?}"))?,
            ),
            None => (item, 1),
        };
        if !(1..=256).contains(&n) {
            return Err(format!("count {n} out of range 1..=256 in {item:?}"));
        }
        let a = a
            .strip_prefix("0x")
            .or_else(|| a.strip_prefix("0X"))
            .unwrap_or(a);
        let base = u32::from_str_radix(a, 16).map_err(|_| format!("bad address in {item:?}"))?;
        addrs.extend((0..n).map(|k| base.wrapping_add(4 * k)));
    }
    if addrs.is_empty() {
        return Err("no addresses".into());
    }
    Ok(addrs)
}

/// `DIGI_DSP_WATCH_OUT`, else `$DIGI_DIAG_DIR/dspwatch.csv` (tools/emulator/digi_env.py
/// `diag_env` sets the directory; the variables that switch a log on carry
/// their own path).
fn watch_out() -> Option<String> {
    std::env::var("DIGI_DSP_WATCH_OUT").ok().or_else(|| {
        let dir = std::env::var_os("DIGI_DIAG_DIR").filter(|d| !d.is_empty())?;
        std::fs::create_dir_all(&dir).ok()?;
        Some(std::path::Path::new(&dir).join("dspwatch.csv").to_string_lossy().into_owned())
    })
}

impl Diag {
    fn from_env() -> Self {
        let mut log = None;
        if let Ok(path) = std::env::var("DIGI_DSPI_LOG") {
            match File::create(&path).and_then(|f| {
                let mut w = BufWriter::new(f);
                w.write_all(b"DSPILOG1")?;
                Ok(w)
            }) {
                Ok(w) => log = Some(w),
                Err(e) => eprintln!("sharc_peer: DIGI_DSPI_LOG {path}: {e}"),
            }
        }
        let spec = std::env::var("DIGI_DSP_WATCH").ok();
        let out = spec.as_ref().and_then(|_| watch_out());
        let mut watch = None;
        match (spec, out) {
            (Some(spec), Some(path)) => match parse_watch(&spec) {
                Ok(addrs) => match File::create(&path).and_then(|f| {
                    let mut w = BufWriter::new(f);
                    let cols: Vec<String> = addrs.iter().map(|a| format!("0x{a:x}")).collect();
                    writeln!(w, "frame,pc,busy,pc_at_rx,{}", cols.join(","))?;
                    Ok(w)
                }) {
                    Ok(out) => {
                        watch = Some(Watch {
                            out,
                            addrs,
                            index: 0,
                        })
                    }
                    Err(e) => eprintln!("sharc_peer: DIGI_DSP_WATCH_OUT {path}: {e}"),
                },
                Err(e) => eprintln!("sharc_peer: DIGI_DSP_WATCH: {e}"),
            },
            (None, None) => {}
            _ => eprintln!(
                "sharc_peer: DIGI_DSP_WATCH and DIGI_DSP_WATCH_OUT must both be set; watch disabled"
            ),
        }
        let mut model = None;
        if let Ok(path) = std::env::var("DIGI_DSP_MODEL_LOG") {
            match File::create(&path).and_then(|f| {
                let mut w = BufWriter::new(f);
                writeln!(
                    w,
                    "frame,executed,busy,reached_idle,interpreted,block_instructions,fast_insns,traps,block_traps,idle_skips,gated,unsafe_block,irq_deferred,timer_gated,mmr_exits,code_mismatch,irq_taken,irq_vectors"
                )?;
                Ok(w)
            }) {
                Ok(out) => {
                    model = Some(ModelLog {
                        out,
                        index: 0,
                        prev_tiers: Default::default(),
                        prev_model: Default::default(),
                    })
                }
                Err(e) => eprintln!("sharc_peer: DIGI_DSP_MODEL_LOG {path}: {e}"),
            }
        }
        Self {
            log,
            log_index: 0,
            watch,
            model,
        }
    }

    /// Append one CSV row of counter deltas since the previous frame (call
    /// after the render). Engines without counters give empty cells.
    fn model_frame<E: DspEngine>(&mut self, engine: &E, stat: &FrameStat) {
        let Some(ml) = &mut self.model else { return };
        let mut row = format!(
            "{},{},{},{}",
            ml.index, stat.executed, stat.busy, stat.reached_idle as u8
        );
        ml.index += 1;
        match engine.tier_counters() {
            Some(t) => {
                let p = &ml.prev_tiers;
                for d in [
                    t.interpreted.wrapping_sub(p.interpreted),
                    t.block_instructions.wrapping_sub(p.block_instructions),
                    t.fast_insns.wrapping_sub(p.fast_insns),
                    t.traps.wrapping_sub(p.traps),
                    t.block_traps.wrapping_sub(p.block_traps),
                    t.idle_skips.wrapping_sub(p.idle_skips),
                ] {
                    row.push_str(&format!(",{d}"));
                }
                ml.prev_tiers = t;
            }
            None => row.push_str(",,,,,,"),
        }
        match engine.model_stats() {
            Some(m) => {
                let p = &ml.prev_model;
                for d in [
                    m.gated.wrapping_sub(p.gated),
                    m.unsafe_block.wrapping_sub(p.unsafe_block),
                    m.irq_deferred.wrapping_sub(p.irq_deferred),
                    m.timer_gated.wrapping_sub(p.timer_gated),
                    m.mmr_exits.wrapping_sub(p.mmr_exits),
                    m.code_mismatch.wrapping_sub(p.code_mismatch),
                    m.irq_taken.wrapping_sub(p.irq_taken),
                ] {
                    row.push_str(&format!(",{d}"));
                }
                let vectors: Vec<String> = (0..64)
                    .filter_map(|v| {
                        let d = m.irq_by_vector[v].wrapping_sub(p.irq_by_vector[v]);
                        (d != 0).then(|| format!("{v}:{d}"))
                    })
                    .collect();
                row.push(',');
                row.push_str(&vectors.join(";"));
                ml.prev_model = m;
            }
            None => row.push_str(",,,,,,,,"),
        }
        row.push('\n');
        if let Err(e) = ml.out.write_all(row.as_bytes()) {
            eprintln!("sharc_peer: DIGI_DSP_MODEL_LOG write failed, log disabled: {e}");
            self.model = None;
        }
    }

    /// Append one DSPI frame record (call after the exchange).
    fn log_frame(&mut self, attached: bool, tx: &[u8], reply: &[u8]) {
        let index = self.log_index;
        self.log_index += 1;
        let Some(w) = &mut self.log else { return };
        let res = (|| {
            w.write_all(&index.to_be_bytes())?;
            w.write_all(&[attached as u8])?;
            w.write_all(&(tx.len() as u32).to_be_bytes())?;
            w.write_all(tx)?;
            w.write_all(&(reply.len() as u32).to_be_bytes())?;
            w.write_all(reply)
        })();
        if let Err(e) = res {
            eprintln!("sharc_peer: DIGI_DSPI_LOG write failed, log disabled: {e}");
            self.log = None;
        }
    }

    /// Append one CSV row of the watched words (call after the render).
    fn watch_frame<E: DspEngine>(&mut self, engine: &E, busy: u32, pc_at_rx: u32) {
        let Some(wt) = &mut self.watch else { return };
        let mut row = format!("{},0x{:x},{},0x{:x}", wt.index, engine.pc(), busy, pc_at_rx);
        wt.index += 1;
        for &a in &wt.addrs {
            row.push(',');
            match engine.peek_word(a) {
                Some(v) => row.push_str(&format!("0x{v:08x}|{:e}", f32::from_bits(v))),
                None => row.push('?'),
            }
        }
        row.push('\n');
        if let Err(e) = wt.out.write_all(row.as_bytes()) {
            eprintln!("sharc_peer: DIGI_DSP_WATCH_OUT write failed, watch disabled: {e}");
            self.watch = None;
        }
    }
}

impl Drop for Diag {
    fn drop(&mut self) {
        if let Some(w) = &mut self.log {
            let _ = w.flush();
        }
        if let Some(wt) = &mut self.watch {
            let _ = wt.out.flush();
        }
        if let Some(ml) = &mut self.model {
            let _ = ml.out.flush();
        }
    }
}

/// Existing Engine option-26 maps, captured only by an explicit worker request.
#[derive(Clone, Debug)]
pub struct ExecutionProfile {
    /// Kinds 0..4: coverage, interpreter entries, transitions, block exits,
    /// and zero-progress entry bails. Each is tab/space-separated text from
    /// `Engine::profile_text`, not a timing measurement.
    pub maps: [String; 5],
}

/// Per-frame record.
#[derive(Clone, Debug, Default)]
pub struct FrameStat {
    pub first_word: u16,
    /// FNV-1a 64 of the TX frame and of the reply.
    pub tx_hash: u64,
    pub reply_hash: u64,
    pub reply_nonzero_bytes: u32,
    /// DSP instructions run before the first chunk ending in the idle range
    /// (the whole period if idle was never reached).
    pub busy: u32,
    pub reached_idle: bool,
    /// Link port 0 work units delivered so far, and bytes still queued
    /// (the ColdFire's sample loads in flight).
    pub lp0_units: u64,
    pub lp0_pending: u32,
    pub executed: u32,
    /// None: SPORTs not running; zeros were appended.
    pub block: bool,
}

/// Cumulative host/worker timing for the opt-in threaded DSP link profiler.
/// Durations are nanoseconds and overlap across pipelined frames, so they are
/// explanatory stage totals rather than additive wall time.
#[derive(Clone, Copy, Debug, Default)]
pub struct ThreadedTimingProfile {
    pub frames_sent: u64,
    pub frames_completed: u64,
    pub host_enqueue_ns: u64,
    pub host_reply_wait_ns: u64,
    pub host_done_wait_ns: u64,
    pub host_collect_ns: u64,
    pub worker_queue_ns: u64,
    pub worker_spi_ns: u64,
    pub worker_dsp_ns: u64,
    pub worker_sport_ns: u64,
}

impl ThreadedTimingProfile {
    fn add_worker(&mut self, timing: WorkerTiming) {
        self.frames_completed = self.frames_completed.saturating_add(1);
        self.worker_queue_ns = self.worker_queue_ns.saturating_add(timing.queue_ns);
        self.worker_spi_ns = self.worker_spi_ns.saturating_add(timing.spi_ns);
        self.worker_dsp_ns = self.worker_dsp_ns.saturating_add(timing.dsp_ns);
        self.worker_sport_ns = self.worker_sport_ns.saturating_add(timing.sport_ns);
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct WorkerTiming {
    queue_ns: u64,
    spi_ns: u64,
    dsp_ns: u64,
    sport_ns: u64,
}

fn elapsed_ns(start: Instant) -> u64 {
    start.elapsed().as_nanos().min(u64::MAX as u128) as u64
}

#[derive(Default)]
pub struct Shared {
    /// L/R interleaved, `-(q / 2^31)`.
    pub pcm: Vec<f32>,
    /// Raw Q31 words as the engine returned them.
    pub raw: Vec<i32>,
    pub frames: Vec<FrameStat>,
    pub first_words: BTreeMap<u16, u64>,
    pub halted: Option<String>,
    pub dsp_instructions: u64,
    pub nonzero_replies: u64,
    pub missing_blocks: u64,
    /// Link port 0 work units delivered, and bytes queued, as of the last frame.
    pub lp0_units: u64,
    pub lp0_pending: u32,
    /// While false, frames get zero replies and the DSP does not run.
    pub attached: bool,
}

/// Everything about one frame that depends on the DSP engine. It is the
/// same code in the synchronous peer and in the DSP worker thread, so the
/// two cannot drift apart. It never touches [`Shared`].
struct Core<E: DspEngine> {
    engine: E,
    period: u32,
    idle: (u32, u32),
    /// Mirror of `Shared::halted` (only the core ever sets it).
    halted: Option<String>,
    /// Experiment only (`DIGI_DSP_RX_PHASE`, DSP instructions, < period): run
    /// the DSP this long into the period before the SPI frame is delivered.
    /// 0 keeps the lock-step behaviour (exchange at period start) bit-for-bit.
    rx_phase: u32,
    /// Instructions run before the exchange this frame (0 when `rx_phase` is 0).
    pre_ran: u32,
    pre_busy: u32,
    pre_idle: bool,
    /// Engine PC right before the SPI exchange.
    pc_at_rx: u32,
    /// Link port 0 bytes the ColdFire has sent that no work unit took yet.
    lp0_pending: Vec<u8>,
    /// Work units delivered to link port 0 so far.
    lp0_units: u64,
}

/// One link port 0 work unit: a tag word and 1024 payload words
/// (docs/findings/15: the descriptor the firmware arms is XCNT 0x401).
pub const LP0_UNIT_BYTES: usize = 0x401 * 4;
/// Work units delivered per frame at most; the rest wait for the next frame.
const LP0_UNITS_PER_FRAME: usize = 4;
/// DSP instructions the firmware may take to re-arm the port after a unit.
const LP0_REARM_BUDGET: u32 = 100_000;

/// What the exchange half of a frame leaves for the render half.
struct Exchanged {
    stat: FrameStat,
    /// Detached or halted: zero reply, zero audio, no DSP time.
    skipped: bool,
}

/// What one frame adds to [`Shared`] once it has been rendered.
struct Done {
    stat: FrameStat,
    skipped: bool,
    block: Option<Vec<u8>>,
    halted: Option<String>,
    timing: Option<WorkerTiming>,
}

impl<E: DspEngine> Core<E> {
    fn new(engine: E, period: u32) -> Self {
        Self {
            engine,
            period,
            idle: DN2_IDLE_RANGE,
            halted: None,
            rx_phase: std::env::var("DIGI_DSP_RX_PHASE")
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok())
                .map_or(0, |v| v.min(period.saturating_sub(1))),
            pre_ran: 0,
            pre_busy: 0,
            pre_idle: false,
            pc_at_rx: 0,
            lp0_pending: Vec::new(),
            lp0_units: 0,
        }
    }

    /// Queue link port 0 bytes (in wire order) for delivery from the next
    /// frame on.
    fn push_link(&mut self, bytes: &[u8]) {
        self.lp0_pending.extend_from_slice(bytes);
    }

    /// Deliver whole work units to link port 0 the way the hardware does:
    /// a unit lands in the buffer the firmware's descriptor names and
    /// completes the DMA (SEC 81); the DSP then runs the firmware's own ISR
    /// and callback until it has re-armed the port, which is counted in this
    /// frame's pre-exchange instructions. A unit that finds the port not yet
    /// armed waits.
    fn deliver_lp0(&mut self) {
        let mut delivered = 0;
        while self.lp0_pending.len() >= LP0_UNIT_BYTES
            && delivered < LP0_UNITS_PER_FRAME
            && self.halted.is_none()
            && self.engine.lp0_ready()
        {
            let unit: Vec<u8> = self.lp0_pending.drain(..LP0_UNIT_BYTES).collect();
            if let Err(e) = self.engine.lp0_transfer(&unit) {
                eprintln!("sharc_peer: link port 0 transfer failed: {e}");
                self.halted = Some(e);
                return;
            }
            let mut ran = 0u32;
            while !self.engine.lp0_ready() && ran < LP0_REARM_BUDGET {
                let n = self.engine.step(2000);
                ran += n;
                if n < 2000 {
                    break;
                }
            }
            self.pre_ran += ran;
            self.pre_busy += ran;
            self.lp0_units += 1;
            if std::env::var_os("DIGI_LP0_LOG").is_some() {
                let w = |i: usize| u32::from_le_bytes(unit[4 * i..4 * i + 4].try_into().unwrap());
                eprintln!(
                    "lp0 unit {} tag {:#x} w1 {:#x} w2 {:#x} rearm_instr {ran} ready {}",
                    self.lp0_units,
                    w(0),
                    w(1),
                    w(2),
                    self.engine.lp0_ready()
                );
            }
            delivered += 1;
        }
        if let Some(h) = self.engine.halt_reason() {
            eprintln!("sharc_peer: DSP halted in link port 0 delivery: {h}");
            self.halted = Some(h);
        }
    }

    /// Run the DSP for one period; returns (executed, busy, reached_idle).
    fn run_period(&mut self) -> (u32, u32, bool) {
        self.run_for(self.period)
    }

    /// Run the DSP for `limit` instructions; returns (executed, busy,
    /// reached_idle).
    fn run_for(&mut self, limit: u32) -> (u32, u32, bool) {
        let mut done = 0u32;
        let mut busy = None;
        let in_idle = |pc: u32, idle: (u32, u32)| (idle.0..idle.1).contains(&pc);
        // `busy` is the first chunk end (a multiple of CHUNK, or the period
        // end) with the PC in the idle range. No chunk end before the PC
        // first enters the range can be one, so run to its first entry at
        // once; if that is not a chunk end, run to the first chunk end with
        // the PC in the range (the engine looks at the same ends, without
        // cutting the generated blocks at the ones before). Engines without
        // these stops are stepped in chunks below.
        let base = self.engine.icount();
        if let Some(ran) = self.engine.step_until_in(limit, self.idle) {
            done = ran;
            if !in_idle(self.engine.pc(), self.idle) {
                // The period ended (or the engine stopped) first.
                return (done, done, false);
            }
            if (done > 0 && done % CHUNK == 0) || done == limit {
                busy = Some(done);
            } else if let Some(more) =
                self.engine
                    .step_until_in_aligned(limit - done, self.idle, CHUNK, base)
            {
                // Stopped at a chunk end or the period end in the range
                // (busy, as in the chunk loop, even if the engine stopped
                // there), or elsewhere out of it (not busy).
                done += more;
                if in_idle(self.engine.pc(), self.idle) {
                    busy = Some(done);
                }
            }
        }
        while done < limit {
            // Once idle is reached the chunking only costs time (the idle
            // skip is per step call); results do not depend on step sizes.
            let n = if busy.is_some() {
                limit - done
            } else {
                ((done / CHUNK + 1) * CHUNK).min(limit) - done
            };
            let ran = self.engine.step(n);
            done += ran;
            if busy.is_none() && in_idle(self.engine.pc(), self.idle) {
                busy = Some(done);
            }
            if ran < n {
                break;
            }
        }
        (done, busy.unwrap_or(done), busy.is_some())
    }

    /// First half: deliver the frame through the SPI2 model, take the reply.
    fn exchange(
        &mut self,
        tx: &[u8],
        attached: bool,
        timing: Option<&mut WorkerTiming>,
    ) -> (Vec<u8>, Exchanged) {
        let first_word = tx.get(..2).map_or(0, |b| u16::from_be_bytes([b[0], b[1]]));
        let mut stat = FrameStat {
            first_word,
            tx_hash: fnv1a(tx),
            ..FrameStat::default()
        };
        if !attached || self.halted.is_some() {
            return (
                vec![0; tx.len()],
                Exchanged {
                    stat,
                    skipped: true,
                },
            );
        }
        self.pre_ran = 0;
        self.pre_busy = 0;
        self.pre_idle = false;
        if self.rx_phase > 0 && self.halted.is_none() {
            let (ran, busy, idle) = self.run_for(self.rx_phase);
            self.pre_ran = ran;
            self.pre_busy = busy;
            self.pre_idle = idle;
            if let Some(h) = self.engine.halt_reason() {
                eprintln!("sharc_peer: DSP halted before frame: {h}");
                self.halted = Some(h);
            }
        }
        self.deliver_lp0();
        self.pc_at_rx = self.engine.pc();
        let spi_start = timing.as_ref().map(|_| Instant::now());
        let poked = frame_poke(tx);
        let exchange = self.engine.spi2_exchange(poked.as_deref().unwrap_or(tx));
        if let (Some(timing), Some(start)) = (timing, spi_start) {
            timing.spi_ns = timing.spi_ns.saturating_add(elapsed_ns(start));
        }
        let reply = match exchange {
            Ok(r) if r.len() == tx.len() => r,
            Ok(r) => {
                eprintln!("sharc_peer: reply length {} != {}", r.len(), tx.len());
                self.halted = Some("bad reply length".into());
                vec![0; tx.len()]
            }
            Err(e) => {
                eprintln!("sharc_peer: spi2_exchange failed: {e}");
                self.halted = Some(e);
                vec![0; tx.len()]
            }
        };
        stat.reply_hash = fnv1a(&reply);
        stat.reply_nonzero_bytes = reply.iter().filter(|&&b| b != 0).count() as u32;
        (
            reply,
            Exchanged {
                stat,
                skipped: false,
            },
        )
    }

    /// Second half: run the DSP for the period and take the audio block.
    /// Reads only the engine state left by the exchange.
    fn render(&mut self, ex: Exchanged, mut timing: Option<&mut WorkerTiming>) -> Done {
        let Exchanged { mut stat, skipped } = ex;
        if skipped {
            return Done {
                stat,
                skipped,
                block: None,
                halted: self.halted.clone(),
                timing: timing.copied(),
            };
        }
        let mut halted = self.halted.clone();
        stat.lp0_units = self.lp0_units;
        stat.lp0_pending = self.lp0_pending.len() as u32;
        if halted.is_none() {
            let dsp_start = timing.as_ref().map(|_| Instant::now());
            let (executed, busy, idle) = if self.pre_ran == 0 {
                self.run_period()
            } else {
                self.run_for(self.period - self.pre_ran)
            };
            if let (Some(timing), Some(start)) = (timing.as_deref_mut(), dsp_start) {
                timing.dsp_ns = timing.dsp_ns.saturating_add(elapsed_ns(start));
            }
            if self.pre_ran == 0 {
                stat.executed = executed;
                stat.busy = busy;
                stat.reached_idle = idle;
            } else {
                stat.executed = self.pre_ran + executed;
                stat.busy = if self.pre_idle {
                    self.pre_busy
                } else {
                    self.pre_busy + busy
                };
                stat.reached_idle = self.pre_idle || idle;
            }
            self.pre_ran = 0;
            halted = self.engine.halt_reason();
            if halted.is_some() {
                eprintln!(
                    "sharc_peer: DSP halted: {}",
                    halted.as_deref().unwrap_or("")
                );
            }
        }
        let block = if halted.is_none() {
            let sport_start = timing.as_ref().map(|_| Instant::now());
            let block = self.engine.sport_block();
            if let (Some(timing), Some(start)) = (timing.as_deref_mut(), sport_start) {
                timing.sport_ns = timing.sport_ns.saturating_add(elapsed_ns(start));
            }
            match block {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("sharc_peer: sport_block failed: {e}");
                    halted = Some(e);
                    None
                }
            }
        } else {
            None
        };
        if halted.is_some() && self.halted.is_none() {
            self.halted = halted;
        }
        Done {
            stat,
            skipped,
            block,
            halted: self.halted.clone(),
            timing: timing.copied(),
        }
    }
}

/// Append one finished frame to the shared record.
fn commit(sh: &mut Shared, d: Done) {
    let Done {
        stat,
        skipped,
        block,
        halted,
        timing: _,
    } = d;
    *sh.first_words.entry(stat.first_word).or_default() += 1;
    if skipped {
        sh.pcm.extend_from_slice(&[0.0; BLOCK_WORDS]);
        sh.raw.extend_from_slice(&[0; BLOCK_WORDS]);
        sh.frames.push(stat);
        return;
    }
    let mut stat = stat;
    sh.lp0_units = stat.lp0_units;
    sh.lp0_pending = stat.lp0_pending;
    sh.nonzero_replies += (stat.reply_nonzero_bytes != 0) as u64;
    sh.dsp_instructions += stat.executed as u64;
    if halted.is_some() && sh.halted.is_none() {
        sh.halted = halted;
    }
    match block {
        Some(b) if b.len() == BLOCK_WORDS * 4 => {
            stat.block = true;
            for w in b.chunks_exact(4) {
                let q = i32::from_le_bytes([w[0], w[1], w[2], w[3]]);
                sh.raw.push(q);
                sh.pcm.push(-(q as f64 / 2_147_483_648.0) as f32);
            }
        }
        _ => {
            sh.missing_blocks += 1;
            sh.raw.extend_from_slice(&[0; BLOCK_WORDS]);
            sh.pcm.extend_from_slice(&[0.0; BLOCK_WORDS]);
        }
    }
    sh.frames.push(stat);
}

/// The synchronous peer: the whole frame runs inside the ColdFire bus write.
pub struct SharcPeer<E: DspEngine> {
    core: Core<E>,
    /// Feed the ColdFire's FlexBus link-port bytes to the DSP (DT2).
    sample_link: bool,
    shared: Rc<RefCell<Shared>>,
    diag: Diag,
}

impl<E: DspEngine> SharcPeer<E> {
    pub fn new(engine: E, period: u32) -> (Self, Rc<RefCell<Shared>>) {
        let shared = Rc::new(RefCell::new(Shared {
            attached: true,
            ..Shared::default()
        }));
        let peer = Self {
            core: Core::new(engine, period),
            sample_link: false,
            shared: shared.clone(),
            diag: Diag::from_env(),
        };
        (peer, shared)
    }

    /// Feed the ColdFire's FlexBus link-port bytes (sample loads) to the DSP.
    pub fn enable_sample_link(&mut self) {
        self.sample_link = true;
    }

    pub fn set_idle_range(&mut self, range: (u32, u32)) {
        self.core.idle = range;
    }

    fn frame(&mut self, tx: &[u8]) -> Vec<u8> {
        let attached = self.shared.borrow().attached;
        let (reply, ex) = self.core.exchange(tx, attached, None);
        self.diag.log_frame(attached, tx, &reply);
        let done = self.core.render(ex, None);
        self.diag
            .watch_frame(&self.core.engine, done.stat.busy, self.core.pc_at_rx);
        self.diag.model_frame(&self.core.engine, &done.stat);
        commit(&mut self.shared.borrow_mut(), done);
        reply
    }
}

impl<E: DspEngine + 'static> SharcPeer<E> {
    pub fn boxed(self) -> Box<dyn Peer> {
        Box::new(self)
    }
}

impl<E: DspEngine> Peer for SharcPeer<E> {
    fn exchange(&mut self, tx: &[u8]) -> Vec<u8> {
        self.frame(tx)
    }
    fn link_bytes(&mut self, bytes: &[u8]) {
        if self.sample_link {
            self.core.push_link(bytes);
        }
    }
}

/// The DSP half of a two-instance session (the browser's DSP worker). Like
/// the native [`ThreadedPeer`] worker it answers frame N as soon as the SPI
/// exchange is done (`reply`, which first finishes rendering frame N-1) and
/// renders the period afterwards (`render`), so the ColdFire side runs
/// ahead of the DSP by up to one frame while every reply and DSP state is
/// exactly the synchronous peer's.
pub struct RemoteDsp<E: DspEngine> {
    core: Core<E>,
    shared: Rc<RefCell<Shared>>,
    pending: Option<Exchanged>,
    sample_link: bool,
}

impl<E: DspEngine> RemoteDsp<E> {
    pub fn new(engine: E, period: u32) -> (Self, Rc<RefCell<Shared>>) {
        let shared = Rc::new(RefCell::new(Shared {
            attached: true,
            ..Shared::default()
        }));
        let remote = Self {
            core: Core::new(engine, period),
            shared: shared.clone(),
            pending: None,
            sample_link: false,
        };
        (remote, shared)
    }

    pub fn enable_sample_link(&mut self) {
        self.sample_link = true;
    }

    /// First half of a frame: the reply. Renders the previous frame first if
    /// the caller has not.
    pub fn reply(&mut self, tx: &[u8]) -> Vec<u8> {
        self.render();
        let attached = self.shared.borrow().attached;
        let (reply, ex) = self.core.exchange(tx, attached, None);
        self.pending = Some(ex);
        reply
    }

    /// Second half: run the DSP for the period and record the audio block.
    pub fn render(&mut self) {
        if let Some(ex) = self.pending.take() {
            let done = self.core.render(ex, None);
            commit(&mut self.shared.borrow_mut(), done);
        }
    }

    pub fn link_bytes(&mut self, bytes: &[u8]) {
        if self.sample_link {
            self.core.push_link(bytes);
        }
    }
}

enum Msg {
    DdrMonitor {
        command: DdrMonitorCommand,
        ack: mpsc::Sender<Result<DdrMonitorStatus, String>>,
    },
    Frame {
        tx: Vec<u8>,
        /// Link port 0 bytes sent by the ColdFire since the previous frame.
        link: Vec<u8>,
        attached: bool,
        enqueued: Option<Instant>,
    },
    Export,
    Report,
    RegionProfile(bool),
    ExecutionProfile {
        enabled: bool,
        ack: mpsc::Sender<bool>,
    },
    ExecutionProfileReport,
    /// Overlay a DSP patch between frames; the ack carries the DSP
    /// instruction count when it took effect.
    Patch {
        patch: crate::dsp_patch::DspPatch,
        ack: mpsc::Sender<Result<u64, String>>,
    },
    /// Read DSP memory between frames.
    Peek {
        addr: u32,
        len: usize,
        ack: mpsc::Sender<Vec<Option<u8>>>,
    },
    /// Read one exact DSP word address without byte alignment.
    PeekWord {
        addr: u32,
        ack: mpsc::Sender<Option<u32>>,
    },
}

/// Most per-period records the worker keeps (about 22 minutes of audio).
const PERIOD_RECORD_LIMIT: usize = 2_000_000;

/// Frames between the counters the worker publishes (0.1 s of audio).
const LIVE_REPORT_FRAMES: u64 = 150;

/// What the DSP worker reports about itself on request. Reading it is cheap
/// and does not touch DSP state.
#[derive(Clone, Debug, Default)]
pub struct WorkerReport {
    /// CPU time of the worker thread so far (CLOCK_THREAD_CPUTIME_ID).
    pub cpu_ns: u64,
    pub tiers: Option<sharc_native::TierCounters>,
    /// Aggregate model-gate counters. Unlike execution-map text, this is a
    /// small Copy value and is available to ordinary boundary reports.
    pub model_stats: Option<sharc_native::ModelStats>,
    /// Per-entry dispatch costs, only after `set_region_profile(true)`.
    pub region_profile: Option<sharc_native::RegionProfile>,
    /// Wall time of each frame the worker handled (SPI exchange + DSP period +
    /// SPORT block), in nanoseconds, in order; only with link timing, at most
    /// [`PERIOD_RECORD_LIMIT`] entries.
    pub period_ns: Vec<u32>,
    /// Frames the worker handled, including ones past the record limit.
    pub periods: u64,
}

/// ColdFire-thread side of the threaded peer.
struct Link {
    tx: Option<mpsc::Sender<Msg>>,
    reply_rx: mpsc::Receiver<Vec<u8>>,
    done_rx: mpsc::Receiver<Done>,
    export_rx: mpsc::Receiver<(Vec<u8>, u64)>,
    report_rx: mpsc::Receiver<WorkerReport>,
    execution_profile_rx: mpsc::Receiver<Option<ExecutionProfile>>,
    frames_sent: u64,
    /// Feed the ColdFire's FlexBus link-port bytes to the DSP (DT2).
    sample_link: bool,
    /// Link port 0 bytes waiting for the next frame message.
    pending_link: Vec<u8>,
    /// Frames sent whose [`Done`] has not been committed yet.
    outstanding: usize,
    timing: Option<ThreadedTimingProfile>,
    live: Arc<Mutex<Option<WorkerReport>>>,
    shared: Rc<RefCell<Shared>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Link {
    fn commit_ready(&mut self, block: bool) {
        while self.outstanding > 0 {
            let done_wait_start = if block && self.timing.is_some() {
                Some(Instant::now())
            } else {
                None
            };
            let done = if block {
                self.done_rx.recv().ok()
            } else {
                self.done_rx.try_recv().ok()
            };
            if let (Some(profile), Some(start)) = (&mut self.timing, done_wait_start) {
                profile.host_done_wait_ns =
                    profile.host_done_wait_ns.saturating_add(elapsed_ns(start));
            }
            let Some(d) = done else { break };
            let collect_start = self.timing.as_ref().map(|_| Instant::now());
            let worker_timing = d.timing;
            commit(&mut self.shared.borrow_mut(), d);
            if let Some(profile) = &mut self.timing {
                if let Some(timing) = worker_timing {
                    profile.add_worker(timing);
                }
                if let Some(start) = collect_start {
                    profile.host_collect_ns =
                        profile.host_collect_ns.saturating_add(elapsed_ns(start));
                }
            }
            self.outstanding -= 1;
        }
    }

    fn worker_died(&mut self, len: usize) -> Vec<u8> {
        let mut sh = self.shared.borrow_mut();
        if sh.halted.is_none() {
            sh.halted = Some("DSP worker thread stopped".into());
        }
        sh.attached = false;
        vec![0; len]
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        self.tx = None; // the worker's recv fails and the thread ends
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

/// Threaded peer. Frame N: the ColdFire thread sends the frame to the DSP
/// worker and waits only for the reply, which the worker produces after it
/// has finished rendering frame N-1 (one thread, in order), so the reply and
/// every DSP state it depends on are exactly those of the synchronous peer.
/// The worker then renders period N while the ColdFire thread continues to
/// frame N+1. [`Shared`] receives each frame's PCM and statistics in order,
/// when the ColdFire thread next looks (`exchange`, [`ThreadedHandle::sync`]);
/// the DSP engine is created inside the worker thread, so it need not be
/// `Send`.
pub struct ThreadedPeer {
    link: Rc<RefCell<Link>>,
}

/// Host-side handle to a [`ThreadedPeer`] that the ColdFire emulator owns.
#[derive(Clone)]
pub struct ThreadedHandle {
    link: Rc<RefCell<Link>>,
}

impl ThreadedPeer {
    /// Start the worker. `make` runs on the worker thread and builds the
    /// engine there (an error is returned here).
    pub fn spawn<E, F>(
        make: F,
        period: u32,
        idle: (u32, u32),
    ) -> Result<(Self, ThreadedHandle, Rc<RefCell<Shared>>), String>
    where
        E: DspEngine + 'static,
        F: FnOnce() -> Result<E, String> + Send + 'static,
    {
        Self::spawn_with_timing(make, period, idle, false)
    }

    /// Start the worker with optional cumulative host/worker timing.
    pub fn spawn_with_timing<E, F>(
        make: F,
        period: u32,
        idle: (u32, u32),
        timing_enabled: bool,
    ) -> Result<(Self, ThreadedHandle, Rc<RefCell<Shared>>), String>
    where
        E: DspEngine + 'static,
        F: FnOnce() -> Result<E, String> + Send + 'static,
    {
        let (tx, rx) = mpsc::channel::<Msg>();
        let (reply_tx, reply_rx) = mpsc::channel::<Vec<u8>>();
        let (done_tx, done_rx) = mpsc::channel::<Done>();
        let (export_tx, export_rx) = mpsc::channel::<(Vec<u8>, u64)>();
        let (report_tx, report_rx) = mpsc::channel::<WorkerReport>();
        let (execution_profile_tx, execution_profile_rx) =
            mpsc::channel::<Option<ExecutionProfile>>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let live = Arc::new(Mutex::new(None::<WorkerReport>));
        let live_worker = Arc::clone(&live);
        let worker = thread::Builder::new()
            .name("sharc-dsp".into())
            .spawn(move || {
                // Keep the DSP worker on performance cores (QOS_CLASS_USER_INTERACTIVE
                // in <sys/qos.h>); the default QoS lets macOS schedule it on
                // efficiency cores under load.
                #[cfg(target_os = "macos")]
                {
                    unsafe extern "C" {
                        fn pthread_set_qos_class_self_np(
                            qos_class: u32,
                            relative_priority: i32,
                        ) -> i32;
                    }
                    // SAFETY: affects only the calling thread's scheduling class.
                    let _ = unsafe { pthread_set_qos_class_self_np(0x21, 0) };
                }
                let engine = match make() {
                    Ok(e) => {
                        let _ = ready_tx.send(Ok(()));
                        e
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                let mut core = Core::new(engine, period);
                core.idle = idle;
                // Flushed when the worker ends (Drop), including early returns.
                let mut diag = Diag::from_env();
                let mut period_ns: Vec<u32> = Vec::new();
                let mut periods = 0u64;
                let mut frames_done = 0u64;
                let mut spike = crate::spikelog::DspWindow::open(crate::cputime::thread_cpu_ns());
                while let Ok(msg) = rx.recv() {
                    match msg {
                        Msg::Frame {
                            tx,
                            link,
                            attached,
                            enqueued,
                        } => {
                            if let Some(w) = &mut spike {
                                w.frame_start();
                            }
                            if attached {
                                core.push_link(&link);
                            }
                            let mut timing = enqueued.map(|start| WorkerTiming {
                                queue_ns: elapsed_ns(start),
                                ..WorkerTiming::default()
                            });
                            let (reply, ex) = core.exchange(&tx, attached, timing.as_mut());
                            diag.log_frame(attached, &tx, &reply);
                            if reply_tx.send(reply).is_err() {
                                return;
                            }
                            let done = core.render(ex, timing.as_mut());
                            diag.watch_frame(&core.engine, done.stat.busy, core.pc_at_rx);
                            diag.model_frame(&core.engine, &done.stat);
                            if let Some(t) = &done.timing {
                                periods += 1;
                                if period_ns.len() < PERIOD_RECORD_LIMIT {
                                    let busy = t
                                        .spi_ns
                                        .saturating_add(t.dsp_ns)
                                        .saturating_add(t.sport_ns);
                                    period_ns.push(busy.min(u32::MAX as u64) as u32);
                                }
                            }
                            if let Some(w) = &mut spike {
                                w.frame_end(done.timing.as_ref().map(|t| {
                                    t.spi_ns.saturating_add(t.dsp_ns).saturating_add(t.sport_ns)
                                }));
                            }
                            if done_tx.send(done).is_err() {
                                return;
                            }
                            frames_done += 1;
                            if frames_done % LIVE_REPORT_FRAMES == 0 {
                                let report = WorkerReport {
                                    cpu_ns: crate::cputime::thread_cpu_ns(),
                                    tiers: core.engine.tier_counters(),
                                    model_stats: core.engine.model_stats(),
                                    periods: frames_done,
                                    ..WorkerReport::default()
                                };
                                if let Some(w) = &mut spike {
                                    let (compile, build, built) =
                                        report.tiers.as_ref().map_or((0, 0, 0), |t| {
                                            (t.fast_compile_ns, t.fast_build_ns, t.fast_built)
                                        });
                                    w.flush(frames_done, report.cpu_ns, compile, build, built);
                                }
                                if let Ok(mut slot) = live_worker.lock() {
                                    *slot = Some(report);
                                }
                            }
                        }
                        Msg::Report => {
                            let _ = report_tx.send(WorkerReport {
                                cpu_ns: crate::cputime::thread_cpu_ns(),
                                tiers: core.engine.tier_counters(),
                                model_stats: core.engine.model_stats(),
                                region_profile: core.engine.region_profile(),
                                period_ns: period_ns.clone(),
                                periods,
                            });
                        }
                        Msg::Export => {
                            let _ = export_tx.send((core.engine.export(), core.engine.icount()));
                        }
                        Msg::RegionProfile(enabled) => core.engine.set_region_profile(enabled),
                        Msg::ExecutionProfile { enabled, ack } => {
                            let _ = ack.send(core.engine.set_execution_profile(enabled));
                        }
                        Msg::ExecutionProfileReport => {
                            let _ = execution_profile_tx.send(core.engine.execution_profile());
                        }
                        Msg::DdrMonitor { command, ack } => {
                            let _ = ack.send(core.engine.ddr_monitor(command));
                        }
                        Msg::Peek { addr, len, ack } => {
                            let _ = ack.send(core.engine.peek_bytes(addr, len));
                        }
                        Msg::PeekWord { addr, ack } => {
                            let _ = ack.send(core.engine.peek_word(addr));
                        }
                        Msg::Patch { patch, ack } => {
                            let result = core
                                .engine
                                .apply_dsp_patch(&patch)
                                .map(|()| core.engine.icount());
                            let _ = ack.send(result);
                        }
                    }
                }
            })
            .map_err(|e| format!("spawn DSP worker: {e}"))?;
        ready_rx
            .recv()
            .map_err(|_| "DSP worker died at start".to_string())??;
        let shared = Rc::new(RefCell::new(Shared {
            attached: true,
            ..Shared::default()
        }));
        let link = Rc::new(RefCell::new(Link {
            tx: Some(tx),
            reply_rx,
            done_rx,
            export_rx,
            report_rx,
            execution_profile_rx,
            frames_sent: 0,
            sample_link: false,
            pending_link: Vec::new(),
            outstanding: 0,
            timing: timing_enabled.then(ThreadedTimingProfile::default),
            live,
            shared: shared.clone(),
            worker: Some(worker),
        }));
        Ok((Self { link: link.clone() }, ThreadedHandle { link }, shared))
    }

    pub fn boxed(self) -> Box<dyn Peer> {
        Box::new(self)
    }

    /// Feed the ColdFire's FlexBus link-port bytes (sample loads) to the DSP's
    /// link port 0 (the device profile's `sample_link = "lp0"`).
    pub fn enable_sample_link(&self) {
        self.link.borrow_mut().sample_link = true;
    }
}

impl Peer for ThreadedPeer {
    fn link_bytes(&mut self, bytes: &[u8]) {
        let mut l = self.link.borrow_mut();
        if l.sample_link {
            l.pending_link.extend_from_slice(bytes);
        }
    }
    fn exchange(&mut self, tx: &[u8]) -> Vec<u8> {
        let mut l = self.link.borrow_mut();
        let attached = l.shared.borrow().attached;
        let enqueue_start = l.timing.as_ref().map(|_| Instant::now());
        let tx = tx.to_vec();
        let tx_len = tx.len();
        let enqueued = l.timing.as_ref().map(|_| Instant::now());
        let link_bytes = std::mem::take(&mut l.pending_link);
        let sent = l.tx.as_ref().is_some_and(|c| {
            c.send(Msg::Frame {
                tx,
                link: link_bytes,
                attached,
                enqueued,
            })
            .is_ok()
        });
        if let (Some(profile), Some(start)) = (&mut l.timing, enqueue_start) {
            profile.host_enqueue_ns = profile.host_enqueue_ns.saturating_add(elapsed_ns(start));
        }
        if !sent {
            return l.worker_died(tx_len);
        }
        l.outstanding += 1;
        l.frames_sent += 1;
        if let Some(profile) = &mut l.timing {
            profile.frames_sent = profile.frames_sent.saturating_add(1);
        }
        let reply_wait_start = l.timing.as_ref().map(|_| Instant::now());
        match l.reply_rx.recv() {
            Ok(r) => {
                if let (Some(profile), Some(start)) = (&mut l.timing, reply_wait_start) {
                    profile.host_reply_wait_ns =
                        profile.host_reply_wait_ns.saturating_add(elapsed_ns(start));
                }
                l.commit_ready(false);
                r
            }
            Err(_) => {
                if let (Some(profile), Some(start)) = (&mut l.timing, reply_wait_start) {
                    profile.host_reply_wait_ns =
                        profile.host_reply_wait_ns.saturating_add(elapsed_ns(start));
                }
                l.worker_died(tx_len)
            }
        }
    }
}

impl ThreadedHandle {
    /// Enable or clear the worker's opt-in per-entry dispatch profile.
    pub fn set_region_profile(&self, enabled: bool) -> bool {
        self.link
            .borrow()
            .tx
            .as_ref()
            .is_some_and(|c| c.send(Msg::RegionProfile(enabled)).is_ok())
    }
    /// Enable the exact, non-timing execution maps and wait for worker order.
    pub fn set_execution_profile(&self, enabled: bool) -> bool {
        let (ack_tx, ack_rx) = mpsc::channel();
        let sent = self.link.borrow().tx.as_ref().is_some_and(|c| {
            c.send(Msg::ExecutionProfile {
                enabled,
                ack: ack_tx,
            })
            .is_ok()
        });
        sent && ack_rx.recv().unwrap_or(false)
    }
    /// Overlay a DSP patch onto the running engine. The worker applies it
    /// between two frames, after every frame already sent (the channel is
    /// ordered), so it lands at a defined frame count. Returns the DSP
    /// instruction count at that point.
    pub fn apply_patch(&self, patch: crate::dsp_patch::DspPatch) -> Result<u64, String> {
        let (ack_tx, ack_rx) = mpsc::channel();
        let sent = self
            .link
            .borrow()
            .tx
            .as_ref()
            .is_some_and(|c| c.send(Msg::Patch { patch, ack: ack_tx }).is_ok());
        if !sent {
            return Err("DSP worker has stopped".into());
        }
        ack_rx
            .recv()
            .map_err(|_| "DSP worker stopped while patching".to_string())?
    }

    /// Read `len` bytes of DSP memory at `addr` between two frames (`None`
    /// for a byte the engine does not know).
    pub fn peek_bytes(&self, addr: u32, len: usize) -> Result<Vec<Option<u8>>, String> {
        let (ack_tx, ack_rx) = mpsc::channel();
        let sent = self
            .link
            .borrow()
            .tx
            .as_ref()
            .is_some_and(|c| c.send(Msg::Peek { addr, len, ack: ack_tx }).is_ok());
        if !sent {
            return Err("DSP worker has stopped".into());
        }
        ack_rx
            .recv()
            .map_err(|_| "DSP worker stopped while reading".to_string())
    }

    /// Apply a monitor command after queued frames and acknowledge the exact
    /// resulting metadata. Status does not inspect the watched data memory.
    pub fn ddr_monitor(&self, command: DdrMonitorCommand) -> Result<DdrMonitorStatus, String> {
        let (ack, reply) = mpsc::channel();
        let sent = self
            .link
            .borrow()
            .tx
            .as_ref()
            .is_some_and(|tx| tx.send(Msg::DdrMonitor { command, ack }).is_ok());
        if !sent {
            return Err("DSP worker has stopped".into());
        }
        reply
            .recv()
            .map_err(|_| "DSP worker stopped during DDR monitor command".to_string())?
    }

    /// Read one exact DSP word address between frames, retaining unknowns.
    pub fn peek_word(&self, addr: u32) -> Result<Option<u32>, String> {
        let (ack_tx, ack_rx) = mpsc::channel();
        let sent = self
            .link
            .borrow()
            .tx
            .as_ref()
            .is_some_and(|c| c.send(Msg::PeekWord { addr, ack: ack_tx }).is_ok());
        if !sent {
            return Err("DSP worker has stopped".into());
        }
        ack_rx
            .recv()
            .map_err(|_| "DSP worker stopped while reading word".to_string())
    }

    /// Commit completed frames without waiting; streaming hosts use this between steps.
    /// Call `sync` instead before reading final totals or exporting state.
    pub fn poll(&self) {
        self.link.borrow_mut().commit_ready(false);
    }

    /// Wait for the DSP to finish every frame sent so far and commit them to
    /// [`Shared`]. Call before reading `Shared` for totals or a snapshot.
    pub fn sync(&self) {
        self.link.borrow_mut().commit_ready(true);
    }

    /// Frames handed to the DSP worker so far (one per DSPI2 exchange).
    pub fn frames_sent(&self) -> u64 {
        self.link.borrow().frames_sent
    }

    /// The worker's own report (thread CPU time, tier counters, per-period
    /// wall times). Waits for the frames in flight, like `sync`; `None` if the
    /// worker has stopped.
    pub fn worker_report(&self) -> Option<WorkerReport> {
        let mut l = self.link.borrow_mut();
        l.commit_ready(true);
        if !l.tx.as_ref().is_some_and(|c| c.send(Msg::Report).is_ok()) {
            return None;
        }
        l.report_rx.recv().ok()
    }

    /// The worker's counters as of its last publication (every
    /// [`LIVE_REPORT_FRAMES`] frames; `periods` counts frames handled, and
    /// `period_ns` and `region_profile` are empty). Unlike
    /// [`Self::worker_report`] this never waits for the pipeline, so polling
    /// it does not disturb the timing it reports.
    pub fn live_report(&self) -> Option<WorkerReport> {
        self.link.borrow().live.lock().ok()?.clone()
    }

    /// Fetch the exact maps after draining frames. This is intentionally a
    /// separate request from the ordinary worker report so default reports
    /// never clone profile text.
    pub fn execution_profile(&self) -> Option<ExecutionProfile> {
        let mut l = self.link.borrow_mut();
        l.commit_ready(true);
        if !l
            .tx
            .as_ref()
            .is_some_and(|c| c.send(Msg::ExecutionProfileReport).is_ok())
        {
            return None;
        }
        l.execution_profile_rx.recv().ok().flatten()
    }

    /// Cumulative opt-in timing, or `None` when the peer was started without it.
    pub fn timing_profile(&self) -> Option<ThreadedTimingProfile> {
        self.link.borrow().timing
    }

    /// Canonical DSP state and instruction counter after every frame sent so
    /// far (empty / 0 when the engine does not export).
    pub fn export(&self) -> (Vec<u8>, u64) {
        let mut l = self.link.borrow_mut();
        l.commit_ready(true);
        let sent = l.tx.as_ref().is_some_and(|c| c.send(Msg::Export).is_ok());
        if !sent {
            return (Vec::new(), 0);
        }
        l.export_rx.recv().unwrap_or_default()
    }
}

/// Diagnostic frame override (spike 0.7): `DSP_FRAME_POKE=OFF=VAL[/TRACKS][,...]`
/// sets the big-endian 16-bit wire word at byte OFF of every frame sent to the
/// DSP (hex or decimal; with `/N`, at OFF + 0x60 * t for t in 0..N). It lets a
/// run drive a per-track frame word that no stock UI control reaches (the
/// ColdFire side of a real parameter is a later patch). Unset: no effect.
fn frame_poke(tx: &[u8]) -> Option<Vec<u8>> {
    static SPEC: std::sync::OnceLock<Vec<(usize, u16, usize)>> = std::sync::OnceLock::new();
    let spec = SPEC.get_or_init(|| {
        let Ok(text) = std::env::var("DSP_FRAME_POKE") else {
            return Vec::new();
        };
        let num = |s: &str| {
            let s = s.trim();
            match s.strip_prefix("0x") {
                Some(h) => usize::from_str_radix(h, 16).ok(),
                None => s.parse().ok(),
            }
        };
        text.split(',')
            .filter_map(|item| {
                let (off, rest) = item.split_once('=')?;
                let (val, tracks) = rest.split_once('/').unwrap_or((rest, "1"));
                Some((num(off)?, num(val)? as u16, num(tracks)?))
            })
            .collect()
    });
    if spec.is_empty() {
        return None;
    }
    let mut out = tx.to_vec();
    for &(off, val, tracks) in spec {
        for t in 0..tracks {
            let at = off + 0x60 * t;
            if at + 2 <= out.len() {
                out[at..at + 2].copy_from_slice(&val.to_be_bytes());
            }
        }
    }
    Some(out)
}

/// The engine glue over the first generated core (the build's SHARC_GEN_DIR).
mod engine_dn2 {
    use super::*;
    use ::sharc_native;
    include!("sharc_engine.rs");
}
pub use engine_dn2::*;

/// The same glue over the second generated core (SHARC_GEN_DIR_2): `second::NativeDsp` runs that
/// device's DSP where `NativeDsp` runs the first one's.
#[cfg(feature = "second-core")]
pub mod second {
    use super::*;
    use ::sharc_native_2 as sharc_native;
    include!("sharc_engine.rs");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ddr_monitor_worker_contract<E, F>(make: F)
    where
        E: DspEngine + 'static,
        F: FnOnce() -> Result<E, String> + Send + 'static,
    {
        use crate::ddr_monitor::{DdrMonitorHit, DdrMonitorKind, DdrMonitorRange};
        let (_peer, handle, _) = ThreadedPeer::spawn(make, 5000, DN2_IDLE_RANGE).unwrap();
        let range = DdrMonitorRange {
            start: 0x8001_0000,
            end: 0x8001_0004,
        };
        let initial = handle.ddr_monitor(DdrMonitorCommand::Status).unwrap();
        assert!(!initial.armed);
        assert_eq!(
            (initial.range, initial.first, initial.dsp_icount),
            (None, None, 73)
        );
        assert!(!initial.active_core_profile.is_empty());
        let armed = handle.ddr_monitor(DdrMonitorCommand::Arm(range)).unwrap();
        assert!(armed.armed);
        assert_eq!((armed.range, armed.first), (Some(range), None));
        assert_eq!(
            handle.ddr_monitor(DdrMonitorCommand::Status).unwrap(),
            armed
        );
        // Queue a deliberate host peek without waiting for its acknowledgement.
        // The subsequent monitor reply must reflect that earlier worker message.
        let (ack, peeked) = mpsc::channel();
        handle
            .link
            .borrow()
            .tx
            .as_ref()
            .unwrap()
            .send(Msg::PeekWord {
                addr: range.start,
                ack,
            })
            .unwrap();
        let hit = handle.ddr_monitor(DdrMonitorCommand::Status).unwrap();
        assert_eq!(peeked.try_recv().unwrap(), Some(0x0403_0201));
        assert_eq!(
            hit.first,
            Some(DdrMonitorHit {
                address: range.start,
                width: 4,
                kind: DdrMonitorKind::CpuRead
            })
        );
        assert_eq!(handle.ddr_monitor(DdrMonitorCommand::Status).unwrap(), hit);
        assert!(
            handle
                .ddr_monitor(DdrMonitorCommand::Arm(DdrMonitorRange {
                    start: range.start,
                    end: range.start
                }))
                .is_err()
        );
        assert_eq!(handle.ddr_monitor(DdrMonitorCommand::Status).unwrap(), hit);
        let cleared = handle.ddr_monitor(DdrMonitorCommand::Clear).unwrap();
        assert!(cleared.armed);
        assert_eq!((cleared.range, cleared.first), (Some(range), None));
        handle.peek_word(range.start).unwrap();
        assert!(
            handle
                .ddr_monitor(DdrMonitorCommand::Status)
                .unwrap()
                .first
                .is_some()
        );
        let replaced = handle
            .ddr_monitor(DdrMonitorCommand::Arm(DdrMonitorRange {
                start: range.start + 4,
                end: range.end + 4,
            }))
            .unwrap();
        assert_eq!(replaced.first, None);
        assert_eq!(replaced.range.unwrap().start, range.start + 4);
        let disarmed = handle.ddr_monitor(DdrMonitorCommand::Disarm).unwrap();
        assert!(!disarmed.armed);
        assert_eq!((disarmed.range, disarmed.first), (None, None));
        handle.link.borrow_mut().tx.take();
        assert!(
            handle
                .ddr_monitor(DdrMonitorCommand::Status)
                .unwrap_err()
                .contains("stopped")
        );
    }

    #[test]
    fn ddr_monitor_ordered_worker_lifecycle() {
        ddr_monitor_worker_contract(|| {
            let mut mem = sharc_native::mem::Mem::new();
            mem.load(0x8001_0000, &[1, 2, 3, 4]);
            mem.reset();
            let mut engine = sharc_native::Engine::new(mem);
            engine.s.icount = 73;
            Ok(NativeDsp(engine))
        });
    }

    #[cfg(feature = "second-core")]
    #[test]
    fn ddr_monitor_second_core_worker_lifecycle() {
        ddr_monitor_worker_contract(|| {
            let mut mem = sharc_native_2::mem::Mem::new();
            mem.load(0x8001_0000, &[1, 2, 3, 4]);
            mem.reset();
            let mut engine = sharc_native_2::Engine::new(mem);
            engine.s.icount = 73;
            Ok(second::NativeDsp(engine))
        });
    }

    #[test]
    fn ddr_monitor_unsupported_engine_acknowledges_error() {
        let (_peer, handle, _) = ThreadedPeer::spawn(|| Ok(WordProbe), 5000, DN2_IDLE_RANGE).unwrap();
        assert!(
            handle
                .ddr_monitor(DdrMonitorCommand::Status)
                .unwrap_err()
                .contains("does not support")
        );
        assert_eq!(handle.peek_word(0x3e002).unwrap(), Some(0x55667788));
    }

    struct WordProbe;

    impl DspEngine for WordProbe {
        fn spi2_exchange(&mut self, frame: &[u8]) -> Result<Vec<u8>, String> {
            Ok(frame.to_vec())
        }
        fn step(&mut self, n: u32) -> u32 {
            n
        }
        fn sport_block(&mut self) -> Result<Option<Vec<u8>>, String> {
            Ok(None)
        }
        fn pc(&self) -> u32 {
            DN2_IDLE_RANGE.0
        }
        fn halt_reason(&self) -> Option<String> {
            None
        }
        fn peek_word(&self, addr: u32) -> Option<u32> {
            match addr {
                0x3e000 => Some(0x11223344),
                0x3e002 => Some(0x55667788),
                _ => None,
            }
        }
    }

    #[test]
    fn exact_word_probe_keeps_unaligned_addresses_and_unknowns() {
        let (_peer, handle, _shared) =
            ThreadedPeer::spawn(|| Ok(WordProbe), 5000, DN2_IDLE_RANGE).unwrap();
        assert_eq!(handle.peek_word(0x3e002).unwrap(), Some(0x55667788));
        assert_eq!(handle.peek_word(0x3e003).unwrap(), None);
        // The existing byte view still aligns each word and slices its bytes;
        // it must not acquire compact-register semantics from the new API.
        assert_eq!(
            handle.peek_bytes(0x3e002, 4).unwrap(),
            vec![Some(0x22), Some(0x11), None, None]
        );
    }

    struct Mock {
        pc: u32,
        idle_after: u32,
        ran: u32,
        halt_at_frame: Option<u32>,
        frames: u32,
        reply_byte: u8,
    }

    impl DspEngine for Mock {
        fn spi2_exchange(&mut self, f: &[u8]) -> Result<Vec<u8>, String> {
            self.frames += 1;
            self.ran = 0;
            Ok(vec![self.reply_byte; f.len()])
        }
        fn step(&mut self, n: u32) -> u32 {
            self.ran += n;
            self.pc = if self.ran >= self.idle_after {
                DN2_IDLE_RANGE.0 + 3
            } else {
                0x1c0000
            };
            n
        }
        fn sport_block(&mut self) -> Result<Option<Vec<u8>>, String> {
            if self.halt_at_frame == Some(self.frames) {
                return Err("boom".into());
            }
            let mut b = Vec::new();
            for i in 0..BLOCK_WORDS as i32 {
                // L = -(2^30), R = 2^30 on every frame.
                let w: i32 = if i % 2 == 0 { -(1 << 30) } else { 1 << 30 };
                b.extend_from_slice(&w.to_le_bytes());
            }
            Ok(Some(b))
        }
        fn pc(&self) -> u32 {
            self.pc
        }
        fn halt_reason(&self) -> Option<String> {
            None
        }
    }

    fn mock(halt_at_frame: Option<u32>) -> Mock {
        Mock {
            pc: 0,
            idle_after: 10_000,
            ran: 0,
            halt_at_frame,
            frames: 0,
            reply_byte: 0xab,
        }
    }

    /// Firmware-free receiver: a transfer disarms DMA until the simulated
    /// callback has run. Audio still uses the ordinary frame mock above.
    struct LinkMock {
        audio: Mock,
        ready: bool,
        received: Vec<Vec<u8>>,
        rearm_after: Option<u32>,
        rearm_ran: u32,
        in_transfer: bool,
    }

    impl DspEngine for LinkMock {
        fn spi2_exchange(&mut self, frame: &[u8]) -> Result<Vec<u8>, String> {
            self.in_transfer = false;
            self.audio.spi2_exchange(frame)
        }
        fn lp0_ready(&self) -> bool {
            self.ready
        }
        fn lp0_transfer(&mut self, bytes: &[u8]) -> Result<(), String> {
            assert!(self.ready);
            self.received.push(bytes.to_vec());
            self.ready = false;
            self.in_transfer = true;
            self.rearm_ran = 0;
            Ok(())
        }
        fn step(&mut self, n: u32) -> u32 {
            if self.in_transfer {
                self.rearm_ran += n;
                if self
                    .rearm_after
                    .is_some_and(|after| self.rearm_ran >= after)
                {
                    self.ready = true;
                }
            }
            self.audio.step(n)
        }
        fn sport_block(&mut self) -> Result<Option<Vec<u8>>, String> {
            self.audio.sport_block()
        }
        fn pc(&self) -> u32 {
            self.audio.pc()
        }
        fn halt_reason(&self) -> Option<String> {
            self.audio.halt_reason()
        }
    }

    fn profiled_link_peer(
        device: &str,
        version: &str,
    ) -> (SharcPeer<LinkMock>, Rc<RefCell<Shared>>) {
        let registry = device_profile::Registry::embedded().unwrap();
        let profile = registry.dsp_for(device, version).unwrap();
        let (mut peer, shared) = SharcPeer::new(
            LinkMock {
                audio: mock(None),
                ready: false,
                received: Vec::new(),
                rearm_after: Some(4000),
                rearm_ran: 0,
                in_transfer: false,
            },
            20_000,
        );
        peer.set_idle_range((profile.idle_start, profile.idle_end));
        // Use the embedded firmware's route selection, as the coupled and cold
        // constructors do. Removing DN2 1.12's capability must break delivery.
        if profile.sample_link == Some(device_profile::SampleLink::Lp0) {
            peer.enable_sample_link();
        }
        (peer, shared)
    }

    #[test]
    fn profiled_lp0_waits_for_complete_units_and_armed_dma_while_audio_continues() {
        for (device, version) in [("dn2", "1.12"), ("dt2", "1.17")] {
            let (mut peer, shared) = profiled_link_peer(device, version);
            let unit: Vec<u8> = (0..LP0_UNIT_BYTES).map(|i| (i % 251) as u8).collect();
            let frame = [0x12, 0x34, 0x56, 0x78];
            peer.core.engine.ready = true;
            peer.link_bytes(&unit[..LP0_UNIT_BYTES - 1]);
            assert_eq!(peer.exchange(&frame), vec![0xab; frame.len()]);
            assert!(
                peer.core.engine.received.is_empty(),
                "partial unit: {device}"
            );
            assert_eq!(shared.borrow().lp0_pending as usize, LP0_UNIT_BYTES - 1);
            peer.core.engine.ready = false;
            peer.link_bytes(&unit[LP0_UNIT_BYTES - 1..]);
            assert_eq!(peer.exchange(&frame), vec![0xab; frame.len()]);
            assert!(
                peer.core.engine.received.is_empty(),
                "unarmed DMA: {device}"
            );
            assert_eq!(shared.borrow().lp0_pending as usize, LP0_UNIT_BYTES);
            peer.core.engine.ready = true;
            assert_eq!(peer.exchange(&frame), vec![0xab; frame.len()]);
            assert_eq!(
                peer.core.engine.received,
                vec![unit],
                "wire bytes: {device}"
            );
            assert!(peer.core.engine.ready);
            assert_eq!(peer.core.engine.rearm_ran, 4000);
            let shared = shared.borrow();
            assert_eq!((shared.lp0_units, shared.lp0_pending), (1, 0));
            assert_eq!(peer.core.engine.audio.frames, 3);
            assert_eq!(shared.pcm.len(), 3 * BLOCK_WORDS);
            assert!(
                shared
                    .pcm
                    .chunks_exact(BLOCK_WORDS)
                    .all(|block| block[0] == 0.5 && block[1] == -0.5)
            );
            assert_eq!(shared.frames[2].executed, 20_000);
            assert!(shared.halted.is_none());
        }
    }

    #[test]
    fn dn2_lp0_rearm_and_units_have_per_frame_budgets() {
        let (mut peer, shared) = profiled_link_peer("dn2", "1.12");
        let units = vec![0x5a; (LP0_UNITS_PER_FRAME + 1) * LP0_UNIT_BYTES];
        peer.core.engine.ready = true;
        peer.link_bytes(&units);
        peer.exchange(&[1, 2]);
        assert_eq!(peer.core.engine.received.len(), LP0_UNITS_PER_FRAME);
        assert!(
            peer.core
                .engine
                .received
                .iter()
                .all(|unit| unit == &units[..LP0_UNIT_BYTES])
        );
        assert_eq!(shared.borrow().lp0_pending as usize, LP0_UNIT_BYTES);
        // A receiver whose callback never rearms must not hang the audio frame.
        peer.core.engine.rearm_after = None;
        assert_eq!(peer.exchange(&[3, 4]), vec![0xab; 2]);
        assert_eq!(peer.core.engine.rearm_ran, LP0_REARM_BUDGET);
        assert!(!peer.core.engine.ready);
        assert_eq!(shared.borrow().lp0_units as usize, LP0_UNITS_PER_FRAME + 1);
        peer.link_bytes(&units[..LP0_UNIT_BYTES]);
        assert_eq!(peer.exchange(&[5, 6]), vec![0xab; 2]);
        let shared = shared.borrow();
        assert_eq!(shared.lp0_pending as usize, LP0_UNIT_BYTES);
        assert_eq!(shared.lp0_units as usize, LP0_UNITS_PER_FRAME + 1);
        assert_eq!(shared.pcm.len(), 3 * BLOCK_WORDS);
        assert!(shared.halted.is_none());
    }

    /// A PC that is in the idle range exactly while the instruction count
    /// lies in one of `idle` ([start, end) intervals), optionally able to
    /// stop at the first such boundary (`fast`), and halting at `halt`.
    struct Spans {
        t: u32,
        idle: Vec<(u32, u32)>,
        halt: Option<u32>,
        fast: bool,
    }

    impl Spans {
        fn idle_at(&self, t: u32) -> bool {
            self.idle.iter().any(|&(a, b)| (a..b).contains(&t))
        }
        fn run(&mut self, n: u32, stop_in_idle: bool) -> u32 {
            let start = self.t;
            for _ in 0..n {
                if stop_in_idle && self.idle_at(self.t) {
                    break;
                }
                if self.halt == Some(self.t) {
                    break;
                }
                self.t += 1;
            }
            self.t - start
        }
    }

    impl DspEngine for Spans {
        fn spi2_exchange(&mut self, f: &[u8]) -> Result<Vec<u8>, String> {
            Ok(vec![0; f.len()])
        }
        fn step(&mut self, n: u32) -> u32 {
            self.run(n, false)
        }
        fn step_until_in(&mut self, n: u32, _range: (u32, u32)) -> Option<u32> {
            self.fast.then(|| self.run(n, true))
        }
        fn sport_block(&mut self) -> Result<Option<Vec<u8>>, String> {
            Ok(None)
        }
        fn pc(&self) -> u32 {
            if self.idle_at(self.t) {
                DN2_IDLE_RANGE.0
            } else {
                0x1c0000
            }
        }
        fn halt_reason(&self) -> Option<String> {
            (self.halt == Some(self.t)).then(|| "halt".into())
        }
    }

    #[test]
    fn stopping_at_idle_entry_finds_the_same_chunk_end() {
        let period = 10_000;
        let cases: Vec<(Vec<(u32, u32)>, Option<u32>)> = vec![
            (vec![], None),
            (vec![(0, period)], None),
            (vec![(2048, period)], None),
            (vec![(2047, period)], None),
            (vec![(2049, period)], None),
            (vec![(3000, 3100), (5000, period)], None),
            (vec![(3000, 3072), (3073, period)], None),
            (vec![(3000, 3073), (3080, period)], None),
            (vec![(9999, period)], None),
            (vec![(period - 10, period + 1)], None),
            (vec![(5000, period)], Some(4000)),
            (vec![(5000, period)], Some(5500)),
            (vec![(1000, 1500), (6000, period)], Some(7000)),
        ];
        for (idle, halt) in cases {
            let mut got = Vec::new();
            for fast in [false, true] {
                let mut core = Core::new(
                    Spans {
                        t: 0,
                        idle: idle.clone(),
                        halt,
                        fast,
                    },
                    period,
                );
                got.push(core.run_period());
            }
            assert_eq!(got[0], got[1], "idle {idle:?} halt {halt:?}");
        }
    }

    #[test]
    fn frame_flow_pcm_and_busy() {
        let (mut peer, sh) = SharcPeer::new(mock(None), 20_000);
        let mut tx = vec![0u8; 2748];
        tx[1] = 3;
        let reply = peer.exchange(&tx);
        assert_eq!(reply.len(), 2748);
        assert!(reply.iter().all(|&b| b == 0xab));
        let sh = sh.borrow();
        assert_eq!(sh.pcm.len(), BLOCK_WORDS);
        assert_eq!(sh.pcm[0], 0.5);
        assert_eq!(sh.pcm[1], -0.5);
        assert_eq!(sh.raw[0], -(1 << 30));
        let f = &sh.frames[0];
        assert_eq!(f.first_word, 3);
        assert_eq!(f.reply_nonzero_bytes, 2748);
        assert!(f.reached_idle);
        // first 1024-instruction chunk ending at or past 10,000
        assert_eq!(f.busy, 10 * 1024);
        assert_eq!(f.executed, 20_000);
        assert_eq!(sh.dsp_instructions, 20_000);
        assert_eq!(sh.first_words[&3], 1);
    }

    #[test]
    fn engine_error_returns_zeros_and_stays_halted() {
        let (mut peer, sh) = SharcPeer::new(mock(Some(1)), 4096);
        let r = peer.exchange(&[0u8; 8]);
        assert_eq!(r, vec![0xab; 8]); // reply was taken before the block failed
        assert_eq!(sh.borrow().halted.as_deref(), Some("boom"));
        let r = peer.exchange(&[0u8; 8]);
        assert_eq!(r, vec![0; 8]);
        let sh = sh.borrow();
        assert_eq!(sh.frames.len(), 2);
        assert_eq!(sh.pcm.len(), 2 * BLOCK_WORDS);
        assert!(sh.pcm[BLOCK_WORDS..].iter().all(|&x| x == 0.0));
    }

    #[test]
    fn detached_gives_zero_reply_without_running() {
        let (mut peer, sh) = SharcPeer::new(mock(None), 4096);
        sh.borrow_mut().attached = false;
        assert_eq!(peer.exchange(&[1u8; 4]), vec![0; 4]);
        assert_eq!(sh.borrow().dsp_instructions, 0);
        assert_eq!(peer.core.engine.frames, 0);
    }
}

#[cfg(test)]
mod threaded_tests {
    use super::*;

    /// State-dependent engine: every reply and block depends on everything
    /// that happened before, so any reordering shows up in the output.
    struct Chain {
        h: u64,
        halt_after: Option<u32>,
        frames: u32,
        pc: u32,
    }

    impl Chain {
        fn new(halt_after: Option<u32>) -> Self {
            Chain {
                h: 0x1234_5678,
                halt_after,
                frames: 0,
                pc: 0,
            }
        }
        fn mix(&mut self, v: u64) {
            self.h = (self.h ^ v).wrapping_mul(0x100_0000_01b3).rotate_left(13);
        }
    }

    impl DspEngine for Chain {
        fn spi2_exchange(&mut self, f: &[u8]) -> Result<Vec<u8>, String> {
            self.frames += 1;
            self.mix(fnv1a(f));
            let h = self.h;
            Ok((0..f.len()).map(|i| (h >> ((i % 8) * 8)) as u8).collect())
        }
        fn step(&mut self, n: u32) -> u32 {
            self.mix(n as u64);
            self.pc = if self.h & 3 == 0 {
                DN2_IDLE_RANGE.0 + 1
            } else {
                0x1c0000
            };
            n
        }
        fn sport_block(&mut self) -> Result<Option<Vec<u8>>, String> {
            if self.halt_after == Some(self.frames) {
                return Err("chain halt".into());
            }
            self.mix(7);
            let mut b = Vec::new();
            for i in 0..BLOCK_WORDS as u64 {
                self.mix(i);
                b.extend_from_slice(&((self.h >> 20) as i32).to_le_bytes());
            }
            Ok(Some(b))
        }
        fn pc(&self) -> u32 {
            self.pc
        }
        fn halt_reason(&self) -> Option<String> {
            None
        }
        fn export(&self) -> Vec<u8> {
            self.h.to_le_bytes().to_vec()
        }
        fn icount(&self) -> u64 {
            self.frames as u64
        }
    }

    fn tx_for(i: u32) -> Vec<u8> {
        let mut tx = vec![0u8; 64];
        tx[0..2].copy_from_slice(&((i % 5) as u16).to_be_bytes());
        tx[8..12].copy_from_slice(&i.to_le_bytes());
        tx
    }

    type Stat = (u16, u64, u64, u32, u32, bool, u32, bool);
    /// (replies, pcm bits, raw, per-frame stats, counters, halted)
    type Out = (
        Vec<Vec<u8>>,
        Vec<u32>,
        Vec<i32>,
        Vec<Stat>,
        (u64, u64, u64),
        Option<String>,
    );

    fn collect(sh: &Shared, replies: Vec<Vec<u8>>) -> Out {
        (
            replies,
            sh.pcm.iter().map(|x| x.to_bits()).collect(),
            sh.raw.clone(),
            sh.frames
                .iter()
                .map(|f| {
                    (
                        f.first_word,
                        f.tx_hash,
                        f.reply_hash,
                        f.reply_nonzero_bytes,
                        f.busy,
                        f.reached_idle,
                        f.executed,
                        f.block,
                    )
                })
                .collect(),
            (sh.dsp_instructions, sh.nonzero_replies, sh.missing_blocks),
            sh.halted.clone(),
        )
    }

    /// Frames 40..60 are sent detached.
    fn script(i: u32) -> bool {
        !(40..60).contains(&i)
    }

    fn run_sync(n: u32, halt: Option<u32>) -> Out {
        let (mut peer, sh) = SharcPeer::new(Chain::new(halt), 5000);
        let mut replies = Vec::new();
        for i in 0..n {
            sh.borrow_mut().attached = script(i);
            replies.push(peer.exchange(&tx_for(i)));
        }
        let sh = sh.borrow();
        collect(&sh, replies)
    }

    fn run_threaded(n: u32, halt: Option<u32>, poll: bool) -> (Out, (Vec<u8>, u64)) {
        let (mut peer, handle, sh) =
            ThreadedPeer::spawn(move || Ok(Chain::new(halt)), 5000, DN2_IDLE_RANGE).unwrap();
        let mut replies = Vec::new();
        for i in 0..n {
            sh.borrow_mut().attached = script(i);
            replies.push(peer.exchange(&tx_for(i)));
            if poll && i % 17 == 0 {
                handle.sync();
            }
        }
        let exp = handle.export();
        let sh = sh.borrow();
        (collect(&sh, replies), exp)
    }

    #[test]
    fn threaded_matches_sync() {
        for halt in [None, Some(150)] {
            let want = run_sync(300, halt);
            for poll in [false, true] {
                let (got, exp) = run_threaded(300, halt, poll);
                assert!(got == want, "halt={halt:?} poll={poll}");
                assert_eq!(exp.0.len(), 8);
                assert_eq!(exp.1, if halt.is_some() { 150 } else { 280 });
            }
        }
        // The script really exercised the detach and halt paths.
        let w = run_sync(300, Some(150));
        assert_eq!(w.5.as_deref(), Some("chain halt"));
        assert!(w.3[45].2 == 0 && !w.3[45].7);
        assert!(w.3[100].7);
    }

    #[test]
    fn threaded_engine_start_error_is_reported() {
        let r = ThreadedPeer::spawn(
            || Err::<Chain, _>("no engine".to_string()),
            1,
            DN2_IDLE_RANGE,
        );
        assert_eq!(r.err().as_deref(), Some("no engine"));
    }
}

/// `Core::run_period` against the plain chunked loop on the real DN2 engine
/// (private fixtures; run with `--ignored`, see the test).
#[cfg(test)]
mod aligned_stop_tests {
    use super::*;

    /// The native engine with only `step`: `run_period` falls back to the
    /// chunk loop alone, the reference the stops must not differ from.
    struct ChunkedOnly(NativeDsp);

    impl DspEngine for ChunkedOnly {
        fn spi2_exchange(&mut self, frame: &[u8]) -> Result<Vec<u8>, String> {
            self.0.spi2_exchange(frame)
        }
        fn step(&mut self, n: u32) -> u32 {
            self.0.step(n)
        }
        fn sport_block(&mut self) -> Result<Option<Vec<u8>>, String> {
            self.0.sport_block()
        }
        fn pc(&self) -> u32 {
            self.0.pc()
        }
        fn halt_reason(&self) -> Option<String> {
            self.0.halt_reason()
        }
        fn export(&self) -> Vec<u8> {
            self.0.export()
        }
        fn icount(&self) -> u64 {
            self.0.icount()
        }
    }

    fn read_frames(path: &str) -> Vec<Vec<u8>> {
        let data = std::fs::read(path).expect("frames file");
        let u32_at = |o: usize| u32::from_le_bytes(data[o..o + 4].try_into().unwrap()) as usize;
        let mut out = Vec::new();
        let mut o = 4;
        for _ in 0..u32_at(0) {
            let len = u32_at(o);
            out.push(data[o + 4..o + 4 + len].to_vec());
            o += 4 + len;
        }
        out
    }

    fn env(name: &str) -> String {
        std::env::var(name).unwrap_or_else(|_| panic!("set {name}"))
    }

    /// DN2_EQUIV_IMAGE (packed image), DN2_EQUIV_STATE (canonical state the
    /// frames START continues), DN2_EQUIV_FRAMES (`u32 count`, then `u32
    /// length, bytes` per DSPI2 TX frame); optional DN2_EQUIV_START /
    /// DN2_EQUIV_END (default 4600 / 5600) and DN2_EQUIV_CLOCK. Every period
    /// must return the same (executed, busy, reached_idle) and the engines
    /// must end in the same state.
    #[test]
    #[ignore = "needs the DN2 replay fixtures"]
    fn run_period_matches_chunk_loop() {
        let image = std::fs::read(env("DN2_EQUIV_IMAGE")).unwrap();
        let state = std::fs::read(env("DN2_EQUIV_STATE")).unwrap();
        let frames = read_frames(&env("DN2_EQUIV_FRAMES"));
        let num = |k: &str, d: usize| std::env::var(k).map_or(d, |v| v.parse().unwrap());
        let (start, end) = (num("DN2_EQUIV_START", 4600), num("DN2_EQUIV_END", 5600));
        let clock = num("DN2_EQUIV_CLOCK", 573_627_620) as u64;
        let period = 667_000;
        let open = || NativeDsp(open_dn2_engine(&image, &state, clock).unwrap());
        let mut new = Core::new(open(), period);
        let mut old = Core::new(ChunkedOnly(open()), period);
        let (mut busy_before_end, mut nonidle) = (0, 0);
        for (i, frame) in frames.iter().enumerate().take(end).skip(start) {
            let (r_new, _) = new.exchange(frame, true, None);
            let (r_old, _) = old.exchange(frame, true, None);
            assert_eq!(r_new, r_old, "SPI2 reply, frame {i}");
            let got = new.run_period();
            let want = old.run_period();
            assert_eq!(got, want, "(executed, busy, reached_idle), frame {i}");
            busy_before_end += (want.2 && want.1 < want.0) as u32;
            nonidle += !want.2 as u32;
            assert_eq!(
                new.engine.sport_block().unwrap(),
                old.engine.sport_block().unwrap(),
                "SPORT4 block, frame {i}"
            );
        }
        assert!(new.engine.halt_reason().is_none() && old.engine.halt_reason().is_none());
        new.engine.0.export_ranges = true;
        old.engine.0 .0.export_ranges = true;
        let (a, b) = (new.engine.export(), old.engine.export());
        assert!(a == b, "final state differs");
        eprintln!(
            "{} periods equal; idle reached before the period end in {busy_before_end}, idle never reached in \
             {nonidle}; state fnv {:016x} ({} bytes)",
            end - start,
            fnv1a(&a),
            a.len()
        );
        assert!(busy_before_end > 0, "the idle stops were never exercised");
    }
}
