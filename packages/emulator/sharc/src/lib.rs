//! Native SHARC+ core, generated from `tools/sharc_dev/sharc_core`.
//!
//! The instruction semantics are not written here. `tools/sharc_dev/sharc_transpile.py`
//! translates `tools/sharc_dev/sharc_core` into Rust (`core_i`/`core_g`), and
//! `tools/sharc_dev/sharc_rsgen.py` turns the firmware's basic blocks into functions
//! that call it with each instruction's decoded fields as constants. Both
//! write under `out/` (the block part embeds firmware); build with
//! `SHARC_GEN_DIR=<that directory>` to link them in. Without it this crate
//! is the runtime alone and every step traps.
//!
//! This crate holds the runtime the generated code runs on ([`rt`],
//! [`mem`]), the dispatcher, the harness's canonical state format
//! (tools/sharc_dev/sharc_diff.py) and its C ABI:
//!
//! ```c
//! void*   sharc_native_create(const uint8_t* image, size_t image_len);
//! void    sharc_native_destroy(void* handle);
//! int32_t sharc_native_import_state(void* handle, const uint8_t* blob, size_t blob_len);
//! int32_t sharc_native_export_state(void* handle, uint8_t* out, size_t out_cap);
//! int32_t sharc_native_step(void* handle, uint32_t n);
//! int32_t sharc_native_halt_reason(void* handle, char* out, size_t out_cap);
//! ```
//!
//! A step that reaches something the native core does not model (a fork,
//! a stop, an unmodelled MMR, an instruction with no native code) undoes
//! that instruction and halts with a reason starting `native-trap:`; the
//! caller then lets the Python core execute it (tools/sharc_dev/sharc_transpile_run.py).

pub mod canon;
mod code_diff;
mod cycmix;
mod diag;
mod irq_probe;
pub mod fast;
pub mod frames;
mod live_capture;
pub mod sha256;
pub mod vectors;
mod pc_dump;
mod reply_watch;
mod write_watch;
pub use sharc_stats::{ModelStats, RegionCost, RegionEntryProfile, RegionProfile, TierCounters};

#[cfg(feature = "region-profile")]
use std::time::Instant;

// The runtime and the generated core live in their own crates (so that
// editing this one does not recompile them); the old paths stay.
// `rt` is sharc-gen's: sharc-rt's with the two functions that need generated
// data (bnd::decode_at, bnd::_load_normal_ureg) filled in.
#[cfg(sharc_gen)]
pub use sharc_gen::generated;
pub use sharc_gen::rt;
pub use sharc_gen::{sym_name, sym_of};
pub use sharc_rt::{
    BlockFn, CHAIN_MAX, EXIT_BUDGET, EXIT_CHAIN, EXIT_NEXT, EXIT_TRAP, addressing, decode, mem,
    no_block,
};

/// `Cfg::refresh` for the image this build was generated with.
pub trait CfgRefresh {
    fn refresh(&mut self);
}

impl CfgRefresh for rt::Cfg {
    fn refresh(&mut self) {
        // With no generated image there are no blocks whose memory model
        // must match; the fast tier reads plain RAM only, where the model
        // makes no difference.
        #[cfg(all(sharc_gen, sharc_image))]
        self.refresh_with(gen_explicit_memory_model());
        #[cfg(not(all(sharc_gen, sharc_image)))]
        self.refresh_with(self.explicit_memory_model);
    }
}

#[cfg(test)]
mod tests;
#[cfg(all(test, sharc_gen))]
mod tests_gen;
#[cfg(all(test, sharc_gen))]
mod tests_gen_programs;
#[cfg(all(test, sharc_gen))]
mod tests_gen_vectors;

use rt::*;

/// A fine-grained monotonic counter (profiling only).
#[inline(always)]
pub fn ticks() -> u64 {
    #[cfg(target_arch = "aarch64")]
    {
        let v: u64;
        // SAFETY: reads the virtual counter, EL0-readable on macOS.
        unsafe { core::arch::asm!("isb", "mrs {}, cntvct_el0", out(reg) v) };
        v
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        std::time::UNIX_EPOCH
            .elapsed()
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0)
    }
}

/// Counter ticks per second (ticks()).
pub fn tick_hz() -> u64 {
    #[cfg(target_arch = "aarch64")]
    {
        let v: u64;
        // SAFETY: reads the counter frequency register.
        unsafe { core::arch::asm!("mrs {}, cntfrq_el0", out(reg) v) };
        v
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        1_000_000_000
    }
}

/// A dispatch entry: the block function and whether the generator classed it
/// model-safe (tools/sharc_dev/sharc_rsgen.py model_safe), i.e. its instructions touch
/// none of the state the bank, stack, timer, interrupt, clock and peripheral
/// models update between instructions.
#[derive(Clone, Copy)]
pub struct Entry {
    pub f: BlockFn,
    pub model_safe: bool,
}

/// pc -> block function, as a two-level table over the short-word PC.
pub struct Dispatch {
    pages: Vec<Option<Box<[Option<Entry>; 4096]>>>,
    pub count: usize,
}

/// `SHARC_AOT_DENY=0xb80509,...` (hex entry PCs): generated blocks to leave
/// out, for bisecting a tier fault (see tools/emulator/coupled.py bisect).
fn aot_deny_from_env() -> Vec<u32> {
    std::env::var("SHARC_AOT_DENY")
        .unwrap_or_default()
        .split(',')
        .filter(|x| !x.trim().is_empty())
        .filter_map(|x| u32::from_str_radix(x.trim().trim_start_matches("0x"), 16).ok())
        .collect()
}

impl Default for Dispatch {
    fn default() -> Self {
        Self::new(&[], &[])
    }
}

impl Dispatch {
    /// BLOCKS: (entry pc, function); SAFE: the sorted entry PCs that are
    /// model-safe.
    pub fn new(blocks: &[(u32, BlockFn)], safe: &[u32]) -> Dispatch {
        let mut pages: Vec<Option<Box<[Option<Entry>; 4096]>>> = Vec::new();
        pages.resize_with(1 << 12, || None);
        for &(pc, f) in blocks {
            let hi = (pc >> 12) as usize;
            if hi >= pages.len() {
                continue;
            }
            let page = pages[hi].get_or_insert_with(|| Box::new([None; 4096]));
            page[(pc & 0xFFF) as usize] = Some(Entry {
                f,
                model_safe: safe.binary_search(&pc).is_ok(),
            });
        }
        Dispatch {
            pages,
            count: blocks.len(),
        }
    }

    /// Drop the entries at `pcs`, so those PCs run in the interpreter (or the
    /// fast tier). Only block entries are affected: a block that continues
    /// into a denied PC without going through dispatch still runs it.
    pub fn without(mut self, pcs: &[u32]) -> Dispatch {
        for &pc in pcs {
            if let Some(Some(page)) = self.pages.get_mut((pc >> 12) as usize)
                && page[(pc & 0xFFF) as usize].take().is_some()
            {
                self.count -= 1;
            }
        }
        self
    }

    #[inline(always)]
    pub fn get_entry(&self, pc: Int) -> Option<Entry> {
        if !(0..(1 << 24)).contains(&pc) {
            return None;
        }
        let pc = pc as u32;
        self.pages[(pc >> 12) as usize]
            .as_ref()
            .and_then(|p| p[(pc & 0xFFF) as usize])
    }

    #[inline(always)]
    pub fn get(&self, pc: Int) -> Option<BlockFn> {
        self.get_entry(pc).map(|e| e.f)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub block_entries: u64,
    pub block_instructions: u64,
    pub single_steps: u64,
    pub traps: u64,
    /// Block exits through a trap (the instruction then runs through the
    /// one-instruction interpreter).
    pub block_traps: u64,
}

pub struct Engine {
    pub s: Box<St>,
    pub halt: Option<String>,
    pub last_trap: Option<Trap>,
    pub use_blocks: bool,
    /// Diagnostic clock only: one EMUCLK tick per completed instruction.
    /// This is explicitly not a cycle-accurate DSP clock.
    pub instruction_clock: bool,
    /// Starting tick for a bounded diagnostic continuation. The host must
    /// supply it explicitly; canonical state does not carry execution counts.
    pub instruction_clock_base: u64,
    /// Diagnostic stop before an unmasked software-interrupt candidate.
    /// This observes register state; it does not emulate interrupt entry.
    pub stop_software_interrupt: bool,
    /// Opt-in instruction-boundary breakpoint; no guest state is changed.
    pub stop_pc: Option<u32>,
    /// `step_until_in`'s PC range [lo, hi): the step ends (without a halt)
    /// at the first instruction boundary whose PC lies in it.
    stop_range: Option<(u32, u32)>,
    /// `step_until_in_aligned`'s (chunk, base instruction count): with
    /// `stop_range`, only boundaries a positive multiple of `chunk`
    /// instructions after `base` stop the step.
    stop_align: Option<(u32, u64)>,
    /// Opt-in functional software IRQ delivery through the L1 ISA IVT.
    pub software_interrupts: bool,
    pub export_ranges: bool,
    pub dispatch: Dispatch,
    pub stats: Stats,
    /// Coverage (tools/sharc_dev/sharc_rsgen.py --coverage): instructions run through
    /// the one-instruction interpreter, by (pc, MODE1 bits, MODE1 known).
    pub cov: Option<std::collections::HashMap<(u32, u32, bool), u64>>,
    /// Stop after one block call or one interpreted instruction (the
    /// divergence search in sharc-frames).
    pub one_dispatch: bool,
    /// Block exits into the interpreter, by (block pc, kind, pc after):
    /// kind 0 a bail at entry, 1 a trap, 2 a budget/pending exit later on.
    pub exits: Option<std::collections::HashMap<(u32, u8, u32), u64>>,
    /// Register state at zero-progress bails with option 26: (PC, MODE1 bits, registers
    /// whose masks are not fully known, delayed transfer pending) -> count.
    /// This is observed state, not a diagnosis of which guard rejected it.
    pub entry_bails: Option<std::collections::HashMap<(u32, u32, u128, bool), u64>>,
    /// Where runs of interpreted instructions start (with `cov`): the PCs
    /// block code is entered at but has no block for.
    pub entries: Option<std::collections::HashMap<u32, u64>>,
    /// Per-block timing (sharc-frames --block-profile): entry pc ->
    /// (counter ticks, instructions, entries), ticks of the generic
    /// counter (cntvct_el0 on aarch64).
    pub prof: Option<std::collections::HashMap<u32, (u64, u64, u64)>>,
    /// Opt-in dispatch costs for coupled-run's per-entry region profile.
    #[cfg(feature = "region-profile")]
    region_profile: Option<RegionProfile>,
    /// Block-to-block transitions (entry pc, next pc) with their counts.
    pub trans: Option<std::collections::HashMap<(u32, u32), u64>>,
    /// The PC the last interpreted instruction left (to tell a run's start).
    last_interp_next: Int,
    /// The first PC of the interpreter run in progress (profile only).
    run_start: Option<Int>,
    /// Opt-in idle-loop skip (option 23): the loop head PC, and the PC range
    /// every instruction of the loop must lie in (options 24, 25).
    pub idle_head: Option<u32>,
    pub idle_lo: u32,
    pub idle_hi: u32,
    idle_snap: Option<Box<IdleSnap>>,
    ddr_monitor_gen: u64,
    pub idle_stats: IdleStats,
    /// Counters for the run-time gate that lets model-safe blocks run with
    /// the bank, stack, timer, peripheral, clock and interrupt models on.
    pub model_stats: ModelStats,
    /// `Mem::code_gen` when `code_ok` was computed, and the result: the
    /// loaded code is what the generated blocks were made from.
    code_checked: u64,
    code_known: bool,
    code_ok: bool,
    /// With `code_ok` false: the entry PCs of the generated regions that touch
    /// a code range that no longer matches (sorted), or None for every entry.
    /// Other blocks keep running.
    code_barred: Option<Vec<u32>>,
    /// Per code range: the hash it last failed with and the PCs whose loaded
    /// instruction differs from the generated table (None: too many, or
    /// unknown), so a re-check with the same words decodes nothing.
    code_diffs: std::collections::HashMap<usize, (String, Option<Vec<u32>>)>,
    parity_logged: bool,
    /// Of the last `verify_code` that found a mismatch: the PCs whose loaded
    /// instruction differs from the generated table, the ranges barred as a
    /// whole (too many differing PCs, or none found), and the entry PCs barred.
    pub code_check: CodeCheck,
    /// The first PC past the generated image's code ranges: code at or above
    /// it was appended by a patch (counted in `model_stats.interp_appended`).
    appended_from: u32,
    /// Optional fast tier (src/fast): tried at its entry PCs before the
    /// generated block. None by default; behaviour is unchanged without it.
    pub fast: Option<Box<dyn fast::FastTier>>,
    /// The fast tier selects its own regions (`FastTier::note`); kept in
    /// step with `fast` by `set_fast`.
    fast_auto: bool,
    /// ... and also at generated-block dispatches.
    fast_auto_dispatch: bool,
    /// Static tier policy, cached by `set_fast` to keep ordinary dispatch cheap.
    fast_has_fallback: bool,
    fast_exclusively_fallback: bool,
    /// SHARC_FAST_HIST: interpreted instructions per PC (diagnostics).
    hist: Option<Box<std::collections::HashMap<u32, u64>>>,
    /// Opt-in raw generated-block call snapshots.  It is configured wholly
    /// through the environment and is absent in ordinary execution.
    live_capture: Option<live_capture::LiveCapture>,
    /// SHARC_WWATCH / SHARC_WWATCH_OUT (diagnostic): see `write_watch`.
    wwatch: Option<Box<write_watch::WriteWatch>>,
    /// SHARC_REPLY_INFLIGHT (diagnostic): see `reply_watch`.
    rwatch: Option<Box<reply_watch::ReplyWatch>>,
    /// SHARC_PCDUMP / SHARC_PCDUMP_OUT (diagnostic): see `pc_dump`.
    pcdump: Option<Box<pc_dump::PcDump>>,
    /// SHARC_CYCMIX (diagnostic): see `cycmix`.
    cycmix: Option<Box<cycmix::CycMix>>,
    /// SHARC_IRQ_PROBE (diagnostic): interrupts at arbitrary instruction boundaries, see `irq_probe`.
    irq_probe: Option<irq_probe::IrqProbe>,
    /// Any of `cov`, `hist`, `wwatch`, `pcdump` is set: the interpreter loop
    /// tests this one flag per instruction and nothing else when it is off.
    diag_on: bool,
    /// `wwatch` or `rwatch` is set: one flag test per dispatch unit, as with `wwatch` alone before.
    ww_any: bool,
}

/// The last code check that found a mismatch (`Engine::verify_code`).
#[derive(Clone, Copy, Debug, Default)]
pub struct CodeCheck {
    pub changed_pcs: u64,
    pub ranges_whole: u64,
    pub entries_barred: u64,
}

/// What a block call needs besides its function: the instruction limit that
/// keeps the core timer from expiring inside it, and the timer count to
/// advance afterwards.
struct BlockPlan {
    limit: u64,
    /// Healthy fallback-only tiers do not participate in this dispatch or chain.
    fast_active: bool,
    models: bool,
    timer_count: Option<u32>,
    /// A bank selection completed ahead of the block (its pending mask): it
    /// selects the banks already active, so completing it changes only the
    /// pending mask, which the block's first instruction would clear. Put
    /// back if the block completes no instruction.
    bank_pre: Option<Int>,
    /// A latched interrupt is deferred only by the active DO loop stack:
    /// block code must end the call at the instruction that empties it
    /// (`St::stop_at_loop_empty`), and the fast tier does not run.
    stop_loop_empty: bool,
}

/// Idle-skip counters (diagnostic).
#[derive(Clone, Copy, Debug, Default)]
pub struct IdleStats {
    /// Loop iterations replayed analytically, and the instructions they cover.
    pub iterations: u64,
    pub instructions: u64,
    /// Skips taken, and verified-but-empty / failed fixed-point checks.
    pub skips: u64,
    pub empty: u64,
    pub rejected: u64,
}

/// The architectural state at the idle loop head (everything an iteration
/// can change except the instruction count, the clock registers and TCOUNT).
struct IdleSnap {
    icount: u64,
    r: [V; rt::NUREG],
    bank_alt: [V; 96],
    masks: [Int; 3],
    loop_depth: Int,
    loop_slots: Vec<(V, V)>,
    pc_stack_pending: Int,
    pc_stack_requested: Int,
    special: [rt::Spec; 7],
    special_present: [bool; 7],
    pending: Option<rt::Pending>,
    steps: Int,
    at_loaded_entry: bool,
    loops: Vec<rt::Loop>,
    call_stack: Vec<Int>,
    pc_stack: Vec<Int>,
    status_stack: Vec<(V, V, V)>,
}

/// Register codes the idle skip treats analytically.
const R_EMUCLK: usize = 105;
const R_EMUCLK2: usize = 106;
const R_TPERIOD: usize = 110;
pub(crate) const R_TCOUNT: usize = 111;
const R_MODE2: usize = 116;
/// Longest loop (instructions) the idle skip will watch for a return to head.
const IDLE_MAX_LOOP: u64 = 512;

/// Swap the two bytes of every 16-bit unit: SPI words are MSB first on
/// the wire and little-endian in DSP memory.
fn swap16(data: &[u8]) -> Vec<u8> {
    let mut out = data.to_vec();
    for pair in out.chunks_exact_mut(2) {
        pair.swap(0, 1);
    }
    out
}

/// Name of a trap: the generated site table or the runtime's own codes.
pub fn trap_name(t: Trap) -> String {
    if t.0 >= TRAP_RT_BASE {
        return rt_trap_name(t).to_string();
    }
    #[cfg(sharc_gen)]
    {
        if let Some(site) = generated::tables::TRAP_SITES.get(t.0 as usize) {
            return site.to_string();
        }
    }
    format!("trap {}", t.0)
}

/// Execute one decoded instruction through the generated core, undoing it
/// if it traps.
#[inline(never)]
pub fn exec_insn(s: &mut St, insn: Insn) -> R<()> {
    #[cfg(sharc_gen)]
    {
        if s.cfg.core_timer {
            s.timer_written = false;
        }
        s.begin();
        match generated::core_g::forms::_execute(s, insn) {
            Ok(()) => {
                if s.probe.is_some() {
                    s.probe_note();
                }
                s.commit()
            }
            Err(t) => {
                s.rollback();
                return Err(t);
            }
        }
        if s.cfg.core_timer {
            // Peripheral time advances after a completed instruction. A
            // failed timer event rolls back itself, retaining that instruction.
            s.begin();
            match generated::core_g::sequencer::_core_timer_tick(s) {
                Ok(_) => s.commit_host(),
                Err(t) => {
                    s.rollback();
                    return Err(t);
                }
            }
        }
        Ok(())
    }
    #[cfg(not(sharc_gen))]
    {
        let _ = (s, insn);
        Err(TRAP_NO_INSN)
    }
}

/// The explicit_memory_model setting the image's block code was generated
/// for (true without an image: the default configuration).
pub fn gen_explicit_memory_model() -> bool {
    #[cfg(all(sharc_gen, sharc_image))]
    {
        generated::image::GEN_EXPLICIT_MEMORY_MODEL
    }
    #[cfg(not(all(sharc_gen, sharc_image)))]
    {
        true
    }
}

fn image_blocks() -> &'static [(u32, BlockFn)] {
    #[cfg(all(sharc_gen, sharc_image))]
    {
        generated::image::BLOCKS
    }
    #[cfg(not(all(sharc_gen, sharc_image)))]
    {
        &[]
    }
}

