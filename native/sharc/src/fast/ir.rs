//! The fast tier's backend-neutral kernel IR.
//!
//! A kernel is the loop body of one region: `pre` runs once, `body` runs
//! `ctx.iters` times, `post` runs once at the end (normal completion or a
//! failed guard). Values are SSA temporaries (`Val`) typed u32 / f32 / u64;
//! loop-carried state lives in mutable variables (`Var`), which a backend
//! maps to locals (wasm) or SSA-constructed variables (Cranelift).
//!
//! The kernel never touches the engine state. It reads and writes one flat
//! context buffer (`CtxLayout`): the iteration count, the window pointers
//! (host addresses of the DM bytes the region touches, resolved and checked
//! by the caller before the call), the entry register values and, at exit,
//! the register values and flag sources. Memory goes through
//! `Load32`/`Store32` on a window pointer plus a byte offset; the caller has
//! proved that every access of every iteration lies inside its window, so the
//! kernel does no per-access checks.
//!
//! A `Guard` leaves the kernel when its condition is 0, recording the
//! instruction index `k` in the context. Instructions are atomic: all guards
//! of an instruction come before its first store and before its first
//! variable update, so a side exit sees the state before instruction `k`.

use std::fmt;

pub type Val = u32;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ty {
    I32,
    F32,
    I64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Var(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Un {
    /// f32 negate / absolute value (bit operations, NaN payloads kept).
    FNeg,
    FAbs,
    /// i32 bits <-> f32 (a reinterpretation).
    BitsToF,
    FToBits,
    /// f32 -> i32, truncating toward zero; defined only for |x| < 2^31
    /// (the lowering guards it first).
    FToI,
    /// i32 (signed) -> f32, round to nearest.
    IToF,
    /// u32 -> u64 zero extension.
    Zext,
    /// Bitwise not (i32).
    Not,
    /// Count of leading zero bits (i32; 32 for 0).
    Clz,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Bin {
    Add,
    Sub,
    Mul,
    And,
    Or,
    Xor,
    Shl,
    /// Logical / arithmetic right shift; the amount is taken mod 32.
    ShrU,
    ShrS,
    Eq,
    Ne,
    LtU,
    LeU,
    GtU,
    GeU,
    LtS,
    GtS,
    /// Ordered f32 compares (false when either operand is NaN); result 0/1.
    FLt,
    FGe,
    FEq,
    FAdd,
    FSub,
    FMul,
    /// u64 add / subtract.
    Add64,
    Sub64,
    /// f32 divide; high 32 bits of the signed 64-bit product.
    FDiv,
    MulHs,
}

#[derive(Clone, Copy, Debug)]
pub enum Op {
    CI32(u32),
    /// f32 constant by bits.
    CF32(u32),
    CI64(u64),
    GetVar(Var),
    /// Context reads at a byte offset (4-byte aligned / 8-byte aligned).
    Ctx32(u32),
    Ctx64(u32),
    /// A 32-bit context word read as an f32 (the kernel constant pool).
    CtxF32(u32),
    Un(Un, Val),
    Bin(Bin, Val, Val),
    /// `Select(c, a, b)`: a when c != 0, else b.
    Select(Val, Val, Val),
    /// 32-bit load at `base + zext(off)` (little endian, any alignment).
    Load32 {
        base: Val,
        off: Val,
    },
}

#[derive(Clone, Copy, Debug)]
pub enum Inst {
    Def(Val, Op),
    /// Assign a variable; an i32 value into an f32 variable or the reverse is
    /// a bit reinterpretation.
    Set(Var, Val),
    Store32 {
        base: Val,
        off: Val,
        v: Val,
    },
    StCtx32 {
        off: u32,
        v: Val,
    },
    /// Continue when `ok` != 0; otherwise perform the fix-ups of exit `exit`
    /// (see `Kernel::exits`), record `k` and go to `post`.
    Guard {
        ok: Val,
        k: u32,
        exit: u32,
    },
    /// As `Guard`, continuing when `a` or `b` is non-zero.
    GuardAny {
        a: Val,
        b: Val,
        k: u32,
        exit: u32,
    },
}

/// Context buffer layout (byte offsets, 8-byte aligned buffer, fixed size).
pub const CTX_ITERS: u32 = 0;
pub const CTX_DONE: u32 = 4;
pub const CTX_EXIT_K: u32 = 8;
pub const CTX_WIN: u32 = 16;
/// Window slot: the host pointer biased by the DM address the window starts
/// at (u64 at +0), so that the host address of DM address A is `slot + A`
/// (zero-extended, wrapping).
pub const WIN_STRIDE: u32 = 8;

/// Windows a region may use, registers it may touch (UREG codes 0..47:
/// R, I and M) and flag sources it may keep.
pub const MAX_WIN: u32 = 12;
pub const NREGS: u32 = 48;
pub const MAX_FSRC: u32 = 64;
pub const CTX_REGS_IN: u32 = CTX_WIN + WIN_STRIDE * MAX_WIN;
pub const CTX_REGS_OUT: u32 = CTX_REGS_IN + 4 * NREGS;
pub const CTX_FSRC: u32 = CTX_REGS_OUT + 4 * NREGS;
/// Constants the kernel loads once (so they live in registers): f32 +inf,
/// the smallest normal, zero.
pub const CTX_CONSTS: u32 = CTX_FSRC + 4 * MAX_FSRC;
pub const CONST_POOL: [u32; 3] = [0x7f80_0000, 0x0080_0000, 0];
pub const CTX_SIZE: u32 = (CTX_CONSTS + 4 * CONST_POOL.len() as u32 + 7) & !7;

pub const fn win_ptr(w: u32) -> u32 {
    CTX_WIN + WIN_STRIDE * w
}

#[derive(Clone, Debug, Default)]
pub struct Kernel {
    pub vars: Vec<Ty>,
    pub val_ty: Vec<Ty>,
    pub pre: Vec<Inst>,
    pub body: Vec<Inst>,
    pub post: Vec<Inst>,
    /// Variable updates a failing guard performs before leaving, so that
    /// the variables hold the registers as of before the instruction. A
    /// value of a different type than its variable is reinterpreted.
    pub exits: Vec<Vec<(Var, Val)>>,
}

impl Kernel {
    fn check_exit(&self, exit: u32, defined: &[bool]) -> Option<String> {
        let Some(list) = self.exits.get(exit as usize) else {
            return Some("undeclared exit".into());
        };
        for &(v, x) in list {
            if self.vars.get(v.0 as usize).is_none() {
                return Some("exit: undeclared var".into());
            }
            if !defined.get(x as usize).copied().unwrap_or(false) {
                return Some("exit: value not defined".into());
            }
        }
        None
    }

    pub fn inst_count(&self) -> usize {
        self.pre.len() + self.body.len() + self.post.len()
    }

    /// Check types and definition order; the builder's output must pass.
    pub fn validate(&self) -> Result<(), String> {
        let mut defined = vec![false; self.val_ty.len()];
        let mut after_pre = Vec::new();
        for (section, insts) in [
            ("pre", &self.pre),
            ("body", &self.body),
            ("post", &self.post),
        ] {
            // `post` runs after a guard exit from `body`, so it may use
            // values of `pre` but never a body temporary.
            if section == "body" {
                after_pre = defined.clone();
            }
            if section == "post" {
                defined = after_pre.clone();
            }
            for (i, inst) in insts.iter().enumerate() {
                let err = |m: &str| Err(format!("{section}[{i}] {inst:?}: {m}"));
                let ty = |v: Val| self.val_ty.get(v as usize).copied();
                let use_ok = |v: Val| defined.get(v as usize).copied().unwrap_or(false);
                match *inst {
                    Inst::Def(d, op) => {
                        let want = match self.val_ty.get(d as usize) {
                            Some(t) => *t,
                            None => return err("undeclared value"),
                        };
                        let got = match op {
                            Op::CI32(_) | Op::Ctx32(_) => Ty::I32,
                            Op::CF32(_) | Op::CtxF32(_) => Ty::F32,
                            Op::CI64(_) | Op::Ctx64(_) => Ty::I64,
                            Op::GetVar(v) => match self.vars.get(v.0 as usize) {
                                Some(t) => *t,
                                None => return err("undeclared var"),
                            },
                            Op::Un(u, a) => {
                                if !use_ok(a) {
                                    return err("operand not defined");
                                }
                                let (need, out) = match u {
                                    Un::FNeg | Un::FAbs => (Ty::F32, Ty::F32),
                                    Un::BitsToF => (Ty::I32, Ty::F32),
                                    Un::FToBits | Un::FToI => (Ty::F32, Ty::I32),
                                    Un::IToF => (Ty::I32, Ty::F32),
                                    Un::Zext => (Ty::I32, Ty::I64),
                                    Un::Not | Un::Clz => (Ty::I32, Ty::I32),
                                };
                                if ty(a) != Some(need) {
                                    return err("unary operand type");
                                }
                                out
                            }
                            Op::Bin(b, x, y) => {
                                if !use_ok(x) || !use_ok(y) {
                                    return err("operand not defined");
                                }
                                let (need, out) = match b {
                                    Bin::FAdd | Bin::FSub | Bin::FMul | Bin::FDiv => {
                                        (Ty::F32, Ty::F32)
                                    }
                                    Bin::FLt | Bin::FGe | Bin::FEq => (Ty::F32, Ty::I32),
                                    Bin::Add64 | Bin::Sub64 => (Ty::I64, Ty::I64),
                                    _ => (Ty::I32, Ty::I32),
                                };
                                if ty(x) != Some(need) || ty(y) != Some(need) {
                                    return err("binary operand type");
                                }
                                out
                            }
                            Op::Select(c, a, b) => {
                                if !use_ok(c) || !use_ok(a) || !use_ok(b) {
                                    return err("operand not defined");
                                }
                                if ty(c) != Some(Ty::I32) || ty(a) != ty(b) {
                                    return err("select types");
                                }
                                ty(a).unwrap()
                            }
                            Op::Load32 { base, off } => {
                                if !use_ok(base) || !use_ok(off) {
                                    return err("operand not defined");
                                }
                                if ty(base) != Some(Ty::I64) || ty(off) != Some(Ty::I32) {
                                    return err("load operand types");
                                }
                                Ty::I32
                            }
                        };
                        if got != want {
                            return err("result type");
                        }
                        defined[d as usize] = true;
                    }
                    Inst::Set(v, x) => {
                        if !use_ok(x) {
                            return err("operand not defined");
                        }
                        let int_or_float = |t: Option<Ty>| matches!(t, Some(Ty::I32 | Ty::F32));
                        let vt = self.vars.get(v.0 as usize).copied();
                        if vt != ty(x) && !(int_or_float(vt) && int_or_float(ty(x))) {
                            return err("var type");
                        }
                    }
                    Inst::Store32 { base, off, v } => {
                        if !use_ok(base) || !use_ok(off) || !use_ok(v) {
                            return err("operand not defined");
                        }
                        if ty(base) != Some(Ty::I64)
                            || ty(off) != Some(Ty::I32)
                            || ty(v) != Some(Ty::I32)
                        {
                            return err("store operand types");
                        }
                    }
                    Inst::StCtx32 { v, .. } => {
                        // The raw 32 bits of an i32 or f32 value.
                        if !use_ok(v) || !matches!(ty(v), Some(Ty::I32 | Ty::F32)) {
                            return err("ctx store operand");
                        }
                    }
                    Inst::Guard { ok, exit, .. } => {
                        if !use_ok(ok) || ty(ok) != Some(Ty::I32) {
                            return err("guard operand");
                        }
                        if let Some(e) = self.check_exit(exit, &defined) {
                            return err(&e);
                        }
                    }
                    Inst::GuardAny { a, b, exit, .. } => {
                        if !use_ok(a)
                            || !use_ok(b)
                            || ty(a) != Some(Ty::I32)
                            || ty(b) != Some(Ty::I32)
                        {
                            return err("guard operand");
                        }
                        if let Some(e) = self.check_exit(exit, &defined) {
                            return err(&e);
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

impl fmt::Display for Kernel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "vars {:?}", self.vars)?;
        for (name, insts) in [
            ("pre", &self.pre),
            ("body", &self.body),
            ("post", &self.post),
        ] {
            writeln!(f, "{name}:")?;
            for i in insts {
                writeln!(f, "  {i:?}")?;
            }
        }
        Ok(())
    }
}

/// A compiled kernel: `run(ctx)` executes it on a context buffer laid out
/// as above and returns 0 for normal completion, 1 for a guard exit.
pub trait CompiledKernel {
    /// # Safety
    /// `ctx` points to a buffer of at least `CTX_SIZE` bytes,
    /// 8-byte aligned, whose window pointers cover every access.
    unsafe fn run(&self, ctx: *mut u8) -> u32;
}

/// A kernel compiler (the reference interpreter, Cranelift, later wasm).
pub trait KernelBackend {
    fn name(&self) -> &'static str;
    fn compile(&mut self, k: &Kernel) -> Result<Box<dyn CompiledKernel>, String>;
}

// -- serialisation (the plug-in boundary) ----------------------------------------

const UNS: [Un; 9] = [
    Un::FNeg,
    Un::FAbs,
    Un::BitsToF,
    Un::FToBits,
    Un::FToI,
    Un::IToF,
    Un::Zext,
    Un::Not,
    Un::Clz,
];

const BINS: [Bin; 27] = [
    Bin::Add,
    Bin::Sub,
    Bin::Mul,
    Bin::And,
    Bin::Or,
    Bin::Xor,
    Bin::Shl,
    Bin::ShrU,
    Bin::ShrS,
    Bin::Eq,
    Bin::Ne,
    Bin::LtU,
    Bin::LeU,
    Bin::GtU,
    Bin::GeU,
    Bin::LtS,
    Bin::GtS,
    Bin::FLt,
    Bin::FGe,
    Bin::FEq,
    Bin::FAdd,
    Bin::FSub,
    Bin::FMul,
    Bin::Add64,
    Bin::Sub64,
    Bin::FDiv,
    Bin::MulHs,
];

fn un_code(u: Un) -> u32 {
    UNS.iter().position(|&x| x == u).unwrap() as u32
}

fn bin_code(b: Bin) -> u32 {
    BINS.iter().position(|&x| x == b).unwrap() as u32
}

fn ty_code(t: Ty) -> u32 {
    match t {
        Ty::I32 => 0,
        Ty::F32 => 1,
        Ty::I64 => 2,
    }
}

fn ty_of(c: u32) -> Result<Ty, String> {
    Ok(match c {
        0 => Ty::I32,
        1 => Ty::F32,
        2 => Ty::I64,
        _ => return Err(format!("bad type code {c}")),
    })
}

impl Kernel {
    /// The kernel as a flat word stream, for a backend in another binary.
    pub fn encode(&self) -> Vec<u32> {
        let mut w: Vec<u32> = vec![0x4b45_5231]; // "KER1"
        w.push(self.vars.len() as u32);
        w.extend(self.vars.iter().map(|&t| ty_code(t)));
        w.push(self.val_ty.len() as u32);
        w.extend(self.val_ty.iter().map(|&t| ty_code(t)));
        w.push(self.exits.len() as u32);
        for e in &self.exits {
            w.push(e.len() as u32);
            for &(v, x) in e {
                w.extend([v.0, x]);
            }
        }
        for insts in [&self.pre, &self.body, &self.post] {
            w.push(insts.len() as u32);
            for i in insts {
                match *i {
                    Inst::Def(d, op) => {
                        w.extend([0, d]);
                        match op {
                            Op::CI32(c) => w.extend([0, c]),
                            Op::CF32(c) => w.extend([1, c]),
                            Op::CI64(c) => w.extend([2, c as u32, (c >> 32) as u32]),
                            Op::GetVar(v) => w.extend([3, v.0]),
                            Op::Ctx32(o) => w.extend([4, o]),
                            Op::Ctx64(o) => w.extend([5, o]),
                            Op::CtxF32(o) => w.extend([6, o]),
                            Op::Un(u, a) => w.extend([7, un_code(u), a]),
                            Op::Bin(b, x, y) => w.extend([8, bin_code(b), x, y]),
                            Op::Select(c, a, b) => w.extend([9, c, a, b]),
                            Op::Load32 { base, off } => w.extend([10, base, off]),
                        }
                    }
                    Inst::Set(v, x) => w.extend([1, v.0, x]),
                    Inst::Store32 { base, off, v } => w.extend([2, base, off, v]),
                    Inst::StCtx32 { off, v } => w.extend([3, off, v]),
                    Inst::Guard { ok, k, exit } => w.extend([4, ok, k, exit]),
                    Inst::GuardAny { a, b, k, exit } => w.extend([5, a, b, k, exit]),
                }
            }
        }
        w
    }

    pub fn decode(w: &[u32]) -> Result<Kernel, String> {
        let mut at = 0usize;
        let mut next = || -> Result<u32, String> {
            let v = *w.get(at).ok_or("truncated kernel")?;
            at += 1;
            Ok(v)
        };
        if next()? != 0x4b45_5231 {
            return Err("not a kernel".into());
        }
        let mut k = Kernel::default();
        for _ in 0..next()? {
            k.vars.push(ty_of(next()?)?);
        }
        for _ in 0..next()? {
            k.val_ty.push(ty_of(next()?)?);
        }
        for _ in 0..next()? {
            let mut e = Vec::new();
            for _ in 0..next()? {
                let (v, x) = (next()?, next()?);
                e.push((Var(v), x));
            }
            k.exits.push(e);
        }
        for section in 0..3 {
            let mut insts = Vec::new();
            for _ in 0..next()? {
                let inst = match next()? {
                    0 => {
                        let d = next()?;
                        let op = match next()? {
                            0 => Op::CI32(next()?),
                            1 => Op::CF32(next()?),
                            2 => Op::CI64(next()? as u64 | (next()? as u64) << 32),
                            3 => Op::GetVar(Var(next()?)),
                            4 => Op::Ctx32(next()?),
                            5 => Op::Ctx64(next()?),
                            6 => Op::CtxF32(next()?),
                            7 => {
                                let u = *UNS.get(next()? as usize).ok_or("bad unary")?;
                                Op::Un(u, next()?)
                            }
                            8 => {
                                let b = *BINS.get(next()? as usize).ok_or("bad binary")?;
                                Op::Bin(b, next()?, next()?)
                            }
                            9 => Op::Select(next()?, next()?, next()?),
                            10 => Op::Load32 {
                                base: next()?,
                                off: next()?,
                            },
                            o => return Err(format!("bad op {o}")),
                        };
                        Inst::Def(d, op)
                    }
                    1 => Inst::Set(Var(next()?), next()?),
                    2 => Inst::Store32 {
                        base: next()?,
                        off: next()?,
                        v: next()?,
                    },
                    3 => Inst::StCtx32 {
                        off: next()?,
                        v: next()?,
                    },
                    4 => Inst::Guard {
                        ok: next()?,
                        k: next()?,
                        exit: next()?,
                    },
                    5 => Inst::GuardAny {
                        a: next()?,
                        b: next()?,
                        k: next()?,
                        exit: next()?,
                    },
                    t => return Err(format!("bad inst {t}")),
                };
                insts.push(inst);
            }
            match section {
                0 => k.pre = insts,
                1 => k.body = insts,
                _ => k.post = insts,
            }
        }
        k.validate()?;
        Ok(k)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Kernel {
        // x = ctx[iters]; guard x != 0; store the f32 sum of two window loads.
        let mut k = Kernel {
            vars: vec![Ty::I32, Ty::F32],
            val_ty: vec![
                Ty::I32,
                Ty::I64,
                Ty::I32,
                Ty::F32,
                Ty::F32,
                Ty::F32,
                Ty::I32,
            ],
            ..Kernel::default()
        };
        k.pre = vec![
            Inst::Def(1, Op::Ctx64(win_ptr(0))),
            Inst::Def(0, Op::CI32(0)),
            Inst::Set(Var(0), 0),
        ];
        k.body = vec![
            Inst::Def(2, Op::GetVar(Var(0))),
            Inst::Def(3, Op::Un(Un::BitsToF, 2)),
            Inst::Def(4, Op::Bin(Bin::FAdd, 3, 3)),
            Inst::Def(5, Op::Un(Un::FAbs, 4)),
            Inst::Def(6, Op::Bin(Bin::FLt, 5, 5)),
            Inst::Guard {
                ok: 6,
                k: 0,
                exit: 0,
            },
            Inst::Set(Var(1), 4),
            Inst::StCtx32 {
                off: CTX_FSRC,
                v: 4,
            },
        ];
        k.exits = vec![vec![(Var(0), 2)]];
        k
    }

    #[test]
    fn encode_roundtrip() {
        let k = sample();
        k.validate().unwrap();
        let w = k.encode();
        let back = Kernel::decode(&w).unwrap();
        assert_eq!(back.encode(), w);
        assert_eq!(back.body.len(), k.body.len());
    }

    #[test]
    fn validate_rejects_a_type_error() {
        let mut k = sample();
        // An f32 add of an i32 value.
        k.body[2] = Inst::Def(4, Op::Bin(Bin::FAdd, 2, 2));
        assert!(k.validate().is_err());
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(Kernel::decode(&[1, 2, 3]).is_err());
        let mut w = sample().encode();
        w.truncate(w.len() - 3);
        assert!(Kernel::decode(&w).is_err());
    }
}
