//! Lowering: SHARC+ instructions to kernel IR.
//!
//! Each instruction follows the interpreter's own order (tools/sharc_dev/sharc_core):
//! operands are read from the register file as it was before the
//! instruction; the transfer, the index update and the compute then write in
//! that order, a later write to the same register winning. Here that is:
//! reads go through the value cache (`cur`), writes are buffered and applied
//! by `commit` after every guard and store of the instruction, so a side exit
//! at instruction `k` sees the state before it.
//!
//! The region is lowered three times with the same code. Pass 0 finds which
//! registers the body writes, pass 1 (with the modifier registers it never
//! writes baked in as constants) derives each address register's
//! per-iteration stride, pass 2 emits the kernel with the address ranges of
//! every access known, which is what lets the memory windows be checked
//! once per call instead of once per access.

pub mod callret;
pub mod cond;
pub mod fixed_alu;
pub mod float_alu;
pub mod flow;
pub mod gen_forms;
pub mod gen_prims;
pub mod helper;
pub mod mem_addr;
pub mod mem_imm;
pub mod move_misc;
pub mod mult;
pub mod multifn;
pub mod shift;
pub mod sysops;

use super::cfg::Site;
use super::decode_view::Dec;
use super::ir::*;
use super::{Base, FlagKind, FlagWriter, Req, WinSpec};

#[derive(Debug, Clone)]
pub struct Refuse(pub String);
pub type LR<T> = Result<T, Refuse>;

pub fn refuse<T>(why: impl Into<String>) -> LR<T> {
    Err(Refuse(why.into()))
}

/// Value range: `base_entry + [lo, hi] + ts * t + nf * N` over the iterations
/// t in [0, N-1] of the one loop whose trip count N is symbolic (the DO loop
/// the region iterates; `ts` is the per-iteration stride inside it, `nf` the
/// total after it ended).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Abs {
    pub base: Base,
    pub lo: i64,
    pub hi: i64,
    pub ts: i64,
    pub nf: i64,
}

const LIM: i64 = 1 << 40;

/// MODE1.CBUFEN (circular buffering of data loads and stores).
pub const MODE1_CBUFEN: u32 = 1 << 24;

impl Abs {
    pub fn konst(c: i64) -> Abs {
        Abs {
            base: Base::None,
            lo: c,
            hi: c,
            ts: 0,
            nf: 0,
        }
    }
    fn ok(self) -> Option<Abs> {
        let big = |v: i64| v.abs() > LIM;
        if big(self.lo) || big(self.hi) || big(self.ts) || big(self.nf) {
            None
        } else {
            Some(self)
        }
    }
    pub fn add(self, o: Abs) -> Option<Abs> {
        let base = match (self.base, o.base) {
            (Base::None, b) | (b, Base::None) => b,
            _ => return None,
        };
        Abs {
            base,
            lo: self.lo + o.lo,
            hi: self.hi + o.hi,
            ts: self.ts + o.ts,
            nf: self.nf + o.nf,
        }
        .ok()
    }
    pub fn sub(self, o: Abs) -> Option<Abs> {
        if o.base != Base::None {
            return None;
        }
        Abs {
            base: self.base,
            lo: self.lo - o.hi,
            hi: self.hi - o.lo,
            ts: self.ts - o.ts,
            nf: self.nf - o.nf,
        }
        .ok()
    }
    pub fn scale(self, k: i64) -> Option<Abs> {
        if self.base != Base::None || k < 0 {
            return None;
        }
        Abs {
            base: Base::None,
            lo: self.lo * k,
            hi: self.hi * k,
            ts: self.ts * k,
            nf: self.nf * k,
        }
        .ok()
    }
    /// Range over all iterations t in [0, n-1] of the value at a fixed point
    /// of the body.
    /// The range as byte addresses: a constant range that lies below zero
    /// is the 32-bit address a `ci` constant above 2^31 stands for
    /// (`ci` sign-extends); the sum is taken modulo 2^32.
    pub fn as_addr(self) -> Abs {
        let constant = self.base == Base::None && self.ts == 0 && self.nf == 0;
        if constant && self.hi < 0 && self.lo >= -(1 << 32) {
            Abs {
                lo: self.lo + (1 << 32),
                hi: self.hi + (1 << 32),
                ..self
            }
        } else {
            self
        }
    }

    pub fn span(&self, n: i64) -> (i64, i64) {
        let d = self.ts * (n - 1);
        let f = self.nf * n;
        (self.lo + d.min(0) + f, self.hi + d.max(0) + f)
    }

    /// Both operands' values at the same point, one or the other: the union
    /// (same base and symbolic terms), else unknown.
    pub fn union(self, o: Abs) -> Option<Abs> {
        if self.base != o.base || self.ts != o.ts || self.nf != o.nf {
            return None;
        }
        Some(Abs {
            lo: self.lo.min(o.lo),
            hi: self.hi.max(o.hi),
            ..self
        })
    }

    /// `self - before` when it is the same constant for every value in the
    /// range (an induction variable's step), else None.
    pub fn step_from(self, before: Abs) -> Option<i64> {
        if self.base != before.base || self.ts != before.ts || self.nf != before.nf {
            return None;
        }
        let (a, b) = (self.lo - before.lo, self.hi - before.hi);
        (a == b).then_some(a)
    }

    /// Shifted by a constant.
    pub fn shifted(self, d: i64) -> Option<Abs> {
        Abs {
            lo: self.lo + d,
            hi: self.hi + d,
            ..self
        }
        .ok()
    }
}

#[derive(Clone, Default)]
pub struct RegInfo {
    pub used: bool,
    pub var: Option<Var>,
    pub vty: Option<Ty>,
    /// The first access in the body is a read (the entry value matters).
    pub first_read: bool,
    pub first_write: Option<u32>,
    pub written: bool,
    /// The kernel writes the register's final value straight into the engine
    /// state (`StSt32`): the register is known at entry (read first, or every
    /// register of a CFG kernel), so only its value changes. The other written
    /// registers go through the context and the caller stores them only when
    /// an instruction that ran wrote them.
    pub direct: bool,
    pub f_uses: u32,
    pub i_uses: u32,
    /// The last write in the body was an f32 value.
    pub last_float: bool,
}

/// What pass N learned for pass N+1.
#[derive(Clone)]
pub struct Plan {
    pub fty: [bool; NREGS as usize],
    pub written: [bool; NREGS as usize],
    pub stride: [Option<i64>; NREGS as usize],
    /// Pass index this plan was made for (0, 1, 2).
    pub pass: u8,
    /// CFG kernels: the registers (bit per code) each instruction writes,
    /// by address (delay slots included), and the per-iteration step of each
    /// register in each loop as the previous pass measured it.
    pub node_written: std::collections::BTreeMap<u32, u128>,
    pub loop_stride: Vec<[Option<i64>; NREGS as usize]>,
    /// Which loops had their loop-back lowered (the others never iterate in
    /// the kernel).
    pub loop_back_seen: Vec<bool>,
    /// The flag-writer kinds of each group in the region (bit per id).
    pub flag_kinds: [u32; NGROUPS as usize],
    /// Index registers (bit per UREG code) the region writes with a plain
    /// move (CJUMP, RFRAME), not as a circular update: their circular buffer
    /// (`circ_range`) says nothing about the value.
    pub plain: u64,
    /// CFG kernels: the register ranges flowing into each call target that
    /// lies before its caller (see `Structure::call_targets`), as the
    /// previous pass saw them.
    pub entry_abs: std::collections::HashMap<u32, Vec<Option<Abs>>>,
    /// CFG kernels: the in-region register-count loop whose trip count is the
    /// region's symbolic N, with its count source (`Lower::reg_sym`).
    pub reg_sym: Option<(usize, u8, i64)>,
}

impl Default for Plan {
    fn default() -> Plan {
        Plan {
            fty: [false; NREGS as usize],
            written: [false; NREGS as usize],
            stride: [None; NREGS as usize],
            pass: 0,
            node_written: Default::default(),
            loop_stride: Vec::new(),
            loop_back_seen: Vec::new(),
            flag_kinds: [0; NGROUPS as usize],
            plain: 0,
            entry_abs: Default::default(),
            reg_sym: None,
        }
    }
}

/// Build-time facts about the engine state the region is made for.
#[derive(Clone)]
pub struct Env {
    /// Fully known register values (UREG codes 0..47) at build time.
    pub regs: [Option<u32>; NREGS as usize],
    /// L<n> and B<n> of the index registers I0-I15 when known (the
    /// circular-buffer length and base, `circ_range`).
    pub dag_l: [Option<u32>; 16],
    pub dag_b: [Option<u32>; 16],
    pub assume_nw32: bool,
    /// MODE1 at build time (the region is made for these rounding and
    /// saturation bits, see `Req::Mode1`).
    pub mode1: u32,
    /// The kernel tracks flags lazily (conditions or branches in the region,
    /// see `cond`); otherwise flag writers are replayed at exit from
    /// remembered sources.
    pub flag_v: bool,
    /// Block-translator clusters (`SHARC_BLOCKTRANS`): a pre-modify load or
    /// store whose modifier register has no known range, through an index
    /// register that is an unchanged entry anchor, is lowered with a dynamic
    /// guarded window (`Lower::dynamic_window`) instead of being refused.
    pub guarded_dyn: bool,
    /// `cfg.bank_model`: a MODE1 write starts a delayed bank selection, which
    /// the kernel does not model (`sysops`).
    pub bank_model: bool,
    /// ASTATX bits 31:24 are all known at build time. A compare's CACC
    /// history is then shifted exactly (`Req::FlagsKnown(u32::MAX)`);
    /// otherwise the kernel is made for unknown history bits
    /// (`Req::CaccUnknown`), which the generic core's compare forgets.
    pub cacc_known: bool,
    /// Every window access of the region is a flat guarded access
    /// (`cfg::flat_auto`): the static windows kept declining at run time.
    pub flat_windows: bool,
}

/// `SHARC_FAST_GEN`: `0` lowers with the hand-written forms only, a comma list
/// of forms picks which use the generated emitters (`gen_forms`); the default is all.
///
/// `GEN_FORCE` overrides it for the lowering that runs on the current thread (`1` hand-written, `2`
/// generated, `0` the setting above); the fuzz tests switch it per test thread, so they run in parallel.
pub fn set_gen_force(v: u8) {
    GEN_FORCE.with(|c| c.set(v));
}