/// The name of the DSP image the linked generated core was made for (`dn2-1.11`; `none` without an
/// image). A binary with two cores picks the one whose name starts with the firmware's device id.
pub fn gen_image_name() -> &'static str {
    #[cfg(all(sharc_gen, sharc_image))]
    {
        generated::image::IMAGE
    }
    #[cfg(not(all(sharc_gen, sharc_image)))]
    {
        "none"
    }
}

/// The number of AOT blocks in the linked generated core (0: the generic core).
pub fn aot_block_count() -> usize {
    image_blocks().len()
}

fn image_model_safe() -> &'static [u32] {
    #[cfg(all(sharc_gen, sharc_image))]
    {
        generated::image::MODEL_SAFE
    }
    #[cfg(not(all(sharc_gen, sharc_image)))]
    {
        &[]
    }
}

fn image_loop_ends() -> &'static [i64] {
    #[cfg(all(sharc_gen, sharc_image))]
    {
        generated::image::LOOP_ENDS
    }
    #[cfg(not(all(sharc_gen, sharc_image)))]
    {
        &[]
    }
}

fn image_insn_at() -> fn(Int) -> Option<Insn> {
    #[cfg(all(sharc_gen, sharc_image))]
    {
        // Build host-only decode metadata while constructing the engine.
        // The first interpreter fallback must not parse the entire image
        // and allocate its instruction table on the audio render path.
        canon::insn_table(generated::image::INSN_BLOB);
        generated::image::insn_at
    }
    #[cfg(not(all(sharc_gen, sharc_image)))]
    {
        fn none(_pc: Int) -> Option<Insn> {
            None
        }
        none
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if let Some(prefix) = std::env::var_os("SHARC_PROFILE_PREFIX") {
            self.write_profile(std::path::Path::new(&prefix));
        }
        if let Some(p) = &self.irq_probe {
            eprintln!(
                "irq_probe: windows {:x?} mask {:#x} every {}: {} latched, {} interrupts taken ({})",
                p.windows,
                p.mask,
                p.every,
                p.latched,
                self.model_stats.irq_taken,
                self.model_stats
                    .irq_by_vector
                    .iter()
                    .enumerate()
                    .filter(|(_, n)| **n > 0)
                    .map(|(bit, n)| format!("bit {bit}: {n}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        if let Some(h) = &self.hist {
            let mut v: Vec<_> = h.iter().map(|(a, b)| (*b, *a)).collect();
            v.sort_unstable_by(|a, b| b.cmp(a));
            let tot: u64 = v.iter().map(|x| x.0).sum();
            eprintln!("hist: {} interpreted insns over {} PCs", tot, v.len());
            eprintln!(
                "helper calls: {}",
                fast::helper::CALLS.load(std::sync::atomic::Ordering::Relaxed)
            );
            for (n, pc) in v.iter().take(3000) {
                eprintln!("hist: {pc:#x} {n}");
            }
        }
        if let Some(f) = &self.fast
            && f.auto()
            && std::env::var_os("SHARC_FAST_LOG").is_some()
        {
            let line = f.engine_report(
                self.s.icount,
                self.stats.single_steps,
                self.stats.block_instructions,
            );
            eprintln!("{line}");
            let m = &self.model_stats;
            eprintln!(
                "fast engine: models gated {} unsafe {} irq_deferred {} timer_gated {} mmr_exits {} code_mismatch {}; idle skipped {}",
                m.gated,
                m.unsafe_block,
                m.irq_deferred,
                m.timer_gated,
                m.mmr_exits,
                m.code_mismatch,
                self.s
                    .icount
                    .saturating_sub(self.stats.single_steps + self.stats.block_instructions)
            );
        }
    }
}

impl Engine {
    pub fn new(mem: mem::Mem) -> Engine {
        let mut s = St::new(mem);
        s.insn_at = image_insn_at();
        s.loop_ends = image_loop_ends();
        let mut engine = Engine {
            s,
            halt: None,
            last_trap: None,
            // SHARC_AOT=0 runs without generated blocks (option 1 can still change it).
            use_blocks: std::env::var("SHARC_AOT").as_deref() != Ok("0"),
            instruction_clock: false,
            instruction_clock_base: 0,
            stop_software_interrupt: false,
            stop_pc: None,
            stop_range: None,
            stop_align: None,
            software_interrupts: false,
            export_ranges: false,
            dispatch: Dispatch::new(image_blocks(), image_model_safe())
                .without(&aot_deny_from_env()),
            stats: Stats::default(),
            cov: None,
            one_dispatch: false,
            exits: None,
            entry_bails: None,
            entries: None,
            trans: None,
            prof: None,
            #[cfg(feature = "region-profile")]
            region_profile: None,
            last_interp_next: -1,
            run_start: None,
            idle_head: None,
            idle_lo: 0,
            idle_hi: 0x00ff_ffff,
            idle_snap: None,
            ddr_monitor_gen: 0,
            idle_stats: IdleStats::default(),
            model_stats: ModelStats::default(),
            code_checked: 0,
            code_known: false,
            code_ok: false,
            code_barred: None,
            code_diffs: Default::default(),
            parity_logged: false,
            code_check: CodeCheck::default(),
            appended_from: u32::MAX,
            fast: None,
            fast_auto: false,
            fast_auto_dispatch: false,
            fast_has_fallback: false,
            fast_exclusively_fallback: false,
            hist: std::env::var_os("SHARC_FAST_HIST")
                .map(|_| Box::<std::collections::HashMap<u32, u64>>::default()),
            live_capture: live_capture::LiveCapture::from_env(),
            wwatch: write_watch::WriteWatch::from_env(),
            rwatch: reply_watch::ReplyWatch::from_env(),
            pcdump: pc_dump::PcDump::from_env(),
            cycmix: cycmix::CycMix::from_env(),
            irq_probe: irq_probe::IrqProbe::from_env(),
            diag_on: false,
            ww_any: false,
        };
        // Live-daemon profile for sharc_rsgen (docs/INSTRUMENTATION.md): same maps as option 26.
        if std::env::var_os("SHARC_PROFILE_PREFIX").is_some() {
            engine.set_option(26, 1);
        }
        engine.refresh_diag();
        engine
    }

    /// Recompute `diag_on` after any of the per-instruction diagnostics changed.
    fn refresh_diag(&mut self) {
        self.ww_any = self.wwatch.is_some() || self.rwatch.is_some();
        self.diag_on = self.cov.is_some()
            || self.hist.is_some()
            || self.wwatch.is_some()
            || self.rwatch.is_some()
            || self.pcdump.is_some()
            || self.cycmix.is_some();
    }

    /// Instruction counts by tier since the engine was created.
    pub fn tier_counters(&self) -> TierCounters {
        let mut t = TierCounters {
            icount: self.s.icount,
            idle_skipped: self.idle_stats.instructions,
            idle_skips: self.idle_stats.skips,
            interpreted: self.stats.single_steps,
            block_entries: self.stats.block_entries,
            block_instructions: self.stats.block_instructions,
            block_traps: self.stats.block_traps,
            traps: self.stats.traps,
            code_gen: self.s.mem.code_gen,
            code_verifies: self.model_stats.code_verifies,
            code_verify_ns: self.model_stats.code_verify_ns,
            code_mismatch: self.model_stats.code_mismatch,
            interp_appended: self.model_stats.interp_appended,
            ..TierCounters::default()
        };
        if let Some(f) = self.fast.as_ref().and_then(|f| f.fast_stats()) {
            t.fast_present = true;
            t.fast_insns = f.insns;
            t.fast_runs = f.runs;
            t.fast_calls = f.calls;
            t.fast_built = f.built;
            t.fast_refused = f.refused;
            t.fast_armed = f.armed;
            t.fast_compile_ns = f.compile_ns;
            t.fast_build_ns = f.build_ns;
            t.fast_declined = f.declined;
        }
        t
    }

    /// Enable or clear the opt-in per-entry dispatch profile.
    pub fn set_region_profile(&mut self, enabled: bool) {
        #[cfg(feature = "region-profile")]
        {
            self.region_profile = enabled.then(RegionProfile::default);
        }
        #[cfg(not(feature = "region-profile"))]
        let _ = enabled;
    }

    /// Cumulative per-entry dispatch costs, when profiling is enabled.
    pub fn region_profile(&self) -> Option<RegionProfile> {
        #[cfg(feature = "region-profile")]
        {
            self.region_profile.clone()
        }
        #[cfg(not(feature = "region-profile"))]
        {
            None
        }
    }

    /// Install (or remove) the fast tier.
    pub fn set_fast(&mut self, fast: Option<Box<dyn fast::FastTier>>) {
        self.fast_has_fallback = fast.as_ref().is_some_and(|f| f.has_fallback());
        self.fast_exclusively_fallback = fast.as_ref().is_some_and(|f| f.exclusively_fallback());
        self.fast_auto = fast.as_ref().is_some_and(|f| f.auto());
        self.fast_auto_dispatch = fast.as_ref().is_some_and(|f| f.auto_dispatch());
        self.fast = fast;
    }

    /// An engine over an image blob (tools/sharc_dev/sharc_transpile_run.py
    /// `pack_image`), memory at the loader image.
    pub fn from_image(bytes: &[u8]) -> Result<Engine, i32> {
        let img = canon::parse_image(bytes)?;
        let mut e = Engine::new(img.mem);
        e.s.named_mmrs = img.named;
        e.s.named_ranges = img.ranges;
        e.s.core_mmr_reset = img.core_reset;
        e.s.set_mmr_windows();
        e.s.mem.reset();
        if let Some(f) = fast::from_env() {
            e.set_fast(Some(Box::new(f)));
        }
        Ok(e)
    }

    /// Select runtime instruction decoding over the engine's loaded memory.
    /// The generated instruction table is dropped for this engine only.
    /// Generated blocks stay, but run only while the loaded code hashes to
    /// what they were generated from (`blocks_code_ok`: the generator's
    /// per-range SHA-256, re-checked when a store touches the code's pages),
    /// so a library made for another image never runs here. `read_sw` maps a
    /// short-word PC to its two little-endian loaded bytes and must return
    /// None for an unmapped word.
    pub fn enable_runtime_decode(&mut self, read_sw: fn(&mem::Mem, u32) -> Option<u16>) {
        self.s.runtime_decode = true;
        self.s.read_sw = read_sw;
        self.s.dec_watch = false;
        self.s.decode_cache.clear();
        self.s.insn_at = |_| None;
        self.watch_block_code();
    }

    /// Drop runtime-decoded metadata after the host replaces executable
    /// loader bytes (for example, after loader INIT blocks install main).
    pub fn invalidate_runtime_decode(&mut self) {
        self.s.decode_cache.clear();
    }

    /// Import a canonical state blob (sharc_native_import_state).
    pub fn import(&mut self, blob: &[u8]) -> Result<(), i32> {
        canon::import_state(&mut self.s, blob)?;
        self.invalidate_runtime_decode();
        self.halt = None;
        self.last_trap = None;
        Ok(())
    }

    /// Canonical state blob with every overlay byte as an explicit range,
    /// so `import` on a fresh engine over the same image restores it fully.
    /// The instruction clock is not part of it: carry `instruction_clock_base
    /// + s.icount` and pass it as option 6 to the importing engine.
    pub fn export(&self) -> Vec<u8> {
        canon::export_state(&self.s, true)
    }

    /// memory._dm_write(ADDRESS + k*WIDTH, WIDTH, value) per WIDTH-byte
    /// chunk of DATA (a host poke). Returns how many took effect.
    pub fn poke(&mut self, address: u64, data: &[u8], width: u32) -> i32 {
        let w = width.max(1) as usize;
        let mut ok = 0;
        for (k, chunk) in data.chunks(w).enumerate() {
            let mut v: u32 = 0;
            for (i, &b) in chunk.iter().enumerate() {
                v |= (b as u32) << (8 * i);
            }
            let addr = VI::I(address as Int + (k * w) as Int);
            self.s.begin();
            match rt::bnd::_dm_write(&mut self.s, addr, chunk.len() as Int, V::c(v as Int), false) {
                Ok(true) => {
                    self.s.commit_host();
                    ok += 1
                }
                Ok(false) => self.s.commit_host(),
                Err(_) => self.s.rollback(),
            }
        }
        ok
    }

    /// Host write of raw bytes at ADDRESS exactly (no short-word alias
    /// redirection, unlike `poke` on an absent address), making them present.
    /// For new code or data in L2 that nothing loaded: `read_sw` and plain
    /// data reads both look at the bytes at their own address. The decode
    /// cache is dropped.
    pub fn poke_raw(&mut self, address: u32, data: &[u8]) {
        for (k, &b) in data.iter().enumerate() {
            self.s.mem.write_byte(address.wrapping_add(k as u32), b);
        }
        self.invalidate_runtime_decode();
    }

    /// memory._dm_read(ADDRESS, WIDTH): Ok(None) when unknown, Err on an
    /// unmodelled MMR.
    pub fn peek(&self, address: u64, width: u32) -> Result<Option<u32>, Trap> {
        rt::bnd::_dm_read(&self.s, VI::I(address as Int), width as Int, false, false)
            .map(|v| v.map(|v| v.b))
    }

    /// A host-side peripheral event in its own transaction: all or nothing.
    pub fn host_event<T>(
        &mut self,
        f: impl FnOnce(&mut rt::St) -> Result<T, Trap>,
    ) -> Result<T, Trap> {
        self.s.begin();
        match f(&mut self.s) {
            Ok(v) => {
                self.s.commit_host();
                Ok(v)
            }
            Err(t) => {
                self.s.rollback();
                Err(t)
            }
        }
    }

    /// tools/sharc_dev/sharc_periph_host.spi2_exchange: FRAME (wire order) lands in
    /// the SPI2 RX DMA work unit; the TX work unit comes back in wire order;
    /// both channels then complete (SEC sources 69 and 70).
    pub fn spi2_exchange(&mut self, frame: &[u8]) -> Result<Vec<u8>, Trap> {
        const TX: u32 = 0x3102_D200;
        const RX: u32 = 0x3102_D280;
        let rx = self.host_event(|s| rt::periph::dma_start(s, RX))?;
        let tx = self.host_event(|s| rt::periph::dma_start(s, TX))?;
        for base in [RX, TX] {
            let step = self.s.mmr_get(base + 0x10).map(|v| v.b);
            let count = self.s.mmr_get(base + 0x0C).map(|v| v.b as usize);
            if step != Some(2) || count.map(|c| 2 * c) != Some(frame.len()) {
                return Err(rt::TRAP_PERIPHERAL);
            }
        }
        if frame.len() % 4 != 0 {
            return Err(rt::TRAP_PERIPHERAL);
        }
        let mut reply = Vec::with_capacity(frame.len());
        for offset in (0..frame.len()).step_by(4) {
            let word = rt::periph::ram_word(&self.s, tx.wrapping_add(offset as u32))?;
            reply.extend_from_slice(&word.to_le_bytes());
        }
        for (k, &byte) in swap16(frame).iter().enumerate() {
            self.s.mem.write_byte(rx.wrapping_add(k as u32), byte);
        }
        self.host_event(|s| rt::periph::dma_done(s, TX, 69))?;
        self.host_event(|s| rt::periph::dma_done(s, RX, 70))?;
        if self.rwatch.is_some() {
            let reg = |s: &rt::St, o: u32| s.mmr_get(TX + o).map_or(0, |v| v.b as u32);
            let dma = [reg(&self.s, 0x08), reg(&self.s, 0x0C), reg(&self.s, 0x10)];
            if let Some(r) = &mut self.rwatch {
                r.arm(self.s.icount, tx, frame.len(), dma, &self.s.mem);
            }
        }
        Ok(swap16(&reply))
    }

    /// Whether link port 0's receive DMA is waiting for a work unit: enabled
    /// in stop flow with RUN set (the firmware's driver arms it, the previous
    /// unit's completion clears RUN). Always false before the firmware opens
    /// the port.
    pub fn lp0_ready(&self) -> bool {
        use rt::periph::LP0_DMA;
        let reg = |a: u32| self.s.mmr_get(a).filter(|v| v.is_c()).map(|v| v.b);
        reg(LP0_DMA + 0x08).is_some_and(|cfg| cfg & 1 != 0 && (cfg >> 12) & 7 == 0)
            && reg(LP0_DMA + 0x30).is_some_and(|stat| stat & 0x700 != 0)
    }

    /// One link port 0 work unit (ADSP-2156x HWR chapter 14): BYTES, the
    /// latched byte stream of exactly XCNT 32-bit words (little-endian
    /// packing, the first byte of a word in its low bits), lands at the
    /// channel's start address and the DMA completes (SEC source 81). The
    /// firmware's own ISR and callback then run as the engine steps. Traps
    /// unless [`Self::lp0_ready`] and the unit size matches the byte count.
    pub fn lp0_transfer(&mut self, bytes: &[u8]) -> Result<(), Trap> {
        use rt::periph::{LP0_DMA, SID_LP0_DMA, dma_done, dma_start};
        let start = self.host_event(|s| dma_start(s, LP0_DMA))?;
        let reg = |s: &rt::St, a: u32| s.mmr_get(a).filter(|v| v.is_c()).map(|v| v.b);
        let xcnt = reg(&self.s, LP0_DMA + 0x0C).ok_or(rt::TRAP_PERIPHERAL)?;
        if reg(&self.s, LP0_DMA + 0x10) != Some(4) || xcnt as usize * 4 != bytes.len() {
            return Err(rt::TRAP_PERIPHERAL);
        }
        for (k, &byte) in bytes.iter().enumerate() {
            self.s.mem.write_byte(start.wrapping_add(k as u32), byte);
        }
        self.host_event(|s| dma_done(s, LP0_DMA, SID_LP0_DMA))
    }

    /// tools/sharc_dev/sharc_periph_host.sport_block: one audio block through SPORT4A
    /// (output, DMA10) and SPORT4B (input, DMA11). Ok(None) while the SPORTs
    /// are not running (nothing changes). BLOCK None means zeros. All or
    /// nothing: a rejected block leaves the state untouched.
    pub fn sport_block(&mut self, block: Option<&[u8]>) -> Result<Option<Vec<u8>>, Trap> {
        use rt::periph::{
            SID_SPORT4A_DMA, SID_SPORT4B_DMA, SPORT4A_DMA, SPORT4B_DMA, dma_done, dma_start,
            ram_word, sport_running,
        };
        // The unit's size in bytes: contiguous 32-bit words, TX reads memory
        // and RX writes it (DMA_CFG.WNR).
        fn unit(s: &rt::St, base: u32, writes: bool) -> Result<usize, Trap> {
            let reg = |a: u32| s.mmr_get(a).filter(|v| v.is_c()).map(|v| v.b);
            let cfg = reg(base + 0x08).ok_or(rt::TRAP_PERIPHERAL)?;
            let size = 1u32 << ((cfg >> 4) & 7);
            if (cfg & 2 != 0) != writes || reg(base + 0x10) != Some(size) || size != 4 {
                return Err(rt::TRAP_PERIPHERAL);
            }
            let count = reg(base + 0x0C).ok_or(rt::TRAP_PERIPHERAL)?;
            Ok(count as usize * 4)
        }
        self.s.begin();
        let result = (|| {
            let s = &mut self.s;
            if !sport_running(s)? {
                return Ok(None);
            }
            let tx = dma_start(s, SPORT4A_DMA)?;
            let rx = dma_start(s, SPORT4B_DMA)?;
            let out_size = unit(s, SPORT4A_DMA, false)?;
            let in_size = unit(s, SPORT4B_DMA, true)?;
            if block.is_some_and(|b| b.len() != in_size) {
                return Err(rt::TRAP_PERIPHERAL);
            }
            let mut reply = Vec::with_capacity(out_size);
            for offset in (0..out_size).step_by(4) {
                let word = ram_word(s, tx.wrapping_add(offset as u32))?;
                reply.extend_from_slice(&word.to_le_bytes());
            }
            dma_done(s, SPORT4A_DMA, SID_SPORT4A_DMA)?;
            dma_done(s, SPORT4B_DMA, SID_SPORT4B_DMA)?;
            // Memory writes are not journaled: nothing may fail after this.
            for k in 0..in_size {
                let byte = block.map_or(0, |b| b[k]);
                s.mem.write_byte(rx.wrapping_add(k as u32), byte);
            }
            Ok(Some(reply))
        })();
        match result {
            Ok(v) => {
                self.s.commit_host();
                Ok(v)
            }
            Err(t) => {
                self.s.rollback();
                Err(t)
            }
        }
    }

    /// sharc_run.fresh_call_state (see sharc_native_fresh_call).
    pub fn fresh_call(&mut self, pc: u32, return_address: Option<Int>) {
        self.s.pc_sw = pc as Int;
        self.s.loops.clear();
        self.s.call_stack.clear();
        self.s.pc_stack.clear();
        self.s.pc_stack_pending = -1;
        self.s.pc_stack_requested = -1;
        if let Some(r) = return_address {
            let _ = self.s.call_stack.push_raw(r);
            if self.s.cfg.stack_model {
                let _ = self.s.pc_stack.push_raw(0x0100_0000 | (r & 0x00ff_ffff));
            }
        }
        self.s.status_stack.clear();
        self.s.pending = None;
        #[cfg(sharc_gen)]
        if self.s.cfg.stack_model {
            let _ = generated::core_g::state::_sync_pc_stack(&mut self.s);
            let _ = generated::core_g::state::_sync_status_stack(&mut self.s);
            self.s.commit_host();
        }
        self.halt = None;
        self.last_trap = None;
    }

    /// A host register poke (sharc_native_set_reg).
    pub fn set_reg(&mut self, code: usize, v: V) {
        self.s.r[code] = v;
    }

    /// sharc_native_set_option.
    pub fn set_option(&mut self, key: u32, value: i64) -> i32 {
        match key {
            1 => self.use_blocks = value != 0,
            2 => self.export_ranges = value != 0,
            4 => {
                if value == 0 {
                    return -1;
                }
                self.enable_runtime_decode(mem::Mem::read_sw);
                self.s.dec_watch = true;
            }
            5 => self.instruction_clock = value != 0,
            26 => {
                // Coverage, entries, transitions and block exits for
                // tools/sharc_dev/sharc_rsgen.py (read back by sharc_native_profile).
                if value != 0 {
                    self.cov.get_or_insert_with(Default::default);
                    self.entries.get_or_insert_with(Default::default);
                    self.trans.get_or_insert_with(Default::default);
                    self.exits.get_or_insert_with(Default::default);
                    self.entry_bails.get_or_insert_with(Default::default);
                } else {
                    self.cov = None;
                    self.entries = None;
                    self.trans = None;
                    self.exits = None;
                    self.entry_bails = None;
                }
                self.refresh_diag();
            }
            7 => self.stop_software_interrupt = value != 0,
            9 => self.software_interrupts = value != 0,
            8 => {
                if value == -1 {
                    self.stop_pc = None;
                } else if (0..=0x00ff_ffff).contains(&value) {
                    self.stop_pc = Some(value as u32);
                } else {
                    return -1;
                }
            }
            23 => {
                if value == -1 {
                    self.idle_head = None;
                } else if (0..=0x00ff_ffff).contains(&value) {
                    self.idle_head = Some(value as u32);
                } else {
                    return -1;
                }
                self.idle_snap = None;
            }
            24 | 25 => {
                if !(0..=0x00ff_ffff).contains(&value) {
                    return -1;
                }
                if key == 24 {
                    self.idle_lo = value as u32;
                } else {
                    self.idle_hi = value as u32;
                }
            }
            6 => {
                if value < 0 {
                    return -1;
                }
                self.instruction_clock_base = value as u64;
            }
            3 => {
                for i in 0..7 {
                    self.s.special_present[i] = value & (1 << i) != 0;
                }
            }
            _ => return canon::set_option(&mut self.s, key, value),
        }
        0
    }

    fn software_interrupt_candidate(&self) -> Option<u32> {
        if self.s.pending.is_some() || !self.s.loops.items().is_empty() {
            return None;
        }
        let [mode, latch, mask, priority] = [114, 122, 123, 124].map(|code| self.s.r[code]);
        if ![mode, latch, mask, priority]
            .iter()
            .all(|value| value.is_c())
            || mode.b & (1 << 12) == 0
        {
            return None;
        }
        let mut candidates = latch.b & mask.b & 0xf000_0000;
        if priority.b != 0 {
            if mode.b & (1 << 11) == 0 {
                return None;
            }
            candidates &= ((1u64 << priority.b.trailing_zeros()) - 1) as u32;
        }
        (candidates != 0).then(|| candidates.trailing_zeros())
    }

    /// Whether any run-time model beyond the plain interpreter's is on.
    fn models_active(&self) -> bool {
        let c = &self.s.cfg;
        c.bank_model
            || c.stack_model
            || c.core_timer
            || c.peripheral_model
            || self.instruction_clock
            || self.software_interrupts
    }

    /// The loaded code is what the generated blocks came from. Without
    /// runtime decoding the image's own table is the code, as always. With
    /// it, the generator's per-range SHA-256 of the short words (and the words
    /// the decoder looks ahead at) is compared with the engine's memory,
    /// again whenever a store touches a watched page.
    fn blocks_code_ok(&mut self) -> bool {
        if !self.s.runtime_decode {
            return true;
        }
        if self.code_known && self.code_checked == self.s.mem.code_gen {
            return self.code_ok;
        }
        self.code_checked = self.s.mem.code_gen;
        self.code_known = true;
        let started = std::time::Instant::now();
        self.code_ok = self.verify_code();
        self.model_stats.code_verifies += 1;
        self.model_stats.code_verify_ns += started.elapsed().as_nanos() as u64;
        self.code_ok
    }

    /// Check the loaded code against the generator's per-range hashes; with
    /// per-region data, record which entries a mismatch bars (`code_barred`)
    /// and end chains at them. A range whose hash differs is narrowed down:
    /// the loaded words are decoded and compared with the generated
    /// instruction table, and only the regions with a block that holds a
    /// differing PC are barred (`code_diff`). A range that cannot be narrowed
    /// down (too many differing PCs, or none) bars all its regions.
    fn verify_code(&mut self) -> bool {
        #[cfg(all(sharc_gen, sharc_image))]
        {
            let ranges = generated::image::CODE_RANGES;
            self.appended_from = ranges.iter().map(|r| r.0 + r.1).max().unwrap_or(u32::MAX);
            let mut bad = Vec::new();
            for (i, &(start, len, want)) in ranges.iter().enumerate() {
                let mut bytes = Vec::with_capacity(len as usize * 2);
                for k in 0..len {
                    match self.s.mem.read_sw(start + k) {
                        Some(w) => bytes.extend_from_slice(&w.to_le_bytes()),
                        None => bytes.extend_from_slice(&[0xDE, 0xAD, 0xBE]),
                    }
                }
                let mut h = sha256::Sha256::new();
                h.update(&bytes);
                let got = frames::hex(&h.finish());
                if got != want {
                    bad.push((i, got));
                }
            }
            if bad.is_empty() {
                if !self.parity_logged && std::env::var_os("SHARC_CODE_PARITY").is_some() {
                    self.parity_logged = true;
                    let (checked, n, first) = self.code_parity();
                    eprintln!("code parity: {checked} PCs compared, {n} differ {first:x?}");
                }
                self.code_barred = Some(Vec::new());
                return true;
            }
            #[cfg(sharc_region_entries)]
            {
                let mut whole = Vec::new();
                let mut changed = Vec::new();
                for (i, got) in bad {
                    match self.range_changes(i, got) {
                        Some(pcs) if !pcs.is_empty() => changed.extend(pcs),
                        _ => whole.push(i),
                    }
                }
                changed.sort_unstable();
                let range_of = |pc: u32| {
                    let i = ranges.partition_point(|r| r.0 <= pc).checked_sub(1)?;
                    (pc < ranges[i].0 + ranges[i].1).then_some(i)
                };
                let barred = code_diff::barred_entries(
                    generated::image::REGION_ENTRIES,
                    &changed,
                    |pc| range_of(pc).is_none_or(|i| whole.contains(&i)),
                );
                for &pc in &barred {
                    self.s.chain_stop_add(pc);
                }
                self.code_check = CodeCheck {
                    changed_pcs: changed.len() as u64,
                    ranges_whole: whole.len() as u64,
                    entries_barred: barred.len() as u64,
                };
                if std::env::var_os("SHARC_CODE_LOG").is_some() {
                    eprintln!(
                        "code check: {} differing PCs {:x?}, {} ranges barred whole {:x?}, {} entries barred",
                        changed.len(),
                        &changed[..changed.len().min(32)],
                        whole.len(),
                        whole.iter().map(|&i| ranges[i].0).collect::<Vec<_>>(),
                        barred.len()
                    );
                }
                self.code_barred = Some(barred);
            }
            #[cfg(not(sharc_region_entries))]
            {
                self.code_barred = None;
            }
            false
        }
        #[cfg(not(all(sharc_gen, sharc_image)))]
        {
            // No generated image: there is no block code to protect.
            true
        }
    }

    /// The PCs of code range RANGE (hash GOT, which differs from the
    /// generator's) where the loaded instruction is not the generated one;
    /// None when there are too many. Remembered per range and hash.
    #[cfg(all(sharc_gen, sharc_image, sharc_region_entries))]
    fn range_changes(&mut self, range: usize, got: String) -> Option<Vec<u32>> {
        if let Some((hash, pcs)) = self.code_diffs.get(&range)
            && *hash == got
        {
            return pcs.clone();
        }
        let (start, len, _) = generated::image::CODE_RANGES[range];
        let pcs = self.changed_pcs_in(start, len, code_diff::MAX_CHANGED);
        self.code_diffs.insert(range, (got, pcs.clone()));
        pcs
    }

    #[cfg(all(sharc_gen, sharc_image))]
    fn changed_pcs_in(&mut self, start: u32, len: u32, cap: usize) -> Option<Vec<u32>> {
        let s = &mut self.s;
        code_diff::changed_pcs(
            start,
            len,
            |pc| generated::image::insn_at(pc as Int).map(code_diff::InsnKey),
            |pc| {
                rt::bnd::decode_at(s, (), None, pc as Int)
                    .ok()
                    .map(code_diff::InsnKey)
            },
            cap,
        )
    }

    /// Compare the loaded code of every generated range with the generated
    /// instruction table: (the number of PCs compared, the number that
    /// differ, the first 64 of those). On an unpatched image none differ.
    #[cfg(all(sharc_gen, sharc_image))]
    pub fn code_parity(&mut self) -> (usize, usize, Vec<u32>) {
        let mut diffs = Vec::new();
        let mut checked = 0;
        for &(start, len, _) in generated::image::CODE_RANGES {
            checked += (start..start + len)
                .filter(|&pc| generated::image::insn_at(pc as Int).is_some())
                .count();
            diffs.extend(self.changed_pcs_in(start, len, usize::MAX - 1).unwrap_or_default());
        }
        let n = diffs.len();
        diffs.truncate(64);
        (checked, n, diffs)
    }

    /// Watch the pages that hold generated block code (runtime decoding).
    fn watch_block_code(&mut self) {
        #[cfg(all(sharc_gen, sharc_image))]
        {
            self.s.mem.unwatch_code();
            for &(start, len, _) in generated::image::CODE_RANGES {
                let (lo, hi) = (start, start + len);
                self.s
                    .mem
                    .watch_code(0x2800_0000 + lo * 2, 0x2800_0000 + hi * 2);
                if lo < 0xc0_0000 && hi > 0xb8_0000 {
                    let (a, b) = (lo.max(0xb8_0000), hi.min(0xc0_0000));
                    self.s.mem.watch_code(
                        0x2000_0000 + (a - 0xb8_0000) * 2,
                        0x2000_0000 + (b - 0xb8_0000) * 2,
                    );
                }
            }
            self.code_known = false;
        }
    }

    /// An interrupt source the engine delivers is latched, enabled and not
    /// blocked by priority, ignoring the delay-slot and loop deferral that
    /// holds it back at this boundary. None: a control register is not
    /// concrete. No block may run while this is true: the deferral could end
    /// at an instruction boundary inside it.
    fn latent_interrupt(&self) -> Option<bool> {
        let [mode, latch, mask, active] = [114, 122, 123, 124].map(|c| self.s.r[c]);
        if ![mode, latch, mask, active].iter().all(|v| v.is_c()) {
            return None;
        }
        let mut allowed = 0u32;
        if self.software_interrupts {
            allowed |= 0xf000_0000;
        }
        if self.s.cfg.core_timer {
            allowed |= 0x0040_0800;
        }
        if self.s.cfg.peripheral_model {
            allowed |= 0x0000_8000;
        }
        if mode.b & 0x1000 == 0 {
            return Some(false);
        }
        let mut candidates = latch.b & mask.b & allowed;
        if active.b != 0 {
            if mode.b & 0x800 == 0 {
                return Some(false);
            }
            candidates &= (active.b & active.b.wrapping_neg()).wrapping_sub(1);
        }
        Some(candidates != 0)
    }

    /// Fallback-only regions replace actual AOT entries barred by code verification.
    /// Recheck freshness here because a preceding kernel may have stored code.
    fn fast_allowed(&mut self, pc: u32) -> bool {
        if !self.fast_has_fallback {
            return self.fast.is_some();
        }
        if !self.fast_exclusively_fallback
            && self.fast.as_ref().is_some_and(|f| !f.fallback_only(pc))
        {
            return true;
        }
        self.use_blocks
            && self.dispatch.get_entry(pc as Int).is_some()
            && !self.blocks_code_ok()
            && self
                .code_barred
                .as_ref()
                .is_none_or(|barred| barred.binary_search(&pc).is_ok())
    }

    /// The block to run at the current PC and its plan, or None to
    /// interpret one instruction. Without a model on, any block runs. With
    /// one, a model-safe block runs only while nothing the models update
    /// between instructions can change inside it: no bank or PC-stack change
    /// in flight, no interrupt source that could become deliverable, the
    /// core timer at least two ticks from expiry (the block is limited to
    /// the ticks before that), and no peripheral store (those leave the block
    /// as TRAP_BLOCK_MODEL).
    fn block_plan(&mut self, limit: u64) -> Option<(Option<Entry>, BlockPlan)> {
        if !(self.s.cfg.block_base_ok
            && !self.stop_software_interrupt
            && self.stop_pc.is_none()
            && self.s.probe.is_none())
        {
            return None;
        }
        // Generated blocks assume every active loop ends at a PC the
        // generator saw; the fast tier does not (it re-derives loops from
        // the loaded code), so a loop it does not know only bars the blocks.
        let loops_ok = self.s.loops_ok;
        if self.idle_head.is_some()
            && (self.idle_lo as Int..=self.idle_hi as Int).contains(&self.s.pc_sw)
        {
            return None;
        }
        // A PC the fast tier handles needs no generated block (firmware-free
        // mode): the same gates apply, the model-safety of its region being
        // established by the fast tier's own register whitelist.
        // `use_blocks` (SHARC_AOT=0) turns off the generated blocks only; the
        // fast tier does not depend on them (fast-only measurement mode).
        // Blocks also need the loaded code to be the code they were generated
        // from; the fast tier checks its own code words (`verify_code` in
        // fast/mod.rs), so a patched image still runs there.
        let code_ok = self.blocks_code_ok();
        let mut entry = if loops_ok && self.use_blocks {
            self.dispatch.get_entry(self.s.pc_sw)
        } else {
            None
        };
        if !code_ok
            && entry.is_some_and(|_| {
                self.code_barred.as_ref().is_none_or(|b| {
                    b.binary_search(&(self.s.pc_sw as u32)).is_ok()
                })
            })
        {
            entry = None;
            self.model_stats.code_mismatch += 1;
        }
        let fast_active = self.fast.is_some() && !(self.fast_exclusively_fallback && code_ok);
        let fast_pc = fast_active
            && self
                .fast
                .as_ref()
                .is_some_and(|f| f.wants(self.s.pc_sw as u32))
            && self.fast_allowed(self.s.pc_sw as u32);
        if entry.is_none() && !fast_pc {
            return None;
        }
        let mut plan = BlockPlan {
            limit,
            fast_active,
            models: false,
            timer_count: None,
            bank_pre: None,
            stop_loop_empty: false,
        };
        if self.models_active() {
            if entry.is_some_and(|e| !e.model_safe) {
                if !fast_pc {
                    self.model_stats.unsafe_block += 1;
                    return None;
                }
                // Only the fast tier may run here.
                entry = None;
            }
            let s = &self.s;
            if s.cfg.bank_model
                && s.bank_requested_mask < 0
                && s.bank_pending_mask >= 0
                && s.bank_pending_mask == s.bank_active_mask
            {
                // St::bank_complete would swap nothing: only the pending
                // mask changes (step completes it ahead of the block).
                plan.bank_pre = Some(s.bank_pending_mask);
            }
            if (s.cfg.bank_model
                && ((s.bank_pending_mask >= 0 && plan.bank_pre.is_none())
                    || s.bank_requested_mask >= 0))
                || (s.cfg.stack_model && (s.pc_stack_pending >= 0 || s.pc_stack_requested >= 0))
            {
                self.model_stats.timer_gated += 1;
                return None;
            }
            if self.software_interrupts || s.cfg.core_timer || s.cfg.peripheral_model {
                match self.latent_interrupt() {
                    Some(false) => {}
                    // A latched, unmasked interrupt that only an active DO
                    // loop defers (the interpreter delivers none while the
                    // loop stack is non-empty or a branch is pending).
                    // Invariant: such a block may run, but its call must end
                    // at the instruction boundary where the loop stack
                    // becomes empty, which is where the interpreter would
                    // deliver the interrupt. Generated code checks
                    // `s.stop_at_loop_empty && s.loops.n == 0` after every
                    // instruction at a loop end and exits as a budget exit
                    // does (state written back); the fast tier is not run
                    // under this plan (`stop_loop_empty`), and a call chain
                    // is made of such blocks. Only a pop at a loop end can
                    // empty the stack in a model-safe block (loop aborts,
                    // PUSH/POP and LADDR/CURLCNTR writes are not).
                    Some(true) if self.s.pending.is_none() && self.s.loops.n != 0 => {
                        plan.stop_loop_empty = true;
                    }
                    _ => {
                        self.model_stats.irq_deferred += 1;
                        return None;
                    }
                }
            }
            if self.s.cfg.core_timer {
                let (mode, count) = (self.s.r[R_MODE2], self.s.r[R_TCOUNT]);
                if !mode.is_c() {
                    return None;
                }
                if mode.b & 0x20 != 0 {
                    if !count.is_c() || count.b < 2 {
                        self.model_stats.timer_gated += 1;
                        return None;
                    }
                    plan.limit = plan.limit.min(self.s.icount + count.b as u64 - 1);
                    plan.timer_count = Some(count.b);
                }
            }
            plan.models = true;
            self.model_stats.gated += 1;
        }
        Some((entry, plan))
    }

    /// After a block call: the models' per-instruction effects the block
    /// does not apply. Its K completed instructions each ticked the core
    /// timer (the plan kept it from expiring) and set EMUCLK for the next;
    /// the interpreter sets EMUCLK at the start of every instruction, so the
    /// value left behind is the last one's.
    fn block_models_after(&mut self, plan: &BlockPlan, before: u64) {
        let k = self.s.icount - before;
        if k == 0 {
            return;
        }
        if let Some(count) = plan.timer_count {
            self.s.r[R_TCOUNT] = V::c((count as u64 - k) as Int);
        }
        if self.instruction_clock {
            let tick = self.instruction_clock_base.wrapping_add(self.s.icount - 1);
            self.s.r[R_EMUCLK] = V::c((tick as u32) as Int);
            self.s.r[R_EMUCLK2] = V::c((tick >> 32) as Int);
        }
    }

    /// The stop range (`step_until_in`) holds the current PC at an instruction
    /// count where it stops (every count, or an aligned one).
    fn at_stop_range(&self) -> bool {
        let Some((lo, hi)) = self.stop_range else {
            return false;
        };
        (lo as Int..hi as Int).contains(&self.s.pc_sw)
            && self.stop_align.is_none_or(|(chunk, base)| {
                let d = self.s.icount - base;
                d > 0 && d % chunk as u64 == 0
            })
    }

    /// A fast-tier kernel just exited normally at the PC of another fast-tier
    /// region: run that one without going back to the dispatch loop, while
    /// every gate `step` would apply between the two still holds. Between
    /// kernels nothing can change them: a kernel retires lowered instructions
    /// only (R/I/M registers, flags, plain RAM through checked windows), so
    /// MODE1, IRPTL, IMASK, IMASKP, the models' state, the stack and bank
    /// requests are as the first kernel's entry checks found them; the budget
    /// (`limit`, with the core timer's bound folded in by `block_plan`) is an
    /// absolute instruction count that each kernel checks against `s.limit`.
    /// The checks that cannot be assumed are repeated here: the stop and idle
    /// settings, the latent interrupt, the code check. A region that declines
    /// leaves the state untouched and is dispatched normally.
    fn fast_continue(&mut self, limit: u64, mut code: Option<u32>) -> Option<u32> {
        if self.one_dispatch
            || self.stop_pc.is_some()
            || self.stop_software_interrupt
            || self.s.probe.is_some()
        {
            return code;
        }
        while code == Some(EXIT_NEXT) && self.s.icount < limit && self.s.pending.is_none() {
            let pc = self.s.pc_sw;
            if self.at_stop_range() {
                break;
            }
            if self.idle_head.is_some() && (self.idle_lo as Int..=self.idle_hi as Int).contains(&pc)
            {
                break;
            }
            if (self.fast_exclusively_fallback && self.blocks_code_ok())
                || self.fast.as_ref().is_none_or(|f| !f.wants(pc as u32))
                || !self.fast_allowed(pc as u32)
            {
                break;
            }
            if self.models_active() {
                let s = &self.s;
                if (s.cfg.bank_model && (s.bank_pending_mask >= 0 || s.bank_requested_mask >= 0))
                    || (s.cfg.stack_model
                        && (s.pc_stack_pending >= 0 || s.pc_stack_requested >= 0))
                {
                    break;
                }
                if (self.software_interrupts || s.cfg.core_timer || s.cfg.peripheral_model)
                    && self.latent_interrupt() != Some(false)
                {
                    break;
                }
            }
            let Some(next) = self
                .fast
                .as_mut()
                .and_then(|f| f.run_chained(&mut self.s, pc as u32))
            else {
                break;
            };
            code = Some(next);
        }
        code
    }

    fn trapped(&mut self, t: Trap) {
        self.stats.traps += 1;
        self.last_trap = Some(t);
        self.halt = Some(format!(
            "native-trap: {} at {:#x}",
            trap_name(t),
            self.s.pc_sw
        ));
    }

    /// The per-instruction diagnostics, off the hot path (`diag_on`).
    #[cold]
    #[inline(never)]
    fn diag_step(&mut self, pc_before: Int) {
        if let Some(cov) = &mut self.cov {
            let m = self.s.r[MODE1];
            *cov.entry((self.s.pc_sw as u32, m.b, m.is_c())).or_default() += 1;
            if let Some(en) = &mut self.entries {
                let pc = self.s.pc_sw;
                if pc != self.last_interp_next {
                    *en.entry(pc as u32).or_default() += 1;
                    self.run_start = Some(pc);
                } else if let Some(start) = self.run_start.take()
                    && self.use_blocks
                    && self.dispatch.get_entry(start).is_some()
                    && self.dispatch.get_entry(pc).is_none()
                {
                    // A block entry the model gate sent to the interpreter
                    // (a pending bank swap, a timer about to tick): the
                    // interpreter then runs on through the block's body,
                    // which has no entry at the next instruction. That PC is
                    // an entry candidate for the generator.
                    *en.entry(pc as u32).or_default() += 1;
                }
            }
        }
        if let Some(h) = &mut self.hist {
            *h.entry(pc_before as u32).or_default() += 1;
        }
        if self.ww_on() {
            self.ww_begin(pc_before as u64, 'i');
        }
        if let Some(d) = &mut self.pcdump {
            d.hit(&self.s, pc_before as u64, self.s.icount);
        }
        if let Some(c) = &mut self.cycmix {
            c.idle = (self.idle_lo, self.idle_hi, self.idle_head.unwrap_or(0));
            c.hit(&self.s, pc_before as u32);
        }
    }

    /// Close the pending write-watch unit (see `write_watch`): flush any
    /// earlier one, then start a new one at PC with the given unit letter.
    fn ww_begin(&mut self, pc: u64, unit: char) {
        self.ww_flush();
        if let Some(w) = &mut self.wwatch {
            w.pending = Some((pc, unit));
        }
        if let Some(r) = &mut self.rwatch {
            r.pending = Some((pc, unit));
        }
    }

    /// A write-watch or the reply-record check is on.
    #[inline(always)]
    fn ww_on(&self) -> bool {
        self.ww_any
    }

    /// Re-read the watched words for the pending unit and log changes.
    #[inline(always)]
    fn ww_flush(&mut self) {
        if self.ww_on() {
            self.ww_flush_pending();
        }
    }

    #[inline(never)]
    fn ww_flush_pending(&mut self) {
        if let Some(mut w) = self.wwatch.take() {
            if w.pending.is_some() {
                let now: Vec<Option<u32>> = w
                    .addrs()
                    .iter()
                    .map(|&a| self.peek(a, 4).ok().flatten())
                    .collect();
                w.record(self.s.icount, &now);
            }
            self.wwatch = Some(w);
        }
        if let Some(r) = &mut self.rwatch {
            r.check(self.s.icount, &self.s.mem);
            if let Some(why) = r.failure.take() {
                self.halt = Some(why);
                self.one_dispatch = true;
            }
        }
    }

    /// Run up to N instructions. Returns how many completed; fewer means a
    /// trap (see `halt`).
    pub fn step(&mut self, n: u32) -> u32 {
        if self.halt.is_some() {
            return 0;
        }
        let start = self.s.icount;
        let limit = start + n as u64;
        self.idle_snap = None;
        self.s.probe = None;
        'steploop: while self.s.icount < limit {
            self.ww_flush();
            if self.at_stop_range() {
                break;
            }
            if self.stop_pc.is_some_and(|pc| self.s.pc_sw == pc as Int) {
                self.halt = Some("diagnostic: PC breakpoint".into());
                break;
            }
            if self.stop_software_interrupt && self.software_interrupt_candidate().is_some() {
                self.halt =
                    Some("diagnostic: unmasked software interrupt; entry not modeled".into());
                break;
            }
            if let Some(p) = &mut self.irq_probe
                && p.due(self.s.pc_sw as u32)
            {
                // IRPTL (ureg 122) as the SEC line latches it: a hardware latch at an instruction boundary, not a guest write.
                let latch = self.s.r[122];
                if latch.is_c() {
                    self.s.begin();
                    match self.s.set_r(122, V::c((latch.b | p.mask) as Int)) {
                        Ok(()) => self.s.commit_host(),
                        Err(_) => self.s.rollback(),
                    }
                }
            }
            if self.software_interrupts
                || self.s.cfg.core_timer
                || self.s.cfg.peripheral_model
                || self.irq_probe.is_some()
            {
                #[cfg(sharc_gen)]
                {
                    let allowed = self.irq_probe.as_ref().map_or(0, |p| p.mask as Int) | (if self.software_interrupts {
                        0xf000_0000
                    } else {
                        0
                    }) | (if self.s.cfg.core_timer {
                        0x0040_0800
                    } else {
                        0
                    }) | (if self.s.cfg.peripheral_model {
                        0x0000_8000
                    } else {
                        0
                    });
                    if self.s.cfg.peripheral_model && rt::periph::sec_line_active(&self.s) {
                        // The SEC request line latches SECI at the boundary
                        // (tools/sharc_dev/sharc_run.py calls periph._sec_line here).
                        self.s.begin();
                        match rt::periph::sec_line(&mut self.s) {
                            Ok(()) => self.s.commit_host(),
                            Err(trap) => {
                                self.s.rollback();
                                self.trapped(trap);
                                break;
                            }
                        }
                    }
                    let candidate =
                        generated::core_g::sequencer::_interrupt_candidate(&mut self.s, allowed);
                    let result = candidate.and_then(|mask| {
                        if mask == 0 {
                            return Ok(());
                        }
                        self.s.begin();
                        let result =
                            generated::core_g::sequencer::_enter_interrupt(&mut self.s, mask);
                        if result.is_ok() {
                            self.s.bank_complete();
                            self.s.bank_complete();
                            self.s.commit_host();
                            self.model_stats.irq_taken += 1;
                            self.model_stats.irq_by_vector[(mask.trailing_zeros() & 63) as usize] +=
                                1;
                        } else {
                            self.s.rollback();
                        }
                        result
                    });
                    if let Err(trap) = result {
                        self.trapped(trap);
                        break;
                    }
                }
                #[cfg(not(sharc_gen))]
                {
                    self.trapped(TRAP_NO_INSN);
                    break;
                }
            }
            if let Some(head) = self.idle_head {
                // The idle skip advances whole iterations at once: with an
                // aligned stop active it must not pass the next aligned
                // boundary, where the PC may be in the stop range.
                let skip_limit = match self.stop_align {
                    Some((chunk, base)) if self.stop_range.is_some() => {
                        let chunk = chunk as u64;
                        let next = base + ((self.s.icount - base) / chunk + 1) * chunk;
                        limit.min(next)
                    }
                    _ => limit,
                };
                let before_skip = self.s.icount;
                if self.ww_on() {
                    self.ww_begin(self.s.pc_sw as u64, 's');
                }
                self.idle_visit(head, skip_limit);
                self.ww_flush();
                if self.s.icount >= limit {
                    break;
                }
                if self.stop_align.is_some() && self.s.icount != before_skip {
                    // The skip may have landed on the aligned boundary:
                    // look at the stop range there (with no snapshot, as at
                    // the start of a step: one taken here is at this very
                    // count).
                    self.idle_snap = None;
                    self.s.probe = None;
                    continue;
                }
            }
            if self.instruction_clock {
                let tick = self.instruction_clock_base.wrapping_add(self.s.icount);
                self.s.r[105] = V::c((tick as u32) as Int);
                self.s.r[106] = V::c((tick >> 32) as Int);
            }
            if let Some((entry, plan)) = self.block_plan(limit) {
                'blk: {
                    self.s.limit = plan.limit;
                    self.s.stop_at_loop_empty = plan.stop_loop_empty;
                    self.s.chain = 0;
                    let before = self.s.icount;
                    let entry_pc = self.s.pc_sw as u32;
                    if self.fast_auto_dispatch
                        && entry.is_some()
                        && let Some(f) = self.fast.as_mut()
                    {
                        // A generated block is dispatched here: a candidate
                        // region start for run-time selection.
                        f.note(entry_pc);
                    }
                    self.last_interp_next = -1;
                    if self.ww_on() {
                        self.ww_begin(entry_pc as u64, 'b');
                    }
                    let t0 = if self.prof.is_some() { ticks() } else { 0 };
                    if plan.bank_pre.is_some() {
                        self.s.bank_pending_mask = -1;
                    }
                    self.s.in_block = plan.models;
                    let mut code = None;
                    // Ordinary tiers may flush followed entries even before `wants`
                    // sees them; only fallback policy may suppress this call.
                    let fast_eligible = plan.fast_active
                        && !plan.stop_loop_empty
                        && self.fast_allowed(entry_pc);
                    if let Some(f) = self.fast.as_mut().filter(|_| fast_eligible) {
                        #[cfg(feature = "region-profile")]
                        {
                            let profile_start =
                                self.region_profile.as_ref().map(|_| Instant::now());
                            code = f.run(&mut self.s, entry_pc);
                            if let (Some(profile), Some(start)) =
                                (&mut self.region_profile, profile_start)
                            {
                                profile.record(
                                    entry_pc,
                                    true,
                                    self.s.icount - before,
                                    start.elapsed().as_nanos().min(u64::MAX as u128) as u64,
                                );
                            }
                        }
                        #[cfg(not(feature = "region-profile"))]
                        {
                            code = f.run(&mut self.s, entry_pc);
                        }
                    }
                    if code.is_some() {
                        if let Some(w) = &mut self.wwatch {
                            w.pending = Some((entry_pc as u64, 'f'));
                        }
                        if let Some(r) = &mut self.rwatch {
                            r.pending = Some((entry_pc as u64, 'f'));
                        }
                    }
                    if code == Some(EXIT_NEXT) && plan.bank_pre.is_none() {
                        code = self.fast_continue(plan.limit, code);
                    }
                    if code.is_none()
                        && let Some(entry) = entry
                    {
                        #[cfg(feature = "region-profile")]
                        {
                            let profile_start =
                                self.region_profile.as_ref().map(|_| Instant::now());
                            if plan.fast_active
                                && let Some(f) = self.fast.as_mut()
                                && !f.sync_chain_stops(&mut self.s)
                            {
                                // The tier needs every dispatch: end the chain at
                                // its first link.
                                self.s.chain = CHAIN_MAX;
                            }
                            let capture = self.live_capture.as_mut().and_then(|capture| {
                                capture.before(
                                    entry_pc,
                                    &self.s,
                                    self.instruction_clock,
                                    self.instruction_clock_base,
                                )
                            });
                            let block_code = (entry.f)(&mut self.s);
                            code = Some(block_code);
                            if let (Some(capture), Some(ticket)) = (&mut self.live_capture, capture)
                            {
                                capture.after(
                                    ticket,
                                    &self.s,
                                    block_code,
                                    self.instruction_clock,
                                    self.instruction_clock_base,
                                );
                            }
                            if let (Some(profile), Some(start)) =
                                (&mut self.region_profile, profile_start)
                            {
                                profile.record(
                                    entry_pc,
                                    false,
                                    self.s.icount - before,
                                    start.elapsed().as_nanos().min(u64::MAX as u128) as u64,
                                );
                            }
                        }
                        #[cfg(not(feature = "region-profile"))]
                        {
                            if plan.fast_active
                                && let Some(f) = self.fast.as_mut()
                                && !f.sync_chain_stops(&mut self.s)
                            {
                                // The tier needs every dispatch: end the chain at
                                // its first link.
                                self.s.chain = CHAIN_MAX;
                            }
                            let capture = self.live_capture.as_mut().and_then(|capture| {
                                capture.before(
                                    entry_pc,
                                    &self.s,
                                    self.instruction_clock,
                                    self.instruction_clock_base,
                                )
                            });
                            let block_code = (entry.f)(&mut self.s);
                            code = Some(block_code);
                            if let (Some(capture), Some(ticket)) = (&mut self.live_capture, capture)
                            {
                                capture.after(
                                    ticket,
                                    &self.s,
                                    block_code,
                                    self.instruction_clock,
                                    self.instruction_clock_base,
                                );
                            }
                        }
                    }
                    self.s.in_block = false;
                    // Neither the fast tier nor a block (none for this PC):
                    // the interpreter runs one instruction.
                    let Some(code) = code else {
                        if let Some(p) = plan.bank_pre {
                            self.s.bank_pending_mask = p;
                        }
                        break 'blk;
                    };
                    if self.s.icount == before {
                        if let Some(p) = plan.bank_pre {
                            self.s.bank_pending_mask = p;
                        }
                    } else if self.s.cfg.bank_model && self.s.bank_requested_mask >= 0 {
                        // The block's last instruction wrote MODE1 (tools/sharc_dev/
                        // sharc_rsgen.py TERMINAL): its completion, as St::commit
                        // does after an interpreted instruction.
                        self.s.bank_complete();
                    }
                    self.block_models_after(&plan, before);
                    let entry = entry_pc;
                    if let Some(p) = &mut self.prof {
                        let t = ticks() - t0;
                        let e = p.entry(entry).or_default();
                        e.0 += t;
                        e.1 += self.s.icount - before;
                        e.2 += 1;
                    }
                    if let Some(t) = &mut self.trans {
                        *t.entry((entry, self.s.pc_sw as u32)).or_default() += 1;
                    }
                    if code != EXIT_NEXT
                        && let Some(x) = &mut self.exits
                    {
                        let (kind, at) = if code == EXIT_TRAP {
                            (1, self.s.trap.map(|t| t.0).unwrap_or(0))
                        } else if self.s.icount == before {
                            if let Some(bails) = &mut self.entry_bails {
                                let unknown =
                                    self.s.r.iter().enumerate().fold(0u128, |m, (c, v)| {
                                        m | if v.is_c() { 0 } else { 1u128 << c }
                                    });
                                *bails
                                    .entry((
                                        entry,
                                        self.s.r[MODE1].b,
                                        unknown,
                                        self.s.pending.is_some(),
                                    ))
                                    .or_default() += 1;
                            }
                            (0, self.s.pc_sw as u32)
                        } else {
                            (2, self.s.pc_sw as u32)
                        };
                        *x.entry((entry, kind, at)).or_default() += 1;
                    }
                    self.stats.block_entries += 1;
                    self.stats.block_instructions += self.s.icount - before;
                    if self.one_dispatch && self.s.icount > before {
                        break 'steploop;
                    }
                    match code {
                        EXIT_NEXT => continue 'steploop,
                        EXIT_TRAP => {
                            // Undone: the one-instruction interpreter runs it
                            // (and traps for good if the core does).
                            self.stats.block_traps += 1;
                            if self.s.trap == Some(TRAP_BLOCK_MODEL) {
                                self.model_stats.mmr_exits += 1;
                            }
                            self.s.trap = None;
                            if self.s.icount >= limit {
                                break 'steploop;
                            }
                            if self.s.icount > before {
                                // Instructions completed: the next boundary gets
                                // its interrupt, clock and peripheral checks.
                                continue 'steploop;
                            }
                        }
                        _ => {
                            if self.s.icount >= limit {
                                break 'steploop;
                            }
                            if self.s.icount > before {
                                continue 'steploop;
                            }
                        }
                    }
                }
            }
            let insn = if self.s.runtime_decode {
                let pc = self.s.pc_sw;
                match rt::bnd::decode_at(&mut self.s, (), None, pc) {
                    Ok(insn) => insn,
                    Err(t) => {
                        self.trapped(t);
                        break;
                    }
                }
            } else if let Some(insn) = (self.s.insn_at)(self.s.pc_sw) {
                insn
            } else {
                self.trapped(TRAP_NO_INSN);
                break;
            };
            self.stats.single_steps += 1;
            self.model_stats.interp_appended += (self.s.pc_sw as u32 >= self.appended_from) as u64;
            let loops_before = self.s.loops.n;
            let pc_before = self.s.pc_sw;
            if self.diag_on {
                self.diag_step(pc_before);
            }
            if let Err(t) = exec_insn(&mut self.s, insn) {
                self.trapped(t);
                break;
            }
            if self.fast_auto && !(1..=3).contains(&(self.s.pc_sw - pc_before)) {
                // A taken branch, call, return or loop-back landed here: a
                // candidate region start for run-time selection.
                if let Some(f) = self.fast.as_mut() {
                    f.note(self.s.pc_sw as u32);
                }
            }
            if self.s.pc_sw as u32 >= self.appended_from
                && !(1..=3).contains(&(self.s.pc_sw - pc_before))
                && let Some(f) = self.fast.as_mut()
            {
                // A landing in appended code: make it an entry (no generated
                // block or root list names code a patch added).
                f.note_appended(self.s.pc_sw as u32);
            }
            if self.s.loops.n != loops_before || !self.s.loops_ok {
                // Block code assumes every loop ends at a PC the generator
                // saw (a loop the interpreter started in runtime-decoded code
                // may not): re-check the loop stack when it changed.
                self.s.check_loops();
            }
            self.last_interp_next = self.s.pc_sw;
            if self.one_dispatch {
                break;
            }
        }
        self.ww_flush();
        self.idle_snap = None;
        self.s.probe = None;
        (self.s.icount - start) as u32
    }

    /// `step(n)` that also ends, without a halt, at the first instruction
    /// boundary (the first one included) whose PC lies in [LO, HI); the
    /// state is the one a `step` ending at that instruction count leaves,
    /// as step sizes never change results. A block exits at the first PC it
    /// has no body for, so the boundary is exact when no generated body
    /// covers [LO, HI) (tools/sharc_dev/sharc_rsgen.py --exclude).
    pub fn step_until_in(&mut self, n: u32, lo: u32, hi: u32) -> u32 {
        self.stop_range = Some((lo, hi));
        let ran = self.step(n);
        self.stop_range = None;
        ran
    }

    /// `step_until_in(n, lo, hi)` that stops only at a boundary whose
    /// instruction count is a positive multiple of `chunk` past `base_icount`
    /// (the chunk ends of a caller that would step in `chunk`-sized pieces).
    /// Boundaries strictly inside a generated block cannot be in [LO, HI)
    /// (see `step_until_in`), so blocks and chains run through aligned
    /// boundaries unharmed; the idle skip is clamped to the next one.
    pub fn step_until_in_aligned(
        &mut self,
        n: u32,
        lo: u32,
        hi: u32,
        chunk: u32,
        base_icount: u64,
    ) -> u32 {
        assert!(chunk > 0, "chunk must be positive");
        self.stop_align = Some((chunk, base_icount));
        let ran = self.step_until_in(n, lo, hi);
        self.stop_align = None;
        ran
    }

    fn idle_snapshot(&self) -> Box<IdleSnap> {
        let s = &self.s;
        Box::new(IdleSnap {
            icount: s.icount,
            r: s.r,
            bank_alt: s.bank_alt,
            masks: [
                s.bank_active_mask,
                s.bank_pending_mask,
                s.bank_requested_mask,
            ],
            loop_depth: s.loop_depth,
            loop_slots: s.loop_slots.items().to_vec(),
            pc_stack_pending: s.pc_stack_pending,
            pc_stack_requested: s.pc_stack_requested,
            special: s.special,
            special_present: s.special_present,
            pending: s.pending,
            steps: s.steps,
            at_loaded_entry: s.at_loaded_entry,
            loops: s.loops.items().to_vec(),
            call_stack: s.call_stack.items().to_vec(),
            pc_stack: s.pc_stack.items().to_vec(),
            status_stack: s.status_stack.items().to_vec(),
        })
    }

    /// True when the state now equals SN's, apart from the instruction
    /// count, `steps` (a bookkeeping counter nothing reads), the EMUCLK
    /// registers and TCOUNT (which the skip advances) and
    /// the iteration changed no memory or MMR.
    fn idle_fixed_point(&self, sn: &IdleSnap) -> bool {
        let s = &self.s;
        if s.probe_bad || s.pending.is_some() {
            return false;
        }
        for (code, (a, b)) in s.r.iter().zip(sn.r.iter()).enumerate() {
            if a != b && !matches!(code, R_EMUCLK | R_EMUCLK2 | R_TCOUNT) {
                return false;
            }
        }
        if s.bank_alt != sn.bank_alt
            || [
                s.bank_active_mask,
                s.bank_pending_mask,
                s.bank_requested_mask,
            ] != sn.masks
            || s.loop_depth != sn.loop_depth
            || s.loop_slots.items() != sn.loop_slots
            || s.pc_stack_pending != sn.pc_stack_pending
            || s.pc_stack_requested != sn.pc_stack_requested
            || s.special != sn.special
            || s.special_present != sn.special_present
            || s.pending != sn.pending
            || s.at_loaded_entry != sn.at_loaded_entry
            || s.loops.items() != sn.loops
            || s.call_stack.items() != sn.call_stack
            || s.pc_stack.items() != sn.pc_stack
            || s.status_stack.items() != sn.status_stack
        {
            return false;
        }
        // Memory: the first write to each byte recorded its old value.
        let mut first: std::collections::HashMap<u32, (u8, u8)> = std::collections::HashMap::new();
        for &(a, byte, flags) in s.probe.as_deref().unwrap_or(&[]) {
            first.entry(a).or_insert((byte, flags));
        }
        first.iter().all(|(&a, &(byte, flags))| {
            let now = (s.mem.present(a) as u8) | ((s.mem.dirty(a) as u8) << 1);
            now == flags && (flags & 1 == 0 || s.mem.byte(a) == byte)
        })
    }

    /// Ticks of the core timer that can pass without a latch, or None when
    /// the timer cannot be advanced analytically.
    fn idle_timer_safe(&self) -> Option<u64> {
        if !self.s.cfg.core_timer {
            return Some(u64::MAX);
        }
        let mode = self.s.r[R_MODE2];
        if !mode.is_c() {
            return None;
        }
        if mode.b & 0x20 == 0 {
            return Some(u64::MAX);
        }
        let (count, period) = (self.s.r[R_TCOUNT], self.s.r[R_TPERIOD]);
        if !count.is_c() || !period.is_c() {
            return None;
        }
        Some(if count.b > 0 {
            count.b as u64 - 1
        } else {
            period.b as u64
        })
    }

    /// Configure the bounded DDR diagnostic and discard idle-loop evidence
    /// across either activation or removal. No CLI/daemon ABI is exposed.
    pub fn set_ddr_monitor(
        &mut self,
        range: Option<(u32, u32)>,
    ) -> Result<(), mem::InvalidDdrRange> {
        self.s.mem.set_ddr_monitor(range)?;
        self.ddr_monitor_gen = self.s.mem.ddr_monitor_gen;
        self.idle_snap = None;
        self.s.probe = None;
        Ok(())
    }

    /// At an instruction boundary with the idle skip armed: at the loop head
    /// verify one iteration was a fixed point and replay the rest.
    fn idle_visit(&mut self, head: u32, limit: u64) {
        if self.ddr_monitor_gen != self.s.mem.ddr_monitor_gen {
            self.ddr_monitor_gen = self.s.mem.ddr_monitor_gen;
            self.idle_snap = None;
            self.s.probe = None;
        }
        if self.s.mem.ddr_monitor_active() {
            self.idle_snap = None;
            self.s.probe = None;
            return;
        }
        let pc = self.s.pc_sw;
        if pc < self.idle_lo as Int || pc > self.idle_hi as Int {
            if self.idle_snap.is_some() {
                self.idle_snap = None;
            }
            if self.s.probe.is_some() {
                self.s.probe = None;
            }
            return;
        }
        if pc != head as Int {
            if self
                .idle_snap
                .as_ref()
                .is_some_and(|sn| self.s.icount - sn.icount > IDLE_MAX_LOOP)
            {
                self.idle_snap = None;
                self.s.probe = None;
            }
            return;
        }
        if let Some(sn) = self.idle_snap.take() {
            if self.idle_fixed_point(&sn) {
                let k = self.s.icount - sn.icount;
                let mut m = (limit - self.s.icount) / k;
                match self.idle_timer_safe() {
                    Some(safe) => m = m.min(safe / k),
                    None => m = 0,
                }
                if m > 0 {
                    self.idle_replay(m, k, self.s.steps - sn.steps);
                } else {
                    self.idle_stats.empty += 1;
                }
            } else {
                self.idle_stats.rejected += 1;
            }
        }
        self.idle_snap = Some(self.idle_snapshot());
        self.s.probe = Some(Vec::new());
        self.s.probe_bad = false;
    }

    /// Advance M whole iterations of K instructions each (no latch, no
    /// state change besides the counters).
    fn idle_replay(&mut self, m: u64, k: u64, steps: Int) {
        let ticks = m * k;
        if self.s.cfg.core_timer {
            let (mode, count, period) =
                (self.s.r[R_MODE2], self.s.r[R_TCOUNT], self.s.r[R_TPERIOD]);
            if mode.is_c() && mode.b & 0x20 != 0 {
                let v = if count.b > 0 {
                    count.b as u64 - ticks
                } else {
                    period.b as u64 - (ticks - 1)
                };
                self.s.r[R_TCOUNT] = V::c(v as Int);
            }
        }
        self.s.icount += ticks;
        self.s.steps += steps * m as Int;
        if self.instruction_clock {
            // The last replayed instruction set the clock at its own start.
            let tick = self.instruction_clock_base.wrapping_add(self.s.icount - 1);
            self.s.r[R_EMUCLK] = V::c((tick as u32) as Int);
            self.s.r[R_EMUCLK2] = V::c((tick >> 32) as Int);
        }
        self.idle_stats.skips += 1;
        self.idle_stats.iterations += m;
        self.idle_stats.instructions += ticks;
    }
}

// ---------------------------------------------------------------------------
// C ABI (tools/sharc_dev/sharc_diff.py NativeEngine, tools/sharc_dev/sharc_transpile_run.py)
// ---------------------------------------------------------------------------

/// Allocate a host-transfer buffer, including in WebAssembly where JavaScript
/// cannot supply an allocation owned by Rust. Release with the same length.
#[cfg_attr(not(sharc_second), unsafe(no_mangle))]
pub extern "C" fn sharc_native_alloc(len: usize) -> *mut u8 {
    let bytes = vec![0u8; len].into_boxed_slice();
    Box::into_raw(bytes) as *mut u8
}

/// # Safety
/// PTR and LEN must be an outstanding allocation from sharc_native_alloc.
#[cfg_attr(not(sharc_second), unsafe(no_mangle))]
pub unsafe extern "C" fn sharc_native_free(ptr: *mut u8, len: usize) {
    if !ptr.is_null() {
        // SAFETY: caller supplies the allocation and its original length.
        drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len)) });
    }
}

