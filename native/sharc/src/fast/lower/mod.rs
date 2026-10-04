//! Lowering: SHARC+ instructions to kernel IR.
//!
//! Each instruction follows the interpreter's own order (tools/sharc_core):
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

pub mod cond;
pub mod fixed_alu;
pub mod float_alu;
pub mod flow;
pub mod mem_addr;
pub mod mem_imm;
pub mod move_misc;
pub mod mult;
pub mod multifn;
pub mod shift;

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
    pub node_written: std::collections::BTreeMap<u32, u64>,
    pub loop_stride: Vec<[Option<i64>; NREGS as usize]>,
    /// Which loops had their loop-back lowered (the others never iterate in
    /// the kernel).
    pub loop_back_seen: Vec<bool>,
    /// The flag-writer kinds of each group in the region (bit per id).
    pub flag_kinds: [u32; 3],
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
            flag_kinds: [0; 3],
        }
    }
}

/// Build-time facts about the engine state the region is made for.
#[derive(Clone)]
pub struct Env {
    /// Fully known register values (UREG codes 0..47) at build time.
    pub regs: [Option<u32>; NREGS as usize],
    pub assume_nw32: bool,
    /// MODE1 at build time (the region is made for these rounding and
    /// saturation bits, see `Req::Mode1`).
    pub mode1: u32,
    /// The kernel tracks flags lazily (conditions or branches in the region,
    /// see `cond`); otherwise flag writers are replayed at exit from
    /// remembered sources.
    pub flag_v: bool,
}

/// Registers the lowering tracks: R, I, M and the pseudo registers (the
/// kernel's ASTATX bits and known mask).
pub const NALL: usize = (NREGS + NPSEUDO) as usize;

/// One instruction's pending effects (applied by `commit`).
#[derive(Default)]
struct Pending {
    writes: Vec<(u8, Val)>,
    stores: Vec<(Val, Val, Val)>,
    flag: Option<(FlagKind, Vec<Val>)>,
    /// A second flag writer of the same instruction (multifunction: the
    /// ALU and the multiplier both write flags).
    flag2: Option<(FlagKind, Vec<Val>)>,
}

