//! Cranelift backend for the SHARC fast tier's kernel IR
//! (`sharc_native::fast::ir`). The kernel is a function
//! `extern "C" fn(ctx: *mut u8) -> u32`; the IR's variables are Cranelift
//! variables (SSA-constructed, kept in registers across the loop), guards are
//! conditional branches to a shared exit that records the instruction index.
//!
//! Nothing about the engine's memory layout is baked into the code: the
//! kernel sees only its context buffer and the window pointers in it.

use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::{self, AbiParam, InstBuilder, MemFlagsData, types};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module, default_libcall_names};
use sharc_native::fast::ir::*;
use std::collections::HashMap;

static DUMPS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub struct ClBackend {
    opt: &'static str,
    verify: bool,
}

impl Default for ClBackend {
    fn default() -> Self {
        ClBackend {
            opt: "speed",
            verify: std::env::var_os("SHARC_FAST_CL_NOVERIFY").is_none(),
        }
    }
}

/// Make `SHARC_FAST_REGIONS` engines use this backend.
pub fn register() {
    sharc_native::fast::register_backend(|| Box::<ClBackend>::default());
}

struct ClKernel {
    module: Option<JITModule>,
    f: unsafe extern "C" fn(*mut u8) -> u32,
}

impl Drop for ClKernel {
    fn drop(&mut self) {
        if let Some(m) = self.module.take() {
            // SAFETY: the kernel is not running (it is only called through
            // `run`, which borrows self) and no pointer to it escapes.
            unsafe { m.free_memory() };
        }
    }
}

impl CompiledKernel for ClKernel {
    unsafe fn run(&self, ctx: *mut u8) -> u32 {
        unsafe { (self.f)(ctx) }
    }
}

fn cl_ty(t: Ty) -> ir::Type {
    match t {
        Ty::I32 => types::I32,
        Ty::F32 => types::F32,
        Ty::I64 => types::I64,
    }
}

fn mem_flags() -> MemFlagsData {
    MemFlagsData::new().with_notrap()
}

struct Emit<'a, 'b> {
    b: &'a mut FunctionBuilder<'b>,
    k: &'a Kernel,
    vars: Vec<Variable>,
    vals: Vec<Option<ir::Value>>,
    /// The i8 form of comparison results (and of and/or of them), so a guard
    /// branches on the flags without materialising a 0/1 word.
    bools: Vec<Option<ir::Value>>,
    ctx: ir::Value,
    post: ir::Block,
    fails: HashMap<(u32, u32), ir::Block>,
    /// The Cranelift block of each IR label, and whether the current block
    /// has been ended by a jump (so a label need not add a fall-through).
    labels: Vec<ir::Block>,
    terminated: bool,
}

impl Emit<'_, '_> {
    fn v(&mut self, x: Val) -> ir::Value {
        if let Some(v) = self.vals[x as usize] {
            return v;
        }
        let b = self.bools[x as usize].expect("value defined before use");
        let v = self.b.ins().uextend(types::I32, b);
        self.vals[x as usize] = Some(v);
        v
    }

    /// The i8 truth value of X.
    fn truth(&mut self, x: Val) -> ir::Value {
        if let Some(b) = self.bools[x as usize] {
            return b;
        }
        let v = self.v(x);
        let z = self.b.ins().iconst(types::I32, 0);
        self.b.ins().icmp(IntCC::NotEqual, v, z)
    }

    /// A value as variable VAR's type (reinterpreting i32 and f32).
    fn as_var_ty(&mut self, var: Var, x: Val) -> ir::Value {
        let want = self.k.vars[var.0 as usize];
        let have = self.k.val_ty[x as usize];
        let v = self.v(x);
        match (have, want) {
            (Ty::I32, Ty::F32) => self.b.ins().bitcast(types::F32, MemFlagsData::new(), v),
            (Ty::F32, Ty::I32) => self.b.ins().bitcast(types::I32, MemFlagsData::new(), v),
            _ => v,
        }
    }

