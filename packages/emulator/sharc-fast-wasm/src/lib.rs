//! WebAssembly backend for the fast tier's kernel IR (`sharc_native::fast::ir`).
//!
//! A kernel becomes one wasm function `(ctx: i32, st: i32) -> i32` over an
//! imported linear memory (`env.memory`): the context buffer, the engine state
//! `St` and the data windows are all addresses in that memory (in the browser
//! build they are the Rust core's own addresses, so the kernels share its
//! memory). Window slots hold `host address - window start` as a u64; only the
//! low 32 bits matter here (the address arithmetic wraps mod 2^32, as the
//! 64-bit arithmetic of the native backends does for in-range accesses).
//!
//! Semantics follow `packages/emulator/sharc-fast-cl`: guards leave through a shared
//! tail (`post`) after applying their variable fix-ups and recording the
//! instruction index, `CTX_DONE` counts finished iterations, the return value
//! is 0 for normal completion and 1 for a guard exit. Control flow (labels)
//! is a dispatch loop over the label segments (`br_table`); straight-line
//! bodies have none.
//!
//! One module can hold many kernels (`emit_module`), each exported as `k<i>`:
//! the form chaining needs (direct calls between regions) will build on it.

use sharc_native::fast::ir::*;
use wasm_encoder::{
    BlockType, CodeSection, EntityType, ExportKind, ExportSection, Function, FunctionSection,
    ImportSection, Instruction as I, MemArg, MemoryType, Module, TypeSection, ValType,
};

/// Wasm local layout of one kernel.
struct Locals {
    ctx: u32,
    st: u32,
    /// Local index of each IR value / variable.
    val: Vec<u32>,
    var: Vec<u32>,
    code: u32,
    t: u32,
    pc: u32,
    /// (count, type) groups after the two parameters.
    decl: Vec<(u32, ValType)>,
}

fn vt(t: Ty) -> ValType {
    match t {
        Ty::I32 => ValType::I32,
        Ty::F32 => ValType::F32,
        Ty::I64 => ValType::I64,
    }
}

fn layout(k: &Kernel) -> Locals {
    // Locals are grouped by type: all i32, then f32, then i64.
    let mut kinds: Vec<(Ty, &str, usize)> = Vec::new();
    for (i, &t) in k.val_ty.iter().enumerate() {
        kinds.push((t, "val", i));
    }
    for (i, &t) in k.vars.iter().enumerate() {
        kinds.push((t, "var", i));
    }
    kinds.push((Ty::I32, "code", 0));
    kinds.push((Ty::I32, "t", 0));
    kinds.push((Ty::I32, "pc", 0));
    let mut l = Locals {
        ctx: 0,
        st: 1,
        val: vec![0; k.val_ty.len()],
        var: vec![0; k.vars.len()],
        code: 0,
        t: 0,
        pc: 0,
        decl: Vec::new(),
    };
    let mut next = 2u32;
    for ty in [Ty::I32, Ty::F32, Ty::I64] {
        let mut n = 0;
        for &(t, what, i) in &kinds {
            if t != ty {
                continue;
            }
            let idx = next + n;
            n += 1;
            match what {
                "val" => l.val[i] = idx,
                "var" => l.var[i] = idx,
                "code" => l.code = idx,
                "t" => l.t = idx,
                _ => l.pc = idx,
            }
        }
        if n > 0 {
            l.decl.push((n, vt(ty)));
            next += n;
        }
    }
    l
}

fn ma(offset: u32, align: u32) -> MemArg {
    MemArg {
        offset: offset as u64,
        align,
        memory_index: 0,
    }
}

struct Emit<'a> {
    k: &'a Kernel,
    l: Locals,
    f: Function,
    /// Structured constructs currently open; `post_level` is the count when
    /// the `post` block was opened, so `br (nest - post_level)` leaves it.
    nest: u32,
    post_level: u32,
    /// Dispatch state of the current label region: nest level of the loop
    /// (a jump is `br (nest - loop_level)`) and the segment index of each label.
    disp: Option<(u32, Vec<u32>)>,
}

