//! The SHARC "fast tier": a load-time translator that turns a hot region of
//! the loaded DSP image into native code and falls back, exactly, to the
//! existing paths (the generated blocks, the interpreter).
//!
//! Layers, all independent of any firmware address or table:
//!  * `region`/`lower`: decode a DO-loop body from loaded memory with the
//!    generic decoder and lower it to the kernel IR (`ir`);
//!  * `ir`: a backend-neutral loop kernel (u32/f32/u64 values, variables,
//!    guards, window loads and stores). `interp` is a reference interpreter;
//!    the Cranelift backend lives in `packages/emulator/sharc-fast-cl`, a WebAssembly
//!    backend can be a second one;
//!  * `glue`: the entry checks, window resolution and exact state
//!    reconstruction around a kernel call;
//!  * `FastEngine`: the per-engine cache of regions by entry PC, behind the
//!    `FastTier` hook of `Engine::step`.
//!
//! Entry PCs come from run-time configuration: `Engine::set_fast`, or the
//! `SHARC_FAST_REGIONS` environment variable (comma separated hex PCs) read
//! by `Engine::from_image`. Off by default.

/// `Instant` for the tier's build timings. `wasm32-unknown-unknown` has no
/// clock (`std::time::Instant::now` panics), so there the timings read zero.
mod clock {
    #[cfg(not(target_arch = "wasm32"))]
    pub use std::time::Instant;

    #[cfg(target_arch = "wasm32")]
    #[derive(Clone, Copy)]
    pub struct Instant;

    #[cfg(target_arch = "wasm32")]
    impl Instant {
        pub fn now() -> Self {
            Instant
        }

        pub fn elapsed(&self) -> std::time::Duration {
            std::time::Duration::ZERO
        }
    }
}

pub mod cfg;
pub mod decode_view;
pub mod dump;
#[allow(unreachable_code)]
#[rustfmt::skip]
pub mod gen_helpers;
pub mod glue;
pub mod helper;
pub mod interp;
pub mod ir;
pub mod lower;
pub mod plugin;
pub mod region;

use crate::rt::St;
use ir::*;
use std::sync::Mutex;

/// What an address-valued quantity is relative to: nothing (absolute), or a
/// register's value at entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Base {
    None,
    Reg(u8),
}

/// How an instruction's ASTATX effect is computed from its remembered
/// sources (`glue::apply_flag`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlagKind {
    /// Float ALU result bits: AZ AN from the bits, AC AV AS AI cleared, AF set.
    Falu,
    /// Float multiply result bits: MN from the sign, MV MU MI cleared.
    Fmul,
    /// Short float multiply: MN MV MU MI become unknown.
    FmulForget,
    /// Fixed-point MRF multiply, MAC and saturate (source: MN and MU at their
    /// ASTATX bit positions): MN MU from the source, MV MI cleared.
    Mfix,
    /// Adder flags of `a + b` / `a - b`; AS AI AF cleared.
    Iadd,
    Isub,
    /// Float dual add/subtract: AZ and AN ORed over the two results (the
    /// sources); the rest as `Falu`.
    FaluOr,
    /// Fixed dual add/subtract: AC AV AN AZ of the add and of the subtract
    /// ORed; the sources are the two operands.
    IaddSubOr,
    /// Logical result: AN AZ, the rest of the ALU bits cleared.
    Logical,
    /// Shifter: SZ from the (shifted) value, SV fixed, SS cleared.
    Shift {
        sv: bool,
    },
    Fext {
        sv: bool,
    },
    /// Float abs: AF set, AZ from the result bits, AN cleared, AS from the
    /// input sign (sources: result, input).
    Fabs,
    /// Float mantissa (sources: result, input): AF set, AZ when the result is
    /// zero, AS from the input sign, AN AC AV AI cleared.
    Fmant,
    /// comp, compu, fcomp (source: value = eq | lt << 2 | gt << 31): AZ AN
    /// from the value, AF set for the float form, AC AV AS AI cleared. The
    /// CACC shift register is not part of this effect: the kernel tracks it
    /// in `PSEUDO_CACC` (`glue::apply_cacc`).
    Compare {
        float: bool,
    },
    /// btst (source: bit 0 = SZ, bit 1 = SV), SS cleared.
    Btst,
    /// leftz (source: the operand): SZ = its MSB, SV = it is zero.
    Leftz,
    /// Register-amount shift (sources: shifted value, amount > 0): SZ from
    /// the shifted value, SV from the amount, SS cleared.
    ShiftDyn,
    /// Fixed abs (source: the input): AC AV AN AZ of `0 - x` for a negative
    /// input and of `0 + x` otherwise, AS = the input's sign, AI AF cleared.
    Iabs,
}

/// The largest writer id within a flag group (`FlagKind::group_id`).
pub const MAX_FLAG_ID: u32 = 11;

impl FlagKind {
    /// The flag group the writer belongs to and its id there (1..=4), as the
    /// kernel records them (`lower::cond`).
    pub fn group_id(self) -> (u32, u32) {
        match self {
            FlagKind::Falu => (0, 1),
            FlagKind::Iadd => (0, 2),
            FlagKind::Isub => (0, 3),
            FlagKind::Logical => (0, 4),
            FlagKind::FaluOr => (0, 5),
            FlagKind::IaddSubOr => (0, 6),
            FlagKind::Fabs => (0, 7),
            FlagKind::Compare { float: false } => (0, 8),
            FlagKind::Compare { float: true } => (0, 9),
            FlagKind::Fmant => (0, 10),
            FlagKind::Iabs => (0, 11),
            FlagKind::Fmul => (1, 1),
            FlagKind::FmulForget => (1, 2),
            FlagKind::Mfix => (1, 3),
            FlagKind::Shift { sv: false } => (2, 1),
            FlagKind::Shift { sv: true } => (2, 2),
            FlagKind::Fext { sv: false } => (2, 3),
            FlagKind::Fext { sv: true } => (2, 4),
            FlagKind::Btst => (2, 5),
            FlagKind::Leftz => (2, 6),
            FlagKind::ShiftDyn => (2, 7),
        }
    }