/// # Safety
/// IMAGE must point to IMAGE_LEN readable bytes (a tools/sharc_dev/sharc_transpile_run.py
/// `pack_image` blob).
#[cfg_attr(not(sharc_second), unsafe(no_mangle))]
pub unsafe extern "C" fn sharc_native_create(image: *const u8, image_len: usize) -> *mut Engine {
    let bytes = if image.is_null() {
        &[][..]
    } else {
        // SAFETY: the caller guarantees IMAGE_LEN readable bytes.
        unsafe { std::slice::from_raw_parts(image, image_len) }
    };
    match Engine::from_image(bytes) {
        Ok(e) => Box::into_raw(Box::new(e)),
        Err(_) => std::ptr::null_mut(),
    }
}

/// # Safety
/// HANDLE must come from sharc_native_create and not be used afterwards.
#[cfg_attr(not(sharc_second), unsafe(no_mangle))]
pub unsafe extern "C" fn sharc_native_destroy(handle: *mut Engine) {
    if !handle.is_null() {
        // SAFETY: created by Box::into_raw in sharc_native_create.
        drop(unsafe { Box::from_raw(handle) });
    }
}

/// # Safety
/// HANDLE from sharc_native_create; BLOB points to BLOB_LEN bytes.
#[cfg_attr(not(sharc_second), unsafe(no_mangle))]
pub unsafe extern "C" fn sharc_native_import_state(
    handle: *mut Engine,
    blob: *const u8,
    blob_len: usize,
) -> i32 {
    // SAFETY: caller contract.
    let (e, bytes) = unsafe { (&mut *handle, std::slice::from_raw_parts(blob, blob_len)) };
    match e.import(bytes) {
        Ok(()) => 0,
        Err(code) => code,
    }
}