impl Emit<'_> {
    fn i(&mut self, x: I<'_>) {
        self.f.instruction(&x);
    }
    fn get(&mut self, v: Val) {
        self.i(I::LocalGet(self.l.val[v as usize]));
    }
    fn open(&mut self, x: I<'_>) {
        self.i(x);
        self.nest += 1;
    }
    fn close(&mut self) {
        self.i(I::End);
        self.nest -= 1;
    }
    fn br_post(&mut self) {
        let d = self.nest - self.post_level;
        self.i(I::Br(d));
    }

    /// The i32 address `low32(base) + off` for a window access.
    fn addr(&mut self, base: Val, off: Val) {
        self.get(base);
        self.i(I::I32WrapI64);
        self.get(off);
        self.i(I::I32Add);
    }

    /// Push value X converted to the type of variable VAR.
    fn as_var(&mut self, var: Var, x: Val) {
        let want = self.k.vars[var.0 as usize];
        let have = self.k.val_ty[x as usize];
        self.get(x);
        match (have, want) {
            (Ty::I32, Ty::F32) => self.i(I::F32ReinterpretI32),
            (Ty::F32, Ty::I32) => self.i(I::I32ReinterpretF32),
            _ => {}
        }
    }

    fn op(&mut self, op: Op) {
        match op {
            Op::CI32(c) => self.i(I::I32Const(c as i32)),
            Op::CF32(c) => self.i(I::F32Const(f32::from_bits(c).into())),
            Op::CI64(c) => self.i(I::I64Const(c as i64)),
            Op::GetVar(v) => self.i(I::LocalGet(self.l.var[v.0 as usize])),
            Op::Ctx32(o) => {
                self.i(I::LocalGet(self.l.ctx));
                self.i(I::I32Load(ma(o, 2)));
            }
            Op::Ctx64(o) => {
                self.i(I::LocalGet(self.l.ctx));
                self.i(I::I64Load(ma(o, 3)));
            }
            Op::CtxF32(o) => {
                self.i(I::LocalGet(self.l.ctx));
                self.i(I::F32Load(ma(o, 2)));
            }
            Op::St32(o) => {
                self.i(I::LocalGet(self.l.st));
                self.i(I::I32Load(ma(o, 2)));
            }
            Op::Un(u, x) => {
                self.get(x);
                match u {
                    Un::FNeg => self.i(I::F32Neg),
                    Un::FAbs => self.i(I::F32Abs),
                    Un::BitsToF => self.i(I::F32ReinterpretI32),
                    Un::FToBits => self.i(I::I32ReinterpretF32),
                    Un::FToI => self.i(I::I32TruncSatF32S),
                    Un::IToF => self.i(I::F32ConvertI32S),
                    Un::Zext => self.i(I::I64ExtendI32U),
                    Un::Not => {
                        self.i(I::I32Const(-1));
                        self.i(I::I32Xor);
                    }
                    Un::Clz => self.i(I::I32Clz),
                }
            }
            Op::Bin(b, x, y) => {
                self.get(x);
                self.get(y);
                match b {
                    Bin::Add => self.i(I::I32Add),
                    Bin::Sub => self.i(I::I32Sub),
                    Bin::Mul => self.i(I::I32Mul),
                    Bin::And => self.i(I::I32And),
                    Bin::Or => self.i(I::I32Or),
                    Bin::Xor => self.i(I::I32Xor),
                    Bin::Shl => self.i(I::I32Shl),
                    Bin::ShrU => self.i(I::I32ShrU),
                    Bin::ShrS => self.i(I::I32ShrS),
                    Bin::Eq => self.i(I::I32Eq),
                    Bin::Ne => self.i(I::I32Ne),
                    Bin::LtU => self.i(I::I32LtU),
                    Bin::LeU => self.i(I::I32LeU),
                    Bin::GtU => self.i(I::I32GtU),
                    Bin::GeU => self.i(I::I32GeU),
                    Bin::LtS => self.i(I::I32LtS),
                    Bin::GtS => self.i(I::I32GtS),
                    Bin::FLt => self.i(I::F32Lt),
                    Bin::FGe => self.i(I::F32Ge),
                    Bin::FEq => self.i(I::F32Eq),
                    Bin::FAdd => self.i(I::F32Add),
                    Bin::FSub => self.i(I::F32Sub),
                    Bin::FMul => self.i(I::F32Mul),
                    Bin::FDiv => self.i(I::F32Div),
                    Bin::Add64 => self.i(I::I64Add),
                    Bin::Sub64 => self.i(I::I64Sub),
                    Bin::MulHs => {
                        // high 32 bits of the signed 64-bit product: the
                        // operands are on the stack as i32; widen both.
                        let t = self.l.pc; // scratch: not live inside a value op
                        self.i(I::LocalSet(t));
                        self.i(I::I64ExtendI32S);
                        self.i(I::LocalGet(t));
                        self.i(I::I64ExtendI32S);
                        self.i(I::I64Mul);
                        self.i(I::I64Const(32));
                        self.i(I::I64ShrS);
                        self.i(I::I32WrapI64);
                    }
                }
            }
            Op::Select(c, a, b) => {
                self.get(a);
                self.get(b);
                self.get(c);
                self.i(I::Select);
            }
            Op::Helper(rec) => {
                // `helper_exec(st, rec)`: the imported function 0 (`env.helper`,
                // the core's `digi_helper_exec` export). On wasm32 the record
                // is a leaked pointer into the core's own memory, so the low
                // 32 bits are the whole address (`emit_function` refuses
                // anything wider).
                self.i(I::LocalGet(self.l.st));
                self.i(I::I32Const(rec as u32 as i32));
                self.i(I::Call(0));
            }
            Op::Load32 { base, off } => {
                self.addr(base, off);
                self.i(I::I32Load(ma(0, 0)));
            }
            Op::Load64 { base, off } => {
                // A pointer table entry of the core's memory: 8 bytes in a
                // 64-bit host's layout, which is what the kernel was lowered
                // for (lowering emits Load32 + Zext on wasm32).
                self.addr(base, off);
                self.i(I::I64Load(ma(0, 0)));
            }
            Op::LoadN { base, off, bytes } => {
                self.addr(base, off);
                if bytes == 1 {
                    self.i(I::I32Load8U(ma(0, 0)));
                } else {
                    self.i(I::I32Load16U(ma(0, 0)));
                }
            }
        }
    }

    /// The failing tail of a guard or `Leave`: variable fix-ups, the
    /// instruction index, return code 1, leave through `post`.
    fn fail(&mut self, k: u32, exit: u32) {
        let list = self.k.exits[exit as usize].clone();
        // All fix-up values are read before any variable is written (a value
        // may be read through `GetVar` of a variable another fix-up writes).
        // Values are SSA locals, so direct assignment is already safe.
        for (var, x) in list {
            self.as_var(var, x);
            self.i(I::LocalSet(self.l.var[var.0 as usize]));
        }
        self.i(I::LocalGet(self.l.ctx));
        self.i(I::I32Const(k as i32));
        self.i(I::I32Store(ma(CTX_EXIT_K, 2)));
        self.i(I::I32Const(1));
        self.i(I::LocalSet(self.l.code));
        self.br_post();
    }

    fn inst(&mut self, inst: &Inst) {
        match *inst {
            Inst::Def(d, op) => {
                self.op(op);
                self.i(I::LocalSet(self.l.val[d as usize]));
            }
            Inst::Set(var, x) => {
                self.as_var(var, x);
                self.i(I::LocalSet(self.l.var[var.0 as usize]));
            }
            Inst::Store32 { base, off, v } => {
                self.addr(base, off);
                self.get(v);
                self.i(I::I32Store(ma(0, 0)));
            }
            Inst::StoreN {
                base,
                off,
                v,
                bytes,
            } => {
                self.addr(base, off);
                self.get(v);
                if bytes == 1 {
                    self.i(I::I32Store8(ma(0, 0)));
                } else {
                    self.i(I::I32Store16(ma(0, 0)));
                }
            }
            Inst::StCtx32 { off, v } => {
                self.i(I::LocalGet(self.l.ctx));
                self.get(v);
                if self.k.val_ty[v as usize] == Ty::F32 {
                    self.i(I::F32Store(ma(off, 2)));
                } else {
                    self.i(I::I32Store(ma(off, 2)));
                }
            }
            Inst::StSt32 { off, v } => {
                self.i(I::LocalGet(self.l.st));
                self.get(v);
                if self.k.val_ty[v as usize] == Ty::F32 {
                    self.i(I::F32Store(ma(off, 2)));
                } else {
                    self.i(I::I32Store(ma(off, 2)));
                }
            }
            Inst::Guard { ok, k, exit } => {
                self.get(ok);
                self.i(I::I32Eqz);
                self.open(I::If(BlockType::Empty));
                self.fail(k, exit);
                self.close();
            }
            Inst::GuardAny { a, b, k, exit } => {
                self.get(a);
                self.get(b);
                self.i(I::I32Or);
                self.i(I::I32Eqz);
                self.open(I::If(BlockType::Empty));
                self.fail(k, exit);
                self.close();
            }
            Inst::Leave { k, exit } => self.fail(k, exit),
            Inst::Jump(l) => self.jump(l),
            Inst::BrIf { c, target } => {
                self.get(c);
                self.open(I::If(BlockType::Empty));
                self.jump(target);
                self.close();
            }
            Inst::Label(_) => unreachable!("labels are handled by `seq`"),
        }
    }

    fn jump(&mut self, label: u32) {
        let (level, segs) = self.disp.as_ref().expect("a jump outside a label region");
        let (level, seg) = (*level, segs[label as usize]);
        self.i(I::I32Const(seg as i32));
        self.i(I::LocalSet(self.l.pc));
        let d = self.nest - level;
        self.i(I::Br(d));
    }

    /// A body or section: straight-line, or a dispatch over its label segments.
    fn seq(&mut self, insts: &[Inst]) {
        if !insts.iter().any(|i| matches!(i, Inst::Label(_))) {
            for i in insts {
                self.inst(i);
            }
            return;
        }
        // Segment 0 is the code before the first label; label L's segment
        // index is its position among the labels, plus one.
        let mut segs: Vec<Vec<&Inst>> = vec![Vec::new()];
        let mut label_seg = vec![0u32; self.k.nlabels as usize];
        for i in insts {
            if let Inst::Label(l) = i {
                segs.push(Vec::new());
                label_seg[*l as usize] = (segs.len() - 1) as u32;
            } else {
                segs.last_mut().unwrap().push(i);
            }
        }
        let n = segs.len() as u32;
        let saved = self.disp.take();
        // pc = 0; loop { block*n { br_table } seg0 end seg1 end ... }
        self.i(I::I32Const(0));
        self.i(I::LocalSet(self.l.pc));
        self.open(I::Loop(BlockType::Empty));
        self.disp = Some((self.nest, label_seg));
        for _ in 0..n {
            self.open(I::Block(BlockType::Empty));
        }
        self.i(I::LocalGet(self.l.pc));
        let targets: Vec<u32> = (0..n).collect();
        self.i(I::BrTable(targets.into(), n - 1));
        for seg in &segs {
            self.close(); // the block whose end is this segment's entry
            for i in seg {
                self.inst(i);
            }
        }
        self.close(); // the loop
        self.disp = saved;
    }
}

