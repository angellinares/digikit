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
            // Default `none` (compile -27%, run +1%, same PCM); SHARC_FAST_CL_OPT=speed|speed_and_size restores/changes it.
            opt: match std::env::var("SHARC_FAST_CL_OPT").as_deref() {
                Ok("speed") => "speed",
                Ok("speed_and_size") => "speed_and_size",
                _ => "none",
            },
            verify: std::env::var_os("SHARC_FAST_CL_NOVERIFY").is_none(),
        }
    }
}

/// Make `SHARC_FAST_REGIONS` engines use this backend.
pub fn register() {
    sharc_native::fast::register_backend(|| Box::<ClBackend>::default());
}

struct ClKernel {
    entry_checked: bool,
    module: Option<JITModule>,
    f: unsafe extern "C" fn(*mut u8, *mut u8) -> u32,
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
    unsafe fn run(&self, ctx: *mut u8, st: *mut u8) -> u32 {
        unsafe { (self.f)(ctx, st) }
    }
    fn checks_entry(&self) -> bool {
        self.entry_checked
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
    st: ir::Value,
    post: ir::Block,
    fails: HashMap<(u32, u32), ir::Block>,
    /// The Cranelift block of each IR label, and whether the current block
    /// has been ended by a jump (so a label need not add a fall-through).
    labels: Vec<ir::Block>,
    terminated: bool,
    call_conv: cranelift_codegen::isa::CallConv,
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
    /// to. A new block must be filled only after the caller terminates its
    /// current block; FunctionBuilder cannot switch away from an open block.
    fn fail_block(&mut self, k: u32, exit: u32) -> (ir::Block, bool) {
        if let Some(b) = self.fails.get(&(k, exit)) {
            return (*b, false);
        }
        let blk = self.b.create_block();
        self.b.set_cold_block(blk);
        self.fails.insert((k, exit), blk);
        (blk, true)
    }

    /// Update the variables, record K and leave through `post`. The caller's
    /// branch already targets BLK, and its continuation is selected afterwards.
    fn fill_fail_block(&mut self, blk: ir::Block, k: u32, exit: u32) {
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
                Inst::StoreN { base, off, v, bytes } => {
                    let (base, off, v) = (self.v(base), self.v(off), self.v(v));
                    let o = self.b.ins().uextend(types::I64, off);
                    let a = self.b.ins().iadd(base, o);
                    if bytes == 1 {
                        self.b.ins().istore8(mem_flags(), v, a, 0);
                    } else {
                        self.b.ins().istore16(mem_flags(), v, a, 0);
                    }
                }
                Inst::StCtx32 { off, v } => {
                    let v = self.v(v);
                    self.b.ins().store(mem_flags(), v, self.ctx, off as i32);
                }
                Inst::StSt32 { off, v } => {
                    let v = self.v(v);
                    self.b.ins().store(mem_flags(), v, self.st, off as i32);
                }
                Inst::Guard { ok, k, exit } => {
                    let ok = self.truth(ok);
                    let cont = self.b.create_block();
                    let (fail, fresh) = self.fail_block(k, exit);
                    self.b.ins().brif(ok, cont, &[], fail, &[]);
                    if fresh {
                        self.fill_fail_block(fail, k, exit);
                    }
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
                    let (fail, fresh) = self.fail_block(k, exit);
                    self.b.ins().jump(fail, &[]);
                    if fresh {
                        self.fill_fail_block(fail, k, exit);
                    }
                    self.terminated = true;
                }
                Inst::GuardAny { a, b, k, exit } => {
                    let (a, b) = (self.truth(a), self.truth(b));
                    let cont = self.b.create_block();
                    let second = self.b.create_block();
                    self.b.set_cold_block(second);
                    let (fail, fresh) = self.fail_block(k, exit);
                    self.b.ins().brif(a, cont, &[], second, &[]);
                    self.b.switch_to_block(second);
                    self.b.ins().brif(b, cont, &[], fail, &[]);
                    if fresh {
                        self.fill_fail_block(fail, k, exit);
                    }
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
            Op::St32(off) => self
                .b
                .ins()
                .load(types::I32, mem_flags(), self.st, off as i32),
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
            Op::LoadN { base, off, bytes } => {
                let (base, off) = (self.v(base), self.v(off));
                let o = self.b.ins().uextend(types::I64, off);
                let a = self.b.ins().iadd(base, o);
                if bytes == 1 {
                    self.b.ins().uload8(types::I32, mem_flags(), a, 0)
                } else {
                    self.b.ins().uload16(types::I32, mem_flags(), a, 0)
                }
            }
            Op::Load32 { base, off } => {
                let (base, off) = (self.v(base), self.v(off));
                let o = self.b.ins().uextend(types::I64, off);
                let a = self.b.ins().iadd(base, o);
                self.b.ins().load(types::I32, mem_flags(), a, 0)
            }
            Op::Load64 { base, off } => {
                let (base, off) = (self.v(base), self.v(off));
                let o = self.b.ins().uextend(types::I64, off);
                let a = self.b.ins().iadd(base, o);
                self.b.ins().load(types::I64, mem_flags(), a, 0)
            }
            Op::Helper(rec) => {
                let mut sig = ir::Signature::new(self.call_conv);
                sig.params.push(AbiParam::new(types::I64));
                sig.params.push(AbiParam::new(types::I64));
                sig.returns.push(AbiParam::new(types::I32));
                let sig = self.b.import_signature(sig);
                let f = self.b.ins().iconst(
                    types::I64,
                    sharc_native::fast::helper::helper_exec as *const () as usize as i64,
                );
                let r = self.b.ins().iconst(types::I64, rec as i64);
                let call = self.b.ins().call_indirect(sig, f, &[self.st, r]);
                self.b.inst_results(call)[0]
            }
        }
    }
}

fn build(
    k: &Kernel,
    entry_chk: &EntryChecks,
    func: &mut ir::Function,
    fctx: &mut FunctionBuilderContext,
    tc: cranelift_codegen::isa::TargetFrontendConfig,
) {
    let mut b = FunctionBuilder::new(func, fctx);
    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.switch_to_block(entry);
    let ctx = b.block_params(entry)[0];
    let st = b.block_params(entry)[1];
    if !entry_chk.known_m.is_empty() || !entry_chk.words.is_empty() {
        // Lean entry contract: every register condition, as straight-line
        // code with constant offsets, before the kernel touches anything.
        let mut known = b.ins().iconst(types::I32, -1);
        for &off in &entry_chk.known_m {
            let m = b.ins().load(types::I32, mem_flags(), st, off as i32);
            known = b.ins().band(known, m);
        }
        let mut bad = b.ins().icmp_imm_s(IntCC::NotEqual, known, -1);
        for &(off, mask, want) in &entry_chk.words {
            let w = b.ins().load(types::I64, mem_flags(), st, off as i32);
            let mk = b.ins().iconst(types::I64, mask as i64);
            let wt = b.ins().iconst(types::I64, want as i64);
            let x = b.ins().band(w, mk);
            let d = b.ins().icmp(IntCC::NotEqual, x, wt);
            bad = b.ins().bor(bad, d);
        }
        let fail = b.create_block();
        let go = b.create_block();
        b.ins().brif(bad, fail, &[], go, &[]);
        b.switch_to_block(fail);
        let two = b.ins().iconst(types::I32, RUN_ENTRY_FAILED as i64);
        b.ins().return_(&[two]);
        b.switch_to_block(go);
    }
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
        st,
        post,
        fails: HashMap::new(),
        labels: Vec::new(),
        terminated: false,
        call_conv: tc.default_call_conv,
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
        self.compile_entry(k, &EntryChecks::default())
    }