    pub fn from_group_id(group: u32, id: u32) -> Option<FlagKind> {
        Some(match (group, id) {
            (0, 1) => FlagKind::Falu,
            (0, 2) => FlagKind::Iadd,
            (0, 3) => FlagKind::Isub,
            (0, 4) => FlagKind::Logical,
            (0, 5) => FlagKind::FaluOr,
            (0, 6) => FlagKind::IaddSubOr,
            (0, 7) => FlagKind::Fabs,
            (0, 8) => FlagKind::Compare { float: false },
            (0, 9) => FlagKind::Compare { float: true },
            (0, 10) => FlagKind::Fmant,
            (0, 11) => FlagKind::Iabs,
            (1, 1) => FlagKind::Fmul,
            (1, 2) => FlagKind::FmulForget,
            (1, 3) => FlagKind::Mfix,
            (2, 1) => FlagKind::Shift { sv: false },
            (2, 2) => FlagKind::Shift { sv: true },
            (2, 3) => FlagKind::Fext { sv: false },
            (2, 4) => FlagKind::Fext { sv: true },
            (2, 5) => FlagKind::Btst,
            (2, 6) => FlagKind::Leftz,
            (2, 7) => FlagKind::ShiftDyn,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct FlagWriter {
    pub insn: u32,
    pub kind: FlagKind,
    /// Flag-source slots (context `fsrc` indices); 255 for none.
    pub fsrc: [u8; 2],
    /// The processing element whose status the writer defines: 0 ASTATX,
    /// 1 ASTATY (SIMD).
    pub pe: u8,
}

/// A condition on the engine state a region needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Req {
    /// The register is fully known.
    Known(u8),
    /// The register is known and has this value (baked into the kernel).
    Eq(u8, u32),
    /// Every address in `base + [lo, hi] + ts*t` (t over the iterations)
    /// lies in the byte-address range where normal-word accesses are plain,
    /// scaled by 4.
    NwPlain {
        base: Base,
        lo: i64,
        hi: i64,
        ts: i64,
        nf: i64,
    },
    /// `MODE1 & mask == value` (rounding and saturation modes the lowering
    /// was made for).
    Mode1 { mask: u32, value: u32 },
    /// The ASTATX bits in `mask` are known (a condition reads them).
    FlagsKnown(u32),
    /// ASTATX bits 31:24 (the CACC compare history) are not all known. The
    /// kernel was made for that state: a compare then forgets the history
    /// (`values._apply_flag_update` shifts it only when the whole old ASTATX
    /// is a known constant, and no instruction of a region defines CACC bits
    /// except a compare), so there is no history to track. The counterpart of
    /// `FlagsKnown(u32::MAX)`, which the exact-shift kernel requires.
    CaccUnknown,
    /// The multiplier accumulator MRF is present and every one of its 80 bits
    /// is known (the kernel keeps it in `PSEUDO_MR`).
    MrFull,
    /// The register is known and its value lies in `lo..=hi` (unsigned): an
    /// index register inside its circular buffer, which is what bounds every
    /// address the wrapped updates produce.
    Within { c: u8, lo: u32, hi: u32 },
    /// With the bank model: the applied bank selection equals MODE1's
    /// alternate-bank bits (`bank_active_mask == MODE1 & 0x4f8`), so that a
    /// MODE1 write of the same value changes nothing after its two-
    /// instruction delay (`lower::sysops`).
    BankSync,
}

/// A memory window the region touches: the bytes
/// `base + [lo, hi) + ts*t`, to be resolved to a host pointer per call.
#[derive(Clone, Copy, Debug)]
pub struct WinSpec {
    pub base: Base,
    pub lo: i64,
    pub hi: i64,
    pub ts: i64,
    /// Coefficient of the (symbolic) iteration count: see `lower::Abs`.
    pub nf: i64,
    pub read: bool,
    pub write: bool,
    /// A read-only window whose span is resolved from this independently
    /// known index register at entry.  The lowering guards each dynamic
    /// address before its load; unlike the fields above, this is not a
    /// statically proved access range.
    pub dynamic_anchor: Option<u8>,
}

/// The engine's view of the fast tier (`Engine::fast`).
pub trait FastTier: Send {
    /// Is PC an entry the tier handles?
    fn wants(&self, pc: u32) -> bool;
    /// This entry is reserved for generated code invalidated by a patch.
    fn fallback_only(&self, _pc: u32) -> bool {
        false
    }
    /// Whether any roots can return true from `fallback_only`; this policy is
    /// cached when the tier is installed in an engine.
    fn has_fallback(&self) -> bool {
        false
    }
    /// Only fallback roots, with no ordinary roots, auto selection or following.
    fn exclusively_fallback(&self) -> bool {
        false
    }
    /// Run the region at PC if it applies. `Some(exit code)` only when at
    /// least one instruction completed; `None` leaves the state untouched.
    fn run(&mut self, s: &mut St, pc: u32) -> Option<u32>;
    /// `run` with a query for a healthy generated entry at a PC (one that
    /// dispatch would run). A tier may use it at build time to refuse a
    /// region that would strand code in the interpreter.
    fn run_with(&mut self, s: &mut St, pc: u32, _aot: &dyn Fn(u32) -> bool) -> Option<u32> {
        self.run(s, pc)
    }
    /// `run` for a kernel entered straight from another kernel's exit
    /// (`Engine::step`'s continuation): only an already built region that
    /// takes the state, with no builds, rebuilds or decline accounting (the
    /// engine's own dispatch does those if this returns `None`).
    fn run_chained(&mut self, _s: &mut St, _pc: u32) -> Option<u32> {
        None
    }
    /// Generated blocks call each other directly (`EXIT_CHAIN`), which would
    /// carry execution into an entry PC without passing `Engine::step`: when
    /// true the engine ends each chain at the first link, so every block
    /// boundary is a dispatch and the tier sees every entry.
    fn caps_chain(&self) -> bool {
        false
    }
    /// Bring `s.chain_stop` up to date with every PC the tier wants control
    /// at, so generated chains end only there. Returns false when the tier
    /// needs every dispatch (the engine then caps the chain at its first
    /// link); the default needs nothing.
    fn sync_chain_stops(&mut self, _s: &mut St) -> bool {
        true
    }
    /// False only when `sync_chain_stops` would change nothing and return
    /// true, so the engine may skip the call. The default is conservative.
    fn stops_pending(&self) -> bool {
        true
    }
    /// False only when `run` at a PC this tier does not `wants` would change
    /// no state and decline, so the engine may skip the call. The default is
    /// conservative: a tier may do work in `run` before `wants` sees a PC.
    fn has_queued(&self) -> bool {
        true
    }
    /// A one-paragraph summary for logs.
    fn report(&self) -> String {
        String::new()
    }
    /// Does the tier pick its own regions (`note`)?
    fn auto(&self) -> bool {
        false
    }
    /// Should the engine also report the entries of generated blocks at
    /// their dispatch (`note`), not only interpreter boundaries?
    fn auto_dispatch(&self) -> bool {
        false
    }
    /// The engine reached a candidate region start at an interpreter
    /// boundary (the target of a taken branch, call or loop-back) or at a
    /// generated-block dispatch. Only called when `auto()`; must be cheap.
    fn note(&mut self, _pc: u32) {}
    /// The engine did not call `note` for a PC that has a usable AOT block.
    fn skipped_for_aot(&mut self) {}
    /// The interpreter landed by a taken branch, call, return or loop-back in
    /// appended code (code a patch added after the generated image's code):
    /// a candidate entry, which no generated block or root list can name.
    fn note_appended(&mut self, _pc: u32) {}
    /// A line for logs about the engine as a whole (instructions per tier).
    fn engine_report(&self, _total: u64, _interp: u64, _aot: u64) -> String {
        String::new()
    }
    /// Cumulative counters (instructions run by the tier, builds, compile time).
    fn fast_stats(&self) -> Option<FastStats> {
        None
    }
}

#[derive(Clone, Debug, Default)]
pub struct FastStats {
    pub built: u64,
    pub refused: u64,
    pub calls: u64,
    pub runs: u64,
    pub insns: u64,
    pub declined: [u64; 5],
    pub compile_ns: u64,
    pub build_ns: u64,
    /// Auto selection: PCs armed (counter passed the threshold), regions
    /// dropped as unprofitable, refusals by normalized reason.
    pub armed: u64,
    pub unprofitable: u64,
    /// Regions rebuilt with flat accesses after their dynamic windows kept failing.
    pub dyn_flat: u64,
    pub cached_refusals: u64,
    pub auto_skipped_aot: u64,
    pub reasons: std::collections::BTreeMap<String, u64>,
}

/// Hot-region selection (`SHARC_FAST_AUTO=threshold,min_insns[,blocks]`).
///
/// Candidates are the PCs the engine reports through `note`: the target of a
/// non-sequential PC change at an interpreter boundary (taken branch, call,
/// return, DO-loop loop-back) and the entry of a generated block at its
/// dispatch. Each is counted in a small direct-mapped table with lossy
/// replacement (a resident count only decays when another PC collides, so a
/// hot PC outlives cold ones). When a count reaches `threshold` the PC joins
/// the region list and the next boundary at it builds a region (loop shape,
/// then cfg). A refusal removes the PC again, remembering the MODE1 and the
/// code generation it was refused under; a region is dropped when, after a
/// warm-up of calls, the instructions it ran per call stay under
/// `min_insns` (the per-call fixed cost is not paid back). Generated-block
/// entries are counted too only with `blocks` (a region then replaces its
/// block; the compile cost is not paid back where blocks already are fast).
struct Auto {
    threshold: u32,
    min_insns: u64,
    blocks: bool,
    table: Box<[(u32, u32)]>,
    /// PC -> (MODE1 bits, dec_gen, tries) of the last refusal.
    tried: std::collections::HashMap<u32, (u32, u64, u8)>,
}

const AUTO_BITS: u32 = 13;
const AUTO_WARMUP: u64 = 32;
const AUTO_TRIES: u8 = 3;
pub const AUTO_DEFAULT_THRESHOLD: u32 = 256;
pub const AUTO_DEFAULT_MIN_INSNS: u64 = 64;

impl Auto {
    fn new(threshold: u32, min_insns: u64, blocks: bool) -> Auto {
        Auto {
            threshold: threshold.max(1),
            min_insns,
            blocks,
            table: vec![(u32::MAX, 0); 1 << AUTO_BITS].into_boxed_slice(),
            tried: Default::default(),
        }
    }
}

/// `SHARC_FAST_AUTO=threshold,min_insns[,blocks]` -> (threshold,
/// min_insns, blocks); None when unset or `0`.
pub fn auto_config() -> Option<(u32, u64, bool)> {
    let v = std::env::var("SHARC_FAST_AUTO").ok()?;
    let v = v.trim();
    if v.is_empty() || v == "0" {
        return None;
    }
    let mut it = v.split(',');
    let t = it
        .next()
        .and_then(|x| x.trim().parse().ok())
        .unwrap_or(AUTO_DEFAULT_THRESHOLD);
    let m = it
        .next()
        .and_then(|x| x.trim().parse().ok())
        .unwrap_or(AUTO_DEFAULT_MIN_INSNS);
    let b = it.next().is_some_and(|x| x.trim() == "1");
    Some((t, m, b))
}

fn reason_class(r: &str) -> String {
    let mut out = String::new();
    let mut digits = false;
    for c in r.chars().take(60) {
        if c.is_ascii_digit() || c == 'x' && digits {
            if !digits {
                out.push('#');
            }
            digits = true;
        } else {
            digits = false;
            out.push(c);
        }
    }
    out
}

#[derive(Default)]
struct Slot {
    /// After a window decline the next `win_skip` calls are not tried (the
    /// interpreter runs them): 1, 3, 7, ... up to 2^`WIN_BACKOFF_MAX` - 1,
    /// reset by a run. A declined call costs a window resolution for nothing.
    win_skip: u32,
    win_back: u8,
    region: Option<region::Region>,
    kernel: Option<Box<dyn CompiledKernel>>,
    refused: Option<String>,
    /// Further variants of the region (most recently replaced first): the
    /// same entry made for other values of the state the build baked in
    /// (MODE1 bits, constants, the CACC history), tried in turn when `region`
    /// declines on a requirement. At most `MAX_VARIANTS - 1`.
    alts: Vec<(region::Region, Box<dyn CompiledKernel>)>,
    /// Calls in a row that no variant accepted (reset by any run).
    alt_fail: u32,
    rebuilds: u32,
    /// M registers whose baked value failed the last requirement check.
    last_eq: u64,
    /// Calls whose first instruction left the kernel before completing.
    exit0: u32,
    /// Calls declined for want of a resolvable window (`cfg::flat_auto`).
    win_fails: u32,
    /// Calls that left the kernel at an instruction with a guarded dynamic
    /// window (`dynflat_range`).
    dyn_exits: u32,
    transient: u32,
    /// `SHARC_FAST_DEFER`: calls still to be interpreted before the freshly
    /// built kernel is used (simulates a background compile that has not
    /// finished). Measurement only.
    defer: u32,
    /// `SHARC_FAST_TL`: why the next build of this slot happens.
    tl_why: &'static str,
    /// Selected by `Auto` (dropped again on refusal or low yield).
    auto: bool,
    /// Made an entry because the instruction before it cannot run in a kernel
    /// (`FastEngine::chase`). Dropped when its calls retire few instructions
    /// (`CHASE_MIN_INSNS`): a call that retires one or two costs more than
    /// interpreting them.
    chased: bool,
    calls: u64,
    insns: u64,
    /// Host ticks spent in calls that ran (`SHARC_FAST_TIME`).
    ticks: u64,
    cov: SlotCov,
}

/// Per-entry coverage counters (`SHARC_FAST_COV`).
#[derive(Default)]
struct SlotCov {
    /// Declines by `glue::Decline` (shape, req, window, budget, exit0).
    declined: [u64; 5],
    /// Kernel exits by (exit pc, kind): count, insns retired by the kernel,
    /// insns until the next fast-tier call (capped per exit).
    exits: std::collections::BTreeMap<(u32, &'static str), [u64; 3]>,
    /// Entry requirement failures by class.
    decl_why: std::collections::BTreeMap<String, u64>,
}

const BLOOM_WORDS: usize = 1024;

/// Variants of one entry (the current region and its alternatives), and the
/// builds one entry may spend on new ones.
const MAX_VARIANTS: usize = 4;
const REBUILD_BUDGET: u32 = 16;
const WIN_BACKOFF_MAX: u8 = 8;
/// A chased entry is dropped after `CHASE_TRIAL` calls that retired fewer than
/// `CHASE_MIN_INSNS` instructions each on average.
const CHASE_TRIAL: u64 = 64;
const CHASE_MIN_INSNS: u64 = 3;
/// Window declines of one entry before it is rebuilt with flat accesses.
const WIN_FLAT_AFTER: u32 = 3;
/// After the build budget is spent, the alternatives are no longer tried
/// once this many calls in a row were declined by every variant.
const ALT_GIVE_UP: u32 = 64;

/// Multiplicative hash for PC keys.
#[derive(Default)]
pub struct PcHash(u64);

impl std::hash::Hasher for PcHash {
    fn finish(&self) -> u64 {
        self.0 ^ (self.0 >> 32)
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ b as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        }
    }
    fn write_u32(&mut self, v: u32) {
        self.0 = (v as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    }
}

pub struct FastEngine {
    backend: Box<dyn KernelBackend>,
    /// Entry PCs and their regions, in step; `bloom` is a 64 Kbit filter of
    /// the PCs so `wants` (asked at every block boundary) is one test, and
    /// `lookup` maps a PC to its index for the few that pass it.
    pcs: Vec<u32>,
    fallback: Vec<u32>,
    slots: Vec<Slot>,
    bloom: Box<[u64]>,
    bloom_stale: u32,
    lookup: std::collections::HashMap<u32, usize, std::hash::BuildHasherDefault<PcHash>>,
    ctx: glue::Ctx,
    pub stats: FastStats,
    log: bool,
    /// Reports at exit (`SHARC_FAST_LOG` set to anything).
    summary: bool,
    /// `SHARC_FAST_TIME`: host ticks per region, reported at exit.
    time: bool,
    /// `SHARC_FAST_FOLLOW=1`: the instruction after an instruction a region
    /// stopped at (one the lowering cannot run) becomes an entry too, so the
    /// code after it runs from a region again once the interpreter has run
    /// the stop instruction. Never dropped as unprofitable.
    follow: bool,
    /// An entry that is refused at its first instruction, which then runs in
    /// the interpreter, makes the next instruction an entry (on by default;
    /// `SHARC_FAST_CHASE=0` turns it off).
    chase: bool,
    follow_queue: Vec<u32>,
    /// Entries to add as chased (`chase`).
    chase_queue: Vec<u32>,
    /// `SHARC_FAST_ONLY=pc,pc,..|@file`: only these PCs may become entries
    /// at run time (`arm`, `add_entry`); `tools/sharc_dev/fast_ddmin.py` shrinks a set
    /// of regions that reproduces a divergence with it.
    only: Option<std::collections::HashSet<u32>>,
    /// `SHARC_FAST_NOBACKOFF` turns the window-decline back-off off.
    win_backoff: bool,
    cap_chain: bool,
    /// Entry PCs not yet added to the engine's chain stop set
    /// (`FastTier::sync_chain_stops`); filled at setup and by `arm`.
    stops_new: Vec<u32>,
    force_cfg: bool,
    auto: Option<Auto>,
    /// Block-translator cluster roots (`SHARC_BLOCKTRANS`), sorted: built as
    /// CFG regions only, with the guarded dynamic windows of the cluster
    /// lowering (`Env::guarded_dyn`).
    clusters: Vec<u32>,
    /// `SHARC_FAST_COV`: coverage report lines (`cov ...`, see
    /// `tools/sharc_dev/blocktrans_coverage.py`); off by default.
    cov: bool,
    /// The last kernel exit awaiting the gap to the next fast-tier call:
    /// (slot, exit pc, exit kind, icount at exit).
    cov_last: Option<(usize, u32, &'static str, u64)>,
    /// `SHARC_FAST_TL=1`: a timeline of builds and compiles on stderr
    /// (`tl build ...`, `tl tick ...`); measurement only.
    tl: Option<Tl>,
    /// `SHARC_FAST_DEFER=N`: interpret the first N..4N calls of a freshly built
    /// kernel before using it (N scaled by the entry PC); measurement only.
    defer_n: u32,
}

/// State of the `SHARC_FAST_TL` timeline.
struct Tl {
    t0: clock::Instant,
    next_tick: u64,
    seen: std::collections::HashSet<u64>,
    dup: u64,
    builds: u64,
}

/// Instructions per timeline tick (1/4 audio second at ~1G insns/s counting idle skip).
const TL_TICK: u64 = 250_000_000;

#[cfg(unix)]
fn thread_cpu_ns() -> u64 {
    #[repr(C)]
    struct Ts {
        sec: i64,
        nsec: i64,
    }
    unsafe extern "C" {
        fn clock_gettime(clock: i32, ts: *mut Ts) -> i32;
    }
    #[cfg(target_os = "macos")]
    const CLK: i32 = 16;
    #[cfg(not(target_os = "macos"))]
    const CLK: i32 = 3;
    let mut ts = Ts { sec: 0, nsec: 0 };
    // SAFETY: valid out pointer.
    if unsafe { clock_gettime(CLK, &mut ts) } != 0 {
        return 0;
    }
    ts.sec as u64 * 1_000_000_000 + ts.nsec as u64
}
#[cfg(not(unix))]
fn thread_cpu_ns() -> u64 {
    0
}

// SAFETY: a FastEngine is owned by one Engine and used from one thread at a
// time (the engine is moved between threads, never shared); its compiled
// kernels are plain code and the buffers they touch are passed per call.
unsafe impl Send for FastEngine {}

type BackendFactory = fn() -> Box<dyn KernelBackend>;
static BACKEND: Mutex<Option<BackendFactory>> = Mutex::new(None);

/// Register the kernel backend `SHARC_FAST_REGIONS` engines use (the
/// Cranelift crate calls this); without one the reference interpreter runs.
pub fn register_backend(f: BackendFactory) {
    *BACKEND.lock().unwrap() = Some(f);
}

/// The registered backend, else the shared library named by
/// `SHARC_FAST_BACKEND`, else the (slow, exact) reference interpreter.
pub fn default_backend() -> Box<dyn KernelBackend> {
    if let Some(f) = *BACKEND.lock().unwrap() {
        return f();
    }
    #[cfg(unix)]
    if let Ok(path) = std::env::var("SHARC_FAST_BACKEND") {
        match plugin::PluginBackend::open(&path) {
            Ok(b) => return Box::new(b),
            Err(e) => eprintln!("fast tier: {e}; using the reference interpreter"),
        }
    }
    Box::new(interp::InterpBackend)
}

impl FastEngine {
    pub fn new(backend: Box<dyn KernelBackend>, pcs: &[u32]) -> FastEngine {
        dump::init();
        let mut list: Vec<u32> = pcs.to_vec();
        list.sort_unstable();
        list.dedup();
        let mut fe = FastEngine {
            backend,
            fallback: Vec::new(),
            bloom: vec![0u64; BLOOM_WORDS].into_boxed_slice(),
            bloom_stale: 0,
            lookup: Default::default(),
            slots: list.iter().map(|_| Slot::default()).collect(),
            pcs: list,
            ctx: glue::Ctx::default(),
            stats: FastStats::default(),
            // SHARC_FAST_LOG=summary: only the reports at exit (tools/devtools/fastcov.py);
            // any other value also logs every event and run.
            log: {
                let v = std::env::var("SHARC_FAST_LOG").ok();
                let on = v.as_deref().is_some_and(|v| v != "summary");
                glue::set_shape_log(on);
                on
            },
            summary: std::env::var_os("SHARC_FAST_LOG").is_some(),
            time: std::env::var("SHARC_FAST_TIME").is_ok_and(|v| !v.is_empty() && v != "0"),
            follow: std::env::var("SHARC_FAST_FOLLOW").is_ok_and(|v| v == "1"),
            chase: std::env::var("SHARC_FAST_CHASE").map_or(true, |v| v != "0"),
            follow_queue: Vec::new(),
            chase_queue: Vec::new(),
            win_backoff: std::env::var_os("SHARC_FAST_NOBACKOFF").is_none(),
            only: std::env::var("SHARC_FAST_ONLY").ok().map(|v| {
                let text = match v.strip_prefix('@') {
                    Some(path) => std::fs::read_to_string(path).unwrap_or_default(),
                    None => v,
                };
                text.split(|c: char| c == ',' || c.is_whitespace())
                    .filter_map(|x| u32::from_str_radix(x.trim().trim_start_matches("0x"), 16).ok())
                    .collect()
            }),
            // SHARC_FAST_CHAIN=0 keeps chaining (entries reached by a chain
            // are then missed).
            cap_chain: std::env::var("SHARC_FAST_CHAIN").map_or(true, |v| v != "0"),
            stops_new: Vec::new(),
            force_cfg: std::env::var("SHARC_FAST_SHAPE").is_ok_and(|v| v == "cfg"),
            auto: None,
            clusters: Vec::new(),
            cov: {
                let on = std::env::var_os("SHARC_FAST_COV").is_some();
                glue::set_cov(on);
                on
            },
            cov_last: None,
            tl: std::env::var_os("SHARC_FAST_TL").map(|_| Tl {
                t0: clock::Instant::now(),
                next_tick: TL_TICK,
                seen: Default::default(),
                dup: 0,
                builds: 0,
            }),
            defer_n: std::env::var("SHARC_FAST_DEFER")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
        };
        fe.stops_new = fe.pcs.clone();
        fe.rebuild_bloom();
        fe
    }

    /// Make the instruction after each stop instruction an entry
    /// (`SHARC_FAST_FOLLOW=1` without an environment).
    pub fn with_follow(mut self, on: bool) -> FastEngine {
        self.follow = on;
        self
    }

    /// Make the entries ROOTS block-translator clusters (adding them as
    /// entries): CFG regions with the cluster lowering (`Env::guarded_dyn`).
    pub fn with_clusters(mut self, roots: &[u32]) -> FastEngine {
        for &pc in roots {
            if self.fallback.binary_search(&pc).is_ok() {
                self.fallback.retain(|&fallback| fallback != pc);
                self.stops_new.push(pc);
            }
            if self.index(pc).is_none() {
                self.lookup.insert(pc, self.pcs.len());
                self.pcs.push(pc);
                self.stops_new.push(pc);
                self.slots.push(Slot::default());
                let (w, m) = Self::bit(pc);
                self.bloom[w] |= m;
            }
        }
        self.clusters = roots.to_vec();
        self.clusters.sort_unstable();
        self.clusters.dedup();
        self
    }

    /// Register fallback roots without intercepting healthy generated chains.
    /// Entries explicitly registered as regular regions or clusters take priority.
    pub fn with_fallback(mut self, roots: &[u32]) -> FastEngine {
        for &pc in roots {
            if self.index(pc).is_none() {
                self.lookup.insert(pc, self.pcs.len());
                self.pcs.push(pc);
                self.slots.push(Slot::default());
                self.fallback.push(pc);
                let (w, m) = Self::bit(pc);
                self.bloom[w] |= m;
            }
        }
        self.fallback.sort_unstable();
        self.fallback.dedup();
        self
    }

    /// Also pick regions at run time (see `Auto`).
    pub fn with_auto(mut self, threshold: u32, min_insns: u64, blocks: bool) -> FastEngine {
        self.auto = Some(Auto::new(threshold, min_insns, blocks));
        if !blocks && self.pcs.len() == self.fallback.len() {
            // Only interpreter boundaries are noted and no ordinary entry is
            // fixed in advance: fallback roots do not intercept healthy chains.
            self.cap_chain = false;
        }
        self
    }

    fn rebuild_bloom(&mut self) {
        self.bloom.fill(0);
        for &pc in &self.pcs {
            let (w, m) = Self::bit(pc);
            self.bloom[w] |= m;
        }
        self.lookup.clear();
        for (i, &pc) in self.pcs.iter().enumerate() {
            self.lookup.insert(pc, i);
        }
        self.bloom_stale = 0;
    }

    /// Forget the slot of an auto-selected PC.
    fn drop_slot(&mut self, pc: u32) {
        let Some(i) = self.lookup.remove(&pc) else {
            return;
        };
        self.pcs.swap_remove(i);
        self.slots.swap_remove(i);
        // Slot indexes moved: the pending exit gap would name another slot
        // (or none: an index past the end panicked under SHARC_FAST_COV).
        self.cov_last = None;
        if let Some(&moved) = self.pcs.get(i) {
            self.lookup.insert(moved, i);
        }
        // The filter keeps the removed PC's bit (a false positive costs one
        // lookup); rebuild it now and then.
        self.bloom_stale += 1;
        if self.bloom_stale >= 256 {
            self.rebuild_bloom();
        }
    }

    #[inline(always)]
    fn bit(pc: u32) -> (usize, u64) {
        let h = pc.wrapping_mul(0x9e37_79b1) >> 16;
        ((h >> 6) as usize, 1u64 << (h & 63))
    }

    #[inline(always)]
    fn index(&self, pc: u32) -> Option<usize> {
        let (w, m) = Self::bit(pc);
        if self.bloom[w] & m == 0 {
            return None;
        }
        self.lookup.get(&pc).copied()
    }

    pub fn backend_name(&self) -> &'static str {
        self.backend.name()
    }

    /// Skip the loop shape and build CFG regions only (tests, comparisons).
    pub fn force_cfg(&mut self, on: bool) {
        self.force_cfg = on;
    }

    /// Entry PCs whose region was refused, with the reason.
    pub fn refusals(&self) -> Vec<(u32, String)> {
        self.pcs
            .iter()
            .zip(&self.slots)
            .filter_map(|(pc, s)| s.refused.clone().map(|r| (*pc, r)))
            .collect()
    }

    /// The built region at PC (tests and reports).
    pub fn region(&self, pc: u32) -> Option<&region::Region> {
        self.slots[self.index(pc)?].region.as_ref()
    }

    fn build(&mut self, s: &mut St, pc: u32, aot: Option<&dyn Fn(u32) -> bool>) -> Option<()> {
        if self.tl.is_none() && self.defer_n == 0 {
            return self.build_inner(s, pc, aot);
        }
        let (b0, c0) = (self.stats.build_ns, self.stats.compile_ns);
        let t = clock::Instant::now();
        let ix = self.lookup.get(&pc).copied();
        let why = ix.map_or("", |i| std::mem::take(&mut self.slots[i].tl_why));
        let kind = if !why.is_empty() {
            why
        } else if ix.is_some_and(|i| self.slots[i].rebuilds > 0) {
            "rebuild"
        } else if ix.is_some_and(|i| self.slots[i].transient > 0) {
            "retry"
        } else {
            "first"
        };
        let (sc, si) = ix.map_or((0, 0), |i| (self.slots[i].calls, self.slots[i].insns));
        let r = self.build_inner(s, pc, aot);
        let wall_us = t.elapsed().as_micros();
        if let Some(i) = ix
            && r.is_some()
            && self.defer_n > 0
        {
            self.slots[i].defer = self.defer_n * (1 + ((pc >> 2) & 3));
        }
        if let Some(tl) = &mut self.tl {
            tl.builds += 1;
            let (ops, helpers, words, hash) = match ix.and_then(|i| self.slots[i].region.as_ref()) {
                Some(reg) if r.is_some() => {
                    let mut w = reg.kernel.encode();
                    let helpers = reg
                        .kernel
                        .pre
                        .iter()
                        .chain(&reg.kernel.body)
                        .chain(&reg.kernel.post)
                        .filter(|i| matches!(i, ir::Inst::Def(_, ir::Op::Helper(_))))
                        .count();
                    let n = w.len();
                    // Helper records are host pointers: not part of the identity.
                    let mut h = 0xcbf2_9ce4_8422_2325u64;
                    let mut i = 0;
                    while i < w.len() {
                        h = (h ^ w[i] as u64).wrapping_mul(0x100_0000_01b3);
                        i += 1;
                    }
                    if helpers == 0 && !tl.seen.insert(h) {
                        tl.dup += 1;
                    } else if helpers == 0
                        && let Some(dir) = std::env::var_os("SHARC_FAST_TL_DUMP")
                    {
                        // Measurement: the pointer-free kernels, for the wasm backend.
                        let bytes: Vec<u8> = w.iter().flat_map(|x| x.to_le_bytes()).collect();
                        let _ = std::fs::create_dir_all(&dir);
                        let _ = std::fs::write(
                            std::path::Path::new(&dir).join(format!("kernel-{h:016x}.bin")),
                            bytes,
                        );
                    }
                    w.clear();
                    (reg.kernel.inst_count(), helpers, n, h)
                }
                _ => (0, 0, 0, 0),
            };
            eprintln!(
                "tl build t_ms={:.3} ic={} pc={pc:#x} kind={kind} ok={} build_us={} compile_us={} wall_us={wall_us} scalls={sc} sinsns={si} ops={ops} helpers={helpers} words={words} hash={hash:016x} dup={}",
                tl.t0.elapsed().as_secs_f64() * 1e3,
                s.icount,
                r.is_some() as u8,
                (self.stats.build_ns - b0) / 1000,
                (self.stats.compile_ns - c0) / 1000,
                tl.dup,
            );
        }
        r
    }

    fn build_inner(
        &mut self,
        s: &mut St,
        pc: u32,
        aot: Option<&dyn Fn(u32) -> bool>,
    ) -> Option<()> {
        let t0 = clock::Instant::now();
        // The remaining iterations of the active DO loop that starts here,
        // else (or when that is refused) the CFG shape from this pc.
        // SHARC_FAST_SHAPE=cfg skips the loop shape (tests, comparisons).
        let cluster = self.clusters.binary_search(&pc).is_ok();
        let loop_try = if self.force_cfg || cluster {
            None
        } else {
            Some(region::build_loop(s, pc))
        };
        let mut no_loop = false;
        let built = match loop_try {
            Some(Ok(r)) => Ok(r),
            other => {
                let loop_err = match other {
                    Some(Err(e)) if e.0.starts_with("no active loop") => {
                        no_loop = true;
                        None
                    }
                    Some(Err(e)) => Some(e),
                    _ => None,
                };
                // An auto region stops at every hand-picked entry.
                let barrier: std::collections::BTreeSet<u32> =
                    if self.lookup.get(&pc).is_some_and(|&i| self.slots[i].auto) {
                        self.lookup
                            .iter()
                            .filter(|&(&p, &i)| p != pc && !self.slots[i].auto)
                            .map(|(&p, _)| p)
                            .collect()
                    } else {
                        Default::default()
                    };
                match region::build_cfg_opts(s, pc, cluster, &barrier) {
                    Ok(r) => Ok(r),
                    Err(e) => Err(match loop_err {
                        Some(l) => {
                            lower::Refuse(format!("loop shape: {}; cfg shape: {}", l.0, e.0))
                        }
                        None => e,
                    }),
                }
            }
        };
        self.stats.build_ns += t0.elapsed().as_nanos() as u64;
        let slot = &mut self.slots[self.lookup.get(&pc).copied()?];
        match built {
            Err(e) => {
                if no_loop && slot.transient < 4 {
                    // The loop shape needs an active loop here, which may
                    // be the case on another visit: try again later.
                    slot.transient += 1;
                    return None;
                }
                if self.log {
                    eprintln!("fast: {pc:#x} refused: {}", e.0);
                }
                if self.cov {
                    eprintln!("cov refused root={pc:#x} why={}", e.0);
                }
                self.stats.refused += 1;
                *self.stats.reasons.entry(reason_class(&e.0)).or_default() += 1;
                let chase_to = if self.chase && !slot.auto {
                    chase_target(s, pc, &e.0)
                } else {
                    None
                };
                slot.refused = Some(e.0);
                self.chase_queue.extend(chase_to);
                None
            }
            Ok(mut r) => {
                // A root over a healthy generated entry must not stop at a
                // PC that only the interpreter would run: it would replace
                // fast generated code with interpreted code. Roots without
                // such an entry (barred, appended) have the interpreter as
                // the alternative anyway.
                if let Some(aot) = aot
                    && aot(pc)
                    && let Some(c) = &r.cfg
                    && let Some((at, _)) = c.stops.iter().find(|(at, why)| {
                        !stop_is_exempt(why) && !self.lookup.contains_key(at) && !aot(*at)
                    })
                {
                    let why = format!("stops before an entry: {at:#x}");
                    if self.log {
                        eprintln!("fast: {pc:#x} refused: {why}");
                    }
                    self.stats.refused += 1;
                    *self.stats.reasons.entry(reason_class(&why)).or_default() += 1;
                    slot.refused = Some(why);
                    return None;
                }
                let t1 = clock::Instant::now();
                let k = self.backend.compile(&r.kernel);
                self.stats.compile_ns += t1.elapsed().as_nanos() as u64;
                match k {
                    Err(e) => {
                        if self.log {
                            eprintln!("fast: {pc:#x} compile failed: {e}");
                        }
                        self.stats.refused += 1;
                        *self
                            .stats
                            .reasons
                            .entry(format!("compile: {}", reason_class(&e)))
                            .or_default() += 1;
                        slot.refused = Some(e);
                        None
                    }
                    Ok(k) => {
                        for &(at, _) in &r.code_words {
                            s.mem.watch_sw(at);
                        }
                        r.verified_gen = s.mem.dec_gen;
                        if self.log {
                            eprintln!(
                                "fast: {pc:#x} built: {} insns, {} windows, {} reqs ({} circular), kernel {} ops",
                                r.len(),
                                r.wins.len(),
                                r.reqs.len(),
                                r.within.len(),
                                r.kernel.inst_count()
                            );
                            if let Some(c) = &r.cfg {
                                for (at, why) in &c.stops {
                                    eprintln!("fast: {pc:#x} region stops at {at:#x}: {why}");
                                }
                            }
                        }
                        if self.cov {
                            cov_built(s, pc, &r);
                        }
                        let chasing = self.chase && slot.chased;
                        if (self.follow || chasing)
                            && let Some(c) = &r.cfg
                        {
                            for (at, why) in &c.stops {
                                if stop_is_exempt(why) {
                                    continue;
                                }
                                if let Ok(d) = region::decode_one(s, *at) {
                                    if self.follow {
                                        self.follow_queue.push(*at + d.len_sw);
                                    } else {
                                        self.chase_queue.push(*at + d.len_sw);
                                    }
                                }
                            }
                        }
                        // A guarded dynamic-window access leaves before its
                        // instruction when the address is outside the span:
                        // the instruction after it is an entry too.
                        if self.follow && r.wins.iter().any(|w| w.dynamic_anchor.is_some()) {
                            for i in &r.insns {
                                if let Ok(d) = region::decode_one(s, i.pc)
                                    && matches!(
                                        d.form,
                                        "3a" | "3b"
                                            | "3c"
                                            | "4a"
                                            | "4b"
                                            | "4d"
                                            | "6a_mem"
                                            | "15a"
                                            | "15b"
                                    )
                                {
                                    self.follow_queue.push(i.pc + d.len_sw);
                                }
                            }
                        }
                        self.stats.built += 1;
                        slot.region = Some(r);
                        slot.kernel = Some(k);
                        Some(())
                    }
                }
            }
        }
    }
}

/// Calls, jumps, loops and delay slots leave the interpreter at another PC
/// (an entry of its own or not worth one): such stops are not mid-block.
fn stop_is_exempt(why: &str) -> bool {
    ["call", "jump", "indirect", "delay", "loop", "zero", "DO"]
        .iter()
        .any(|k| why.starts_with(k))
}

/// On by default (`SHARC_FAST_DYNFLAT=0` turns it off; `LO-HI`, hex PCs, limits it to a range): a region whose calls mostly
/// leave the kernel at an instruction with a guarded dynamic window (the span
/// anchored on an index register cannot hold an address that is a loaded
/// offset from it, so the guard fails on every call) is rebuilt with that
/// instruction as a flat guarded access (`cfg::mark_flat_pc`). Exact: a flat
/// access guards every access and leaves before the instruction.
fn dynflat_range() -> Option<(u32, u32)> {
    static R: std::sync::OnceLock<Option<(u32, u32)>> = std::sync::OnceLock::new();
    *R.get_or_init(|| {
        let v = std::env::var("SHARC_FAST_DYNFLAT").unwrap_or_else(|_| "1".into());
        let hex = |t: &str| u32::from_str_radix(t.trim().trim_start_matches("0x"), 16).ok();
        match v.as_str() {
            "" | "0" => None,
            "1" => Some((0, u32::MAX)),
            r => r
                .split_once('-')
                .and_then(|(a, b)| Some((hex(a)?, hex(b)?))),
        }
    })
}

/// Cap on the gap (instructions) recorded after one kernel exit.
const COV_GAP_CAP: u64 = 4096;

/// `SHARC_FAST_COV`: what a built region covers and where it stops.
fn cov_built(s: &St, pc: u32, r: &region::Region) {
    let mut pcs: Vec<String> = r.insns.iter().map(|i| format!("{:#x}", i.pc)).collect();
    pcs.sort();
    eprintln!(
        "cov built root={pc:#x} insns={} kernel_ops={} windows={} pcs={}",
        r.len(),
        r.kernel.inst_count(),
        r.wins.len(),
        pcs.join(",")
    );
    if let Some(c) = &r.cfg {
        for (at, why) in &c.stops {
            let form = region::decode_one(s, *at).map_or("undecodable", |d| d.form);
            eprintln!("cov stop root={pc:#x} at={at:#x} form={form} why={why}");
        }
    }
}

/// The failing requirement is only "this index register lies inside its
/// circular buffer" (`Req::Within`): a property of the state, not a baked
/// constant, so re-making the region from it gives the same region again.
fn req_is_within(s: &St, r: &region::Region) -> bool {
    for &c in &r.known {
        if s.r[c as usize].m != u32::MAX {
            return false;
        }
    }
    if r.eqs.iter().any(|&(c, k)| {
        let v = s.r[c as usize];
        v.m != u32::MAX || v.b != k
    }) {
        return false;
    }
    r.within.iter().any(|&(c, lo, hi)| {
        let v = s.r[c as usize];
        !(v.is_c() && (lo..=hi).contains(&v.b))
    })
}

/// Which kind of requirement a `Decline::Req` was (diagnostics).
fn req_failure(s: &St, r: &region::Region) -> String {
    for &c in &r.known {
        if s.r[c as usize].m != u32::MAX {
            return format!("known r{c}");
        }
    }
    for &(c, k) in &r.eqs {
        let v = s.r[c as usize];
        if v.m != u32::MAX || v.b != k {
            return format!(
                "eq r{c} {k:#x} -> {:#x} (mode1 {:#x})",
                v.b,
                s.r[crate::rt::MODE1].b
            );
        }
    }
    for q in &r.nw {
        if !glue::eval_req(s, q, 1) {
            return "nw-plain address".into();
        }
    }
    for &(c, lo, hi) in &r.within {
        let v = s.r[c as usize];
        if !(v.is_c() && (lo..=hi).contains(&v.b)) {
            return format!("within r{c} {lo:#x}..{hi:#x} -> {:#x}", v.b);
        }
    }
    "misc (mode1/flags)".into()
}

/// Are the code words R was made from still the ones in memory? (Cached
/// after the check while `Mem::dec_gen` stays.)
fn verify_code(s: &St, r: &mut region::Region) -> bool {
    if s.mem.dec_gen == r.verified_gen {
        return true;
    }
    let same = r
        .code_words
        .iter()
        .all(|&(at, w)| s.mem.read_sw(at).unwrap_or(0) == w);
    if same {
        r.verified_gen = s.mem.dec_gen;
    }
    same
}

/// One try of a variant: `Err` leaves the state untouched.
#[inline(always)]
fn run_variant(
    s: &mut St,
    r: &region::Region,
    k: &dyn CompiledKernel,
    ctx: &mut glue::Ctx,
) -> Result<u32, glue::Decline> {
    if r.cfg.is_some() {
        glue::run_cfg(s, r, k, ctx)
    } else {
        glue::run(s, r, k, ctx)
    }
}

fn mode1_bits(s: &St) -> u32 {
    let m = s.r[crate::rt::MODE1];
    if m.is_c() { m.b as u32 } else { u32::MAX }
}

impl FastEngine {
    /// Make PC an entry (kept for good: not auto-selected, so never dropped).
    fn add_entry(&mut self, pc: u32) {
        if self.index(pc).is_some() || self.only.as_ref().is_some_and(|o| !o.contains(&pc)) {
            return;
        }
        if self.log {
            eprintln!("fast: {pc:#x} followed");
        }
        self.lookup.insert(pc, self.pcs.len());
        self.pcs.push(pc);
        self.stops_new.push(pc);
        self.slots.push(Slot::default());
        let (w, m) = Self::bit(pc);
        self.bloom[w] |= m;
    }

    fn drain_chase(&mut self) {
        while let Some(next) = self.chase_queue.pop() {
            self.add_chased(next);
        }
    }

    /// Make PC an entry after a refused instruction (see `chase`).
    fn add_chased(&mut self, pc: u32) {
        if self.index(pc).is_some() {
            return;
        }
        self.add_entry(pc);
        if let Some(ix) = self.index(pc) {
            self.slots[ix].chased = true;
        }
    }

    /// A counter reached the threshold: make the PC an entry.
    fn arm(&mut self, pc: u32) {
        if self.index(pc).is_some() || self.only.as_ref().is_some_and(|o| !o.contains(&pc)) {
            return;
        }
        if self
            .auto
            .as_ref()
            .is_some_and(|a| a.tried.get(&pc).is_some_and(|t| t.2 >= AUTO_TRIES))
        {
            return;
        }
        self.stats.armed += 1;
        if self.log {
            eprintln!("fast: {pc:#x} armed");
        }
        self.lookup.insert(pc, self.pcs.len());
        self.pcs.push(pc);
        self.stops_new.push(pc);
        self.slots.push(Slot {
            auto: true,
            ..Slot::default()
        });
        let (w, m) = Self::bit(pc);
        self.bloom[w] |= m;
    }
}

/// The instruction after the refused entry PC, when the interpreter will run
/// PC and then reach that address in sequence: the refusal is "the entry
/// instruction cannot run here" and the instruction is neither a branch, a
/// call, a return, a loop setup nor a delay slot (`cfg` builds those stops as
/// their own kinds, and the instruction after them is not the next PC).
fn chase_target(s: &St, pc: u32, why: &str) -> Option<u32> {
    let why = why.strip_prefix("the entry instruction cannot run here: ")?;
    if why.starts_with("delay slot") {
        return None;
    }
    let d = region::decode_one(s, pc).ok()?;
    let digits = d.form.len()
        - d.form
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .len();
    let control = matches!(
        &d.form[..digits],
        "8" | "9" | "10" | "11" | "12" | "13" | "25"
    );
    (!control).then(|| pc + d.len_sw)
}

impl Drop for FastEngine {
    fn drop(&mut self) {
        if dump::enabled() {
            let rows: Vec<(u32, u64, u64)> = self
                .pcs
                .iter()
                .zip(&self.slots)
                .filter(|(_, sl)| sl.calls > 0)
                .map(|(pc, sl)| (*pc, sl.calls, sl.insns))
                .collect();
            dump::totals(&rows);
        }
        if self.summary && (self.stats.calls > 0 || self.stats.armed > 0) {
            eprintln!("{}", self.report());
        }
        if self.time {
            let mut rows: Vec<(&u32, &Slot)> = self
                .pcs
                .iter()
                .zip(&self.slots)
                .filter(|(_, sl)| sl.calls > 0)
                .collect();
            rows.sort_by(|a, b| b.1.ticks.cmp(&a.1.ticks));
            eprintln!(
                "fast time: {} regions with calls, tick rate {} Hz (pc, origin, shape, len, calls, insns, ticks, ticks/insn)",
                rows.len(),
                crate::tick_hz()
            );
            for (pc, sl) in rows.into_iter().take(60) {
                let shape = match &sl.region {
                    Some(r) if r.cfg.is_some() => "cfg",
                    Some(_) => "loop",
                    None => "none",
                };
                eprintln!(
                    "fast time {pc:#x} {} {shape} {} {} {} {} {:.3}",
                    if sl.auto { "auto" } else { "list" },
                    sl.region.as_ref().map_or(0, |r| r.len()),
                    sl.calls,
                    sl.insns,
                    sl.ticks,
                    sl.ticks as f64 / sl.insns.max(1) as f64
                );
            }
        }
        if self.cov {
            for (pc, s) in self.pcs.iter().zip(&self.slots) {
                let exits: Vec<String> = s
                    .cov
                    .exits
                    .iter()
                    .map(|((at, k), e)| format!("{at:#x}:{k}:{}:{}:{}", e[0], e[1], e[2]))
                    .collect();
                let why: Vec<String> = s
                    .cov
                    .decl_why
                    .iter()
                    .map(|(w, n)| format!("{n}x {w}"))
                    .collect();
                eprintln!(
                    "cov run root={pc:#x} built={} refused={} calls={} insns={} declined={:?} exits={} declwhy={}",
                    s.region.is_some() as u8,
                    s.refused.is_some() as u8,
                    s.calls,
                    s.insns,
                    s.cov.declined,
                    exits.join(";"),
                    why.join(";")
                );
            }
        }
    }
}

impl FastEngine {
    fn run_inner(
        &mut self,
        s: &mut St,
        pc: u32,
        chained: bool,
        aot: Option<&dyn Fn(u32) -> bool>,
    ) -> Option<u32> {
        if let Some(tl) = &mut self.tl
            && s.icount >= tl.next_tick
        {
            tl.next_tick = s.icount - s.icount % TL_TICK + TL_TICK;
            eprintln!(
                "tl tick t_ms={:.1} ic={} cpu_ms={:.1} compile_ms={:.1} build_ms={:.1} builds={} calls={} runs={} insns={}",
                tl.t0.elapsed().as_secs_f64() * 1e3,
                s.icount,
                thread_cpu_ns() as f64 / 1e6,
                self.stats.compile_ns as f64 / 1e6,
                self.stats.build_ns as f64 / 1e6,
                tl.builds,
                self.stats.calls,
                self.stats.runs,
                self.stats.insns,
            );
        }
        while let Some(next) = self.follow_queue.pop() {
            self.add_entry(next);
        }
        self.drain_chase();
        let ix = self.index(pc)?;
        if chained {
            // Only a built, current region of the CFG shape; nothing is
            // counted unless the kernel runs.
            let slot = &self.slots[ix];
            let region = slot.region.as_ref()?;
            if slot.refused.is_some()
                || slot.kernel.is_none()
                || slot.defer > 0
                || region.cfg.is_none()
                || s.mem.dec_gen != region.verified_gen
            {
                return None;
            }
        }
        let cov_prev = self.cov_last.take();
        if let Some((lix, at, kind, ic)) = cov_prev
            && let Some(e) = self.slots[lix].cov.exits.get_mut(&(at, kind))
        {
            e[2] += (s.icount - ic).min(COV_GAP_CAP);
        }
        if self.slots[ix].refused.is_some() {
            return None;
        }
        if self.slots[ix].region.is_none() {
            let m1 = mode1_bits(s);
            if self.slots[ix].auto
                && let Some(a) = &self.auto
                && a.tried
                    .get(&pc)
                    .is_some_and(|&(m, g, _)| m == m1 && g == s.mem.dec_gen)
            {
                // Refused before under the same MODE1 and code.
                self.stats.cached_refusals += 1;
                self.drop_slot(pc);
                return None;
            }
            let built = self.build(s, pc, aot);
            self.drain_chase();
            if built.is_none() {
                if self.slots[ix].auto && self.slots[ix].refused.is_some() {
                    let gen_ = s.mem.dec_gen;
                    if let Some(a) = &mut self.auto {
                        let t = a.tried.entry(pc).or_insert((m1, gen_, 0));
                        *t = (m1, gen_, t.2.saturating_add(1));
                    }
                    self.drop_slot(pc);
                }
                return None;
            }
        }
        let slot = &mut self.slots[ix];
        if slot.defer > 0 {
            slot.defer -= 1;
            return None;
        }
        if slot.win_skip > 0 && self.win_backoff {
            slot.win_skip -= 1;
            return None;
        }
        let region = slot.region.as_mut()?;
        if !verify_code(s, region) {
            slot.region = None;
            slot.kernel = None;
            slot.tl_why = "code";
            return None;
        }
        self.stats.calls += 1;
        let before = s.icount;
        let kernel = slot.kernel.as_deref()?;
        let t0 = if self.time { crate::ticks() } else { 0 };
        let mut res = run_variant(s, region, kernel, &mut self.ctx);
        if res == Err(glue::Decline::Req)
            && !slot.alts.is_empty()
            && (slot.rebuilds < REBUILD_BUDGET || slot.alt_fail < ALT_GIVE_UP)
        {
            // The baked state differs: another variant may take this one.
            let mut i = 0;
            while i < slot.alts.len() {
                let (ar, ak) = &mut slot.alts[i];
                if !verify_code(s, ar) {
                    slot.alts.remove(i);
                    continue;
                }
                let r2 = run_variant(s, ar, &**ak, &mut self.ctx);
                if r2 != Err(glue::Decline::Req) {
                    // The variant that ran becomes the current one.
                    if let (Some(cr), Some(ck)) = (slot.region.as_mut(), slot.kernel.as_mut()) {
                        std::mem::swap(cr, ar);
                        std::mem::swap(ck, ak);
                    }
                    res = r2;
                    break;
                }
                i += 1;
            }
        }
        if res.is_err() {
            slot.alt_fail = slot.alt_fail.saturating_add(1);
        } else {
            slot.alt_fail = 0;
        }
        match res {
            Err(glue::Decline::Window) => {
                slot.win_back = (slot.win_back + 1).min(WIN_BACKOFF_MAX);
                slot.win_skip = (1u32 << slot.win_back) - 1;
            }
            Ok(_) => slot.win_back = 0,
            _ => {}
        }
        if chained && res.is_err() {
            self.stats.calls -= 1;
            self.cov_last = cov_prev;
            return None;
        }
        let region = slot.region.as_ref()?;
        if self.log {
            eprintln!(
                "fast: run {pc:#x}: icount {before} -> {} (limit {}) {:?} pc {:#x} pending {:?}",
                s.icount,
                s.limit,
                res.as_ref().map(|c| *c).map_err(|d| *d),
                s.pc_sw,
                s.pending
            );
        }
        slot.calls += 1;
        slot.insns += s.icount - before;
        if self.time && res.is_ok() {
            slot.ticks += crate::ticks() - t0;
        }
        if self.cov {
            match &res {
                Ok(_) => {
                    // A kernel that ran a return leaves at its run-time
                    // target; the report names the return instruction, as
                    // for a region that stopped at it.
                    let ret = if region.cfg.is_some() {
                        self.ctx.ret_at
                    } else {
                        0
                    };
                    let x = if ret != 0 { ret } else { s.pc_sw as u32 };
                    let at_stop = ret != 0
                        || region
                            .cfg
                            .as_ref()
                            .is_some_and(|c| c.stops.iter().any(|(p, _)| *p == x));
                    let site_kind = region
                        .cfg
                        .as_ref()
                        .map(|c| c.sites[self.ctx.get32(CTX_EXIT_K) as usize].kind);
                    let kind = if s.icount >= s.limit {
                        "budget"
                    } else {
                        match site_kind {
                            Some("insn") if at_stop => "stop",
                            Some("insn") => "guard",
                            Some(k) if k.starts_with("g@") => k,
                            Some(k) => k,
                            None if at_stop => "stop",
                            None => "exit",
                        }
                    };
                    let e = slot.cov.exits.entry((x, kind)).or_default();
                    e[0] += 1;
                    e[1] += s.icount - before;
                    self.cov_last = Some((ix, x, kind, s.icount));
                }
                Err(d) => {
                    slot.cov.declined[*d as usize] += 1;
                    let why = match d {
                        glue::Decline::Req => format!("req: {}", req_failure(s, region)),
                        glue::Decline::Shape | glue::Decline::Window => glue::take_cov_why(),
                        _ => String::new(),
                    };
                    if !why.is_empty() {
                        *slot.cov.decl_why.entry(reason_class(&why)).or_default() += 1;
                    }
                }
            }
        }
        let mut drop_after = false;
        if slot.auto
            && slot.calls >= AUTO_WARMUP
            && slot.calls.is_power_of_two()
            && let Some(a) = &mut self.auto
            && slot.insns < a.min_insns * slot.calls
        {
            // Too little work per call to pay the fixed call cost.
            if self.log {
                eprintln!(
                    "fast: {pc:#x} dropped: {} insns in {} calls",
                    slot.insns, slot.calls
                );
            }
            self.stats.unprofitable += 1;
            a.tried
                .insert(pc, (mode1_bits(s), s.mem.dec_gen, AUTO_TRIES));
            drop_after = true;
        }
        let ret = match res {
            Ok(code) => {
                self.stats.runs += 1;
                self.stats.insns += s.icount - before;
                dump::finish(pc, s.icount - before);
                if self.auto.is_some() && s.pc_sw as u32 != pc {
                    // The kernel left at another PC (a call it performed, a
                    // stop): the interpreter never saw a branch to it, so
                    // `note` was not called. A candidate region start.
                    self.note(s.pc_sw as u32);
                }
                Some(code)
            }
            Err(d) => {
                self.stats.declined[d as usize] += 1;
                if self.log && d == glue::Decline::Req {
                    let why = req_failure(s, region);
                    *self
                        .stats
                        .reasons
                        .entry(format!("decline req: {why}"))
                        .or_default() += 1;
                }
                if d == glue::Decline::Req {
                    // An M register that differs from its baked value twice in
                    // a row is data, not a constant: do not bake it near here.
                    let mut fails = 0u64;
                    for &(c, k) in &region.eqs {
                        if (32..48).contains(&(c as usize)) {
                            let v = s.r[c as usize];
                            if !v.is_c() || v.b != k {
                                fails |= 1 << (c - 32);
                            }
                        }
                    }
                    let both = fails & slot.last_eq;
                    slot.last_eq = fails;
                    if both != 0 {
                        region::mark_volatile(pc, both);
                    }
                }
                let mut exit0_rebuild = false;
                if d == glue::Decline::Exit0 {
                    slot.exit0 += 1;
                    if slot.exit0 == 4
                        && slot.rebuilds < REBUILD_BUDGET
                        && let Ok(dec) = region::decode_one(s, pc)
                        && cfg::helper_ok(&dec)
                    {
                        // The first instruction's guard keeps failing: make it
                        // a helper call in the regions built from here on.
                        cfg::force_helper(pc);
                        exit0_rebuild = true;
                    }
                } else if !matches!(d, glue::Decline::Exit0) {
                    slot.exit0 = 0;
                }
                let mut win_rebuild = false;
                if d == glue::Decline::Window && cfg::flat_auto() {
                    slot.win_fails += 1;
                    if slot.win_fails == WIN_FLAT_AFTER
                        && slot.rebuilds < REBUILD_BUDGET
                        && cfg::mark_flat_win(pc)
                    {
                        // The static windows keep failing (an unwritten byte
                        // in a hull): guard each access instead.
                        win_rebuild = true;
                        slot.win_back = 0;
                        slot.win_skip = 0;
                    }
                }
                if (d == glue::Decline::Req
                    && slot.rebuilds < REBUILD_BUDGET
                    && !req_is_within(s, region))
                    || exit0_rebuild
                    || win_rebuild
                {
                    // No variant fits the state (a baked constant, MODE1 or
                    // the CACC history differs): make another from this
                    // state, a bounded number of times. The variants that
                    // exist stay (most recent first, at most MAX_VARIANTS).
                    slot.rebuilds += 1;
                    slot.tl_why = if win_rebuild {
                        "rebuild-win"
                    } else if exit0_rebuild {
                        "rebuild-exit0"
                    } else {
                        "rebuild-req"
                    };
                    let n = slot.rebuilds;
                    let old = (slot.region.take(), slot.kernel.take());
                    if self.build(s, pc, aot).is_some() {
                        let slot = &mut self.slots[ix];
                        slot.rebuilds = n;
                        if let (Some(r), Some(k)) = old {
                            slot.alts.insert(0, (r, k));
                            slot.alts.truncate(MAX_VARIANTS - 1);
                        }
                    } else if self.slots[ix].auto && self.slots[ix].refused.is_some() {
                        let (m1, g) = (mode1_bits(s), s.mem.dec_gen);
                        if let Some(a) = &mut self.auto {
                            a.tried.insert(pc, (m1, g, AUTO_TRIES));
                        }
                        drop_after = true;
                    } else {
                        // This state cannot be built: keep what runs.
                        let slot = &mut self.slots[ix];
                        slot.refused = None;
                        slot.region = old.0;
                        slot.kernel = old.1;
                    }
                }
                None
            }
        };
        let mut dyn_flat: Option<Vec<u32>> = None;
        if let Some((lo, hi)) = dynflat_range()
            && res.is_ok()
        {
            let at = s.pc_sw as u32;
            let sl = &mut self.slots[ix];
            if let Some(r) = sl.region.as_ref()
                && !r.dyn_pcs.is_empty()
            {
                sl.dyn_exits += r.dyn_pcs.contains(&at) as u32;
                if sl.calls >= 64
                    && sl.calls.is_power_of_two()
                    && sl.dyn_exits as u64 * 2 >= sl.calls
                {
                    let pcs: Vec<u32> = r
                        .dyn_pcs
                        .iter()
                        .copied()
                        .filter(|p| (lo..=hi).contains(p))
                        .collect();
                    if !pcs.is_empty() {
                        dyn_flat = Some(pcs);
                    }
                }
            }
        }
        if let Some(pcs) = dyn_flat {
            for p in pcs {
                cfg::mark_flat_dyn(p);
            }
            // Built again from the next call's state, with the flat accesses.
            self.slots[ix].region = None;
            self.slots[ix].kernel = None;
            self.slots[ix].tl_why = "dynflat";
            self.slots[ix].alts.clear();
            self.slots[ix].dyn_exits = 0;
            self.stats.dyn_flat += 1;
        }
        let sl = &mut self.slots[ix];
        if sl.chased && sl.calls == CHASE_TRIAL && sl.insns < CHASE_MIN_INSNS * CHASE_TRIAL {
            // Too little work per call to pay the fixed call cost.
            sl.region = None;
            sl.kernel = None;
            sl.refused = Some("chased entry retires few instructions".into());
        }
        if drop_after {
            self.drop_slot(pc);
        }
        ret
    }
}

impl FastTier for FastEngine {
    #[inline(always)]
    fn wants(&self, pc: u32) -> bool {
        self.index(pc).is_some()
    }

    fn fallback_only(&self, pc: u32) -> bool {
        self.fallback.binary_search(&pc).is_ok()
    }

    fn has_fallback(&self) -> bool {
        !self.fallback.is_empty()
    }

    fn exclusively_fallback(&self) -> bool {
        !self.fallback.is_empty()
            && self.pcs.len() == self.fallback.len()
            && self.auto.is_none()
            && !self.follow
    }

    fn run(&mut self, s: &mut St, pc: u32) -> Option<u32> {
        self.run_inner(s, pc, false, None)
    }

    fn run_with(&mut self, s: &mut St, pc: u32, aot: &dyn Fn(u32) -> bool) -> Option<u32> {
        self.run_inner(s, pc, false, Some(aot))
    }

    fn run_chained(&mut self, s: &mut St, pc: u32) -> Option<u32> {
        self.run_inner(s, pc, true, None)
    }

    fn caps_chain(&self) -> bool {
        self.cap_chain
    }

    fn sync_chain_stops(&mut self, s: &mut St) -> bool {
        if !self.cap_chain {
            return true;
        }
        // Auto selection counts every dispatched entry (`note`): chains
        // would hide candidates, so those engines still end each chain at
        // its first link.
        if self.auto_dispatch() {
            return false;
        }
        for pc in self.stops_new.drain(..) {
            s.chain_stop_add(pc);
        }
        true
    }

    fn stops_pending(&self) -> bool {
        self.cap_chain && (self.auto_dispatch() || !self.stops_new.is_empty())
    }

    /// `run_inner` at a PC with no entry only drains the follow and chase
    /// queues (and prints the `tl` timeline tick), then declines.
    fn has_queued(&self) -> bool {
        !self.follow_queue.is_empty() || !self.chase_queue.is_empty() || self.tl.is_some()
    }

    fn auto(&self) -> bool {
        self.auto.is_some()
    }

    fn auto_dispatch(&self) -> bool {
        self.auto.as_ref().is_some_and(|a| a.blocks)
    }

    fn skipped_for_aot(&mut self) {
        self.stats.auto_skipped_aot += 1;
    }

    #[inline(always)]
    fn note(&mut self, pc: u32) {
        let Some(a) = &mut self.auto else { return };
        let e = &mut a.table[(pc.wrapping_mul(0x9e37_79b1) >> (32 - AUTO_BITS)) as usize];
        if e.0 != pc {
            // Lossy replacement: a resident count decays per collision.
            if e.1 <= 1 {
                *e = (pc, 1);
            } else {
                e.1 -= 1;
            }
            return;
        }
        if e.1 >= a.threshold {
            return;
        }
        e.1 += 1;
        if e.1 == a.threshold {
            self.arm(pc);
        }
    }

    fn note_appended(&mut self, pc: u32) {
        if self.chase && self.index(pc).is_none() {
            self.add_chased(pc);
        }
    }

    fn fast_stats(&self) -> Option<FastStats> {
        Some(self.stats.clone())
    }

    fn engine_report(&self, total: u64, interp: u64, blocks: u64) -> String {
        let f = self.stats.insns;
        // Instructions run by the fast tier also count as block instructions.
        let aot = blocks.saturating_sub(f);
        let pct = |n: u64| 100.0 * n as f64 / total.max(1) as f64;
        format!(
            "fast engine: {total} insns: fast tier {f} ({:.2}%), aot {aot} ({:.2}%), interpreter {interp} ({:.2}%)",
            pct(f),
            pct(aot),
            pct(interp)
        )
    }

    fn report(&self) -> String {
        let s = &self.stats;
        let mut reasons: Vec<_> = s.reasons.iter().collect();
        reasons.sort_by(|a, b| b.1.cmp(a.1));
        let reasons: Vec<String> = reasons
            .iter()
            .take(40)
            .map(|(r, n)| format!("{n}x {r}"))
            .collect();
        format!(
            "fast tier [{}]: {} armed ({} skipped for aot), {} built ({} ms compile, {} ms build), {} refused ({} cached), {} unprofitable; {} calls: {} ran ({} insns), declined shape/req/window/budget/exit0 = {:?}; refusals: [{}]",
            self.backend.name(),
            s.armed,
            s.auto_skipped_aot,
            s.built,
            s.compile_ns / 1_000_000,
            s.build_ns / 1_000_000,
            s.refused,
            s.cached_refusals,
            s.unprofitable,
            s.calls,
            s.runs,
            s.insns,
            s.declined,
            reasons.join("; "),
        )
    }
}

/// The environment variable of the fast-tier region lists for this engine instance: regions and cluster roots
/// are addresses in one device's DSP image, so the second instance (`sharc_second`, the second generated core)
/// reads its own, `SHARC_FAST_REGIONS_2` and `SHARC_BLOCKTRANS_2`.
const fn region_env(first: &'static str, second: &'static str) -> &'static str {
    if cfg!(sharc_second) { second } else { first }
}

/// `SHARC_BLOCKTRANS=0xb8276c,...` (hex cluster roots).
pub fn blocktrans_roots() -> Vec<u32> {
    parse_regions(region_env("SHARC_BLOCKTRANS", "SHARC_BLOCKTRANS_2"))
}

fn parse_regions(name: &str) -> Vec<u32> {
    let mut text = std::env::var(name).unwrap_or_default();
    if let Some(path) = text.strip_prefix('@') {
        text = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("{name}=@{path}: {e}"))
            .replace(char::is_whitespace, ",");
    }
    text.split(',')
        .filter(|x| !x.trim().is_empty())
        .filter_map(|x| u32::from_str_radix(x.trim().trim_start_matches("0x"), 16).ok())
        .collect()
}

/// `SHARC_FAST_REGIONS=0x1c399a,...` (hex entry PCs) as a fast tier.
/// `SHARC_FAST_AUTO=threshold,min_insns[,blocks]` adds run-time selection.
pub fn from_env() -> Option<FastEngine> {
    let name = region_env("SHARC_FAST_REGIONS", "SHARC_FAST_REGIONS_2");
    let mut text = std::env::var(name).unwrap_or_default();
    // `@FILE`: the PC list (hex, separated by commas or white space) is in FILE
    // (a program database's blocks or instructions do not fit in the environment).
    if let Some(path) = text.strip_prefix('@') {
        text = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("{name}=@{path}: {e}"))
            .replace(char::is_whitespace, ",");
    }
    let pcs: Vec<u32> = text
        .split(',')
        .filter(|x| !x.trim().is_empty())
        .filter_map(|x| u32::from_str_radix(x.trim().trim_start_matches("0x"), 16).ok())
        .collect();
    let auto = auto_config();
    let roots = blocktrans_roots();
    let fallback = parse_regions(region_env(
        "SHARC_FAST_FALLBACK_REGIONS",
        "SHARC_FAST_FALLBACK_REGIONS_2",
    ));
    if pcs.is_empty() && auto.is_none() && roots.is_empty() && fallback.is_empty() {
        return None;
    }
    let f = FastEngine::new(default_backend(), &pcs)
        .with_clusters(&roots)
        .with_fallback(&fallback);
    Some(match auto {
        Some((t, m, b)) => f.with_auto(t, m, b),
        None => f,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every writer id of a group is within `MAX_FLAG_ID` and round-trips.
    #[test]
    fn flag_ids_round_trip() {
        for g in 0..3 {
            let mut n = 0;
            for id in 1..=MAX_FLAG_ID + 4 {
                if let Some(k) = FlagKind::from_group_id(g, id) {
                    assert!(id <= MAX_FLAG_ID, "group {g} id {id} above MAX_FLAG_ID");
                    assert_eq!(k.group_id(), (g, id));
                    n += 1;
                }
            }
            assert!(n > 0);
        }
    }
}