/// # Safety
/// HANDLE from sharc_native_create; OUT points to OUT_CAP writable bytes.
#[cfg_attr(not(sharc_second), unsafe(no_mangle))]
pub unsafe extern "C" fn sharc_native_export_state(
    handle: *mut Engine,
    out: *mut u8,
    out_cap: usize,
) -> i32 {
    // SAFETY: caller contract.
    let e = unsafe { &mut *handle };
    let blob = canon::export_state(&e.s, e.export_ranges);
    if blob.len() > out_cap {
        return -(blob.len() as i32);
    }
    // SAFETY: OUT has OUT_CAP >= blob.len() writable bytes.
    unsafe { std::ptr::copy_nonoverlapping(blob.as_ptr(), out, blob.len()) };
    blob.len() as i32
}

/// # Safety
/// HANDLE from sharc_native_create.
#[cfg_attr(not(sharc_second), unsafe(no_mangle))]
pub unsafe extern "C" fn sharc_native_step(handle: *mut Engine, n: u32) -> i32 {
    // SAFETY: caller contract.
    let e = unsafe { &mut *handle };
    e.step(n) as i32
}

/// # Safety
/// HANDLE from sharc_native_create; OUT points to OUT_CAP writable bytes.
#[cfg_attr(not(sharc_second), unsafe(no_mangle))]
pub unsafe extern "C" fn sharc_native_halt_reason(
    handle: *mut Engine,
    out: *mut u8,
    out_cap: usize,
) -> i32 {
    // SAFETY: caller contract.
    let e = unsafe { &mut *handle };
    let Some(reason) = &e.halt else {
        return 0;
    };
    let n = reason.len().min(out_cap);
    // SAFETY: OUT has OUT_CAP >= n writable bytes.
    unsafe { std::ptr::copy_nonoverlapping(reason.as_ptr(), out, n) };
    n as i32
}