    fn compile_checked(
        &mut self,
        k: &Kernel,
        entry: &EntryChecks,
    ) -> Result<Box<dyn CompiledKernel>, String> {
        self.compile_entry(k, entry)
    }
}

/// Machine code of one kernel, position independent (no relocations: the
/// helper entry point and its record are immediates), so it can be stored and
/// loaded into another process by `load_blob` on the same target and
/// Cranelift flags. Measurement and cache building block; the engine does not
/// use it yet (docs/design/fast-compile-offload.md).
pub struct CodeBlob {
    pub bytes: Vec<u8>,
    pub align: u64,
    pub entry_checked: bool,
    /// Relocations the code needs (0 for every kernel measured).
    pub relocs: usize,
}

/// Load a blob into fresh executable memory.
pub fn load_blob(b: &CodeBlob) -> Result<Box<dyn CompiledKernel>, String> {
    if b.relocs != 0 {
        return Err("blob needs relocations".into());
    }
    let isa = ClBackend::default().isa()?;
    let mut module = JITModule::new(JITBuilder::with_isa(isa, default_libcall_names()));
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(types::I64));
    sig.returns.push(AbiParam::new(types::I32));
    let id = module
        .declare_function("kernel", Linkage::Local, &sig)
        .map_err(|e| e.to_string())?;
    module
        .define_function_bytes(id, b.align, &b.bytes, &[])
        .map_err(|e| e.to_string())?;
    module.finalize_definitions().map_err(|e| e.to_string())?;
    let p = module.get_finalized_function(id);
    // SAFETY: the bytes are the code of a function of this signature.
    let f = unsafe {
        std::mem::transmute::<*const u8, unsafe extern "C" fn(*mut u8, *mut u8) -> u32>(p)
    };
    Ok(Box::new(ClKernel {
        entry_checked: b.entry_checked,
        module: Some(module),
        f,
    }))
}