pub struct Lower {
    pub env: Env,
    pub plan: Plan,
    pub pass: u8,
    /// The last writer of each flag group, as far as this block knows.
    pub lw: [Option<cond::LastW>; 3],
    /// The writer kinds seen so far (bit per id), for the next pass.
    pub flag_kinds_out: [u32; 3],
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
    pub fm_static: u32,
    forget_mask: u32,
    read_mask: u32,
    /// MODE1 bits the lowering relies on.
    pub mode1_mask: u32,
    /// Registers (R, I, M codes) written by the instruction just lowered.
    pub written_mask: u64,
    /// The kernel is a CFG kernel (loop strides are tracked by `flow`).
    pub cfg_shape: bool,
    /// CFG kernels: the address of the node whose lowering failed.
    pub fail_pc: Option<u32>,
    /// CFG kernels: what this pass learns for the next (see `Plan`), and the
    /// ranges of the registers at each loop's entry and head.
    pub node_written: std::collections::BTreeMap<u32, u64>,
    pub loop_stride_out: Vec<[Option<i64>; NREGS as usize]>,
    pub loop_back_seen_out: Vec<bool>,
    pub loop_in: Vec<Vec<Option<Abs>>>,
    pub loop_head: Vec<Vec<Option<Abs>>>,
    /// The abstract value of each register's variable at the current point
    /// (entry value plus the region's strides; carried across blocks).
    pub var_abs: Vec<Option<Abs>>,
    /// Extra kernel variables with their context in/out slots (CFG kernels).
    pub extras: Vec<Extra>,
    /// Exit-site bookkeeping of a CFG kernel.
    pub cfgx: Option<CfgX>,
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
            env,
            plan,
            pass,
            lw: [None; 3],
            flag_kinds_out: [0; 3],
            val_ty: Vec::new(),
            abs: Vec::new(),
            vars: Vec::new(),
            pre: Vec::new(),
            body: Vec::new(),
            regs: vec![RegInfo::default(); NALL],
            cur: vec![None; NALL],
            written_iter: vec![false; NALL],
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
            loop_in: Vec::new(),
            loop_head: Vec::new(),
            var_abs: Vec::new(),
            extras: Vec::new(),
            cfgx: None,
        };
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
            Bin::FAdd | Bin::FSub | Bin::FMul => Ty::F32,
            Bin::Add64 => Ty::I64,
            _ => Ty::I32,
        };
        self.emit_abs(ty, Op::Bin(b, x, y), abs)
    }

    pub fn un(&mut self, u: Un, x: Val) -> Val {
        let ty = match u {
            Un::FNeg | Un::FAbs | Un::BitsToF | Un::IToF => Ty::F32,
            Un::FToBits | Un::FToI | Un::Not => Ty::I32,
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
        let Some(x) = self.cfgx.as_mut() else {
            return self.idx;
        };
        let site = Site {
            pc: x.pc,
            off: x.nblock,
            active: x.active.clone(),
            pending: x.pending,
        };
        if let Some(i) = x.sites.iter().position(|s| *s == site) {
            return i as u32;
        }
        x.sites.push(site);
        (x.sites.len() - 1) as u32
    }

    /// A site with an explicit address (a branch to a pc, a stop).
    pub fn site_at(&mut self, pc: u32, off: u32, active: Vec<u16>) -> u32 {
        let x = self.cfgx.as_mut().expect("cfg lowering");
        let site = Site {
            pc,
            off,
            active,
            pending: None,
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

    pub fn guard(&mut self, ok: Val) {
        let ok = self.weaken(ok);
        let exit = self.exit_for_insn();
        let k = self.site_k();
        self.body.push(Inst::Guard { ok, k, exit });
    }

    pub fn guard_any(&mut self, a: Val, b: Val) {
        let a = self.weaken(a);
        let exit = self.exit_for_insn();
        let k = self.site_k();
        self.body.push(Inst::GuardAny { a, b, k, exit });
    }

    // -- registers ---------------------------------------------------------

    fn check_code(&self, code: u32) -> LR<usize> {
        if code < NREGS {
            Ok(code as usize)
        } else {
            refuse(format!("register code {code} outside R/I/M"))
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
        let v = self.rd(code)?;
        self.regs[code as usize].i_uses += 1;
        Ok(self.to_i(v))
    }

    pub fn rd_f(&mut self, code: u32) -> LR<Val> {
        let v = self.rd(code)?;
        self.regs[code as usize].f_uses += 1;
        Ok(self.to_f(v))
    }

    /// Typeless read (moves, stores).
    pub fn rd_bits(&mut self, code: u32) -> LR<Val> {
        let v = self.rd(code)?;
        Ok(self.to_i(v))
    }

    /// Buffer a write of CODE (applied in order by `commit`).
    pub fn wr(&mut self, code: u32, v: Val) -> LR<()> {
        let c = self.check_code(code)?;
        self.pending.writes.push((c as u8, v));
        Ok(())
    }

    pub fn wr_i(&mut self, code: u32, v: Val) -> LR<()> {
        let c = self.check_code(code)?;
        self.regs[c].i_uses += 1;
        self.wr(code, v)
    }

    pub fn wr_f(&mut self, code: u32, v: Val) -> LR<()> {
        let c = self.check_code(code)?;
        self.regs[c].f_uses += 1;
        self.wr(code, v)
    }

    pub fn pend_store(&mut self, base: Val, off: Val, v: Val) {
        self.pending.stores.push((base, off, v));
    }

    pub fn pend_flag(&mut self, kind: FlagKind, srcs: Vec<Val>) {
        self.pending.flag = Some((kind, srcs));
    }

    /// A second flag writer for the same instruction (applied after the first).
    pub fn pend_flag_also(&mut self, kind: FlagKind, srcs: Vec<Val>) {
        self.pending.flag2 = Some((kind, srcs));
    }

    /// The flag effect of an instruction that has no source to remember.
    pub fn pend_flag_none(&mut self, kind: FlagKind) {
        self.pending.flag = Some((kind, Vec::new()));
    }

    // -- memory windows ----------------------------------------------------

    /// Byte address in range `a` is a window access of `bytes` bytes.
    pub fn window_access(&mut self, addr: Val, bytes: i64, write: bool) -> LR<(Val, Val)> {
        let abs = match self.abs[addr as usize] {
            Some(a) => a,
            None if !self.strict() => Abs::konst(0),
            None => return refuse("address range unknown"),
        };
        let key = (abs.base, abs.ts, abs.nf);
        let w = match self.wins.iter().position(|w| (w.base, w.ts, w.nf) == key) {
            Some(w) => w,
            None => {
                if self.wins.len() as u32 >= MAX_WIN {
                    return refuse("too many windows");
                }
                self.wins.push(WinSpec {
                    base: abs.base,
                    lo: abs.lo,
                    hi: abs.hi + bytes,
                    ts: abs.ts,
                    nf: abs.nf,
                    read: false,
                    write: false,
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

    /// Require a known-register-free fact: the byte range of address A is in
    /// the range where normal-word accesses are plain byte addresses scaled
    /// by 4 (no normal-word mapping).
    pub fn require_plain(&mut self, addr: Val, bytes: i64) -> LR<()> {
        match self.abs[addr as usize] {
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
            None => refuse("address range unknown"),
        }
    }

    // -- instruction end ---------------------------------------------------

    pub fn begin_insn(&mut self, idx: u32) {
        self.idx = idx;
        self.exit_id = None;
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
        for (kind, srcs) in [self.pending.flag.take(), self.pending.flag2.take()]
            .into_iter()
            .flatten()
        {
            if self.flag_v() {
                // The kernel notes the writer in pseudo registers, written
                // (and made conditional) with the others.
                self.flag_note(kind, &srcs)?;
            } else {
                flag_srcs.push((kind, srcs));
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
            for (b, o, v) in stores {
                self.body.push(Inst::Store32 { base: b, off: o, v });
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
            if !self.regs[c].used {
                self.regs[c].used = true;
                self.regs[c].first_read = false;
            }
            self.regs[c].written = true;
            self.regs[c].first_write.get_or_insert(self.idx);
            self.regs[c].last_float = self.ty(v) == Ty::F32;
            self.written_iter[c] = true;
            if c < 64 {
                self.written_mask |= 1 << c;
            }
            self.cur[c] = Some(v);
        }
        for (kind, srcs) in flag_srcs {
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
            });
        }
        Ok(())
    }

    // -- region ------------------------------------------------------------

    /// Lower one decoded instruction.
    pub fn lower_insn(&mut self, idx: u32, d: &Dec) -> LR<()> {
        self.lower_insn_inner(idx, d)
            .map_err(|e| Refuse(format!("insn {idx} ({}): {}", d.form, e.0)))
    }

    fn lower_insn_inner(&mut self, idx: u32, d: &Dec) -> LR<()> {
        self.begin_insn(idx);
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
        match d.form {
            "2a" | "2a_short" => self.form_2a(d)?,
            "2c" => self.form_2c(d)?,
            "6b_shiftimm" => self.form_6b(d)?,
            "6a_mem" => self.form_6a_mem(d)?,
            "3a" => self.form_3a(d)?,
            "3b" => self.form_3b(d)?,
            "3c" => self.form_3c(d)?,
            "7a" => self.form_7a(d)?,
            "5a_move" | "5b_move" => self.form_5_move(d)?,
            "17a" => self.form_17a(d)?,
            "4a" => self.form_4a(d)?,
            "4b" | "4d" => self.form_4b_4d(d)?,
            "15a" => self.form_15a(d)?,
            "15b" => self.form_15b(d)?,
            "14a" => self.form_14a(d)?,
            "19a" | "19a_scaled" => self.form_19a(d)?,
            "7b" => self.form_7b(d)?,
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
            if let Some(off) = e.out {
                let g = self.new_val(ty, None);
                post.push(Inst::Def(g, Op::GetVar(e.var)));
                post.push(Inst::StCtx32 { off, v: g });
            }
        }
        for c in 0..NALL {
            if self.regs[c].used
                && let Some(var) = self.regs[c].var
            {
                let vty = self.vars[var.0 as usize];
                let li = self.new_val(Ty::I32, None);
                pre.push(Inst::Def(li, Op::Ctx32(ctx_reg_in(c as u32))));
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
                    post.push(Inst::StCtx32 {
                        off: ctx_reg_out(c as u32),
                        v: gi,
                    });
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
        let mut reqs = std::mem::take(&mut self.reqs);
        if self.need_flags_known != 0 {
            reqs.push(Req::FlagsKnown(self.need_flags_known));
        }
        // Float rounding: TRUNCATE (bit 15) must stay clear; ALUSAT (bit 13)
        // is part of the requirement when a condition reads it.
        let mode1_mask = self.mode1_mask | (1 << 15);
        if self.env.mode1 & (1 << 15) != 0 {
            return refuse("MODE1.TRUNCATE set");
        }
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
        let flag_groups = [
            self.flag_kinds_out[0] != 0,
            self.flag_kinds_out[1] != 0,
            self.flag_kinds_out[2] != 0,
        ];
        Ok(Lowered {
            kernel,
            regs: self.regs,
            reqs,
            flag_v,
            flag_groups,
            wins: self.wins,
            flags: self.flags,
            next,
        })
    }
}

pub struct Lowered {
    /// The kernel tracks flags in pseudo registers, and the groups with
    /// writers (their last writer's kind and sources are written out).
    pub flag_v: bool,
    pub flag_groups: [bool; 3],
    pub kernel: Kernel,
    pub regs: Vec<RegInfo>,
    pub reqs: Vec<Req>,
    pub wins: Vec<WinSpec>,
    pub flags: Vec<FlagWriter>,
    pub next: Plan,
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

/// The f32 result of a multiply must be a finite normal number or zero: a
/// denormal result sets MU (and the sticky MUS) in the interpreter.
pub fn result_normal_or_zero(l: &mut Lower, r: Val) {
    let abs = l.un(Un::FAbs, r);
    let inf = l.cf(0x7f80_0000);
    let min = l.cf(0x0080_0000);
    let zero = l.cf(0);
    let fin = l.bin(Bin::FLt, abs, inf);
    l.guard(fin);
    let big = l.bin(Bin::FGe, abs, min);
    let z = l.bin(Bin::FEq, abs, zero);
    l.guard_any(big, z);
}