    /// The block a failing guard of instruction K with fix-up list EXIT goes
    /// to: update the variables, record K, leave through `post`.
    fn fail_block(&mut self, k: u32, exit: u32) -> ir::Block {
        if let Some(b) = self.fails.get(&(k, exit)) {
            return *b;
        }
        let cur = self.b.current_block();
        let blk = self.b.create_block();
        // Rarely taken: laid out after the loop so the guards fall through.
        self.b.set_cold_block(blk);
        self.b.switch_to_block(blk);
        // A 0/1 word made from a comparison here lives in this block only:
        // the cache must not hand it to the code after the guard.
        let fresh: Vec<Val> = self.k.exits[exit as usize]
            .iter()
            .map(|&(_, x)| x)
            .filter(|&x| self.vals[x as usize].is_none())
            .collect();
        for &(var, x) in &self.k.exits[exit as usize] {
            let v = self.as_var_ty(var, x);
            self.b.def_var(self.vars[var.0 as usize], v);
        }
        for x in fresh {
            self.vals[x as usize] = None;
        }
        let kv = self.b.ins().iconst(types::I32, k as i64);
        self.b
            .ins()
            .store(mem_flags(), kv, self.ctx, CTX_EXIT_K as i32);
        let one = self.b.ins().iconst(types::I32, 1);
        self.b.ins().jump(self.post, &[one.into()]);
        if let Some(c) = cur {
            self.b.switch_to_block(c);
        }
        self.fails.insert((k, exit), blk);
        blk
    }

    fn insts(&mut self, insts: &[Inst]) {
        for inst in insts {
            match *inst {
                Inst::Def(d, op) => {
                    if let Op::Bin(bin, x, y) = op
                        && let Some(c) = self.compare(bin, x, y)
                    {
                        self.bools[d as usize] = Some(c);
                        continue;
                    }
                    let r = self.op(op);
                    self.vals[d as usize] = Some(r);
                }
                Inst::Set(var, x) => {
                    let x = self.as_var_ty(var, x);
                    self.b.def_var(self.vars[var.0 as usize], x);
                }
                Inst::Store32 { base, off, v } => {
                    let (base, off, v) = (self.v(base), self.v(off), self.v(v));
                    let o = self.b.ins().uextend(types::I64, off);
                    let a = self.b.ins().iadd(base, o);
                    self.b.ins().store(mem_flags(), v, a, 0);
                }
                Inst::StCtx32 { off, v } => {
                    let v = self.v(v);
                    self.b.ins().store(mem_flags(), v, self.ctx, off as i32);
                }
                Inst::Guard { ok, k, exit } => {
                    let ok = self.truth(ok);
                    let cont = self.b.create_block();
                    let fail = self.fail_block(k, exit);
                    self.b.ins().brif(ok, cont, &[], fail, &[]);
                    self.b.switch_to_block(cont);
                }
                Inst::Label(l) => {
                    let blk = self.labels[l as usize];
                    if !self.terminated {
                        self.b.ins().jump(blk, &[]);
                    }
                    self.b.switch_to_block(blk);
                    self.terminated = false;
                }
                Inst::Jump(l) => {
                    let blk = self.labels[l as usize];
                    self.b.ins().jump(blk, &[]);
                    self.terminated = true;
                }
                Inst::BrIf { c, target } => {
                    let c = self.truth(c);
                    let cont = self.b.create_block();
                    let blk = self.labels[target as usize];
                    self.b.ins().brif(c, blk, &[], cont, &[]);
                    self.b.switch_to_block(cont);
                }
                Inst::Leave { k, exit } => {
                    let fail = self.fail_block(k, exit);
                    self.b.ins().jump(fail, &[]);
                    self.terminated = true;
                }
                Inst::GuardAny { a, b, k, exit } => {
                    let (a, b) = (self.truth(a), self.truth(b));
                    let cont = self.b.create_block();
                    let second = self.b.create_block();
                    self.b.set_cold_block(second);
                    let fail = self.fail_block(k, exit);
                    self.b.ins().brif(a, cont, &[], second, &[]);
                    self.b.switch_to_block(second);
                    self.b.ins().brif(b, cont, &[], fail, &[]);
                    self.b.switch_to_block(cont);
                }
            }
        }
    }