/// Options beyond the harness contract. KEY: 1 use block code (default
/// 1), 2 export every overlay byte as memory ranges (default 0), 3 which
/// special-register slots are present as dict keys (bit per
/// SPECIAL_SLOTS entry; the canonical format cannot say, and
/// `"BFF_HI" in special` reads it), 4 enables runtime decoding,
/// 5 enables the diagnostic instruction clock, 10+ run configuration
/// (canon::set_option).
///
/// # Safety
/// HANDLE from sharc_native_create.
#[cfg_attr(not(sharc_second), unsafe(no_mangle))]
pub unsafe extern "C" fn sharc_native_set_option(handle: *mut Engine, key: u32, value: i64) -> i32 {
    // SAFETY: caller contract.
    let e = unsafe { &mut *handle };
    e.set_option(key, value)
}

/// state.provisional_interpretations[NAME] = MODE (both UTF-8).
///
/// # Safety
/// HANDLE from sharc_native_create; the pointers cover their lengths.
#[cfg_attr(not(sharc_second), unsafe(no_mangle))]
pub unsafe extern "C" fn sharc_native_set_provisional(
    handle: *mut Engine,
    name: *const u8,
    name_len: usize,
    mode: *const u8,
    mode_len: usize,
) -> i32 {
    // SAFETY: caller contract.
    let (e, name, mode) = unsafe {
        (
            &mut *handle,
            std::slice::from_raw_parts(name, name_len),
            std::slice::from_raw_parts(mode, mode_len),
        )
    };
    let (Ok(name), Ok(mode)) = (std::str::from_utf8(name), std::str::from_utf8(mode)) else {
        return -1;
    };
    let (Some(n), Some(m)) = (sym_of(name), sym_of(mode)) else {
        return -2;
    };
    e.s.cfg.provisional_interp.retain(|(k, _)| *k != n);
    e.s.cfg.provisional_interp.push((n, m));
    e.s.cfg.refresh();
    0
}