/// Whether K has a helper call (`wide`: only one whose record needs more than
/// 32 bits).
fn uses_helper(k: &Kernel, wide: bool) -> bool {
    [&k.pre, &k.body, &k.post]
        .into_iter()
        .flatten()
        .any(|i| matches!(i, Inst::Def(_, Op::Helper(r)) if !wide || *r > u32::MAX as u64))
}

/// The wasm function body of kernel K.
fn emit_function(k: &Kernel) -> Result<Function, String> {
    k.validate()?;
    if [&k.pre, &k.body, &k.post]
        .into_iter()
        .flatten()
        .any(|i| matches!(i, Inst::Def(_, Op::CI64(_))))
    {
        return Err(
            "a kernel with a host table address (recips, rsqrts) is not lowered to wasm".into(),
        );
    }
    if uses_helper(k, true) {
        return Err(
            "a helper record above 4 GiB (kernels with helper calls need a wasm32 core)".into(),
        );
    }
    let l = layout(k);
    let f = Function::new(l.decl.clone());
    let mut e = Emit {
        k,
        l,
        f,
        nest: 0,
        post_level: 0,
        disp: None,
    };
    e.i(I::I32Const(0));
    e.i(I::LocalSet(e.l.code));
    // The unrolled sections: `pre`, the loop body (or one pass of a CFG body)
    // and `post`. Guards leave to the end of the block `post` below.
    e.seq(&k.pre.clone());
    e.open(I::Block(BlockType::Empty));
    e.post_level = e.nest;
    if k.cfg {
        let body = k.body.clone();
        e.seq(&body);
        e.close();
        e.i(I::LocalGet(e.l.ctx));
        e.i(I::I32Const(0));
        e.i(I::I32Store(ma(CTX_DONE, 2)));
    } else {
        e.i(I::I32Const(0));
        e.i(I::LocalSet(e.l.t));
        e.open(I::Block(BlockType::Empty)); // done
        e.open(I::Loop(BlockType::Empty));
        e.i(I::LocalGet(e.l.t));
        e.i(I::LocalGet(e.l.ctx));
        e.i(I::I32Load(ma(CTX_ITERS, 2)));
        e.i(I::I32GeU);
        e.i(I::BrIf(1)); // to the end of `done`
        let body = k.body.clone();
        e.seq(&body);
        e.i(I::LocalGet(e.l.t));
        e.i(I::I32Const(1));
        e.i(I::I32Add);
        e.i(I::LocalSet(e.l.t));
        e.i(I::Br(0));
        e.close(); // loop
        e.close(); // done
        e.close(); // post
        e.i(I::LocalGet(e.l.ctx));
        e.i(I::LocalGet(e.l.t));
        e.i(I::I32Store(ma(CTX_DONE, 2)));
    }
    let post = k.post.clone();
    e.seq(&post);
    e.i(I::LocalGet(e.l.code));
    e.i(I::End);
    Ok(e.f)
}