impl ClBackend {
    fn isa(&self) -> Result<std::sync::Arc<dyn cranelift_codegen::isa::TargetIsa>, String> {
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
        cranelift_native::builder()
            .map_err(|e| e.to_string())?
            .finish(settings::Flags::new(flags))
            .map_err(|e| e.to_string())
    }

    /// Compile to machine code without loading it.
    pub fn compile_blob(&mut self, k: &Kernel, entry: &EntryChecks) -> Result<CodeBlob, String> {
        k.validate()?;
        let isa = self.isa()?;
        let mut sig = ir::Signature::new(isa.default_call_conv());
        sig.params.push(AbiParam::new(types::I64));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(types::I32));
        let mut cctx = cranelift_codegen::Context::new();
        cctx.func.signature = sig;
        cctx.func.name = ir::UserFuncName::user(0, 0);
        let mut fctx = FunctionBuilderContext::new();
        build(k, entry, &mut cctx.func, &mut fctx, isa.frontend_config());
        let code = cctx
            .compile(&*isa, &mut cranelift_codegen::control::ControlPlane::default())
            .map_err(|e| format!("{:?}", e.inner))?;
        Ok(CodeBlob {
            bytes: code.code_buffer().to_vec(),
            align: isa.function_alignment().minimum as u64,
            entry_checked: !entry.known_m.is_empty() || !entry.words.is_empty(),
            relocs: code.buffer.relocs().len(),
        })
    }

    fn compile_entry(
        &mut self,
        k: &Kernel,
        entry: &EntryChecks,
    ) -> Result<Box<dyn CompiledKernel>, String> {
        k.validate()?;
        let isa = self.isa()?;
        let mut module = JITModule::new(JITBuilder::with_isa(isa, default_libcall_names()));
        let mut sig = module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(types::I32));
        let id = module
            .declare_function("kernel", Linkage::Local, &sig)
            .map_err(|e| e.to_string())?;
        let mut cctx = module.make_context();
        cctx.func.signature = sig;
        cctx.func.name = ir::UserFuncName::user(0, id.as_u32());
        let mut fctx = FunctionBuilderContext::new();
        build(k, entry, &mut cctx.func, &mut fctx, module.target_config());
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
        if std::env::var_os("SHARC_FAST_TL").is_some() {
            // Measurement: is the machine code position independent?
            let relocs = cctx
                .compiled_code()
                .map(|c| c.buffer.relocs().len())
                .unwrap_or(0);
            eprintln!("tl cl bytes={code_size} relocs={relocs}");
        }
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
        let f = unsafe {
            std::mem::transmute::<*const u8, unsafe extern "C" fn(*mut u8, *mut u8) -> u32>(p)
        };
        Ok(Box::new(ClKernel {
            entry_checked: !entry.known_m.is_empty() || !entry.words.is_empty(),
            module: Some(module),
            f,
        }))
    }
}

// -- the shared-library interface (sharc_native::fast::plugin) ---------------------

/// # Safety
/// `words` points to `n` words; `err` to `cap` writable bytes.
#[cfg_attr(not(sharc_second), unsafe(no_mangle))]
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
/// `h` came from `sharc_fast_compile`; `ctx` is a context buffer and `st` the
/// engine state the kernel was lowered for.
#[cfg_attr(not(sharc_second), unsafe(no_mangle))]
pub unsafe extern "C" fn sharc_fast_run2(
    h: *mut std::ffi::c_void,
    ctx: *mut u8,
    st: *mut u8,
) -> u32 {
    let k = unsafe { &*(h as *const Box<dyn CompiledKernel>) };
    unsafe { k.run(ctx, st) }
}

/// # Safety
/// `h` came from `sharc_fast_compile` and is not used again.
#[cfg_attr(not(sharc_second), unsafe(no_mangle))]
pub unsafe extern "C" fn sharc_fast_free(h: *mut std::ffi::c_void) {
    drop(unsafe { Box::from_raw(h as *mut Box<dyn CompiledKernel>) });
}

#[cfg(test)]
mod tests {
    use super::*;
    use sharc_native::fast::{glue::Ctx, interp::InterpBackend};