/// Execute one instruction described by BLOB (canon::parse_insn: type
/// name, length, kind, fields) through the generated core, without the
/// image's instruction table: the compute corpus's fabricated cases.
/// Returns 1 when it completed, 0 when it trapped (see halt_reason), <0
/// for a malformed blob.
///
/// # Safety
/// HANDLE from sharc_native_create; BLOB points to BLOB_LEN bytes.
#[cfg_attr(not(sharc_second), unsafe(no_mangle))]
pub unsafe extern "C" fn sharc_native_exec_insn(
    handle: *mut Engine,
    blob: *const u8,
    blob_len: usize,
) -> i32 {
    // SAFETY: caller contract.
    let (e, bytes) = unsafe { (&mut *handle, std::slice::from_raw_parts(blob, blob_len)) };
    let insn = match canon::parse_insn(bytes) {
        Ok(i) => i,
        Err(code) => return code,
    };
    match exec_insn(&mut e.s, insn) {
        Ok(()) => 1,
        Err(t) => {
            e.trapped(t);
            0
        }
    }
}

/// Counters: [instructions, block entries, block instructions, single
/// steps, traps, blocks in the image, special-slot presence bits, idle-skipped
/// instructions, block exits through traps, then ModelStats: gated calls,
/// unsafe blocks refused, interrupt-deferred, timer/pipeline-gated,
/// peripheral-store exits, code mismatches].
/// Returns how many were written.
///
/// # Safety
/// HANDLE from sharc_native_create; OUT points to OUT_CAP u64s.
#[cfg_attr(not(sharc_second), unsafe(no_mangle))]
pub unsafe extern "C" fn sharc_native_stats(
    handle: *mut Engine,
    out: *mut u64,
    out_cap: usize,
) -> i32 {
    // SAFETY: caller contract.
    let e = unsafe { &mut *handle };
    let v = [
        e.s.icount,
        e.stats.block_entries,
        e.stats.block_instructions,
        e.stats.single_steps,
        e.stats.traps,
        e.dispatch.count as u64,
        (0..7).map(|i| (e.s.special_present[i] as u64) << i).sum(),
        e.idle_stats.instructions,
        e.stats.block_traps,
        e.model_stats.gated,
        e.model_stats.unsafe_block,
        e.model_stats.irq_deferred,
        e.model_stats.timer_gated,
        e.model_stats.mmr_exits,
        e.model_stats.code_mismatch,
    ];
    let n = v.len().min(out_cap);
    // SAFETY: OUT has OUT_CAP >= n u64s.
    unsafe { std::ptr::copy_nonoverlapping(v.as_ptr(), out, n) };
    n as i32
}

