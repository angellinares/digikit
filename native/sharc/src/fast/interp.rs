//! Reference interpreter for the kernel IR: the oracle that tells a lowering
//! bug from a backend bug, and the fallback backend when no compiler is
//! linked. It is slow (a value table and a match per instruction) but
//! exact, and it checks that every memory access lies inside its window
//! when given the window extents.

use super::ir::*;

#[derive(Clone, Copy, Default)]
struct Slot(u64);

pub struct InterpKernel {
    k: Kernel,
    /// Index of each label in `body`.
    labels: Vec<usize>,
}

pub struct InterpBackend;

impl KernelBackend for InterpBackend {
    fn name(&self) -> &'static str {
        "ir-interp"
    }
    fn compile(&mut self, k: &Kernel) -> Result<Box<dyn CompiledKernel>, String> {
        k.validate()?;
        let mut labels = vec![0usize; k.nlabels as usize];
        for (i, inst) in k.body.iter().enumerate() {
            if let Inst::Label(l) = *inst {
                labels[l as usize] = i;
            }
        }
        Ok(Box::new(InterpKernel {
            k: k.clone(),
            labels,
        }))
    }
}

#[inline]
fn f(x: u64) -> f32 {
    f32::from_bits(x as u32)
}

#[inline]
fn fb(x: f32) -> u64 {
    x.to_bits() as u64
}

impl InterpKernel {
    /// A failed guard: apply the exit's variable fix-ups, record the index.
    fn leave(&self, exit: u32, k: u32, vals: &[Slot], vars: &mut [Slot], ctx: *mut u8) -> u32 {
        for &(v, x) in &self.k.exits[exit as usize] {
            vars[v.0 as usize] = vals[x as usize];
        }
        unsafe { (ctx.add(CTX_EXIT_K as usize) as *mut u32).write_unaligned(k) };
        1
    }

    /// Test mode (`CTX_WIN_CHECK`): the access of 4 bytes at OFF through the
    /// window pointer BASE must lie inside that window.
    fn check_window(&self, ctx: *mut u8, base: u64, off: u32) {
        let rd32 = |o: u32| unsafe { (ctx.add(o as usize) as *const u32).read_unaligned() };
        if rd32(CTX_WIN_CHECK) == 0 {
            return;
        }
        for w in 0..MAX_WIN {
            let slot = unsafe { (ctx.add(win_ptr(w) as usize) as *const u64).read_unaligned() };
            if slot != base {
                continue;
            }
            let (lo, hi) = (rd32(CTX_WIN_EXT + 8 * w), rd32(CTX_WIN_EXT + 8 * w + 4));
            if off >= lo && off as u64 + 4 <= hi as u64 {
                return;
            }
        }
        panic!("window access at {off:#x} outside every window with this base");
    }