    #[test]
    fn guards_share_failure_blocks_without_leaking_boolean_fixups() {
        let k = Kernel {
            vars: vec![Ty::I32],
            val_ty: vec![Ty::I32; 11],
            pre: vec![
                Inst::Def(0, Op::Ctx32(CTX_REGS_IN)),
                Inst::Def(1, Op::CI32(1)),
                Inst::Def(2, Op::CI32(2)),
                Inst::Def(3, Op::CI32(4)),
                Inst::Def(4, Op::CI32(0x44)),
                Inst::Set(Var(0), 4),
            ],
            body: vec![
                Inst::Def(5, Op::Bin(Bin::And, 0, 1)),
                Inst::Def(6, Op::Bin(Bin::And, 0, 2)),
                Inst::Def(7, Op::Bin(Bin::Ne, 0, 1)),
                Inst::Guard {
                    ok: 5,
                    k: 7,
                    exit: 0,
                },
                // The first failure block materializes comparison 7 as a
                // word. The continuation must materialize its own copy.
                Inst::Def(8, Op::Bin(Bin::Add, 7, 1)),
                Inst::Guard {
                    ok: 6,
                    k: 7,
                    exit: 0,
                },
                Inst::Def(9, Op::Bin(Bin::And, 0, 3)),
                Inst::Guard {
                    ok: 9,
                    k: 9,
                    exit: 1,
                },
                Inst::Set(Var(0), 4),
            ],
            post: vec![
                Inst::Def(10, Op::GetVar(Var(0))),
                Inst::StCtx32 {
                    off: CTX_REGS_OUT,
                    v: 10,
                },
            ],
            exits: vec![vec![(Var(0), 7)], vec![(Var(0), 8)]],
            ..Kernel::default()
        };
        let mut backend = ClBackend {
            opt: "none",
            verify: true,
        };
        let kernels = [
            InterpBackend.compile(&k).unwrap(),
            backend.compile(&k).unwrap(),
        ];
        for kernel in kernels {
            for (input, expected) in [
                (0, (1, 7, 0, 1)),
                (1, (1, 7, 0, 0)),
                (2, (1, 7, 0, 1)),
                (3, (1, 9, 0, 2)),
                (7, (0, 0xbad, 1, 0x44)),
            ] {
                let mut ctx = Ctx::default();
                ctx.set32(CTX_ITERS, 1);
                ctx.set32(CTX_REGS_IN, input);
                ctx.set32(CTX_EXIT_K, 0xbad);
                // SAFETY: this kernel accesses only the live context buffer.
                let result = unsafe { kernel.run(ctx.ptr(), std::ptr::null_mut()) };
                assert_eq!(
                    (
                        result,
                        ctx.get32(CTX_EXIT_K),
                        ctx.get32(CTX_DONE),
                        ctx.get32(CTX_REGS_OUT)
                    ),
                    expected,
                    "input {input}"
                );
            }
        }
    }

    #[test]
    fn guard_any_and_leave_fill_failure_blocks_after_terminating_the_predecessor() {
        let k = Kernel {
            vars: vec![Ty::F32],
            val_ty: vec![Ty::I32, Ty::I32, Ty::I32, Ty::I32, Ty::F32, Ty::I32],
            pre: vec![
                Inst::Def(0, Op::Ctx32(CTX_REGS_IN)),
                Inst::Def(1, Op::Ctx32(CTX_REGS_IN + 4)),
                Inst::Def(2, Op::CI32(0x3f80_0000)),
                Inst::Def(3, Op::CI32(0x4000_0000)),
                Inst::Set(Var(0), 3),
            ],
            body: vec![
                Inst::GuardAny {
                    a: 0,
                    b: 1,
                    k: 5,
                    exit: 0,
                },
                Inst::Leave { k: 6, exit: 1 },
            ],
            post: vec![
                Inst::Def(4, Op::GetVar(Var(0))),
                Inst::Def(5, Op::Un(Un::FToBits, 4)),
                Inst::StCtx32 {
                    off: CTX_REGS_OUT,
                    v: 5,
                },
            ],
            exits: vec![vec![(Var(0), 2)], vec![(Var(0), 3)]],
            cfg: true,
            ..Kernel::default()
        };
        let mut backend = ClBackend {
            opt: "none",
            verify: true,
        };
        let kernels = [
            InterpBackend.compile(&k).unwrap(),
            backend.compile(&k).unwrap(),
        ];
        for kernel in kernels {
            for (a, b, exit, bits) in [
                (0, 0, 5, 0x3f80_0000),
                (0, 2, 6, 0x4000_0000),
                (2, 0, 6, 0x4000_0000),
                (3, 4, 6, 0x4000_0000),
            ] {
                let mut ctx = Ctx::default();
                ctx.set32(CTX_REGS_IN, a);
                ctx.set32(CTX_REGS_IN + 4, b);
                // SAFETY: this kernel accesses only the live context buffer.
                let result = unsafe { kernel.run(ctx.ptr(), std::ptr::null_mut()) };
                assert_eq!(
                    (
                        result,
                        ctx.get32(CTX_EXIT_K),
                        ctx.get32(CTX_DONE),
                        ctx.get32(CTX_REGS_OUT)
                    ),
                    (1, exit, 0, bits),
                    "inputs {a}, {b}"
                );
            }
        }
    }
}