/// The profile maps (option 26) as text, for tools/sharc_dev/sharc_rsgen.py: KIND 0
/// coverage ("pc MODE1 known count", sharc-frames --coverage), 1 entries
/// ("pc count"), 2 block transitions ("from to count"), 3 block exits
/// ("block kind at count"), 4 entry-bail state
/// ("pc MODE1 unknown-register-bitset pending count"). Returns the length, or -needed when OUT_CAP is
/// too small, or -1 for a bad KIND or no profile.
///
/// # Safety
/// HANDLE from sharc_native_create; OUT points to OUT_CAP writable bytes.
#[cfg_attr(not(sharc_second), unsafe(no_mangle))]
pub unsafe extern "C" fn sharc_native_profile(
    handle: *mut Engine,
    kind: u32,
    out: *mut u8,
    out_cap: usize,
) -> i32 {
    // SAFETY: caller contract.
    let e = unsafe { &mut *handle };
    let Some(text) = e.profile_text(kind) else {
        return -1;
    };
    if text.len() > out_cap {
        return -(text.len() as i32);
    }
    // SAFETY: OUT has OUT_CAP >= len writable bytes.
    unsafe { std::ptr::copy_nonoverlapping(text.as_ptr(), out, text.len()) };
    text.len() as i32
}

impl Engine {
    /// Writes the coverage, entries and transitions maps to
    /// `PREFIX.e<pid>-<n>`, `.entries` and `.trans` (one set per engine).
    /// Does nothing for maps that are off (option 26 unset).
    fn write_profile(&self, prefix: &std::path::Path) {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        for (kind, suffix) in [(0, ""), (1, ".entries"), (2, ".trans")] {
            if let Some(text) = self.profile_text(kind) {
                let mut path = prefix.as_os_str().to_owned();
                path.push(format!(".e{}-{n}{suffix}", std::process::id()));
                let _ = std::fs::write(path, text);
            }
        }
    }