    /// A comparison (or and/or of comparisons) as an i8 truth value.
    fn compare(&mut self, bin: Bin, x: Val, y: Val) -> Option<ir::Value> {
        let int = |cc: IntCC| Some(cc);
        let cc = match bin {
            Bin::Eq => int(IntCC::Equal),
            Bin::Ne => int(IntCC::NotEqual),
            Bin::LtU => int(IntCC::UnsignedLessThan),
            Bin::LeU => int(IntCC::UnsignedLessThanOrEqual),
            Bin::GtU => int(IntCC::UnsignedGreaterThan),
            Bin::GeU => int(IntCC::UnsignedGreaterThanOrEqual),
            Bin::LtS => int(IntCC::SignedLessThan),
            Bin::GtS => int(IntCC::SignedGreaterThan),
            _ => None,
        };
        if let Some(cc) = cc {
            let (x, y) = (self.v(x), self.v(y));
            return Some(self.b.ins().icmp(cc, x, y));
        }
        let fc = match bin {
            Bin::FLt => Some(FloatCC::LessThan),
            Bin::FGe => Some(FloatCC::GreaterThanOrEqual),
            Bin::FEq => Some(FloatCC::Equal),
            _ => None,
        };
        if let Some(fc) = fc {
            let (x, y) = (self.v(x), self.v(y));
            return Some(self.b.ins().fcmp(fc, x, y));
        }
        if matches!(bin, Bin::And | Bin::Or)
            && self.bools[x as usize].is_some()
            && self.bools[y as usize].is_some()
        {
            let (x, y) = (self.truth(x), self.truth(y));
            return Some(if bin == Bin::And {
                self.b.ins().band(x, y)
            } else {
                self.b.ins().bor(x, y)
            });
        }
        None
    }

    fn op(&mut self, op: Op) -> ir::Value {
        match op {
            Op::CI32(c) => self.b.ins().iconst(types::I32, c as i32 as i64),
            Op::CF32(c) => self.b.ins().f32const(f32::from_bits(c)),
            Op::CI64(c) => self.b.ins().iconst(types::I64, c as i64),
            Op::GetVar(var) => self.b.use_var(self.vars[var.0 as usize]),
            Op::Ctx32(off) => self
                .b
                .ins()
                .load(types::I32, mem_flags(), self.ctx, off as i32),
            Op::Ctx64(off) => self
                .b
                .ins()
                .load(types::I64, mem_flags(), self.ctx, off as i32),
            Op::CtxF32(off) => self
                .b
                .ins()
                .load(types::F32, mem_flags(), self.ctx, off as i32),
            Op::Un(u, x) => {
                let x = self.v(x);
                match u {
                    Un::FNeg => self.b.ins().fneg(x),
                    Un::FAbs => self.b.ins().fabs(x),
                    Un::BitsToF => self.b.ins().bitcast(types::F32, MemFlagsData::new(), x),
                    Un::FToBits => self.b.ins().bitcast(types::I32, MemFlagsData::new(), x),
                    Un::FToI => self.b.ins().fcvt_to_sint_sat(types::I32, x),
                    Un::IToF => self.b.ins().fcvt_from_sint(types::F32, x),
                    Un::Zext => self.b.ins().uextend(types::I64, x),
                    Un::Not => self.b.ins().bnot(x),
                    Un::Clz => self.b.ins().clz(x),
                }
            }
            Op::Bin(bin, x, y) => {
                let (x, y) = (self.v(x), self.v(y));
                match bin {
                    Bin::Add | Bin::Add64 => self.b.ins().iadd(x, y),
                    Bin::Sub | Bin::Sub64 => self.b.ins().isub(x, y),
                    Bin::Mul => self.b.ins().imul(x, y),
                    Bin::And => self.b.ins().band(x, y),
                    Bin::Or => self.b.ins().bor(x, y),
                    Bin::Xor => self.b.ins().bxor(x, y),
                    Bin::Shl => self.b.ins().ishl(x, y),
                    Bin::ShrU => self.b.ins().ushr(x, y),
                    Bin::ShrS => self.b.ins().sshr(x, y),
                    Bin::Eq
                    | Bin::Ne
                    | Bin::LtU
                    | Bin::LeU
                    | Bin::GtU
                    | Bin::GeU
                    | Bin::LtS
                    | Bin::GtS
                    | Bin::FLt
                    | Bin::FGe
                    | Bin::FEq => unreachable!("comparisons are handled by `compare`"),
                    Bin::FAdd => self.b.ins().fadd(x, y),
                    Bin::FSub => self.b.ins().fsub(x, y),
                    Bin::FMul => self.b.ins().fmul(x, y),
                    Bin::FDiv => self.b.ins().fdiv(x, y),
                    Bin::MulHs => self.b.ins().smulhi(x, y),
                }
            }
            Op::Select(c, a, b) => {
                let (c, a, b) = (self.v(c), self.v(a), self.v(b));
                self.b.ins().select(c, a, b)
            }
            Op::Load32 { base, off } => {
                let (base, off) = (self.v(base), self.v(off));
                let o = self.b.ins().uextend(types::I64, off);
                let a = self.b.ins().iadd(base, o);
                self.b.ins().load(types::I32, mem_flags(), a, 0)
            }
        }
    }
}