/// One module holding KERNELS, exported as `k0`, `k1`, ...; it imports
/// `env.memory` (at least `min_pages` 64 KiB pages).
pub fn emit_module(kernels: &[&Kernel], min_pages: u64) -> Result<Vec<u8>, String> {
    let mut m = Module::new();
    let mut types = TypeSection::new();
    types
        .ty()
        .function([ValType::I32, ValType::I32], [ValType::I32]);
    m.section(&types);
    let mut imports = ImportSection::new();
    imports.import(
        "env",
        "memory",
        EntityType::Memory(MemoryType {
            minimum: min_pages,
            maximum: None,
            memory64: false,
            shared: false,
            page_size_log2: None,
        }),
    );
    // `env.helper(st, rec) -> status`, imported only when a kernel calls it
    // (function index 0, so the kernels are `k<i>` at index `helper + i`).
    let helper = kernels.iter().any(|k| uses_helper(k, false)) as u32;
    if helper != 0 {
        imports.import("env", "helper", EntityType::Function(0));
    }
    m.section(&imports);
    let mut funcs = FunctionSection::new();
    let mut exports = ExportSection::new();
    let mut code = CodeSection::new();
    for (i, k) in kernels.iter().enumerate() {
        funcs.function(0);
        exports.export(&format!("k{i}"), ExportKind::Func, helper + i as u32);
        code.function(&emit_function(k).map_err(|e| format!("kernel {i}: {e}"))?);
    }
    m.section(&funcs);
    m.section(&exports);
    m.section(&code);
    Ok(m.finish())
}