    /// The profile maps (option 26) as text (sharc_native_profile's KIND).
    pub fn profile_text(&self, kind: u32) -> Option<String> {
        let e = self;
        match kind {
            0 => e.cov.as_ref().map(|m| {
                let mut rows: Vec<_> = m.iter().collect();
                rows.sort();
                rows.iter()
                    .map(|((pc, mode, known), c)| {
                        format!("{pc:#x} {mode:#x} {} {c}\n", *known as u8)
                    })
                    .collect::<String>()
            }),
            1 => e.entries.as_ref().map(|m| {
                let mut rows: Vec<_> = m.iter().collect();
                rows.sort();
                rows.iter()
                    .map(|(pc, c)| format!("{pc:#x} {c}\n"))
                    .collect::<String>()
            }),
            2 => e.trans.as_ref().map(|m| {
                let mut rows: Vec<_> = m.iter().collect();
                rows.sort();
                rows.iter()
                    .map(|((a, b), c)| format!("{a:#x} {b:#x} {c}\n"))
                    .collect::<String>()
            }),
            3 => e.exits.as_ref().map(|m| {
                let mut rows: Vec<_> = m.iter().collect();
                rows.sort();
                rows.iter()
                    .map(|((b, k, at), c)| format!("{b:#x} {k} {at:#x} {c}\n"))
                    .collect::<String>()
            }),
            4 => e.entry_bails.as_ref().map(|m| {
                let mut rows: Vec<_> = m.iter().collect();
                rows.sort();
                rows.iter()
                    .map(|((pc, mode, unknown, pending), c)| {
                        format!("{pc:#x} {mode:#x} {unknown:#x} {} {c}\n", *pending as u8)
                    })
                    .collect::<String>()
            }),
            _ => None,
        }
    }
}

/// Set UREG CODE to (KIND 0 Unknown / 1 Const / 2 PartialConst, VALUE,
/// MASK), as a host poke between runs.
///
/// # Safety
/// HANDLE from sharc_native_create.
#[cfg_attr(not(sharc_second), unsafe(no_mangle))]
pub unsafe extern "C" fn sharc_native_set_reg(
    handle: *mut Engine,
    code: u32,
    kind: u32,
    value: u32,
    mask: u32,
) -> i32 {
    // SAFETY: caller contract.
    let e = unsafe { &mut *handle };
    if code as usize >= NUREG {
        return -1;
    }
    let v = match kind {
        1 => V::c(value as Int),
        2 => V::partial(mask as Int, value as Int),
        _ => V::UNK,
    };
    e.set_reg(code as usize, v);
    0
}

/// UREG CODE as (value, mask) packed into a u64 (mask in the high half).
///
/// # Safety
/// HANDLE from sharc_native_create.
#[cfg_attr(not(sharc_second), unsafe(no_mangle))]
pub unsafe extern "C" fn sharc_native_get_reg(handle: *mut Engine, code: u32) -> u64 {
    // SAFETY: caller contract.
    let e = unsafe { &mut *handle };
    let v = e.s.r[code as usize % NUREG];
    ((v.m as u64) << 32) | v.b as u64
}

/// The architectural software PC. Returns -1 for a null HANDLE.
///
/// Unlike UREG `PC`, which is a separately modelled register slot, this is
/// `State.pc_sw`, the address the single-step dispatcher will execute next.
///
/// # Safety
/// HANDLE from sharc_native_create, or null.
#[cfg_attr(not(sharc_second), unsafe(no_mangle))]
pub unsafe extern "C" fn sharc_native_get_pc(handle: *mut Engine) -> i64 {
    if handle.is_null() {
        return -1;
    }
    // SAFETY: checked non-null HANDLE is from sharc_native_create.
    unsafe { (&*handle).s.pc_sw as i64 }
}

/// memory._dm_write(state, ADDRESS + i, 1, byte) for each byte (a host
/// poke, as sharc_harness._poke does). Returns how many took effect.
///
/// # Safety
/// HANDLE from sharc_native_create; DATA points to LEN bytes.
#[cfg_attr(not(sharc_second), unsafe(no_mangle))]
pub unsafe extern "C" fn sharc_native_poke(
    handle: *mut Engine,
    address: u64,
    data: *const u8,
    len: usize,
    width: u32,
) -> i32 {
    // SAFETY: caller contract.
    let (e, bytes) = unsafe { (&mut *handle, std::slice::from_raw_parts(data, len)) };
    e.poke(address, bytes, width)
}

/// memory._dm_read(state, ADDRESS, WIDTH): the value in the low 32 bits,
/// bit 32 set when known, -1 on an unmodelled MMR.
///
/// # Safety
/// HANDLE from sharc_native_create.
#[cfg_attr(not(sharc_second), unsafe(no_mangle))]
pub unsafe extern "C" fn sharc_native_peek(handle: *mut Engine, address: u64, width: u32) -> i64 {
    // SAFETY: caller contract.
    let e = unsafe { &mut *handle };
    match e.peek(address, width) {
        Ok(Some(v)) => (1i64 << 32) | v as i64,
        Ok(None) => 0,
        Err(_) => -1,
    }
}

/// periph._sec_raise(state, SID) as a host event: 0, or -1 when the
/// peripheral model rejects it (nothing changes).
///
/// # Safety
/// HANDLE from sharc_native_create.
#[cfg_attr(not(sharc_second), unsafe(no_mangle))]
pub unsafe extern "C" fn sharc_native_sec_raise(handle: *mut Engine, sid: u32) -> i32 {
    // SAFETY: caller contract.
    let e = unsafe { &mut *handle };
    e.host_event(|s| rt::periph::sec_raise(s, sid))
        .map_or(-1, |()| 0)
}

/// periph._dma_start(state, BASE): the work unit's start address, or -1.
///
/// # Safety
/// HANDLE from sharc_native_create.
#[cfg_attr(not(sharc_second), unsafe(no_mangle))]
pub unsafe extern "C" fn sharc_native_dma_start(handle: *mut Engine, base: u32) -> i64 {
    // SAFETY: caller contract.
    let e = unsafe { &mut *handle };
    e.host_event(|s| rt::periph::dma_start(s, base))
        .map_or(-1, |a| a as i64)
}

/// periph._dma_done(state, BASE, SID): 0, or -1 when rejected.
///
/// # Safety
/// HANDLE from sharc_native_create.
#[cfg_attr(not(sharc_second), unsafe(no_mangle))]
pub unsafe extern "C" fn sharc_native_dma_done(handle: *mut Engine, base: u32, sid: u32) -> i32 {
    // SAFETY: caller contract.
    let e = unsafe { &mut *handle };
    e.host_event(|s| rt::periph::dma_done(s, base, sid))
        .map_or(-1, |()| 0)
}

/// One SPI2 slave frame (Engine::spi2_exchange): FRAME holds LEN bytes in
/// wire order and the reply (LEN bytes) is written to OUT. Returns LEN, or
/// -1 when the peripheral model rejects the exchange.
///
/// # Safety
/// HANDLE from sharc_native_create; FRAME points to LEN readable bytes and
/// OUT to LEN writable bytes.
#[cfg_attr(not(sharc_second), unsafe(no_mangle))]
pub unsafe extern "C" fn sharc_native_spi2_exchange(
    handle: *mut Engine,
    frame: *const u8,
    len: usize,
    out: *mut u8,
) -> i64 {
    // SAFETY: caller contract.
    let (e, bytes) = unsafe { (&mut *handle, std::slice::from_raw_parts(frame, len)) };
    match e.spi2_exchange(bytes) {
        Ok(reply) => {
            // SAFETY: OUT has LEN writable bytes and the reply is LEN long.
            unsafe { std::ptr::copy_nonoverlapping(reply.as_ptr(), out, reply.len()) };
            reply.len() as i64
        }
        Err(_) => -1,
    }
}

/// One audio block (Engine::sport_block). BLOCK holds LEN input bytes
/// (LEN 0 with a null BLOCK means zeros); the output block is written to OUT
/// (OUT_CAP bytes). Returns the output length, -1 when rejected (nothing
/// changes) or -2 while the SPORTs are not running.
///
/// # Safety
/// HANDLE from sharc_native_create; BLOCK points to LEN readable bytes (or is
/// null with LEN 0) and OUT to OUT_CAP writable bytes.
#[cfg_attr(not(sharc_second), unsafe(no_mangle))]
pub unsafe extern "C" fn sharc_native_sport_block(
    handle: *mut Engine,
    block: *const u8,
    len: usize,
    out: *mut u8,
    out_cap: usize,
) -> i64 {
    // SAFETY: caller contract.
    let e = unsafe { &mut *handle };
    let input = if block.is_null() {
        None
    } else {
        // SAFETY: caller contract.
        Some(unsafe { std::slice::from_raw_parts(block, len) })
    };
    match e.sport_block(input) {
        Ok(Some(reply)) if reply.len() <= out_cap => {
            // SAFETY: OUT has OUT_CAP writable bytes and the reply fits.
            unsafe { std::ptr::copy_nonoverlapping(reply.as_ptr(), out, reply.len()) };
            reply.len() as i64
        }
        Ok(None) => -2,
        _ => -1,
    }
}

/// sharc_run.fresh_call_state: a new call at PC with empty loop, PC and
/// status stacks, no delayed transfer and no stop (RETURN_ADDRESS >= 0
/// becomes the PC stack's only entry).
///
/// # Safety
/// HANDLE from sharc_native_create.
#[cfg_attr(not(sharc_second), unsafe(no_mangle))]
pub unsafe extern "C" fn sharc_native_fresh_call(
    handle: *mut Engine,
    pc: u32,
    return_address: i64,
) -> i32 {
    // SAFETY: caller contract.
    let e = unsafe { &mut *handle };
    e.fresh_call(pc, (return_address >= 0).then_some(return_address as Int));
    0
}

/// A JSON description of the build: generated core and image hashes.
///
/// # Safety
/// OUT points to OUT_CAP writable bytes.
#[cfg_attr(not(sharc_second), unsafe(no_mangle))]
pub unsafe extern "C" fn sharc_native_info(out: *mut u8, out_cap: usize) -> i32 {
    let info = build_info();
    if info.len() > out_cap {
        return -(info.len() as i32);
    }
    // SAFETY: OUT has OUT_CAP >= len writable bytes.
    unsafe { std::ptr::copy_nonoverlapping(info.as_ptr(), out, info.len()) };
    info.len() as i32
}

/// The build's description (sharc_native_info): the SHA-256 of the
/// tools/sharc_dev/sharc_core sources and the generator version the generated code
/// came from (0: generated before the version existed), the image's
/// SHA-256 and the block count. Loaders compare the first two with the
/// current sources and refuse a stale library
/// (tools/sharc_dev/sharc_transpile_run.check_build_info, packages/emulator/live).
#[allow(unused_assignments)]
pub fn build_info() -> String {
    #[allow(unused_mut)]
    let mut core = "none";
    #[allow(unused_mut)]
    let mut generator: u32 = 0;
    #[allow(unused_mut)]
    let mut image = "none";
    #[cfg(sharc_gen)]
    {
        core = generated::tables::CORE_SHA256;
    }
    #[cfg(all(sharc_gen, sharc_gen_version))]
    {
        generator = generated::tables::GENERATOR_VERSION;
    }
    #[cfg(all(sharc_gen, sharc_image))]
    {
        image = generated::image::IMAGE_SHA256;
    }
    format!(
        "{{\"core_sha256\": \"{core}\", \"generator_version\": {generator}, \"image_sha256\": \"{image}\", \"blocks\": {}}}",
        image_blocks().len()
    )
}