fn build(
    k: &Kernel,
    func: &mut ir::Function,
    fctx: &mut FunctionBuilderContext,
    tc: cranelift_codegen::isa::TargetFrontendConfig,
) {
    let mut b = FunctionBuilder::new(func, fctx);
    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.switch_to_block(entry);
    let ctx = b.block_params(entry)[0];
    let vars: Vec<Variable> = k.vars.iter().map(|t| b.declare_var(cl_ty(*t))).collect();
    let tvar = b.declare_var(types::I32);
    let post = b.create_block();
    let code = b.append_block_param(post, types::I32);
    let mut e = Emit {
        b: &mut b,
        k,
        vars,
        vals: vec![None; k.val_ty.len()],
        bools: vec![None; k.val_ty.len()],
        ctx,
        post,
        fails: HashMap::new(),
        labels: Vec::new(),
        terminated: false,
    };
    let _ = e.k;
    e.labels = (0..k.nlabels).map(|_| e.b.create_block()).collect();
    e.insts(&k.pre);
    if k.cfg {
        // One pass through the body; it leaves only through `post`.
        e.insts(&k.body);
        e.b.switch_to_block(post);
        let z = e.b.ins().iconst(types::I32, 0);
        e.b.ins().store(mem_flags(), z, ctx, CTX_DONE as i32);
        e.insts(&k.post);
        e.b.ins().return_(&[code]);
        e.b.seal_all_blocks();
        b.finalize(tc);
        return;
    }
    let iters =
        e.b.ins()
            .load(types::I32, mem_flags(), ctx, CTX_ITERS as i32);
    let zero = e.b.ins().iconst(types::I32, 0);
    e.b.def_var(tvar, zero);
    let header = e.b.create_block();
    let body = e.b.create_block();
    let done = e.b.create_block();
    e.b.ins().jump(header, &[]);
    e.b.switch_to_block(header);
    let t = e.b.use_var(tvar);
    let more = e.b.ins().icmp(IntCC::UnsignedLessThan, t, iters);
    e.b.ins().brif(more, body, &[], done, &[]);
    e.b.switch_to_block(body);
    e.insts(&k.body);
    let t = e.b.use_var(tvar);
    let one = e.b.ins().iconst(types::I32, 1);
    let t1 = e.b.ins().iadd(t, one);
    e.b.def_var(tvar, t1);
    e.b.ins().jump(header, &[]);
    e.b.switch_to_block(done);
    let z = e.b.ins().iconst(types::I32, 0);
    e.b.ins().jump(post, &[z.into()]);
    e.b.switch_to_block(post);
    let t = e.b.use_var(tvar);
    e.b.ins().store(mem_flags(), t, ctx, CTX_DONE as i32);
    e.insts(&k.post);
    e.b.ins().return_(&[code]);
    e.b.seal_all_blocks();
    b.finalize(tc);
}