thread_local! {
    static GEN_FORCE: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

fn gen_enabled(form: &str) -> bool {
    match GEN_FORCE.with(|c| c.get()) {
        1 => return false,
        2 => return true,
        _ => {}
    }
    static SET: std::sync::OnceLock<Option<Vec<String>>> = std::sync::OnceLock::new();
    let set = SET.get_or_init(|| {
        // On by default (PCM-identical, fuzzed): SHARC_FAST_GEN=0 turns it off,
        // a comma list of forms selects some.
        let v = std::env::var("SHARC_FAST_GEN").unwrap_or_else(|_| "all".into());
        if v == "0" || v == "none" {
            return None;
        }
        Some(v.split(',').map(str::to_string).collect())
    });
    match set {
        Some(l) => l.iter().any(|f| f == "all" || f == form),
        None => false,
    }
}

/// Registers the lowering tracks: R, I, M and the pseudo registers (the
/// kernel's ASTATX bits and known mask).
pub const NALL: usize = (NREGS + NPSEUDO) as usize;

/// One instruction's pending effects (applied by `commit`).
#[derive(Default)]
struct Pending {
    writes: Vec<(u8, Val)>,
    stores: Vec<(Val, Val, Val, u8)>,
    flag: Option<(FlagKind, Vec<Val>)>,
    /// A second flag writer of the same instruction (multifunction: the
    /// ALU and the multiplier both write flags).
    flag2: Option<(FlagKind, Vec<Val>)>,
    /// The same two for PEy (SIMD): ASTATY.
    flag_y: Option<(FlagKind, Vec<Val>)>,
    flag2_y: Option<(FlagKind, Vec<Val>)>,
}

pub struct Lower {
    /// Test-only simulation of the pointer-table layout used by wasm32. The
    /// production lowering always selects the compiling target's pointer size.
    #[cfg(test)]
    flat_ptr64: Option<bool>,
    pub env: Env,
    pub plan: Plan,
    pub pass: u8,
    /// The last writer of each flag group, as far as this block knows.
    pub lw: [Option<cond::LastW>; 3],
    /// The writer kinds seen so far (bit per id), for the next pass.
    pub flag_kinds_out: [u32; NGROUPS as usize],
    /// `Plan::plain` as this pass found it.
    pub plain_out: u64,
    /// A circular index register was found written outside its buffer in a
    /// pass that should have emitted the kernel: lower again with it plain.
    pub retry: bool,
    /// Addresses of the instructions lowered with a guarded dynamic window
    /// (`dyn_xfer`): one that fails its guard on most calls is lowered flat
    /// instead (`SHARC_FAST_DYNFLAT`, `FastEngine::run_inner`).
    pub dyn_pcs: Vec<u32>,
    /// The instruction being lowered is followed in the kernel by nothing
    /// that reads the registers it writes (RFRAME before the exit of a
    /// return): its plain writes of index registers need no circular range.
    pub tail_write: bool,
    /// The ranges that flowed into the call targets in this pass.
    pub entry_abs_out: std::collections::HashMap<u32, Vec<Option<Abs>>>,
    pub val_ty: Vec<Ty>,
    pub abs: Vec<Option<Abs>>,
    pub vars: Vec<Ty>,
    pub pre: Vec<Inst>,
    pub body: Vec<Inst>,
    pub regs: Vec<RegInfo>,
    /// A register's current value in this iteration (any type; converted
    /// on use), None until it is read or written.
    cur: Vec<Option<Val>>,
    /// Registers written so far in this iteration.
    written_iter: Vec<bool>,
    /// The register whose entry value a register's value is that value moved
    /// and post-modified (None once something else was written to it): the
    /// anchor to resolve a guarded window from. A hint for choosing the
    /// anchor, never a proof (every guarded access is checked).
    pub hint: Vec<Option<u8>>,
    /// Index registers the current instruction post-modifies by themselves.
    self_update: Vec<u8>,
    /// Register moves (destination, source) of the current instruction.
    pub moves: Vec<(u8, u8)>,
    pub reqs: Vec<Req>,
    pub wins: Vec<WinSpec>,
    win_vals: Vec<Val>,
    pub flags: Vec<FlagWriter>,
    nfsrc: u32,
    fconsts: Vec<(u32, Val)>,
    /// Bit reinterpretations already made, and the value each came from.
    conv: Vec<(Val, Ty, Val)>,
    pub exits: Vec<Vec<(Var, Val)>>,
    /// The exit id of the current instruction's guards.
    exit_id: Option<u32>,
    pub idx: u32,
    pending: Pending,
    /// The current instruction's execution condition (0/1) and its negation.
    pub cond: Option<Val>,
    ncond: Option<Val>,
    pub nlabels: u32,
    /// ASTATX bits a condition reads that no earlier writer in the region
    /// defines (they must be known at entry), the bits known so far, and the
    /// bits some writer forgets or a condition reads (checked at the end).
    pub need_flags_known: u32,
    /// Float values the kernel has shown not to be a nonzero denormal (a
    /// guard in this block, or a result built to be normal or zero): the
    /// device reads a denormal operand as zero, which the kernel does not
    /// do, so such an operand leaves (`guard_not_denormal`).
    ftz_ok: std::collections::HashSet<Val>,
    pub fm_static: u32,
    forget_mask: u32,
    read_mask: u32,
    /// MODE1 bits the lowering relies on.
    pub mode1_mask: u32,
    /// Registers (R, I, M codes) written by the instruction just lowered.
    pub written_mask: u128,
    /// The kernel is a CFG kernel (loop strides are tracked by `flow`).
    pub cfg_shape: bool,
    /// CFG kernels: the address of the node whose lowering failed.
    pub fail_pc: Option<u32>,
    /// CFG kernels: what this pass learns for the next (see `Plan`), and the
    /// ranges of the registers at each loop's entry and head.
    pub node_written: std::collections::BTreeMap<u32, u128>,
    pub loop_stride_out: Vec<[Option<i64>; NREGS as usize]>,
    pub loop_back_seen_out: Vec<bool>,
    /// CFG kernels, per loop: the exact constant a register trip count has
    /// at its DO (the loop is then a literal-count loop).
    pub reg_lit: Vec<Option<i64>>,
    /// CFG kernels: the region has call targets placed before their callers
    /// (`Structure::call_targets`).
    pub has_call_targets: bool,
    /// The one register-count loop whose count is exactly `R[code] + k` at
    /// region entry (loop index, code, k): the region's symbolic N when it
    /// has no entry loop. The glue evaluates it at entry.
    pub reg_sym: Option<(usize, u8, i64)>,
    pub loop_in: Vec<Vec<Option<Abs>>>,
    pub loop_head: Vec<Vec<Option<Abs>>>,
    /// The abstract value of each register's variable at the current point
    /// (entry value plus the region's strides; carried across blocks).
    pub var_abs: Vec<Option<Abs>>,
    /// Extra kernel variables with their context in/out slots (CFG kernels).
    pub extras: Vec<Extra>,
    /// Exit-site bookkeeping of a CFG kernel.
    pub cfgx: Option<CfgX>,
    /// A compare op was lowered: the CACC history is tracked in a pseudo
    /// register (`compare_flag`) and the entry ASTATX must be fully known.
    pub has_compare: bool,
    /// CFG kernels with kernel flags: the multiplier flags a short multiply
    /// forgot on some path to here (`MULT_MASK` or 0), the addresses of the
    /// forgetting writers and of the compares, whether a compare was lowered
    /// after a forget on some path, and the address ranges that can repeat
    /// (backward branches and loops). `finish` refuses a compare that can
    /// follow a forget (`compare_flag`).
    pub fg_static: u32,
    pub forget_pcs: Vec<u32>,
    pub compare_pcs: Vec<u32>,
    pub cmp_after_forget: bool,
    pub cycles: Vec<(u32, u32)>,
    /// `sysops`: the next instruction may be lowered as the second half of a
    /// MODE1 write pair (set by `flow` for a straight-line successor), the
    /// exit site the pair's second instruction reports (the state before the
    /// write), and the one the write has armed for the next instruction.
    pub bank_next_ok: bool,
    pub bank_site: Option<(u32, u32)>,
    pub bank_site_next: Option<(u32, u32)>,
    /// Generated forms (`gen_prims`): the value a `Xfer::LoadV` produced, the
    /// register each value was read from (for anchors and moves), and MODE1 as
    /// the lowering knows it at this point of the region.
    pub xout: Option<Val>,
    pub gtag: Vec<Option<gen_prims::Tag>>,
    pub mode1_cur: u32,
    /// 80 while a compute of PEy runs (R/F codes 0..15 mean S/SF), else 0.
    pub pe_off: u32,
    /// The instruction being lowered gets a flat access when it has no address range.
    pub flat_cur: bool,
    /// As `flat_cur`, for an instruction whose dynamic window kept failing.
    pub flat_dyn_cur: bool,
    /// The address of the instruction being lowered in a loop or straight
    /// kernel (a CFG kernel has it in `cfgx`).
    pub cur_pc: Option<u32>,
}

/// A kernel variable that is not a register: loaded from the context in
/// `pre` (or set to a constant) and stored back in `post`.
#[derive(Clone, Copy)]
pub struct Extra {
    pub var: Var,
    pub init: ExtraInit,
    pub out: Option<u32>,
}

#[derive(Clone, Copy)]
pub enum ExtraInit {
    Const(u32),
    Ctx(u32),
}

/// What exit sites of a CFG kernel record (see `fast::cfg`).
pub struct CfgX {
    /// Address of the instruction being lowered, instructions retired in the
    /// current block before it, the loops active (indices into the region's
    /// loop table, outermost first) and a pending delayed branch.
    pub pc: u32,
    pub nblock: u32,
    pub active: Vec<u16>,
    pub pending: Option<(u16, u8)>,
    pub sites: Vec<Site>,
}

impl Lower {
    pub fn new(env: Env, plan: Plan, pass: u8) -> Lower {
        let mut l = Lower {
            #[cfg(test)]
            flat_ptr64: None,
            env,
            plan,
            pass,
            lw: [None; 3],
            flag_kinds_out: [0; NGROUPS as usize],
            plain_out: 0,
            retry: false,
            dyn_pcs: Vec::new(),
            tail_write: false,
            entry_abs_out: Default::default(),
            val_ty: Vec::new(),
            abs: Vec::new(),
            vars: Vec::new(),
            pre: Vec::new(),
            body: Vec::new(),
            regs: vec![RegInfo::default(); NALL],
            cur: vec![None; NALL],
            written_iter: vec![false; NALL],
            hint: (0..NALL).map(|c| (c < NREGS as usize).then_some(c as u8)).collect(),
            self_update: Vec::new(),
            moves: Vec::new(),
            reqs: Vec::new(),
            wins: Vec::new(),
            win_vals: Vec::new(),
            flags: Vec::new(),
            nfsrc: 0,
            fconsts: Vec::new(),
            conv: Vec::new(),
            exits: Vec::new(),
            exit_id: None,
            idx: 0,
            pending: Pending::default(),
            cond: None,
            ncond: None,
            nlabels: 0,
            need_flags_known: 0,
            ftz_ok: std::collections::HashSet::new(),
            fm_static: 0,
            forget_mask: 0,
            read_mask: 0,
            mode1_mask: 0,
            written_mask: 0,
            cfg_shape: false,
            fail_pc: None,
            node_written: Default::default(),
            loop_stride_out: Vec::new(),
            loop_back_seen_out: Vec::new(),
            reg_lit: Vec::new(),
            has_call_targets: false,
            reg_sym: None,
            loop_in: Vec::new(),
            loop_head: Vec::new(),
            var_abs: Vec::new(),
            extras: Vec::new(),
            cfgx: None,
            has_compare: false,
            fg_static: 0,
            forget_pcs: Vec::new(),
            compare_pcs: Vec::new(),
            cmp_after_forget: false,
            cycles: Vec::new(),
            bank_next_ok: false,
            bank_site: None,
            bank_site_next: None,
            xout: None,
            gtag: Vec::new(),
            mode1_cur: 0,
            pe_off: 0,
            flat_cur: false,
            flat_dyn_cur: false,
            cur_pc: None,
        };
        l.mode1_cur = l.env.mode1;
        l.var_abs = (0..NREGS as usize).map(|c| l.init_abs(c)).collect();
        l
    }

    pub fn strict(&self) -> bool {
        self.pass >= 2
    }

    // -- values ------------------------------------------------------------

    fn new_val(&mut self, ty: Ty, abs: Option<Abs>) -> Val {
        self.val_ty.push(ty);
        self.abs.push(abs);
        (self.val_ty.len() - 1) as Val
    }

    pub fn emit(&mut self, ty: Ty, op: Op) -> Val {
        let v = self.new_val(ty, None);
        self.body.push(Inst::Def(v, op));
        v
    }

    fn emit_abs(&mut self, ty: Ty, op: Op, abs: Option<Abs>) -> Val {
        let v = self.new_val(ty, abs);
        self.body.push(Inst::Def(v, op));
        v
    }

    pub fn ci(&mut self, c: u32) -> Val {
        self.emit_abs(Ty::I32, Op::CI32(c), Some(Abs::konst(c as i32 as i64)))
    }

    /// An f32 constant, defined once before the loop (kept in a register
    /// rather than rebuilt from an integer on every use).
    pub fn cf(&mut self, bits: u32) -> Val {
        if let Some(&(_, v)) = self.fconsts.iter().find(|c| c.0 == bits) {
            return v;
        }
        let v = self.new_val(Ty::F32, None);
        let op = match CONST_POOL.iter().position(|&c| c == bits) {
            Some(i) => Op::CtxF32(CTX_CONSTS + 4 * i as u32),
            None => Op::CF32(bits),
        };
        self.pre.push(Inst::Def(v, op));
        self.fconsts.push((bits, v));
        v
    }

    pub fn konst_of(&self, v: Val) -> Option<i64> {
        match self.abs[v as usize] {
            Some(a) if a.base == Base::None && a.lo == a.hi && a.ts == 0 && a.nf == 0 => Some(a.lo),
            _ => None,
        }
    }

    pub fn ty(&self, v: Val) -> Ty {
        self.val_ty[v as usize]
    }

    /// Integer binary op with range tracking.
    pub fn bin(&mut self, b: Bin, x: Val, y: Val) -> Val {
        let ax = self.abs[x as usize];
        let ay = self.abs[y as usize];
        let ky = self.konst_of(y);
        let abs = match b {
            Bin::Add => ax.zip(ay).and_then(|(p, q)| p.add(q)),
            Bin::Sub => ax.zip(ay).and_then(|(p, q)| p.sub(q)),
            Bin::Mul => match (ax, ay, self.konst_of(x), ky) {
                (Some(p), _, _, Some(k)) => p.scale(k),
                (_, Some(q), Some(k), _) => q.scale(k),
                _ => None,
            },
            Bin::Shl => match (ax, ky) {
                (Some(p), Some(k)) if (0..16).contains(&k) => p.scale(1 << k),
                _ => None,
            },
            Bin::And => match ky.or(self.konst_of(x)) {
                Some(m) if m >= 0 => Some(Abs {
                    base: Base::None,
                    lo: 0,
                    hi: m,
                    ts: 0,
                    nf: 0,
                }),
                _ => None,
            },
            Bin::ShrU => match (ax, ky) {
                (Some(p), Some(k))
                    if p.base == Base::None
                        && p.ts == 0
                        && p.nf == 0
                        && p.lo >= 0
                        && (0..32).contains(&k) =>
                {
                    Some(Abs {
                        base: Base::None,
                        lo: p.lo >> k,
                        hi: p.hi >> k,
                        ts: 0,
                        nf: 0,
                    })
                }
                _ => None,
            },
            _ => None,
        };
        let ty = match b {
            Bin::FAdd | Bin::FSub | Bin::FMul | Bin::FDiv => Ty::F32,
            Bin::Add64 => Ty::I64,
            _ => Ty::I32,
        };
        self.emit_abs(ty, Op::Bin(b, x, y), abs)
    }

    pub fn un(&mut self, u: Un, x: Val) -> Val {
        let ty = match u {
            Un::FNeg | Un::FAbs | Un::BitsToF | Un::IToF => Ty::F32,
            Un::FToBits | Un::FToI | Un::Not | Un::Clz => Ty::I32,
            Un::Zext => Ty::I64,
        };
        self.emit(ty, Op::Un(u, x))
    }

    pub fn select(&mut self, c: Val, a: Val, b: Val) -> Val {
        let ty = self.ty(a);
        self.emit(ty, Op::Select(c, a, b))
    }

    /// V as raw i32 bits (a reinterpretation of an f32; the reinterpretation
    /// of a value that is itself one is the original).
    pub fn to_i(&mut self, v: Val) -> Val {
        match self.ty(v) {
            Ty::I32 => v,
            Ty::F32 => self.reinterpret(v, Ty::I32),
            Ty::I64 => unreachable!("i64 register value"),
        }
    }

    pub fn to_f(&mut self, v: Val) -> Val {
        match self.ty(v) {
            Ty::F32 => v,
            Ty::I32 => self.reinterpret(v, Ty::F32),
            Ty::I64 => unreachable!("i64 register value"),
        }
    }

    fn reinterpret(&mut self, v: Val, to: Ty) -> Val {
        // `(source, to, result)`: result is `source` seen as `to`.
        if let Some(&(_, _, r)) = self.conv.iter().find(|c| c.0 == v && c.1 == to) {
            return r;
        }
        let u = if to == Ty::I32 {
            Un::FToBits
        } else {
            Un::BitsToF
        };
        let r = self.emit(to, Op::Un(u, v));
        // The abs of an integer survives the round trip.
        if to == Ty::I32 {
            self.abs[r as usize] = None;
        }
        self.conv.push((v, to, r));
        // And the reverse: reinterpreting the result gives back V.
        self.conv.push((r, self.ty(v), v));
        r
    }

    /// The record `k` of a guard in the current instruction: the instruction
    /// index in a loop kernel, an exit site of the CFG in a CFG kernel.
    pub fn site_k(&mut self) -> u32 {
        self.site_k_for("insn")
    }

    /// `site_k` for a guard: with coverage on, the site is told apart by the
    /// source line that emitted the guard (the report names which guard fails).
    fn site_k_guard(&mut self, at: &'static std::panic::Location<'static>) -> u32 {
        let kind = if super::glue::cov_on() {
            let file = at.file().rsplit('/').next().unwrap_or("");
            Box::leak(format!("g@{file}@{}", at.line()).into_boxed_str())
        } else {
            "insn"
        };
        self.site_k_for(kind)
    }

    fn site_k_for(&mut self, kind: &'static str) -> u32 {
        let Some(x) = self.cfgx.as_mut() else {
            return self.idx;
        };
        let (pc, off) = self.bank_site.unwrap_or((x.pc, x.nblock));
        let site = Site {
            pc,
            off,
            active: x.active.clone(),
            pending: x.pending,
            ret: 0,
            tail: Default::default(),
            kind,
        };
        if let Some(i) = x.sites.iter().position(|s| *s == site) {
            return i as u32;
        }
        x.sites.push(site);
        (x.sites.len() - 1) as u32
    }

    /// A site with an explicit address (a branch to a pc, a stop).
    pub fn site_at(&mut self, pc: u32, off: u32, active: Vec<u16>) -> u32 {
        self.site_ret(pc, off, active, 0)
    }

    /// `site_at` for the exit after a return instruction RET (`DYN_PC`).
    pub fn site_ret(&mut self, pc: u32, off: u32, active: Vec<u16>, ret: u32) -> u32 {
        self.site_tail(pc, off, active, ret, Default::default())
    }

    /// `site_ret` for the exit that completes a call or return (`Tail`).
    pub fn site_tail(
        &mut self,
        pc: u32,
        off: u32,
        active: Vec<u16>,
        ret: u32,
        tail: crate::fast::cfg::Tail,
    ) -> u32 {
        let x = self.cfgx.as_mut().expect("cfg lowering");
        let site = Site {
            pc,
            off,
            active,
            pending: None,
            ret,
            tail,
            kind: match tail {
                crate::fast::cfg::Tail::Call(_) => "call",
                crate::fast::cfg::Tail::Rts => "ret",
                crate::fast::cfg::Tail::None if pc == crate::fast::cfg::DYN_PC => "ret",
                crate::fast::cfg::Tail::None => "edge",
            },
        };
        if let Some(i) = x.sites.iter().position(|s| *s == site) {
            return i as u32;
        }
        x.sites.push(site);
        (x.sites.len() - 1) as u32
    }

    /// A kernel variable outside the register file.
    pub fn new_var(&mut self, ty: Ty, init: ExtraInit, out: Option<u32>) -> Var {
        self.vars.push(ty);
        let var = Var((self.vars.len() - 1) as u32);
        self.extras.push(Extra { var, init, out });
        var
    }

    /// End a basic block: the registers it wrote go back to their variables
    /// and nothing computed in it is carried into the next block.
    pub fn end_block(&mut self) {
        for c in 0..NALL {
            if self.written_iter[c]
                && let (Some(var), Some(v)) = (self.regs[c].var, self.cur[c])
            {
                self.body.push(Inst::Set(var, v));
                if c < NREGS as usize {
                    self.var_abs[c] = self.abs[v as usize];
                }
            }
            self.cur[c] = None;
            self.written_iter[c] = false;
        }
        self.conv.clear();
        self.ftz_ok.clear();
        self.exit_id = None;
        self.lw = [None; 3];
    }

    /// The exit fix-ups for the current instruction: the registers written
    /// so far in this iteration, as variables.
    fn exit_for_insn(&mut self) -> u32 {
        if let Some(e) = self.exit_id {
            return e;
        }
        let mut list = Vec::new();
        for c in 0..NALL {
            if self.written_iter[c]
                && let (Some(var), Some(v)) = (self.regs[c].var, self.cur[c])
            {
                list.push((var, v));
            }
        }
        self.exits.push(list);
        let e = (self.exits.len() - 1) as u32;
        self.exit_id = Some(e);
        e
    }

    #[track_caller]
    pub fn guard(&mut self, ok: Val) {
        let ok = self.weaken(ok);
        let exit = self.exit_for_insn();
        let k = self.site_k_guard(std::panic::Location::caller());
        self.body.push(Inst::Guard { ok, k, exit });
    }

    #[track_caller]
    pub fn guard_any(&mut self, a: Val, b: Val) {
        let a = self.weaken(a);
        let exit = self.exit_for_insn();
        let k = self.site_k_guard(std::panic::Location::caller());
        self.body.push(Inst::GuardAny { a, b, k, exit });
    }

    // -- registers ---------------------------------------------------------

    fn check_code(&self, code: u32) -> LR<usize> {
        if code < 96 || sysops::plain_sys(code) {
            Ok(code as usize)
        } else {
            refuse(format!("register code {code} outside R/I/M/L/B/S"))
        }
    }

    /// The register a compute operand names: R/F 0..15 are S/SF on PEy.
    fn pe_code(&self, code: u32) -> u32 {
        if self.pe_off != 0 && code < 16 {
            code + self.pe_off
        } else {
            code
        }
    }

    fn reg_var(&mut self, c: usize) -> Var {
        if let Some(v) = self.regs[c].var {
            return v;
        }
        let ty = if c < NREGS as usize && self.plan.fty[c] {
            Ty::F32
        } else {
            Ty::I32
        };
        self.vars.push(ty);
        let v = Var((self.vars.len() - 1) as u32);
        self.regs[c].var = Some(v);
        self.regs[c].vty = Some(ty);
        v
    }

    pub fn init_abs_pub(&self, c: usize) -> Option<Abs> {
        self.init_abs(c)
    }

    /// Entry-iteration range of register C's value.
    fn init_abs(&self, c: usize) -> Option<Abs> {
        if let Some((b, top)) = self.circ_range(c) {
            // A circular index stays inside its buffer whatever it steps by.
            return Some(Abs {
                base: Base::None,
                lo: b as i64,
                hi: top as i64 - 1,
                ts: 0,
                nf: 0,
            });
        }
        if !self.plan.written[c] {
            return Some(Abs {
                base: Base::Reg(c as u8),
                lo: 0,
                hi: 0,
                ts: 0,
                nf: 0,
            });
        }
        if self.env.flag_v && self.cfg_shape {
            // CFG kernels: the loops' strides come from `flow`.
            return Some(Abs {
                base: Base::Reg(c as u8),
                lo: 0,
                hi: 0,
                ts: 0,
                nf: 0,
            });
        }
        match self.pass {
            0 | 1 => Some(Abs {
                base: Base::Reg(c as u8),
                lo: 0,
                hi: 0,
                ts: 0,
                nf: 0,
            }),
            _ => self.plan.stride[c].map(|s| Abs {
                base: Base::Reg(c as u8),
                lo: 0,
                hi: 0,
                ts: s,
                nf: 0,
            }),
        }
    }

    /// The circular buffer `[B, B + 4*L)` (byte space, normal words) of index
    /// register C (UREG code 16..31) when the body writes C and the state the
    /// region is made for has L != 0 and a known B. From pass 1 on (pass 0
    /// finds which registers are written). The buffer must end below 2^32
    /// (the manual's "memory map overflow" case is outside the rule). A
    /// register with such a buffer is assumed to stay inside it: every write
    /// of it in the body must be a wrapped update (`commit` refuses others),
    /// and `Req::Within` makes it so at entry.
    pub fn circ_range(&self, c: usize) -> Option<(u32, u32)> {
        if self.pass == 0
            || !(16..32).contains(&c)
            || !self.plan.written[c]
            || (self.plan.plain >> c) & 1 != 0
        {
            return None;
        }
        let n = c - 16;
        if self.lb_stale(n) {
            return None;
        }
        let (len, base) = (self.env.dag_l[n]?, self.env.dag_b[n]?);
        if len == 0 || len > (1 << 28) {
            return None;
        }
        let top = base as u64 + 4 * len as u64;
        (top <= u32::MAX as u64).then_some((base, top as u32))
    }

    /// `iv` advanced by the constant DELTA bytes inside the circular buffer
    /// `[base, base + len)` (all in bytes), the interpreter's single
    /// correction (`memory._circular_step`: `candidate >= B + L` -> `- L`,
    /// else `< B` -> `+ L`, unsigned 32-bit). IV must lie in the buffer (a
    /// `Req::Within` the caller adds), |DELTA| <= LEN, and the sum must not
    /// wrap 32 bits (refused). The result's range is the buffer.
    pub fn wrap_add(&mut self, iv: Val, delta: Val, d: i64, base: u32, len: i64) -> LR<Val> {
        let top = base as i64 + len;
        let new = if d == 0 {
            iv
        } else if d > 0 {
            // iv <= top - 1, so the sum is below 2^32 when top - 1 + d is;
            // it is >= B, so only the upper edge can be crossed.
            if top - 1 + d > u32::MAX as i64 {
                return refuse("circular sum overflows 32 bits");
            }
            let t = self.bin(Bin::Add, iv, delta);
            let edge = self.ci(top as u32);
            let over = self.bin(Bin::GeU, t, edge);
            let l = self.ci(len as u32);
            let back = self.bin(Bin::Sub, t, l);
            self.select(over, back, t)
        } else {
            if (base as i64) + d < 0 {
                return refuse("circular sum below zero");
            }
            let t = self.bin(Bin::Add, iv, delta);
            let edge = self.ci(base);
            let under = self.bin(Bin::LtU, t, edge);
            let l = self.ci(len as u32);
            let fwd = self.bin(Bin::Add, t, l);
            self.select(under, fwd, t)
        };
        self.abs[new as usize] = Some(Abs {
            base: Base::None,
            lo: base as i64,
            hi: top - 1,
            ts: 0,
            nf: 0,
        });
        Ok(new)
    }

    /// The baked circular parameters of index register N for an update by
    /// DELTA (scaled bytes): `(B, buffer bytes)` after the checks every
    /// circular lowering shares, with the requirements they imply (L and B
    /// baked, the index inside the buffer).
    fn circ_params(&mut self, src: u32, iv_len_scale: i64, delta: Val) -> LR<(u32, i64, i64)> {
        let n = (src - 16) as usize;
        let len = self.env.dag_l[n].unwrap_or(0);
        let (Some(base), true) = (self.env.dag_b[n], len != 0) else {
            return refuse("circular buffer unusable (B unknown)");
        };
        let lenb = len as i64 * iv_len_scale;
        let top = base as i64 + lenb;
        if len > (1 << 28) || top > u32::MAX as i64 {
            return refuse("circular buffer beyond 2^32");
        }
        let Some(d) = self.konst_of(delta) else {
            return refuse("circular modifier not constant");
        };
        if d.abs() > lenb {
            return refuse("circular modifier larger than the buffer");
        }
        self.require_eq(48 + n as u32, len);
        self.require_eq(64 + n as u32, base);
        let w = Req::Within {
            c: src as u8,
            lo: base,
            hi: (top - 1) as u32,
        };
        if !self.reqs.contains(&w) {
            self.reqs.push(w);
        }
        Ok((base, lenb, d))
    }

    /// The post-modify update of index register IC (UREG code 16..31): IV
    /// is its value, MODIFIED the plain sum IV + delta, DELTA the modifier
    /// already scaled by 4 (normal word). The interpreter's rule
    /// (`memory._post_modify_index`): with L == 0 the update is the sum; with
    /// L != 0 it is the wrapped sum when MODE1.CBUFEN is set (and the sum when
    /// it is clear, which is not lowered).
    pub fn post_update(&mut self, ic: u32, iv: Val, delta: Val, modified: Val) -> LR<()> {
        self.post_update_scaled(ic, iv, delta, modified, 4)
    }

    /// `post_update` for an access of SCALE bytes per modifier unit (4 normal
    /// word, 2 short word, 1 byte): the interpreter scales L by the same
    /// factor (`_post_modify_index`'s `scaled_length`).
    pub fn post_update_scaled(
        &mut self,
        ic: u32,
        iv: Val,
        delta: Val,
        modified: Val,
        scale: i64,
    ) -> LR<()> {
        let v = self.post_value(ic, iv, delta, modified, scale)?;
        self.wr_i(ic, v)
    }

    /// The new value of index register IC after a post-modify (see
    /// `post_update_scaled`), without writing it.
    pub fn post_value(
        &mut self,
        ic: u32,
        iv: Val,
        delta: Val,
        modified: Val,
        scale: i64,
    ) -> LR<Val> {
        self.self_update.push(ic as u8);
        let n = (ic - 16) as usize;
        if self.lb_stale(n) {
            return refuse("L/B of the index register written in the region");
        }
        let Some(len) = self.env.dag_l[n] else {
            return refuse("index length register unknown");
        };
        if len == 0 {
            // No circular buffer: the plain sum whatever MODE1 says.
            self.require_eq(48 + n as u32, 0);
            return Ok(modified);
        }
        if self.mode1_cur & MODE1_CBUFEN == 0 {
            return refuse("MODE1.CBUFEN clear with L != 0");
        }
        if self.pass == 0 {
            // Pass 0 only finds which registers are written.
            return Ok(modified);
        }
        self.mode1_mask |= MODE1_CBUFEN;
        if (self.plan.plain | self.plain_out) >> ic & 1 != 0 {
            // The register is also written plainly: no in-buffer entry value.
            return self.wrap_general(ic, iv, delta, scale);
        }
        let (base, lenb, d) = self.circ_params(ic, scale, delta)?;
        self.wrap_add(iv, delta, d, base, lenb)
    }

    /// `Id = MODIFY(Is, delta)` for the forms that wrap on the source's B and
    /// L whatever MODE1.CBUFEN says (19a_scaled, 7b: `forms_dag.py`). IV is
    /// the source's value, DELTA the modifier in bytes, PLAIN the sum, SCALE
    /// the access size that scales L (4 normal word, 2 short word, 1 plain).
    /// UNSIGNED: the interpreter compares the modifier's 32-bit register value
    /// unsigned (7b), so a negative one is not a wrap but an unknown result
    /// there; such a region is refused.
    #[allow(clippy::too_many_arguments)]
    pub fn modify_update(
        &mut self,
        src: u32,
        dst: u32,
        iv: Val,
        delta: Val,
        plain: Val,
        scale: i64,
        unsigned: bool,
    ) -> LR<()> {
        self.note_modify(src, dst);
        self.no_plain_circ(src)?;
        let n = (src - 16) as usize;
        if self.lb_stale(n) {
            return refuse("L/B of the index register written in the region");
        }
        let Some(len) = self.env.dag_l[n] else {
            return refuse("index length register unknown");
        };
        if len == 0 {
            self.require_eq(48 + n as u32, 0);
            return self.wr_i(dst, plain);
        }
        if self.pass == 0 {
            return self.wr_i(dst, plain);
        }
        let (base, lenb, d) = self.circ_params(src, scale, delta)?;
        if unsigned && d < 0 {
            return refuse("circular MODIFY by a negative M (the interpreter reads M unsigned)");
        }
        let new = self.wrap_add(iv, delta, d, base, lenb)?;
        self.wr_i(dst, new)
    }

    /// `dst = modify(src, ..)`: the anchor hint follows the index value
    /// (`hint`): kept for an update in place, copied for another destination.
    pub fn note_modify(&mut self, src: u32, dst: u32) {
        if src == dst {
            self.self_update.push(dst as u8);
        } else {
            self.moves.push((dst as u8, src as u8));
        }
    }

    /// A circular update through index register IC needs its entry value in
    /// the buffer (`Req::Within`); a register the region also writes with a
    /// plain move has no such value, so the update is not lowered when the
    /// register has a circular buffer at all.
    fn no_plain_circ(&self, ic: u32) -> LR<()> {
        if ((self.plan.plain | self.plain_out) >> ic) & 1 != 0
            && self.env.dag_l[(ic - 16) as usize] != Some(0)
        {
            return refuse("circular update of an index register that is also written plainly");
        }
        Ok(())
    }

    /// A requirement that register CODE has exactly value K.
    pub fn require_eq(&mut self, code: u32, k: u32) {
        let r = Req::Eq(code as u8, k);
        if !self.reqs.contains(&r) {
            self.reqs.push(r);
        }
    }

    /// Whether register C (an M register the body never writes) is baked
    /// into the code as its build-time value.
    fn baked(&self, c: usize) -> Option<u32> {
        if self.pass >= 1 && (32..48).contains(&c) && !self.plan.written[c] {
            self.env.regs[c]
        } else {
            None
        }
    }

    /// The register's current value (before the instruction), as a typed Val.
    fn rd(&mut self, code: u32) -> LR<Val> {
        if code >= 96 && !sysops::plain_sys(code) {
            return self.rd_sys(code);
        }
        let c = self.check_code(code)?;
        if let Some(k) = self.baked(c) {
            if !self.reqs.contains(&Req::Eq(c as u8, k)) {
                self.reqs.push(Req::Eq(c as u8, k));
            }
            self.regs[c].used = true;
            return Ok(self.ci(k));
        }
        let first = !self.regs[c].used;
        self.regs[c].used = true;
        if first {
            self.regs[c].first_read = true;
        }
        if let Some(v) = self.cur[c] {
            return Ok(v);
        }
        let var = self.reg_var(c);
        let ty = self.vars[var.0 as usize];
        let abs = if ty == Ty::I32 { self.var_abs[c] } else { None };
        let v = self.emit_abs(ty, Op::GetVar(var), abs);
        self.cur[c] = Some(v);
        Ok(v)
    }

    pub fn rd_i(&mut self, code: u32) -> LR<Val> {
        let code = self.pe_code(code);
        self.check_code(code)?;
        let v = self.rd(code)?;
        self.regs[code as usize].i_uses += 1;
        Ok(self.to_i(v))
    }

    pub fn rd_f(&mut self, code: u32) -> LR<Val> {
        let code = self.pe_code(code);
        self.check_code(code)?;
        let v = self.rd(code)?;
        self.regs[code as usize].f_uses += 1;
        Ok(self.to_f(v))
    }

    /// `rd_f` for a computational float op: a nonzero denormal operand reads
    /// as a zero on the device (`floats._float32`), which the kernel does
    /// not model, so the kernel leaves at one.
    pub fn rd_f_op(&mut self, code: u32) -> LR<Val> {
        let x = self.rd_f(code)?;
        self.guard_not_denormal(x);
        Ok(x)
    }

    /// Guard that the f32 value X is not a nonzero denormal.
    pub fn guard_not_denormal(&mut self, x: Val) {
        if self.ftz_ok.contains(&x) {
            return;
        }
        // The magnitude shifted left by one, minus 2, is below 0xfffffd
        // exactly for the exponent field 0 with a nonzero mantissa.
        let xb = self.to_i(x);
        let k1 = self.ci(1);
        let mag2 = self.bin(Bin::Shl, xb, k1);
        let k2 = self.ci(2);
        let m = self.bin(Bin::Sub, mag2, k2);
        let lim = self.ci(0x00ff_fffd);
        let ok = self.bin(Bin::GeU, m, lim);
        self.guard(ok);
        if self.cond.is_none() {
            self.ftz_ok.insert(x);
        }
    }

    /// The f32 sum or difference R of two operands that are normal or zero,
    /// with a denormal result returned as the zero of its sign (the device
    /// returns post-rounded denormals as zeros; add and subtract of floats
    /// are exact there, so the native result is the exact one).
    pub fn flush_denormal_result(&mut self, r: Val) -> Val {
        let rb = self.to_i(r);
        let me = self.ci(0x7f80_0000);
        let e = self.bin(Bin::And, rb, me);
        let z = self.ci(0);
        let tiny = self.bin(Bin::Eq, e, z);
        let sg = self.ci(0x8000_0000);
        let zero = self.bin(Bin::And, rb, sg);
        let bits = self.select(tiny, zero, rb);
        let out = self.to_f(bits);
        self.ftz_ok.insert(out);
        out
    }

    /// Typeless read (moves, stores).
    pub fn rd_bits(&mut self, code: u32) -> LR<Val> {
        let code = self.pe_code(code);
        let v = self.rd(code)?;
        Ok(self.to_i(v))
    }

    /// Buffer a write of CODE (applied in order by `commit`).
    pub fn wr(&mut self, code: u32, v: Val) -> LR<()> {
        let code = self.pe_code(code);
        let c = self.check_code(code)?;
        // A write of L or B is an ordinary register write; the circular
        // lowering bakes their entry values, so a region that writes them
        // does not lower circular updates of that index (`lb_stale`).
        self.pending.writes.push((c as u8, v));
        Ok(())
    }

    pub fn wr_i(&mut self, code: u32, v: Val) -> LR<()> {
        let code = self.pe_code(code);
        let c = self.check_code(code)?;
        self.regs[c].i_uses += 1;
        self.wr(code, v)
    }

    pub fn wr_f(&mut self, code: u32, v: Val) -> LR<()> {
        let code = self.pe_code(code);
        let c = self.check_code(code)?;
        self.regs[c].f_uses += 1;
        self.wr(code, v)
    }

    pub fn pend_store(&mut self, base: Val, off: Val, v: Val) {
        self.pending.stores.push((base, off, v, 4));
    }

    /// A store of the low BYTES (1, 2 or 4) bytes of V.
    pub fn pend_store_n(&mut self, base: Val, off: Val, v: Val, bytes: u8) {
        self.pending.stores.push((base, off, v, bytes));
    }

    pub fn pend_flag(&mut self, kind: FlagKind, srcs: Vec<Val>) {
        if self.pe_off != 0 {
            self.pending.flag_y = Some((kind, srcs));
        } else {
            self.pending.flag = Some((kind, srcs));
        }
    }

    /// A second flag writer for the same instruction (applied after the first).
    pub fn pend_flag_also(&mut self, kind: FlagKind, srcs: Vec<Val>) {
        if self.pe_off != 0 {
            self.pending.flag2_y = Some((kind, srcs));
        } else {
            self.pending.flag2 = Some((kind, srcs));
        }
    }

    /// The flag effect of an instruction that has no source to remember.
    pub fn pend_flag_none(&mut self, kind: FlagKind) {
        self.pend_flag(kind, Vec::new());
    }

    // -- memory windows ----------------------------------------------------

    /// Whether an access with no window may be a flat guarded access here.
    fn flat_ok(&self) -> bool {
        self.flat_cur || self.flat_dyn_cur || self.env.flat_windows
    }

    /// The access at ADDR through the engine's page table when no window can
    /// serve it (`mem_addr::flat_access`), else the refusal WHY.
    fn flat_window(&mut self, addr: Val, bytes: i64, write: bool, why: &str) -> LR<(Val, Val)> {
        if !self.flat_ok() {
            return refuse(why);
        }
        if bytes == 4 {
            self.guard_plain(addr);
        }
        self.flat_access(addr, bytes as u8, write)
    }

    /// Byte address in range `a` is a window access of `bytes` bytes.
    pub fn window_access(&mut self, addr: Val, bytes: i64, write: bool) -> LR<(Val, Val)> {
        if self.env.flat_windows && self.strict() {
            return self.flat_access(addr, bytes as u8, write);
        }
        let abs = match self.abs[addr as usize] {
            Some(a) => a.as_addr(),
            None if !self.strict() => Abs::konst(0),
            None => return self.flat_window(addr, bytes, write, "address range unknown"),
        };
        let key = (abs.base, abs.ts, abs.nf);
        let w = match self
            .wins
            .iter()
            .position(|w| w.dynamic_anchor.is_none() && (w.base, w.ts, w.nf) == key)
        {
            Some(w) => w,
            None => {
                if self.wins.len() as u32 >= MAX_WIN {
                    return self.flat_window(addr, bytes, write, "too many windows");
                }
                self.wins.push(WinSpec {
                    base: abs.base,
                    lo: abs.lo,
                    hi: abs.hi + bytes,
                    ts: abs.ts,
                    nf: abs.nf,
                    read: false,
                    write: false,
                    dynamic_anchor: None,
                });
                let w = self.wins.len() - 1;
                let pv = self.new_val(Ty::I64, None);
                self.pre.push(Inst::Def(pv, Op::Ctx64(win_ptr(w as u32))));
                self.win_vals.push(pv);
                w
            }
        };
        let spec = &mut self.wins[w];
        spec.lo = spec.lo.min(abs.lo);
        spec.hi = spec.hi.max(abs.hi + bytes);
        if write {
            spec.write = true;
        } else {
            spec.read = true;
        }
        // The window slot is biased by the window's start address, so the
        // address itself is the offset.
        Ok((self.win_vals[w], addr))
    }

    /// A read-only dynamic window anchored by an independently known index
    /// register.  The host selects one present, normal-word-plain subspan at
    /// entry; the caller must guard every actual address against that span
    /// before issuing a load.
    pub fn dynamic_read_window(&mut self, addr: Val, anchor: u8) -> LR<(Val, Val, u32)> {
        self.dynamic_window(addr, anchor, false)
    }

    /// A dynamic window anchored by an independently known index register,
    /// for loads and (WRITE) stores. The host selects one present (and, with
    /// a store in the kernel, wholly overlay-written) normal-word-plain
    /// subspan at entry; the caller guards every actual address against that
    /// span before the access, so an address outside it leaves the kernel
    /// before the instruction. One slot per anchor serves both directions.
    pub fn dynamic_window(&mut self, addr: Val, anchor: u8, write: bool) -> LR<(Val, Val, u32)> {
        if let Some(pc) = self.cfgx.as_ref().map(|x| x.pc).or(self.cur_pc)
            && !self.dyn_pcs.contains(&pc)
        {
            self.dyn_pcs.push(pc);
        }
        let w = match self
            .wins
            .iter()
            .position(|w| w.dynamic_anchor == Some(anchor))
        {
            Some(w) => {
                if write {
                    self.wins[w].write = true;
                }
                w
            }
            None => {
                if self.wins.len() as u32 >= MAX_WIN {
                    return refuse("too many windows");
                }
                self.wins.push(WinSpec {
                    base: Base::None,
                    lo: 0,
                    hi: 0,
                    ts: 0,
                    nf: 0,
                    read: true,
                    write,
                    dynamic_anchor: Some(anchor),
                });
                let w = self.wins.len() - 1;
                let pv = self.new_val(Ty::I64, None);
                self.pre.push(Inst::Def(pv, Op::Ctx64(win_ptr(w as u32))));
                self.win_vals.push(pv);
                w
            }
        };
        Ok((self.win_vals[w], addr, w as u32))
    }

    /// Require a known-register-free fact: the byte range of address A is in
    /// the range where normal-word accesses are plain byte addresses scaled
    /// by 4 (no normal-word mapping).
    pub fn require_plain(&mut self, addr: Val, bytes: i64) -> LR<()> {
        match self.abs[addr as usize].map(Abs::as_addr) {
            Some(a) => {
                let r = Req::NwPlain {
                    base: a.base,
                    lo: a.lo,
                    hi: a.hi + bytes - 1,
                    ts: a.ts,
                    nf: a.nf,
                };
                if !self.reqs.contains(&r) {
                    self.reqs.push(r);
                }
                Ok(())
            }
            None if !self.strict() => Ok(()),
            // No range to require: guard each access instead (flat accesses).
            None if self.flat_ok() => {
                self.guard_plain(addr);
                Ok(())
            }
            None => {
                refuse("address range unknown")
            }
        }
    }

    // -- instruction end ---------------------------------------------------

    pub fn begin_insn(&mut self, idx: u32) {
        self.idx = idx;
        self.exit_id = None;
        self.self_update.clear();
        self.moves.clear();
        self.pending = Pending::default();
        self.cond = None;
        self.ncond = None;
    }

    /// Emit the instruction's stores, register updates and flag effect. Under
    /// an execution condition every effect is conditional: a store sits in a
    /// skipped block, a register gets `select(cond, new, old)`, the flag
    /// state is selected the same way.
    pub fn commit(&mut self) -> LR<()> {
        let cond = self.cond;
        let mut flag_srcs = Vec::new();
        // One writer per flag group (a multifunction instruction has two).
        for (pe, w) in [
            (0u8, self.pending.flag.take()),
            (0, self.pending.flag2.take()),
            (1, self.pending.flag_y.take()),
            (1, self.pending.flag2_y.take()),
        ] {
            let Some((kind, srcs)) = w else { continue };
            if self.flag_v() {
                // The kernel notes the writer in pseudo registers, written
                // (and made conditional) with the others.
                self.flag_note(kind, &srcs, pe)?;
            } else {
                flag_srcs.push((kind, srcs, pe));
            }
        }
        let stores = std::mem::take(&mut self.pending.stores);
        if !stores.is_empty() {
            let skip = match cond {
                Some(_) => {
                    let nc = self.not_cond();
                    let l = self.new_label();
                    self.body.push(Inst::BrIf { c: nc, target: l });
                    Some(l)
                }
                None => None,
            };
            for (b, o, v, n) in stores {
                self.body.push(if n == 4 {
                    Inst::Store32 { base: b, off: o, v }
                } else {
                    Inst::StoreN {
                        base: b,
                        off: o,
                        v,
                        bytes: n,
                    }
                });
            }
            if let Some(l) = skip {
                self.body.push(Inst::Label(l));
            }
        }
        let writes = std::mem::take(&mut self.pending.writes);
        for (c, v) in writes {
            let c = c as usize;
            self.reg_var(c);
            let v = match cond {
                Some(cv) => {
                    // The register's value before this instruction (also
                    // for a register the region has not read yet).
                    let old = self.rd_any(c);
                    let old = if self.ty(v) == Ty::F32 {
                        self.to_f(old)
                    } else {
                        self.to_i(old)
                    };
                    self.select(cv, v, old)
                }
                None => v,
            };
            if let Some((b, top)) = self.circ_range(c) {
                // Every write of a circular index must keep it in its
                // buffer (the range of the loop-carried value assumes it).
                // A write nothing in the kernel reads (`tail_write`: the
                // frame registers of a return's delay slots) still counts:
                // the earlier reads of the register were lowered under the
                // in-buffer assumption, and no `Req::Within` backs it unless
                // a wrapped update of the register asks for one.
                let inside = !self.tail_write
                    && self.abs[v as usize].is_some_and(|a| {
                        a.base == Base::None
                            && a.ts == 0
                            && a.nf == 0
                            && a.lo >= b as i64
                            && a.hi < top as i64
                    });
                if !inside {
                    // The next pass lowers the register's circular updates
                    // without the in-buffer assumption (`wrap_general`). A
                    // pass that was meant to emit the kernel is repeated
                    // (`Lowered::retry`).
                    self.plain_out |= 1 << c;
                    self.retry |= self.pass >= 2;
                }
            }
            if !self.regs[c].used {
                self.regs[c].used = true;
                self.regs[c].first_read = false;
            }
            self.regs[c].written = true;
            self.regs[c].first_write.get_or_insert(self.idx);
            self.regs[c].last_float = self.ty(v) == Ty::F32;
            self.written_iter[c] = true;
            if !self.self_update.contains(&(c as u8)) {
                self.hint[c] = self
                    .moves
                    .iter()
                    .find(|m| m.0 == c as u8)
                    .and_then(|m| self.hint[m.1 as usize]);
            }
            if c < 128 {
                self.written_mask |= 1u128 << c;
            }
            self.cur[c] = Some(v);
        }
        for (kind, srcs, pe) in flag_srcs {
            let mut slots = [255u8; 2];
            for (i, v) in srcs.iter().enumerate() {
                let n = self.nfsrc;
                if n >= MAX_FSRC {
                    return refuse("too many flag sources");
                }
                self.nfsrc += 1;
                slots[i] = n as u8;
                self.body.push(Inst::StCtx32 {
                    off: CTX_FSRC + 4 * n,
                    v: *v,
                });
            }
            self.flags.push(FlagWriter {
                insn: self.idx,
                kind,
                fsrc: slots,
                pe,
            });
        }
        Ok(())
    }

    // -- region ------------------------------------------------------------

    /// Lower one decoded instruction.
    pub fn lower_insn(&mut self, idx: u32, d: &Dec) -> LR<()> {
        self.bank_site = self.bank_site_next.take();
        let r = self
            .lower_insn_inner(idx, d)
            .map_err(|e| Refuse(format!("insn {idx} ({}): {}", d.form, e.0)));
        // The adjusted exit site belongs to this instruction only: the guards
        // that follow it (a budget check at the next cut point, a branch)
        // report their own position.
        self.bank_site = None;
        r
    }

    fn lower_insn_inner(&mut self, idx: u32, d: &Dec) -> LR<()> {
        self.begin_insn(idx);
        let at = self.cfgx.as_ref().map(|x| x.pc).or(self.cur_pc);
        self.flat_cur = at.is_some_and(super::cfg::is_flat_pc);
        self.flat_dyn_cur = at.is_some_and(super::cfg::is_flat_dyn);
        // Only an instruction that cannot be lowered becomes a helper (region.rs
        // `deny`); one with a guarded dynamic window keeps it.
        // An execution condition is handled here, for every form: the form
        // code sees the unconditional instruction.
        let plain;
        let d = match d.field("cond") {
            Some(c) if c != 0x1f => {
                if !cond::COND_FORMS.contains(&d.form) {
                    return refuse(format!("conditional {}", d.form));
                }
                let cv = self.cond_value(c as u32)?;
                self.cond = Some(cv);
                plain = d.with_unconditional();
                &plain
            }
            _ => d,
        };
        if gen_enabled(d.form) && self.gen_lower(d)? {
            return self.commit();
        }
        if self.mode1_cur & (1 << 21) != 0 && !matches!(d.form, "21a" | "21c" | "22c" | "25c_rframe") {
            // The hand-written forms are made for SISD.
            return refuse(format!("{} in SIMD mode (no generated lowering)", d.form));
        }
        match d.form {
            "2a" | "2a_short" => self.form_2a(d)?,
            "2c" => self.form_2c(d)?,
            "6b_shiftimm" => self.form_6b(d)?,
            "6a_mem" => self.form_6a_mem(d)?,
            "3a" => self.form_3a(d)?,
            "3b" => self.form_3b(d)?,
            "3c" => self.form_3c(d)?,
            "3d" => self.form_3d(d)?,
            "7a" => self.form_7a(d)?,
            "5a_move" | "5b_move" => self.form_5_move(d)?,
            "17a" => self.form_17a(d)?,
            "17b" => self.form_17b(d)?,
            "18a" => self.form_18a(d)?,
            "21a" | "21c" | "22c" => self.form_nop()?,
            "4a" => self.form_4a(d)?,
            "4b" | "4d" => self.form_4b_4d(d)?,
            "15a" => self.form_15a(d)?,
            "15b" => self.form_15b(d)?,
            "14a" => self.form_14a(d)?,
            "14d" => self.form_14d(d)?,
            "19a" | "19a_scaled" => self.form_19a(d)?,
            "7b" => self.form_7b(d)?,
            // The call and return sequence of the stack-model firmware, in
            // cluster kernels only.
            "16a" => self.form_16a(d)?,
            "25c_rframe" => self.form_25c()?,
            other => return refuse(format!("form {other}")),
        }
        self.commit()
    }

    /// Finish: the pre section (register and flag-source initialisation), the
    /// post section (state write-out), and the per-register summary.
    pub fn finish(mut self) -> LR<Lowered> {
        // End of the iteration: the registers it wrote go back to their
        // variables (the loop-carried state).
        let cfg = self.cfgx.is_some();
        if !cfg && self.mode1_cur != self.env.mode1 {
            return refuse("MODE1 changes in a kernel that is not a CFG kernel");
        }
        if !cfg {
            for c in 0..NALL {
                if self.written_iter[c]
                    && let (Some(var), Some(v)) = (self.regs[c].var, self.cur[c])
                {
                    self.body.push(Inst::Set(var, v));
                }
            }
        }
        let mut pre = std::mem::take(&mut self.pre);
        let mut post = Vec::new();
        // The kernel variables some instruction assigns (a body `Set` or an
        // exit fix-up). An extra variable nothing assigns keeps its initial
        // value, so storing it back is skipped where the caller does not need
        // the slot (everything but the instruction and loop counts): the
        // caller knows from `Lowered::stored` which slots hold a value.
        let mut assigned = vec![false; self.vars.len()];
        for i in &self.body {
            if let Inst::Set(v, _) = i {
                assigned[v.0 as usize] = true;
            }
        }
        for list in &self.exits {
            for &(v, _) in list {
                assigned[v.0 as usize] = true;
            }
        }
        let always = |off: u32| {
            off == CTX_ICNT
                || (CTX_LOOP_REM_OUT..CTX_LOOP_REM_OUT + 4 * MAX_CFG_LOOPS).contains(&off)
                || (CTX_TAKEN..CTX_TAKEN + 4 * MAX_CFG_JUMPS).contains(&off)
        };
        let mut stored = Vec::new();
        for e in std::mem::take(&mut self.extras) {
            let ty = self.vars[e.var.0 as usize];
            let v = self.new_val(ty, None);
            pre.push(Inst::Def(
                v,
                match e.init {
                    ExtraInit::Const(c) => Op::CI32(c),
                    ExtraInit::Ctx(o) => Op::Ctx32(o),
                },
            ));
            pre.push(Inst::Set(e.var, v));
            if let Some(off) = e.out
                && (always(off) || assigned[e.var.0 as usize])
            {
                let g = self.new_val(ty, None);
                post.push(Inst::Def(g, Op::GetVar(e.var)));
                post.push(Inst::StCtx32 { off, v: g });
                stored.push(off);
            }
        }
        for c in 0..NALL {
            if self.regs[c].used
                && let Some(var) = self.regs[c].var
            {
                let vty = self.vars[var.0 as usize];
                self.regs[c].direct = c < NREGS as usize && (cfg || self.regs[c].first_read);
                let li = self.new_val(Ty::I32, None);
                // Registers come straight from the engine state; of the
                // pseudo registers only the entry ASTATX bits are not zero.
                match c as u32 {
                    c if c < NREGS => pre.push(Inst::Def(li, Op::St32(st_reg_b(c)))),
                    PSEUDO_FB0 => pre.push(Inst::Def(li, Op::St32(st_reg_b(118)))),
                    // Not fully known ASTATX: the history is not tracked; bit 0
                    // marks "no compare has run" (a compare clears it).
                    PSEUDO_CACC if !self.env.cacc_known => pre.push(Inst::Def(li, Op::CI32(1))),
                    PSEUDO_CACC => {
                        let a = self.new_val(Ty::I32, None);
                        pre.push(Inst::Def(a, Op::St32(st_reg_b(118))));
                        let m = self.new_val(Ty::I32, None);
                        pre.push(Inst::Def(m, Op::CI32(0xff00_0000)));
                        pre.push(Inst::Def(li, Op::Bin(Bin::And, a, m)));
                    }
                    _ => pre.push(Inst::Def(li, Op::CI32(0))),
                }
                let lv = if vty == Ty::F32 {
                    let f = self.new_val(Ty::F32, None);
                    pre.push(Inst::Def(f, Op::Un(Un::BitsToF, li)));
                    f
                } else {
                    li
                };
                pre.push(Inst::Set(var, lv));
                if self.regs[c].written {
                    let g = self.new_val(vty, None);
                    post.push(Inst::Def(g, Op::GetVar(var)));
                    let gi = if vty == Ty::F32 {
                        let b = self.new_val(Ty::I32, None);
                        post.push(Inst::Def(b, Op::Un(Un::FToBits, g)));
                        b
                    } else {
                        g
                    };
                    if self.regs[c].direct {
                        post.push(Inst::StSt32 {
                            off: st_reg_b(c as u32),
                            v: gi,
                        });
                    } else {
                        post.push(Inst::StCtx32 {
                            off: ctx_reg_out(c as u32),
                            v: gi,
                        });
                    }
                }
            }
        }
        let kernel = Kernel {
            vars: self.vars.clone(),
            val_ty: self.val_ty.clone(),
            pre,
            body: std::mem::take(&mut self.body),
            post,
            exits: std::mem::take(&mut self.exits),
            nlabels: self.nlabels,
            cfg,
        };
        if self.forget_mask & self.read_mask != 0 {
            return refuse("a condition reads a flag that a writer in the region forgets");
        }
        let cacc_unknown = self.has_compare && !self.env.cacc_known;
        let mut reqs = std::mem::take(&mut self.reqs);
        if cacc_unknown {
            // ASTATX is not fully known at entry, so the generic core's
            // compare forgets CACC (bits 31:24) whatever the region does
            // before: nothing to shift, no refusal for a forgetting writer.
            reqs.push(Req::CaccUnknown);
        } else if self.has_compare {
            // The CACC shift register is exact only while ASTATX is fully
            // known before each compare (`values._apply_flag_update`): known
            // at entry, and no writer in the region forgets bits.
            if self.flags.iter().any(|w| w.kind == FlagKind::FmulForget)
                || self.flag_kinds_out[1] >> FlagKind::FmulForget.group_id().1 & 1 != 0
            {
                // A compare ahead of every forget is exact: ASTATX is fully
                // known when it runs (the entry requirement below), so the
                // history shifts as the core's does, and a later forget
                // leaves CACC alone. In a kernel with kernel flags the order
                // is known along the paths (`fg_static`) and the cycles.
                if !(self.cfg_shape && self.flag_v()) {
                    return refuse("compare next to a flag-forgetting multiply");
                }
                let in_cycle = self.cycles.iter().any(|&(lo, hi)| {
                    let inside = |p: &&u32| (lo..=hi).contains(*p);
                    self.forget_pcs.iter().any(|p| inside(&p))
                        && self.compare_pcs.iter().any(|p| inside(&p))
                });
                if self.cmp_after_forget || in_cycle {
                    return refuse("compare after a flag-forgetting multiply");
                }
            }
            reqs.push(Req::FlagsKnown(u32::MAX));
        }
        if self.need_flags_known != 0 {
            reqs.push(Req::FlagsKnown(self.need_flags_known));
        }
        // Float rounding: TRUNCATE (bit 15) is part of every requirement (the
        // core models it only in `fix`, `falu_fix`; every other float op
        // rounds to nearest in either mode); ALUSAT (bit 13) is part of the
        // requirement when a condition reads it.
        // Bit 21 (PEYEN): a region is made for one SIMD state.
        let mode1_mask = self.mode1_mask | (1 << 15) | (1 << 21);
        reqs.push(Req::Mode1 {
            mask: mode1_mask,
            value: self.env.mode1 & mode1_mask,
        });
        // What the next pass learns.
        let mut next = Plan {
            pass: self.pass + 1,
            node_written: std::mem::take(&mut self.node_written),
            loop_stride: std::mem::take(&mut self.loop_stride_out),
            loop_back_seen: std::mem::take(&mut self.loop_back_seen_out),
            flag_kinds: self.flag_kinds_out,
            plain: self.plain_out | self.plan.plain,
            entry_abs: std::mem::take(&mut self.entry_abs_out),
            reg_sym: self.reg_sym,
            ..Plan::default()
        };
        for c in 0..NREGS as usize {
            let r = &self.regs[c];
            next.fty[c] = if r.written {
                r.last_float
            } else {
                r.f_uses > r.i_uses
            };
            next.written[c] = r.written;
            if r.written
                && let Some(v) = self.cur[c]
                && let Some(a) = self.abs[v as usize]
                && a.base == Base::Reg(c as u8)
                && a.lo == a.hi
                && a.ts == 0
            {
                next.stride[c] = Some(a.lo);
            }
        }
        let flag_v = self.flag_v();
        let flag_groups = self.flag_kinds_out.map(|k| k != 0);
        Ok(Lowered {
            kernel,
            regs: self.regs,
            reqs,
            flag_v,
            flag_groups,
            cacc: self.has_compare,
            cacc_unknown,
            wins: self.wins,
            flags: self.flags,
            stored,
            next,
            reg_n: self.reg_sym.map(|(_, c, k)| (c, k)),
            retry: self.retry,
            dyn_pcs: self.dyn_pcs,
        })
    }
}

pub struct Lowered {
    /// The kernel tracks flags in pseudo registers, and the groups with
    /// writers (their last writer's kind and sources are written out).
    pub flag_v: bool,
    pub flag_groups: [bool; NGROUPS as usize],
    /// The kernel tracks the CACC compare history (`PSEUDO_CACC`).
    pub cacc: bool,
    /// See `Region::cacc_unknown`.
    pub cacc_unknown: bool,
    pub kernel: Kernel,
    pub regs: Vec<RegInfo>,
    pub reqs: Vec<Req>,
    pub wins: Vec<WinSpec>,
    pub flags: Vec<FlagWriter>,
    /// The context slots the kernel's `post` writes for the extra variables
    /// (CFG kernels); the others are not touched.
    pub stored: Vec<u32>,
    pub next: Plan,
    /// The symbolic trip count of a CFG kernel without an entry loop: the
    /// register and the constant added to it (`Lower::reg_sym`).
    pub reg_n: Option<(u8, i64)>,
    /// See `Lower::retry`.
    pub retry: bool,
    /// See `Lower::dyn_pcs`.
    pub dyn_pcs: Vec<u32>,
}

/// The f32 result must be finite (not a NaN or an infinity): those make the
/// interpreter set AV or AI (and their sticky bits), which the fast path does
/// not model. A denormal result is fine for the float ALU ops (it is exact
/// and only its bit pattern shows in the flags).
pub fn result_finite(l: &mut Lower, r: Val) {
    let abs = l.un(Un::FAbs, r);
    let inf = l.cf(0x7f80_0000);
    let ok = l.bin(Bin::FLt, abs, inf);
    l.guard(ok);
}

/// The f32 result M of the multiply X * Y must be a finite normal number, or
/// zero because an operand is zero. A product below the smallest normal
/// (a denormal, or a zero made by underflow) is returned as a zero by the
/// device and sets MU and the sticky MUS, which the kernel does not do, so
/// it leaves (the interpreter runs `floats._float_multiply`). The operands
/// are normal or zero (`guard_not_denormal`), so a zero result with a
/// nonzero operand is an underflow.
pub fn result_no_underflow(l: &mut Lower, x: Val, y: Val, m: Val) {
    let abs = l.un(Un::FAbs, m);
    let inf = l.cf(0x7f80_0000);
    let min = l.cf(0x0080_0000);
    let zero = l.cf(0);
    let fin = l.bin(Bin::FLt, abs, inf);
    l.guard(fin);
    let big = l.bin(Bin::FGe, abs, min);
    let xz = l.bin(Bin::FEq, x, zero);
    let yz = l.bin(Bin::FEq, y, zero);
    let either = l.bin(Bin::Or, xz, yz);
    l.guard_any(big, either);
    if l.cond.is_none() {
        l.ftz_ok.insert(m);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> Env {
        Env {
            regs: [None; NREGS as usize],
            dag_l: [None; 16],
            dag_b: [None; 16],
            assume_nw32: true,
            mode1: 0,
            flag_v: false,
            guarded_dyn: false,
            bank_model: false,
            cacc_known: true,
            flat_windows: false,
        }
    }

    #[test]
    fn combined_px_move_writes_both_halves() {
        let mut l = Lower::new(env(), Plan::default(), 0);
        let value = l.ci(0x1234_5678);
        l.g_wr(crate::rt::UREG_PX as u32, value).unwrap();
        assert_eq!(l.pending.writes.len(), 2);
        assert_eq!(l.pending.writes[0].0, crate::rt::UREG_PX1 as u8);
        assert_eq!(l.konst_of(l.pending.writes[0].1), Some(0));
        assert_eq!(l.pending.writes[1], (crate::rt::UREG_PX2 as u8, value));
    }

    #[test]
    fn type_14d_only_lowers_unsigned_short_loads_in_sisd() {
        // Each combination of d/ex/l/w/x, decoded through the real table.
        for bits in 0..32u64 {
            let raw = 0x1a00_0000_0000 | (bits << 36) | (6 << 32) | 0x25_590c;
            let d = Dec::from_decoded(&crate::decode::decode_isa(raw)).unwrap();
            assert_eq!(d.form, "14d");
            for simd in [false, true] {
                let mut e = env();
                e.mode1 = if simd { 1 << 21 } else { 0 };
                let mut l = Lower::new(e, Plan::default(), 0);
                assert_eq!(
                    l.lower_insn(0, &d).is_ok(),
                    bits == 4 && !simd,
                    "opcode fields {bits:#07b}, SIMD {simd}"
                );
            }
        }
    }

    #[test]
    fn dynamic_and_static_windows_do_not_share_a_slot() {
        let mut l = Lower::new(env(), Plan::default(), 2);
        let addr = l.ci(0x20_0000);
        l.dynamic_read_window(addr, 18).unwrap();
        l.window_access(addr, 4, false).unwrap();
        l.window_access(addr, 4, true).unwrap();
        assert_eq!(l.wins.len(), 2);
        assert_eq!(l.wins[0].dynamic_anchor, Some(18));
        assert_eq!(l.wins[1].dynamic_anchor, None);
        assert!(l.wins[1].write);
    }

    #[test]
    fn dynamic_window_serves_loads_and_stores_in_one_slot() {
        let mut l = Lower::new(env(), Plan::default(), 2);
        let addr = l.ci(0x20_0000);
        l.dynamic_window(addr, 18, false).unwrap();
        assert!(!l.wins[0].write);
        l.dynamic_window(addr, 18, true).unwrap();
        l.dynamic_read_window(addr, 18).unwrap();
        assert_eq!(l.wins.len(), 1);
        assert!(l.wins[0].read && l.wins[0].write);
        // Another anchor gets its own slot.
        l.dynamic_window(addr, 19, true).unwrap();
        assert_eq!(l.wins.len(), 2);
    }
    #[test]
    fn flat_windows_make_no_window_slot() {
        let mut e = env();
        e.flat_windows = true;
        let mut l = Lower::new(e, Plan::default(), 2);
        let addr = l.ci(0x20_0000);
        l.window_access(addr, 4, true).unwrap();
        l.window_access(addr, 4, false).unwrap();
        assert!(l.wins.is_empty());
    }

    // Unit tests and the Cranelift dev-dependency use separate instances of
    // sharc-native. Use the backend's public serialized-IR boundary between them.
    #[cfg(not(target_family = "wasm"))]
    struct FlatNativeKernel(*mut std::ffi::c_void);

    #[cfg(not(target_family = "wasm"))]
    impl Drop for FlatNativeKernel {
        fn drop(&mut self) {
            // SAFETY: this handle was compiled below and is freed exactly once.
            unsafe { sharc_fast_cl::sharc_fast_free(self.0) }
        }
    }

    #[cfg(not(target_family = "wasm"))]
    impl CompiledKernel for FlatNativeKernel {
        unsafe fn run(&self, ctx: *mut u8, st: *mut u8) -> u32 {
            unsafe { sharc_fast_cl::sharc_fast_run2(self.0, ctx, st) }
        }
    }

    fn flat_test_ir(bytes: u8, write: bool) -> Kernel {
        flat_test_ir_for_layout(bytes, write, None)
    }

    fn flat_test_ir_for_layout(bytes: u8, write: bool, ptr64: Option<bool>) -> Kernel {
        let mut l = Lower::new(env(), Plan::default(), 2);
        l.flat_ptr64 = ptr64;
        let addr = l.emit(Ty::I32, Op::Ctx32(CTX_REGS_IN));
        let (p, o) = l.flat_access(addr, bytes, write).unwrap();
        if write {
            let v = l.ci(0xa17c_39e5);
            l.body.push(if bytes == 4 {
                Inst::Store32 { base: p, off: o, v }
            } else {
                Inst::StoreN {
                    base: p,
                    off: o,
                    v,
                    bytes,
                }
            });
        } else {
            let op = if bytes == 4 {
                Op::Load32 { base: p, off: o }
            } else {
                Op::LoadN {
                    base: p,
                    off: o,
                    bytes,
                }
            };
            let v = l.emit(Ty::I32, op);
            l.body.push(Inst::StCtx32 {
                off: CTX_REGS_OUT,
                v,
            });
        }
        l.finish().unwrap().kernel
    }

    /// Real Lower output with a simulated wasm32 pointer table, not a
    /// target-native build. WASM_FLAT_OUT optionally exports the serialized
    /// kernels and layout metadata for wasmgen and the V8 flat-access check.
    #[test]
    fn flat_access_wasm32_fixture() {
        use crate::mem::{PAGE_BITS, PAGE_DIRTY_OFF, PAGE_PRESENT_OFF, PAGE_SIZE, Page};
        // Page contains only fixed-size byte and u64 arrays; reject any change
        // that would make this host layout unsuitable for the simulation.
        assert_eq!(std::mem::offset_of!(Page, data), 0);
        assert_eq!(PAGE_PRESENT_OFF as usize, PAGE_SIZE);
        assert_eq!(PAGE_DIRTY_OFF as usize, PAGE_SIZE + PAGE_SIZE / 8);
        assert_eq!(std::mem::size_of::<Page>(), PAGE_SIZE + PAGE_SIZE / 4);
        let out = std::env::var_os("WASM_FLAT_OUT").map(std::path::PathBuf::from);
        if let Some(out) = &out {
            std::fs::create_dir_all(out).unwrap();
        }
        let mut entries = Vec::new();
        for bytes in [1, 2, 4] {
            for write in [false, true] {
                let k = flat_test_ir_for_layout(bytes, write, Some(false));
                k.validate().unwrap();
                for inst in k.pre.iter().chain(&k.body).chain(&k.post) {
                    assert!(
                        !matches!(
                            inst,
                            Inst::Def(_, Op::Load64 { .. } | Op::St32(_) | Op::Helper(_))
                                | Inst::StSt32 { .. }
                        ),
                        "fixture must not depend on host pointers or engine-state layout"
                    );
                    if let Inst::Guard { k, .. } = inst {
                        assert_eq!(*k, 0, "fixture requires default guard-index encoding");
                    }
                }
                let name = format!("kernel-{}-{bytes}", if write { "write" } else { "read" });
                if let Some(out) = &out {
                    let data: Vec<u8> = k.encode().iter().flat_map(|w| w.to_le_bytes()).collect();
                    std::fs::write(out.join(format!("{name}.bin")), data).unwrap();
                }
                entries.push(format!(
                    "{{\"name\":\"{name}\",\"bytes\":{bytes},\"write\":{write}}}"
                ));
            }
        }
        if let Some(out) = out {
            let metadata = format!(
                concat!(
                    "{{\"layout\":\"simulated_wasm32\",\"pointer_bytes\":4,",
                    "\"page_bits\":{},\"page_size\":{},\"page_present_off\":{},\"page_dirty_off\":{},",
                    "\"ctx\":{{\"size\":{},\"iters\":{},\"done\":{},\"exit_k\":{},",
                    "\"regs_in\":{},\"regs_out\":{},\"flat_eff\":{},\"flat_watch\":{}}},",
                    "\"exit_k\":0,\"store_value\":{},\"kernels\":[{}]}}\n"
                ),
                PAGE_BITS,
                PAGE_SIZE,
                PAGE_PRESENT_OFF,
                PAGE_DIRTY_OFF,
                CTX_SIZE,
                CTX_ITERS,
                CTX_DONE,
                CTX_EXIT_K,
                CTX_REGS_IN,
                CTX_REGS_OUT,
                CTX_FLAT_EFF,
                CTX_FLAT_WATCH,
                0xa17c_39e5u32,
                entries.join(",")
            );
            std::fs::write(out.join("layout.json"), metadata).unwrap();
        }
    }

    fn flat_test_kernels(bytes: u8, write: bool) -> Vec<Box<dyn CompiledKernel>> {
        let k = flat_test_ir(bytes, write);
        let mut kernels = vec![super::super::interp::InterpBackend.compile(&k).unwrap()];
        #[cfg(not(target_family = "wasm"))]
        {
            let words = k.encode();
            let mut err = [0i8; 512];
            // SAFETY: valid serialized input and writable error buffer.
            let h = unsafe {
                sharc_fast_cl::sharc_fast_compile(
                    words.as_ptr(),
                    words.len(),
                    err.as_mut_ptr(),
                    err.len(),
                )
            };
            assert!(!h.is_null(), "{}", unsafe {
                std::ffi::CStr::from_ptr(err.as_ptr()).to_string_lossy()
            });
            kernels.push(Box::new(FlatNativeKernel(h)));
        }
        kernels
    }

    fn flat_test_run(k: &dyn CompiledKernel, mem: &mut crate::mem::Mem, addr: u32) -> (u32, u32) {
        let mut ctx = super::super::glue::Ctx::default();
        let (eff, watch) = mem.flat_tables();
        ctx.set64(CTX_FLAT_EFF, eff as u64);
        ctx.set64(CTX_FLAT_WATCH, watch as u64);
        ctx.set32(CTX_ITERS, 1);
        ctx.set32(CTX_REGS_IN, addr);
        ctx.set32(CTX_REGS_OUT, 0xdead_beef);
        ctx.set32(CTX_WIN_CHECK, 1);
        // SAFETY: the tables remain live for the call, all memory operations
        // are guarded, and this kernel never accesses engine state.
        let result = unsafe { k.run(ctx.ptr(), std::ptr::null_mut()) };
        (result, ctx.get32(CTX_REGS_OUT))
    }

    #[test]
    fn ddr_monitor_flat_access_declines_before_load_or_store() {
        use crate::mem::{DdrAccessKind, Mem};
        use crate::rt::{Int, St, V, VI, bnd};
        const A: u32 = 0x8001_0000;
        for bytes in [1, 2, 4] {
            for write in [false, true] {
                for k in flat_test_kernels(bytes, write) {
                    let mut mem = Mem::new();
                    for i in 0..4 {
                        mem.write_byte(A + i, 0x55);
                    }
                    // Compile and run before attachment, then reuse the same
                    // kernel with changed page policy.
                    assert_eq!(flat_test_run(&*k, &mut mem, A).0, 0);
                    let old = mem.read_le(A, bytes as u32);
                    mem.set_ddr_monitor(Some((A, A + 4))).unwrap();
                    assert_eq!(flat_test_run(&*k, &mut mem, A), (1, 0xdead_beef));
                    assert_eq!(mem.first_ddr_access(), None);
                    let mut s = St::new(mem);
                    // Simulate the normal instruction fallback after the
                    // kernel exits before its data access.
                    if write {
                        assert!(
                            bnd::_dm_write_b(
                                &mut s,
                                VI::I(A as Int),
                                bytes as Int,
                                V::c(old as Int),
                                false
                            )
                            .unwrap()
                        );
                        assert_eq!(
                            s.mem.first_ddr_access().unwrap().kind,
                            DdrAccessKind::CpuWrite
                        );
                    } else {
                        assert_eq!(
                            bnd::_dm_read_b(&s, VI::I(A as Int), bytes as Int, false, false)
                                .unwrap(),
                            Some(V::c(old as Int))
                        );
                        assert_eq!(
                            s.mem.first_ddr_access().unwrap().kind,
                            DdrAccessKind::CpuRead
                        );
                    }
                    assert_eq!(s.mem.read_le(A, bytes as u32), old);
                    s.mem.set_ddr_monitor(None).unwrap();
                    assert_eq!(flat_test_run(&*k, &mut s.mem, A).0, 0);
                }
            }
        }
    }

    #[test]
    fn flat_access_loads_across_presence_words() {
        use crate::mem::{Mem, PAGE_SIZE};
        const BASE: u32 = 0x20_0000;
        const DATA: [u8; 4] = [0x83, 0x17, 0xe9, 0x46];
        for bytes in [1, 2, 4] {
            let kernels = flat_test_kernels(bytes, false);
            let offsets = (28..=33)
                .chain(60..=65)
                .chain([PAGE_SIZE as u32 - bytes as u32, PAGE_SIZE as u32 - 1]);
            for off in offsets {
                for missing in 0..=bytes {
                    let mut mem = Mem::new();
                    for i in 0..bytes {
                        if i != missing {
                            mem.write_byte(BASE + off + i as u32, DATA[i as usize]);
                        }
                    }
                    let accepted = missing == bytes && off + bytes as u32 <= PAGE_SIZE as u32;
                    let value = match bytes {
                        1 => DATA[0] as u32,
                        2 => u16::from_le_bytes([DATA[0], DATA[1]]) as u32,
                        4 => u32::from_le_bytes(DATA),
                        _ => unreachable!(),
                    };
                    let expected = if accepted {
                        (0, value)
                    } else {
                        (1, 0xdead_beef)
                    };
                    for (backend, k) in kernels.iter().enumerate() {
                        assert_eq!(
                            flat_test_run(&**k, &mut mem, BASE + off),
                            expected,
                            "backend {backend}, width {bytes}, offset {off}, missing {missing}"
                        );
                    }
                }
            }
            // A missing page and a present MMR page must both side-exit.
            let mut mem = Mem::new();
            for k in &kernels {
                assert_eq!(flat_test_run(&**k, &mut mem, BASE), (1, 0xdead_beef));
            }
            mem.load(BASE, &DATA);
            mem.reset();
            for k in &kernels {
                let expected = mem.read_le(BASE, bytes as u32);
                assert_eq!(flat_test_run(&**k, &mut mem, BASE), (0, expected));
            }
            let mut mmr = vec![false; 1 << 16];
            mmr[(BASE >> 16) as usize] = true;
            mem.set_mmr_pages(mmr);
            for k in &kernels {
                assert_eq!(flat_test_run(&**k, &mut mem, BASE), (1, 0xdead_beef));
            }
            // The low-address alias must use the canonical page's presence bits.
            let mut mem = Mem::new();
            for (i, b) in DATA.iter().enumerate() {
                mem.write_byte(0x2800_0000 + BASE + 63 + i as u32, *b);
            }
            for k in &kernels {
                let expected = mem.read_le(0x2800_0000 + BASE + 63, bytes as u32);
                assert_eq!(flat_test_run(&**k, &mut mem, BASE + 63), (0, expected));
            }
        }
    }

    #[test]
    fn flat_access_stores_keep_bitmap_and_watch_guards() {
        use crate::mem::Mem;
        const BASE: u32 = 0x20_0000;
        for bytes in [1, 2, 4] {
            let kernels = flat_test_kernels(bytes, true);
            for k in &kernels {
                for off in (28..=33)
                    .chain(60..=65)
                    .chain([65536 - bytes as u32, 65535])
                {
                    for missing in 0..=bytes {
                        let mut mem = Mem::new();
                        for i in 0..bytes {
                            if i != missing {
                                mem.write_byte(BASE + off + i as u32, 0x55);
                            }
                        }
                        let accepted = missing == bytes && off % 32 + bytes as u32 <= 32;
                        let before: Vec<_> = (0..bytes)
                            .map(|i| mem.byte(BASE + off + i as u32))
                            .collect();
                        let result = flat_test_run(&**k, &mut mem, BASE + off).0;
                        assert_eq!(
                            result,
                            if accepted { 0 } else { 1 },
                            "width {bytes}, offset {off}, missing {missing}"
                        );
                        let after: Vec<_> = (0..bytes)
                            .map(|i| mem.byte(BASE + off + i as u32))
                            .collect();
                        if accepted {
                            assert_eq!(after, 0xa17c_39e5u32.to_le_bytes()[..bytes as usize]);
                        } else {
                            assert_eq!(after, before);
                        }
                    }
                }
                let mut mem = Mem::new();
                mem.load(BASE, &[0x55; 4]);
                mem.reset();
                assert_eq!(flat_test_run(&**k, &mut mem, BASE).0, 1); // present, not dirty
                for i in 0..bytes {
                    mem.write_byte(BASE + i as u32, 0x55);
                }
                mem.watch_code(BASE, BASE + 4);
                assert_eq!(flat_test_run(&**k, &mut mem, BASE).0, 1);
                assert_eq!(
                    mem.read_le(BASE, bytes as u32),
                    match bytes {
                        1 => 0x55,
                        2 => 0x5555,
                        4 => 0x5555_5555,
                        _ => unreachable!(),
                    }
                );
            }
        }
    }

    #[test]
    fn a_negative_constant_range_is_a_high_address() {
        // `ci` sign-extends: 0x8052d790 is the range -2142053488, the address
        // 0x8052d790 modulo 2^32.
        let a = Abs::konst(0x8052_d790u32 as i32 as i64).as_addr();
        assert_eq!((a.lo, a.hi), (0x8052_d790, 0x8052_d790));
        // A range that crosses zero, or has a base, stays as it is.
        let b = Abs { lo: -4, hi: 4, ..Abs::konst(0) };
        assert_eq!(b.as_addr(), b);
        let c = Abs { base: Base::Reg(2), ..Abs::konst(-8) };
        assert_eq!(c.as_addr(), c);
    }
}
