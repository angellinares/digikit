//! The SHARC "fast tier": a load-time translator that turns a hot region of
//! the loaded DSP image into native code and falls back, exactly, to the
//! existing paths (the generated blocks, the interpreter).
//!
//! Layers, all independent of any firmware address or table:
//!  * `region`/`lower`: decode a DO-loop body from loaded memory with the
//!    generic decoder and lower it to the kernel IR (`ir`);
//!  * `ir`: a backend-neutral loop kernel (u32/f32/u64 values, variables,
//!    guards, window loads and stores). `interp` is a reference interpreter;
//!    the Cranelift backend lives in `native/sharc-fast-cl`, a WebAssembly
//!    backend can be a second one;
//!  * `glue`: the entry checks, window resolution and exact state
//!    reconstruction around a kernel call;
//!  * `FastEngine`: the per-engine cache of regions by entry PC, behind the
//!    `FastTier` hook of `Engine::step`.
//!
//! Entry PCs come from run-time configuration: `Engine::set_fast`, or the
//! `SHARC_FAST_REGIONS` environment variable (comma separated hex PCs) read
//! by `Engine::from_image`. Off by default.

pub mod cfg;
pub mod decode_view;
pub mod glue;
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
}

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
            FlagKind::Fmul => (1, 1),
            FlagKind::FmulForget => (1, 2),
            FlagKind::Shift { sv: false } => (2, 1),
            FlagKind::Shift { sv: true } => (2, 2),
            FlagKind::Fext { sv: false } => (2, 3),
            FlagKind::Fext { sv: true } => (2, 4),
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
            (1, 1) => FlagKind::Fmul,
            (1, 2) => FlagKind::FmulForget,
            (2, 1) => FlagKind::Shift { sv: false },
            (2, 2) => FlagKind::Shift { sv: true },
            (2, 3) => FlagKind::Fext { sv: false },
            (2, 4) => FlagKind::Fext { sv: true },
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
}

/// The engine's view of the fast tier (`Engine::fast`).
pub trait FastTier: Send {
    /// Is PC an entry the tier handles?
    fn wants(&self, pc: u32) -> bool;
    /// Run the region at PC if it applies. `Some(exit code)` only when at
    /// least one instruction completed; `None` leaves the state untouched.
    fn run(&mut self, s: &mut St, pc: u32) -> Option<u32>;
    /// Generated blocks call each other directly (`EXIT_CHAIN`), which would
    /// carry execution into an entry PC without passing `Engine::step`: when
    /// true the engine ends each chain at the first link, so every block
    /// boundary is a dispatch and the tier sees every entry.
    fn caps_chain(&self) -> bool {
        false
    }
    /// A one-paragraph summary for logs.
    fn report(&self) -> String {
        String::new()
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
}

#[derive(Default)]
struct Slot {
    region: Option<region::Region>,
    kernel: Option<Box<dyn CompiledKernel>>,
    refused: Option<String>,
    rebuilds: u32,
    transient: u32,
}

pub struct FastEngine {
    backend: Box<dyn KernelBackend>,
    /// Entry PCs and their regions, in step; `bloom` is a 64-bit filter of
    /// the PCs so `wants` (asked at every block boundary) is one test.
    pcs: Vec<u32>,
    slots: Vec<Slot>,
    bloom: u64,
    ctx: glue::Ctx,
    pub stats: FastStats,
    log: bool,
    cap_chain: bool,
    force_cfg: bool,
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
        let mut list: Vec<u32> = pcs.to_vec();
        list.sort_unstable();
        list.dedup();
        FastEngine {
            backend,
            bloom: list.iter().fold(0, |b, &pc| b | Self::bit(pc)),
            slots: list.iter().map(|_| Slot::default()).collect(),
            pcs: list,
            ctx: glue::Ctx::default(),
            stats: FastStats::default(),
            log: {
                let on = std::env::var_os("SHARC_FAST_LOG").is_some();
                glue::set_shape_log(on);
                on
            },
            // SHARC_FAST_CHAIN=0 keeps chaining (entries reached by a chain
            // are then missed).
            cap_chain: std::env::var("SHARC_FAST_CHAIN").map_or(true, |v| v != "0"),
            force_cfg: std::env::var("SHARC_FAST_SHAPE").is_ok_and(|v| v == "cfg"),
        }
    }

    #[inline(always)]
    fn bit(pc: u32) -> u64 {
        1u64 << (pc.wrapping_mul(0x9e37_79b1) >> 26)
    }