/// The module of a single kernel.
pub fn emit_kernel(k: &Kernel) -> Result<Vec<u8>, String> {
    emit_module(&[k], 1)
}

/// A kernel backend that produces WebAssembly. Compiling keeps the module
/// bytes; running is the host's business (the browser instantiates them over
/// the core's memory), so `run` is only available through a registered
/// runner.
pub struct WasmBackend;

pub struct WasmKernel {
    pub bytes: Vec<u8>,
}

impl CompiledKernel for WasmKernel {
    unsafe fn run(&self, _ctx: *mut u8, _st: *mut u8) -> u32 {
        panic!("a WasmKernel runs in the host's WebAssembly engine, not here")
    }
}

impl KernelBackend for WasmBackend {
    fn name(&self) -> &'static str {
        "wasm"
    }
    fn compile(&mut self, k: &Kernel) -> Result<Box<dyn CompiledKernel>, String> {
        Ok(Box::new(WasmKernel {
            bytes: emit_kernel(k)?,
        }))
    }
}

/// The backend of the browser core (wasm32): kernels are emitted here and
/// instantiated by the page over this module's memory, which is the same
/// memory the kernels address (a host pointer of this core is a linear-memory
/// address). The page provides three imports from module `env`:
///
/// * `host_wasm_compile(ptr, len) -> i32`: compile the module at `ptr..ptr+len`
///   and return a kernel id (>= 0), or < 0 on failure;
/// * `host_wasm_run(id, ctx, st) -> u32`: run kernel `id`;
/// * `host_wasm_free(id)`: drop it.
#[cfg(target_arch = "wasm32")]
pub mod host {
    use super::*;