impl KernelBackend for ClBackend {
    fn name(&self) -> &'static str {
        "cranelift"
    }

    fn compile(&mut self, k: &Kernel) -> Result<Box<dyn CompiledKernel>, String> {
        k.validate()?;
        let mut flags = settings::builder();
        flags
            .set("use_colocated_libcalls", "false")
            .map_err(|e| e.to_string())?;
        flags.set("is_pic", "false").map_err(|e| e.to_string())?;
        flags
            .set("opt_level", self.opt)
            .map_err(|e| e.to_string())?;
        flags
            .set(
                "enable_verifier",
                if self.verify { "true" } else { "false" },
            )
            .map_err(|e| e.to_string())?;
        let isa = cranelift_native::builder()
            .map_err(|e| e.to_string())?
            .finish(settings::Flags::new(flags))
            .map_err(|e| e.to_string())?;
        let mut module = JITModule::new(JITBuilder::with_isa(isa, default_libcall_names()));
        let mut sig = module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(types::I32));
        let id = module
            .declare_function("kernel", Linkage::Local, &sig)
            .map_err(|e| e.to_string())?;
        let mut cctx = module.make_context();
        cctx.func.signature = sig;
        cctx.func.name = ir::UserFuncName::user(0, id.as_u32());
        let mut fctx = FunctionBuilderContext::new();
        build(k, &mut cctx.func, &mut fctx, module.target_config());
        let dump = std::env::var_os("SHARC_FAST_CL_DISASM");
        cctx.set_disasm(dump.is_some());
        module
            .define_function(id, &mut cctx)
            .map_err(|e| format!("{e:?}"))?;
        if let Some(dir) = &dump
            && let Some(text) = cctx.compiled_code().and_then(|c| c.vcode.clone())
        {
            let n = DUMPS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::path::Path::new(dir).join(format!("kernel-{n}.txt"));
            let _ = std::fs::write(path, text);
        }
        let code_size = cctx
            .compiled_code()
            .map(|c| c.code_buffer().len())
            .unwrap_or(0);
        if std::env::var_os("SHARC_FAST_LOG").is_some() {
            eprintln!(
                "cranelift: kernel of {} ops compiled to {code_size} bytes",
                k.inst_count()
            );
        }
        module.clear_context(&mut cctx);
        module.finalize_definitions().map_err(|e| e.to_string())?;
        let p = module.get_finalized_function(id);
        // SAFETY: the function was compiled with this exact signature.
        let f =
            unsafe { std::mem::transmute::<*const u8, unsafe extern "C" fn(*mut u8) -> u32>(p) };
        Ok(Box::new(ClKernel {
            module: Some(module),
            f,
        }))
    }
}

// -- the shared-library interface (sharc_native::fast::plugin) ---------------------

/// # Safety
/// `words` points to `n` words; `err` to `cap` writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sharc_fast_compile(
    words: *const u32,
    n: usize,
    err: *mut std::ffi::c_char,
    cap: usize,
) -> *mut std::ffi::c_void {
    let words = unsafe { std::slice::from_raw_parts(words, n) };
    let result = Kernel::decode(words).and_then(|k| ClBackend::default().compile(&k));
    match result {
        Ok(k) => Box::into_raw(Box::new(k)) as *mut std::ffi::c_void,
        Err(e) => {
            let bytes = e.as_bytes();
            let m = bytes.len().min(cap.saturating_sub(1));
            if cap > 0 {
                unsafe {
                    std::ptr::copy_nonoverlapping(bytes.as_ptr(), err as *mut u8, m);
                    *err.add(m) = 0;
                }
            }
            std::ptr::null_mut()
        }
    }
}

/// # Safety
/// `h` came from `sharc_fast_compile`; `ctx` is a context buffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sharc_fast_run(h: *mut std::ffi::c_void, ctx: *mut u8) -> u32 {
    let k = unsafe { &*(h as *const Box<dyn CompiledKernel>) };
    unsafe { k.run(ctx) }
}

/// # Safety
/// `h` came from `sharc_fast_compile` and is not used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sharc_fast_free(h: *mut std::ffi::c_void) {
    drop(unsafe { Box::from_raw(h as *mut Box<dyn CompiledKernel>) });
}