    fn exec(
        &self,
        insts: &[Inst],
        vals: &mut [Slot],
        vars: &mut [Slot],
        ctx: *mut u8,
    ) -> Option<u32> {
        let rd32 = |off: u32| unsafe { (ctx.add(off as usize) as *const u32).read_unaligned() };
        let rd64 = |off: u32| unsafe { (ctx.add(off as usize) as *const u64).read_unaligned() };
        let mut pc = 0usize;
        while pc < insts.len() {
            let inst = &insts[pc];
            pc += 1;
            match *inst {
                Inst::Label(_) => {}
                Inst::Jump(l) => pc = self.labels[l as usize],
                Inst::BrIf { c, target } => {
                    if vals[c as usize].0 as u32 != 0 {
                        pc = self.labels[target as usize];
                    }
                }
                Inst::Leave { k, exit } => {
                    return Some(self.leave(exit, k, vals, vars, ctx));
                }
                Inst::Def(d, op) => {
                    let v = |x: Val| vals[x as usize].0;
                    let r: u64 = match op {
                        Op::CI32(c) => c as u64,
                        Op::CF32(c) => c as u64,
                        Op::CI64(c) => c,
                        Op::GetVar(x) => vars[x.0 as usize].0,
                        Op::Ctx32(o) => rd32(o) as u64,
                        Op::Ctx64(o) => rd64(o),
                        Op::CtxF32(o) => rd32(o) as u64,
                        Op::Un(u, a) => {
                            let a = v(a);
                            match u {
                                Un::FNeg => (a as u32 ^ 0x8000_0000) as u64,
                                Un::FAbs => (a as u32 & 0x7fff_ffff) as u64,
                                Un::BitsToF | Un::FToBits => a,
                                Un::FToI => (f(a) as i32) as u32 as u64,
                                Un::IToF => fb(a as u32 as i32 as f32),
                                Un::Zext => a as u32 as u64,
                                Un::Not => (!(a as u32)) as u64,
                                Un::Clz => (a as u32).leading_zeros() as u64,
                            }
                        }
                        Op::Bin(b, x, y) => {
                            let (x, y) = (v(x), v(y));
                            let (xu, yu) = (x as u32, y as u32);
                            match b {
                                Bin::Add => xu.wrapping_add(yu) as u64,
                                Bin::Sub => xu.wrapping_sub(yu) as u64,
                                Bin::Mul => xu.wrapping_mul(yu) as u64,
                                Bin::And => (xu & yu) as u64,
                                Bin::Or => (xu | yu) as u64,
                                Bin::Xor => (xu ^ yu) as u64,
                                Bin::Shl => xu.wrapping_shl(yu & 31) as u64,
                                Bin::ShrU => xu.wrapping_shr(yu & 31) as u64,
                                Bin::ShrS => ((xu as i32).wrapping_shr(yu & 31)) as u32 as u64,
                                Bin::Eq => (xu == yu) as u64,
                                Bin::Ne => (xu != yu) as u64,
                                Bin::LtU => (xu < yu) as u64,
                                Bin::LeU => (xu <= yu) as u64,
                                Bin::GtU => (xu > yu) as u64,
                                Bin::GeU => (xu >= yu) as u64,
                                Bin::LtS => ((xu as i32) < (yu as i32)) as u64,
                                Bin::GtS => ((xu as i32) > (yu as i32)) as u64,
                                Bin::FLt => (f(x) < f(y)) as u64,
                                Bin::FGe => (f(x) >= f(y)) as u64,
                                Bin::FEq => (f(x) == f(y)) as u64,
                                Bin::FAdd => fb(f(x) + f(y)),
                                Bin::FSub => fb(f(x) - f(y)),
                                Bin::FMul => fb(f(x) * f(y)),
                                Bin::Add64 => x.wrapping_add(y),
                                Bin::Sub64 => x.wrapping_sub(y),
                                Bin::FDiv => fb(f(x) / f(y)),
                                Bin::MulHs => {
                                    (((xu as i32 as i64) * (yu as i32 as i64)) >> 32) as u32 as u64
                                }
                            }
                        }
                        Op::Select(c, a, b) => {
                            if v(c) as u32 != 0 {
                                v(a)
                            } else {
                                v(b)
                            }
                        }
                        Op::Load32 { base, off } => {
                            self.check_window(ctx, v(base), v(off) as u32);
                            let p = (v(base) as usize).wrapping_add(v(off) as u32 as usize);
                            unsafe { (p as *const u32).read_unaligned() as u64 }
                        }
                    };
                    vals[d as usize] = Slot(r);
                }
                Inst::Set(x, a) => vars[x.0 as usize] = vals[a as usize],
                Inst::Store32 { base, off, v } => {
                    self.check_window(ctx, vals[base as usize].0, vals[off as usize].0 as u32);
                    let p = (vals[base as usize].0 as usize)
                        .wrapping_add(vals[off as usize].0 as u32 as usize);
                    unsafe { (p as *mut u32).write_unaligned(vals[v as usize].0 as u32) }
                }
                Inst::StCtx32 { off, v } => unsafe {
                    (ctx.add(off as usize) as *mut u32).write_unaligned(vals[v as usize].0 as u32)
                },
                Inst::Guard { ok, k, exit } => {
                    if vals[ok as usize].0 as u32 == 0 {
                        return Some(self.leave(exit, k, vals, vars, ctx));
                    }
                }
                Inst::GuardAny { a, b, k, exit } => {
                    if vals[a as usize].0 as u32 == 0 && vals[b as usize].0 as u32 == 0 {
                        return Some(self.leave(exit, k, vals, vars, ctx));
                    }
                }
            }
        }
        None
    }
}

impl CompiledKernel for InterpKernel {
    unsafe fn run(&self, ctx: *mut u8) -> u32 {
        let k = &self.k;
        let mut vals = vec![Slot::default(); k.val_ty.len()];
        let mut vars = vec![Slot::default(); k.vars.len()];
        let iters = unsafe { (ctx.add(CTX_ITERS as usize) as *const u32).read_unaligned() };
        let mut code = 0;
        let mut done = iters;
        self.exec(&k.pre, &mut vals, &mut vars, ctx);
        if k.cfg {
            // Runs once and leaves through a guard or a `Leave`.
            code = self
                .exec(&k.body, &mut vals, &mut vars, ctx)
                .expect("a cfg body ends in a terminator");
            done = 0;
        } else {
            'outer: for t in 0..iters {
                if let Some(c) = self.exec(&k.body, &mut vals, &mut vars, ctx) {
                    code = c;
                    done = t;
                    break 'outer;
                }
            }
        }
        unsafe { (ctx.add(CTX_DONE as usize) as *mut u32).write_unaligned(done) };
        self.exec(&k.post, &mut vals, &mut vars, ctx);
        code
    }
}