    #[link(wasm_import_module = "env")]
    unsafe extern "C" {
        fn host_wasm_compile(ptr: *const u8, len: usize) -> i32;
        fn host_wasm_run(id: i32, ctx: *mut u8, st: *mut u8) -> u32;
        fn host_wasm_free(id: i32);
    }

    pub struct HostBackend;

    struct HostKernel(i32);

    impl CompiledKernel for HostKernel {
        unsafe fn run(&self, ctx: *mut u8, st: *mut u8) -> u32 {
            unsafe { host_wasm_run(self.0, ctx, st) }
        }
    }

    impl Drop for HostKernel {
        fn drop(&mut self) {
            unsafe { host_wasm_free(self.0) }
        }
    }

    impl KernelBackend for HostBackend {
        fn name(&self) -> &'static str {
            "wasm-host"
        }
        fn compile(&mut self, k: &Kernel) -> Result<Box<dyn CompiledKernel>, String> {
            let bytes = emit_kernel(k)?;
            // SAFETY: the page reads the bytes during the call only.
            let id = unsafe { host_wasm_compile(bytes.as_ptr(), bytes.len()) };
            if id < 0 {
                return Err(format!("the host refused the kernel module ({id})"));
            }
            Ok(Box::new(HostKernel(id)))
        }
    }

    /// Make `FastEngine`s use the host backend.
    pub fn register() {
        sharc_native::fast::register_backend(|| Box::new(HostBackend));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One kernel: `ok = helper(st, rec)`; a zero status ends the kernel with exit 3.
    fn helper_kernel(rec: u64) -> Kernel {
        Kernel {
            val_ty: vec![Ty::I32],
            body: vec![
                Inst::Def(0, Op::Helper(rec)),
                Inst::Guard {
                    ok: 0,
                    k: 3,
                    exit: 0,
                },
            ],
            exits: vec![Vec::new()],
            ..Kernel::default()
        }
    }

    /// A helper call imports `env.helper` (function 0, so the kernel is function 1
    /// and still exported as `k0`); a kernel without one imports nothing extra.
    /// `WASM_HELPER_OUT=FILE` also writes the module for
    /// `packages/emulator/sharc-fast-wasm/helper_check.mjs`.
    #[test]
    fn helper_call_imports_the_host_function() {
        let k = helper_kernel(0x1234);
        let bytes = emit_module(&[&k], 1).unwrap();
        assert!(bytes.windows(6).any(|w| w == b"helper"));
        if let Some(out) = std::env::var_os("WASM_HELPER_OUT") {
            std::fs::write(out, &bytes).unwrap();
        }
        let plain = Kernel {
            val_ty: vec![Ty::I32],
            body: vec![
                Inst::Def(0, Op::CI32(1)),
                Inst::Guard {
                    ok: 0,
                    k: 3,
                    exit: 0,
                },
            ],
            exits: vec![Vec::new()],
            ..Kernel::default()
        };
        let bytes = emit_module(&[&plain], 1).unwrap();
        assert!(!bytes.windows(6).any(|w| w == b"helper"));
    }

    /// A record that does not fit in a wasm32 address is refused, not truncated.
    #[test]
    fn wide_helper_record_is_refused() {
        let err = emit_module(&[&helper_kernel(1 << 40)], 1).unwrap_err();
        assert!(err.contains("wasm32"), "{err}");
    }
}