    #[inline(always)]
    fn index(&self, pc: u32) -> Option<usize> {
        if self.bloom & Self::bit(pc) == 0 {
            return None;
        }
        self.pcs.iter().position(|&p| p == pc)
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

    fn build(&mut self, s: &mut St, pc: u32) -> Option<()> {
        let t0 = std::time::Instant::now();
        // The remaining iterations of the active DO loop that starts here,
        // else (or when that is refused) the CFG shape from this pc.
        // SHARC_FAST_SHAPE=cfg skips the loop shape (tests, comparisons).
        let loop_try = if self.force_cfg {
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
                match region::build_cfg(s, pc) {
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
        let slot = &mut self.slots[self.pcs.iter().position(|&p| p == pc)?];
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
                self.stats.refused += 1;
                slot.refused = Some(e.0);
                None
            }
            Ok(mut r) => {
                let t1 = std::time::Instant::now();
                let k = self.backend.compile(&r.kernel);
                self.stats.compile_ns += t1.elapsed().as_nanos() as u64;
                match k {
                    Err(e) => {
                        if self.log {
                            eprintln!("fast: {pc:#x} compile failed: {e}");
                        }
                        self.stats.refused += 1;
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
                                "fast: {pc:#x} built: {} insns, {} windows, {} reqs, kernel {} ops",
                                r.len(),
                                r.wins.len(),
                                r.reqs.len(),
                                r.kernel.inst_count()
                            );
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

impl Drop for FastEngine {
    fn drop(&mut self) {
        if self.log && self.stats.calls > 0 {
            eprintln!("{}", self.report());
        }
    }
}

impl FastTier for FastEngine {
    #[inline(always)]
    fn wants(&self, pc: u32) -> bool {
        self.index(pc).is_some()
    }

    fn run(&mut self, s: &mut St, pc: u32) -> Option<u32> {
        let ix = self.index(pc)?;
        if self.slots[ix].refused.is_some() {
            return None;
        }
        if self.slots[ix].region.is_none() {
            self.build(s, pc)?;
        }
        let slot = &mut self.slots[ix];
        let region = slot.region.as_mut()?;
        if s.mem.dec_gen != region.verified_gen {
            let same = region
                .code_words
                .iter()
                .all(|&(at, w)| s.mem.read_sw(at).unwrap_or(0) == w);
            if same {
                region.verified_gen = s.mem.dec_gen;
            } else {
                slot.region = None;
                slot.kernel = None;
                return None;
            }
        }
        self.stats.calls += 1;
        let before = s.icount;
        let kernel = slot.kernel.as_deref()?;
        let res = if region.cfg.is_some() {
            glue::run_cfg(s, region, kernel, &mut self.ctx)
        } else {
            glue::run(s, region, kernel, &mut self.ctx)
        };
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
        match res {
            Ok(code) => {
                self.stats.runs += 1;
                self.stats.insns += s.icount - before;
                Some(code)
            }
            Err(d) => {
                self.stats.declined[d as usize] += 1;
                if d == glue::Decline::Req && slot.rebuilds < 16 {
                    // A baked constant changed: re-make the region from
                    // this state, a bounded number of times.
                    slot.rebuilds += 1;
                    let n = slot.rebuilds;
                    slot.region = None;
                    slot.kernel = None;
                    if self.build(s, pc).is_some() {
                        self.slots[ix].rebuilds = n;
                    }
                }
                None
            }
        }
    }

    fn caps_chain(&self) -> bool {
        self.cap_chain
    }

    fn report(&self) -> String {
        let s = &self.stats;
        format!(
            "fast tier [{}]: {} built ({} ms compile, {} ms build), {} refused; {} calls: {} ran ({} insns), declined shape/req/window/budget/exit0 = {:?}",
            self.backend.name(),
            s.built,
            s.compile_ns / 1_000_000,
            s.build_ns / 1_000_000,
            s.refused,
            s.calls,
            s.runs,
            s.insns,
            s.declined,
        )
    }
}

/// `SHARC_FAST_REGIONS=0x1c399a,...` (hex entry PCs) as a fast tier.
pub fn from_env() -> Option<FastEngine> {
    let text = std::env::var("SHARC_FAST_REGIONS").ok()?;
    let pcs: Vec<u32> = text
        .split(',')
        .filter(|x| !x.trim().is_empty())
        .filter_map(|x| u32::from_str_radix(x.trim().trim_start_matches("0x"), 16).ok())
        .collect();
    if pcs.is_empty() {
        return None;
    }
    Some(FastEngine::new(default_backend(), &pcs))
}
