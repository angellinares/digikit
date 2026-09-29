//! A region: the basic blocks reachable from an entry PC along static
//! control flow (limited to executed code), translated into one
//! WebAssembly function. Registers live in locals from entry to exit; each
//! instruction that traps is undone (its start state written back, the
//! runtime's undo log rolled back) and left to the interpreter.

use super::eval::{Fail, Flow, Pe, R, fail};
use super::ir::{self, Label, Node, W, mem};
use super::mach::{self, Ctx, DInsn, MODE1, MState, PARTIAL_REGS};
use super::types::Types;
use super::val::*;
use crate::rs::ast::Item;
use crate::rs::parse::Program;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;
use wasm_encoder::{
    CodeSection, EntityType, ExportKind, ExportSection, Function, FunctionSection, ImportSection, MemoryType,
    Module, TypeSection, ValType,
};
use wasm_encoder::Instruction as I;

pub const EXIT_NEXT: i32 = 0;
pub const EXIT_BUDGET: i32 = 1;
pub const EXIT_TRAP: i32 = 2;
pub const EXIT_BAIL: i32 = 3;

/// Limits on a region's size.
#[derive(Clone, Debug)]
pub struct Limits {
    pub max_insns: usize,
    pub max_blocks: usize,
    pub max_block_insns: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_insns: 400,
            max_blocks: 48,
            max_block_insns: 64,
        }
    }
}

pub struct Region {
    pub wasm: Vec<u8>,
    pub blocks: Vec<u32>,
    pub insns: usize,
    /// Instructions left to the interpreter (their PE failed), with why.
    pub failures: Vec<(u32, String)>,
    /// A readable listing of the code (Ctx::trace only).
    pub text: String,
}

/// The function body as indented text (debugging).
pub fn listing(body: &[I<'static>]) -> String {
    let mut out = String::new();
    let mut depth = 1usize;
    for i in body {
        if matches!(i, I::End | I::Else) {
            depth = depth.saturating_sub(1);
        }
        out.push_str(&"  ".repeat(depth));
        out.push_str(&format!("{i:?}\n"));
        if matches!(i, I::Block(_) | I::Loop(_) | I::If(_) | I::Else) {
            depth += 1;
        }
    }
    out
}

/// What region formation may assume and where it may go.
pub struct Spec<'a> {
    pub entry: u32,
    /// MODE1 at entry (checked by the prologue), when known.
    pub mode1: Option<u32>,
    /// Whether PC has run (the interpreter's evidence): blocks are added
    /// only at executed PCs.
    pub seen: &'a dyn Fn(u32) -> bool,
    pub limits: Limits,
}

pub struct Translator {
    pub prog: Program,
    pub types: Types,
    pub syms: BTreeMap<String, u16>,
}

impl Translator {
    pub fn new(prog: Program) -> Result<Translator, String> {
        let types = Types::new(&prog);
        let mut syms = BTreeMap::new();
        if let Some(Item::Const(_, _, crate::rs::ast::Expr::Array(names))) = prog.items.get("syms::SYM_NAMES") {
            for (k, e) in names.iter().enumerate() {
                if let crate::rs::ast::Expr::Str(s) = e {
                    syms.entry(s.clone()).or_insert(k as u16);
                }
            }
        } else {
            return Err("syms.rs has no SYM_NAMES".into());
        }
        Ok(Translator { prog, types, syms })
    }
}

struct BlockState {
    /// Block index in the region.
    idx: u32,
}

struct Rg<'a, 'p> {
    pe: Pe<'p>,
    spec: &'a Spec<'a>,
    starts: Vec<u32>,
    index: BTreeMap<u32, BlockState>,
    /// Instructions covered by a translated block (pc -> block start).
    covered: BTreeMap<u32, u32>,
    queue: VecDeque<u32>,
    split: bool,
    insns: usize,
    failures: Vec<(u32, String)>,
    // Locals shared by all blocks.
    l_bi: u32,
    l_pc: u32,
    l_ic: u32,
    l_lim: u32,
    l_code: u32,
    top: Label,
    exit: Label,
    trap_exit: Label,
    bodies: BTreeMap<u32, Vec<Node>>,
}

/// Translate the region at SPEC.entry.
pub fn translate(tr: &Translator, cx: &Ctx, spec: &Spec) -> Result<Region, String> {
    let mut starts = vec![spec.entry];
    for _round in 0..8 {
        let mut rg = Rg::new(tr, cx, spec, starts.clone());
        rg.run()?;
        if !rg.split {
            return rg.finish();
        }
        starts = rg.starts.clone();
    }
    Err("region formation did not settle".into())
}

impl<'a, 'p> Rg<'a, 'p> {
    fn new(tr: &'p Translator, cx: &'p Ctx, spec: &'a Spec<'a>, starts: Vec<u32>) -> Rg<'a, 'p> {
        let mut pe = Pe::new(&tr.prog, &tr.types, cx, 1);
        let l_bi = pe.local(W::I32);
        let l_pc = pe.local(W::I64);
        let l_ic = pe.local(W::I64);
        let l_lim = pe.local(W::I64);
        let top = pe.label();
        let exit = pe.label();
        let trap_exit = pe.label();
        let mut index = BTreeMap::new();
        for (k, &s) in starts.iter().enumerate() {
            index.insert(s, BlockState { idx: k as u32 });
        }
        let queue = starts.iter().copied().collect();
        Rg {
            pe,
            spec,
            starts,
            index,
            covered: BTreeMap::new(),
            queue,
            split: false,
            insns: 0,
            failures: Vec::new(),
            l_bi,
            l_pc,
            l_ic,
            l_lim,
            l_code: 0,
            top,
            exit,
            trap_exit,
            bodies: BTreeMap::new(),
        }
    }

    fn block_entry_state(&self, pc: u32) -> MState {
        let mut ms = MState::new();
        ms.pc = Av::Int(pc as i128, IT::I128);
        if let Some(m) = self.spec.mode1 {
            ms.r[MODE1 as usize] = st("V", vec![Av::Int(m as i128, IT::U32), Av::Int(u32::MAX as i128, IT::U32)]);
        }
        ms
    }

    fn run(&mut self) -> Result<(), String> {
        let mut bodies: BTreeMap<u32, Vec<Node>> = BTreeMap::new();
        while let Some(pc) = self.queue.pop_front() {
            if bodies.contains_key(&pc) {
                continue;
            }
            let body = self.block(pc)?;
            if self.split {
                return Ok(());
            }
            bodies.insert(pc, body);
        }
        // Blocks in index order.
        self.bodies = bodies;
        Ok(())
    }

    /// Translate the block at PC; its code ends in a jump (to another
    /// block, or out of the region).
    fn block(&mut self, start: u32) -> Result<Vec<Node>, String> {
        self.pe.code = Vec::new();
        self.pe.ms = self.block_entry_state(start);
        // The instruction budget: leave before the block when it may not
        // fit (the interpreter runs single steps up to the limit).
        let n_est = self.estimate_len(start) as i64;
        self.pe.emit(I::LocalGet(self.l_ic));
        self.pe.emit(I::I64Const(n_est));
        self.pe.emit(I::I64Add);
        self.pe.emit(I::LocalGet(self.l_lim));
        self.pe.emit(I::I64GtU);
        let ex = self.exit_code_static(start, EXIT_BUDGET);
        self.pe.code.push(Node::If(ex, vec![]));
        let mut pc = start;
        let mut k: u32 = 0;
        loop {
            self.covered.insert(pc, start);
            let Some(d) = self.pe.cx.insns.insn(pc) else {
                self.exit_here(pc, k)?;
                break;
            };
            if d.length_bytes.is_none() || d.kind != "confident" {
                self.exit_here(pc, k)?;
                break;
            }
            let ms0 = self.pe.ms.clone();
            let code0 = self.pe.code.clone();
            match self.insn(pc, &d, k) {
                Ok(true) => {}
                Ok(false) | Err(_) => {
                    // Leave the instruction to the interpreter.
                    self.pe.ms = ms0;
                    self.pe.code = code0;
                    self.exit_here(pc, k)?;
                    break;
                }
            }
            k += 1;
            self.insns += 1;
            // Where next?
            let next = pc + d.length_bytes.unwrap() / 2;
            let pending_none = self.pe.ms.pending == none();
            match self.pe.ms.pc.clone() {
                Av::Int(p, _) => {
                    let p = p as u32;
                    let in_delay = !pending_none;
                    let fall = p == next;
                    let start_here = self.index.contains_key(&p);
                    if fall && (k as usize) < self.spec.limits.max_block_insns && (in_delay || !start_here) {
                        pc = p;
                        continue;
                    }
                    self.edge(p, k)?;
                }
                Av::D(d) => {
                    self.dyn_edge(d, k)?;
                }
                other => return Err(format!("pc {other:?}")),
            }
            break;
        }
        Ok(std::mem::take(&mut self.pe.code))
    }

    /// Instructions in the block at START (by static fall-through), for the
    /// budget check (an upper bound: the block may end earlier).
    fn estimate_len(&self, start: u32) -> usize {
        let mut pc = start;
        let mut n = 0;
        while n < self.spec.limits.max_block_insns {
            let Some(d) = self.pe.cx.insns.insn(pc) else { break };
            let Some(len) = d.length_bytes else { break };
            n += 1;
            pc += len / 2;
            if n > 1 && self.index.contains_key(&pc) {
                break;
            }
        }
        // Delay slots may extend a block past a jump: add their room.
        n + 2
    }

    /// Translate one instruction; Ok(false) when it cannot be (left to the
    /// interpreter).
    fn insn(&mut self, pc: u32, d: &Rc<DInsn>, k: u32) -> Result<bool, String> {
        let start = self.pe.ms.clone();
        let trap = self.pe.label();
        let ok_l = self.pe.label();
        let outer = std::mem::take(&mut self.pe.code);
        let saved_trap = self.pe.trap;
        self.pe.trap = trap;
        let insn = self.pe.cx.insn_av(d);
        let r = self.execute(insn);
        self.pe.trap = saved_trap;
        let body = std::mem::replace(&mut self.pe.code, outer);
        let flow = match r {
            Ok(f) => f,
            Err(Fail(why)) => {
                self.failures.push((pc, why));
                return Ok(false);
            }
        };
        if !matches!(flow, Flow::V(_)) {
            self.failures.push((pc, "always traps".into()));
            return Ok(false);
        }
        // The trap handler: back to the instruction's start state.
        let saved = std::mem::take(&mut self.pe.code);
        let after = std::mem::replace(&mut self.pe.ms, start.clone());
        let hres = self.write_homes(&start).map_err(|e| e.0);
        self.pe.emit(I::I64Const(pc as i64));
        self.pe.emit(I::LocalSet(self.l_pc));
        self.bump_icount(k);
        let pres = self.store_pending(&start.pending).map_err(|e| e.0);
        self.pe.code.push(Node::Br(self.trap_exit));
        let handler = std::mem::replace(&mut self.pe.code, saved);
        self.pe.ms = after;
        if let Err(e) = hres.and(pres) {
            self.failures.push((pc, e));
            return Ok(false);
        }
        let mut inner = body;
        inner.push(Node::Br(ok_l));
        let mut okb = vec![Node::Block(trap, inner)];
        okb.extend(handler);
        self.pe.code.push(Node::Block(ok_l, okb));
        if self.pe.ms.logged {
            self.pe.emit(I::LocalGet(0));
            self.pe.emit(I::I32Const(0));
            self.pe.emit(I::I32Store(mem(self.pe.cx.lay.un as u64, 2)));
            self.pe.ms.logged = false;
        }
        if self.pe.cx.trace {
            let r = self.pe.ms.r.clone();
            for (c, v) in r.iter().enumerate() {
                let v = match v {
                    // Unchanged in this block: its home locals (a register
                    // an earlier block wrote), if it has any.
                    Av::Undef => match self.pe.homes.get(&(c as u32)) {
                        Some(_) => self.pe.entry_reg(c as u32).map_err(|e| e.0)?,
                        None => continue,
                    },
                    v => v.clone(),
                };
                let (b, m) = mach::v_parts(&v).map_err(|e| e.0)?;
                self.pe.emit(I::LocalGet(0));
                self.pe.emit(I::I32Const(c as i32));
                self.pe.push_as(&b, Kind::Int(IT::U32, Rep::I32)).map_err(|e| e.0)?;
                self.pe.push_as(&m, Kind::Int(IT::U32, Rep::I32)).map_err(|e| e.0)?;
                self.pe.emit(I::Call(mach::helper("jit_trace")));
            }
            self.pe.emit(I::LocalGet(0));
            self.pe.emit(I::I64Const(pc as i64));
            self.pe.emit(I::Call(mach::helper("jit_trace_end")));
        }
        Ok(true)
    }

    fn execute(&mut self, insn: Av) -> R<Flow> {
        let Some(Item::Fn(fd)) = self.pe.prog.items.get("core::forms::_execute") else {
            return fail("no core::forms::_execute");
        };
        let fd = fd.clone();
        self.pe.module = "core::forms".into();
        self.pe.call_fn(&fd, vec![Av::St, insn], None)
    }

    /// icount local += K (at an exit after K instructions of the block).
    fn bump_icount(&mut self, k: u32) {
        if k > 0 {
            self.pe.emit(I::LocalGet(self.l_ic));
            self.pe.emit(I::I64Const(k as i64));
            self.pe.emit(I::I64Add);
            self.pe.emit(I::LocalSet(self.l_ic));
        }
    }

    /// Write every register the state holds (not Undef) to its home locals.
    /// Inside the region a register's mask must be all known (non-partial
    /// registers); leaving it, any mask goes back to St.
    fn write_homes(&mut self, ms: &MState) -> R<()> {
        self.write_homes_as(ms, true)
    }

    fn write_homes_as(&mut self, ms: &MState, leaving: bool) -> R<()> {
        // A parallel copy: a value may be another register's home local
        // (`I7 = I6` leaves I7 holding I6's entry value), so every source is
        // read (pushed) before any home is written.
        let mut dests = Vec::new();
        for c in 0..ms.r.len() as u32 {
            let v = ms.r[c as usize].clone();
            if matches!(v, Av::Undef) {
                continue;
            }
            let (b, m) = mach::v_parts(&v)?;
            if !leaving && !PARTIAL_REGS.contains(&c) && m != Av::Int(u32::MAX as i128, IT::U32) {
                return fail(format!("register {c} with a mask that is not all known"));
            }
            let (lb, lm) = self.pe.home(c);
            // Already in place: nothing to move.
            if matches!(&b, Av::D(d) if d.l == lb) && matches!(&m, Av::D(d) if d.l == lm) {
                self.pe.written.insert(c);
                continue;
            }
            self.pe.push_as(&b, Kind::Int(IT::U32, Rep::I32))?;
            self.pe.push_as(&m, Kind::Int(IT::U32, Rep::I32))?;
            dests.push(lb);
            dests.push(lm);
            self.pe.written.insert(c);
        }
        for l in dests.into_iter().rev() {
            self.pe.emit(I::LocalSet(l));
        }
        Ok(())
    }

    /// Store a pending transfer that is not None into St.
    fn store_pending(&mut self, p: &Av) -> R<()> {
        if *p == none() {
            return Ok(());
        }
        let (present, fields): (Av, Vec<Av>) = match p {
            Av::Enum(_, 1, v) => (Av::Bool(true), match &v[0] {
                Av::Struct(_, f) => (**f).clone(),
                _ => return fail("pending record"),
            }),
            Av::DEnum(_, d, ps) => (
                Av::D((**d).clone()),
                match ps[1].first() {
                    Some(Av::Struct(_, f)) => (**f).clone(),
                    _ => return fail("pending record"),
                },
            ),
            _ => return fail("pending"),
        };
        self.pe.emit(I::LocalGet(0));
        self.pe.push_as(&present, Kind::Bool)?;
        // target: Option<Int>, call: bool, slots: Int, return_from_call: bool,
        // return_sw: Option<Int>.
        self.push_opt(&fields[0])?;
        self.pe.push_as(&undef0(&fields[1]), Kind::Bool)?;
        self.pe.push_as(&undef0(&fields[2]), Kind::Int(IT::I128, Rep::I64))?;
        self.pe.push_as(&undef0(&fields[3]), Kind::Bool)?;
        self.push_opt(&fields[4])?;
        self.pe.emit(I::Call(mach::helper("jit_set_pending")));
        Ok(())
    }

    fn push_opt(&mut self, v: &Av) -> R<()> {
        match v {
            Av::Enum(_, 0, _) | Av::Undef => {
                self.pe.emit(I::I32Const(0));
                self.pe.emit(I::I64Const(0));
            }
            Av::Enum(_, 1, p) => {
                self.pe.emit(I::I32Const(1));
                self.pe.push_as(&p[0], Kind::Int(IT::I128, Rep::I64))?;
            }
            Av::DEnum(_, d, ps) => {
                self.pe.push_as(&Av::D((**d).clone()), Kind::Bool)?;
                let x = ps[1].first().cloned().unwrap_or(Av::Int(0, IT::I128));
                self.pe.push_as(&undef0(&x), Kind::Int(IT::I128, Rep::I64))?;
            }
            _ => return fail(format!("optional {v:?}")),
        }
        Ok(())
    }

    /// Code that leaves the region at block START before running anything
    /// (all registers are in their home locals).
    fn exit_code_static(&mut self, start: u32, code: i32) -> Vec<Node> {
        let _ = code;
        vec![
            Node::I(I::I64Const(start as i64)),
            Node::I(I::LocalSet(self.l_pc)),
            Node::I(I::I32Const(EXIT_BUDGET)),
            Node::I(I::LocalSet(self.pe_exit_code())),
            Node::Br(self.exit),
        ]
    }

    fn pe_exit_code(&mut self) -> u32 {
        if self.l_code == 0 {
            self.l_code = self.pe.local(W::I32);
        }
        self.l_code
    }

    /// Leave the region at PC after K instructions of the block: the
    /// interpreter runs PC next.
    fn exit_here(&mut self, pc: u32, k: u32) -> Result<(), String> {
        let ms = self.pe.ms.clone();
        self.write_homes(&ms).map_err(|e| e.0)?;
        self.store_pending(&ms.pending).map_err(|e| e.0)?;
        self.pe.emit(I::I64Const(pc as i64));
        self.pe.emit(I::LocalSet(self.l_pc));
        self.bump_icount(k);
        let lc = self.pe_exit_code();
        // The interpreter runs PC before any region is tried there again.
        self.pe.emit(I::I32Const(EXIT_BAIL));
        self.pe.emit(I::LocalSet(lc));
        self.pe.code.push(Node::Br(self.exit));
        Ok(())
    }

    /// Control goes to static P after K instructions.
    fn edge(&mut self, p: u32, k: u32) -> Result<(), String> {
        let internal = self.admit(p);
        let ms = self.pe.ms.clone();
        let mode_ok = match self.spec.mode1 {
            Some(m) => match &ms.r[MODE1 as usize] {
                Av::Struct(_, it) => it[0] == Av::Int(m as i128, IT::U32),
                _ => false,
            },
            None => true,
        };
        let masks_ok = (0..ms.r.len()).all(|c| {
            PARTIAL_REGS.contains(&(c as u32))
                || match &ms.r[c] {
                    Av::Struct(_, it) => it[1] == Av::Int(u32::MAX as i128, IT::U32),
                    _ => true,
                }
        });
        if !internal || ms.pending != none() || !mode_ok || !masks_ok {
            if self.pe.cx.trace {
                self.failures.push((
                    p,
                    format!(
                        "edge leaves: admitted {internal} pending-none {} mode1 {mode_ok} masks {masks_ok}",
                        ms.pending == none()
                    ),
                ));
            }
            self.pe.emit(I::I64Const(p as i64));
            self.pe.emit(I::LocalSet(self.l_pc));
            return self.leave(k, EXIT_NEXT);
        }
        let mut ms2 = ms.clone();
        if self.spec.mode1.is_some() {
            // MODE1 is the block's static fact, not a home local.
            ms2.r[MODE1 as usize] = Av::Undef;
        }
        self.write_homes_as(&ms2, false).map_err(|e| e.0)?;
        self.bump_icount(k);
        let idx = self.index[&p].idx;
        self.pe.emit(I::I32Const(idx as i32));
        self.pe.emit(I::LocalSet(self.l_bi));
        self.pe.code.push(Node::Br(self.top));
        Ok(())
    }

    /// Leave with the PC already in its local.
    fn leave(&mut self, k: u32, code: i32) -> Result<(), String> {
        let ms = self.pe.ms.clone();
        self.write_homes(&ms).map_err(|e| e.0)?;
        self.store_pending(&ms.pending).map_err(|e| e.0)?;
        self.bump_icount(k);
        let lc = self.pe_exit_code();
        self.pe.emit(I::I32Const(code));
        self.pe.emit(I::LocalSet(lc));
        self.pe.code.push(Node::Br(self.exit));
        Ok(())
    }

    /// Control goes to run-time PC D after K instructions.
    fn dyn_edge(&mut self, d: Dv, k: u32) -> Result<(), String> {
        let targets: Vec<i128> = d.vset.as_ref().map(|s| (**s).clone()).unwrap_or_default();
        for t in targets {
            if t < 0 || t > u32::MAX as i128 {
                continue;
            }
            // if pc == t { edge(t) }
            let saved = std::mem::take(&mut self.pe.code);
            let ms0 = self.pe.ms.clone();
            self.pe.ms.pc = Av::Int(t, IT::I128);
            self.edge(t as u32, k)?;
            let body = std::mem::replace(&mut self.pe.code, saved);
            self.pe.ms = ms0;
            self.pe.push_as(&Av::D(d.clone()), Kind::Int(IT::I128, Rep::I64)).map_err(|e| e.0)?;
            self.pe.emit(I::I64Const(t as i64));
            self.pe.emit(I::I64Eq);
            self.pe.code.push(Node::If(body, vec![]));
        }
        self.pe.push_as(&Av::D(d), Kind::Int(IT::I128, Rep::I64)).map_err(|e| e.0)?;
        self.pe.emit(I::LocalSet(self.l_pc));
        self.leave(k, EXIT_NEXT)
    }

    /// Whether block P is (now) part of the region.
    fn admit(&mut self, p: u32) -> bool {
        if self.index.contains_key(&p) {
            return true;
        }
        if !(self.spec.seen)(p)
            || self.index.len() >= self.spec.limits.max_blocks
            || self.insns >= self.spec.limits.max_insns
        {
            return false;
        }
        if self.covered.contains_key(&p) {
            // Inside a translated block: split it and start over.
            self.split = true;
        }
        let idx = self.starts.len() as u32;
        self.starts.push(p);
        self.index.insert(p, BlockState { idx });
        self.queue.push_back(p);
        true
    }

    fn finish(mut self) -> Result<Region, String> {
        let bodies = std::mem::take(&mut self.bodies);
        let lay = self.pe.cx.lay.clone();
        let lc = self.pe_exit_code();
        let mut code: Vec<Node> = Vec::new();
        // Prologue: home locals from St, mask and MODE1 guards.
        let homes: Vec<(u32, (u32, u32))> = self.pe.homes.iter().map(|(a, b)| (*a, *b)).collect();
        for &(c, (lb, lm)) in &homes {
            let off = lay.r + c * lay.vsize;
            code.push(Node::I(I::LocalGet(0)));
            code.push(Node::I(I::I32Load(mem((off + lay.vb) as u64, 2))));
            code.push(Node::I(I::LocalSet(lb)));
            code.push(Node::I(I::LocalGet(0)));
            code.push(Node::I(I::I32Load(mem((off + lay.vm) as u64, 2))));
            code.push(Node::I(I::LocalSet(lm)));
        }
        let mut guards: Vec<u32> = self.pe.needs_known.iter().copied().collect();
        if let Some(m) = self.spec.mode1 {
            let (lb, _) = self.pe.home(MODE1);
            if !homes.iter().any(|h| h.0 == MODE1) {
                let off = lay.r + MODE1 * lay.vsize;
                let (lb2, lm2) = self.pe.homes[&MODE1];
                code.push(Node::I(I::LocalGet(0)));
                code.push(Node::I(I::I32Load(mem((off + lay.vb) as u64, 2))));
                code.push(Node::I(I::LocalSet(lb2)));
                code.push(Node::I(I::LocalGet(0)));
                code.push(Node::I(I::I32Load(mem((off + lay.vm) as u64, 2))));
                code.push(Node::I(I::LocalSet(lm2)));
            }
            code.push(Node::I(I::LocalGet(lb)));
            code.push(Node::I(I::I32Const(m as i32)));
            code.push(Node::I(I::I32Ne));
            code.push(Node::If(vec![Node::I(I::I32Const(EXIT_BAIL)), Node::I(I::Return)], vec![]));
            if !guards.contains(&MODE1) {
                guards.push(MODE1);
            }
        }
        for c in guards {
            let (_, lm) = self.pe.homes[&c];
            code.push(Node::I(I::LocalGet(lm)));
            code.push(Node::I(I::I32Const(-1)));
            code.push(Node::I(I::I32Ne));
            code.push(Node::If(vec![Node::I(I::I32Const(EXIT_BAIL)), Node::I(I::Return)], vec![]));
        }
        code.push(Node::I(I::LocalGet(0)));
        code.push(Node::I(I::I64Load(mem(lay.icount as u64, 3))));
        code.push(Node::I(I::LocalSet(self.l_ic)));
        code.push(Node::I(I::LocalGet(0)));
        code.push(Node::I(I::I64Load(mem(lay.limit as u64, 3))));
        code.push(Node::I(I::LocalSet(self.l_lim)));
        code.push(Node::I(I::I32Const(0)));
        code.push(Node::I(I::LocalSet(self.l_bi)));
        // The dispatch loop over the blocks.
        let n = self.starts.len();
        let labels: Vec<Label> = (0..n).map(|_| self.pe.label()).collect();
        // Build nested blocks: block b_{n-1} { ... block b_0 { dispatch } code_0 } code_1 ...
        let mut cur = vec![Node::I(I::LocalGet(self.l_bi)), Node::Hole(u32::MAX)];
        for (k, &start) in self.starts.iter().enumerate() {
            let body = bodies.get(&start).cloned().unwrap_or_default();
            let mut next = vec![Node::Block(labels[k], cur)];
            next.extend(body);
            cur = next;
        }
        let dispatch_loop = Node::Loop(self.top, cur);
        let exit_block = Node::Block(self.exit, vec![dispatch_loop]);
        let trap_block = {
            let mut v = vec![exit_block];
            // Normal exit: write back and return the exit code.
            v.extend(self.writeback());
            v.push(Node::I(I::LocalGet(lc)));
            v.push(Node::I(I::Return));
            Node::Block(self.trap_exit, v)
        };
        code.push(trap_block);
        // Trap exit.
        code.extend(self.writeback());
        code.push(Node::I(I::LocalGet(0)));
        code.push(Node::I(I::Call(mach::helper("jit_rollback_log"))));
        code.push(Node::I(I::I32Const(EXIT_TRAP)));
        // Lower.
        let mut out = Vec::new();
        let mut stack = Vec::new();
        lower_with_table(&code, &self.pe.holes, &mut stack, &mut out, &labels, self.exit);
        let text = if self.pe.cx.trace { listing(&out) } else { String::new() };
        let wasm = module(&self.pe.loc.types, out);
        Ok(Region {
            wasm,
            blocks: self.starts.clone(),
            insns: self.insns,
            failures: self.failures,
            text,
        })
    }

    /// Store the registers the region writes, the PC and the instruction
    /// count back into St.
    fn writeback(&mut self) -> Vec<Node> {
        let lay = self.pe.cx.lay.clone();
        let mut v = Vec::new();
        let written: Vec<u32> = self.pe.written.iter().copied().collect();
        for c in written {
            let (lb, lm) = self.pe.homes[&c];
            let off = lay.r + c * lay.vsize;
            v.push(Node::I(I::LocalGet(0)));
            v.push(Node::I(I::LocalGet(lb)));
            v.push(Node::I(I::I32Store(mem((off + lay.vb) as u64, 2))));
            v.push(Node::I(I::LocalGet(0)));
            v.push(Node::I(I::LocalGet(lm)));
            v.push(Node::I(I::I32Store(mem((off + lay.vm) as u64, 2))));
        }
        // pc_sw is an i128: low word, then the sign extension.
        v.push(Node::I(I::LocalGet(0)));
        v.push(Node::I(I::LocalGet(self.l_pc)));
        v.push(Node::I(I::I64Store(mem(lay.pc as u64, 3))));
        v.push(Node::I(I::LocalGet(0)));
        v.push(Node::I(I::LocalGet(self.l_pc)));
        v.push(Node::I(I::I64Const(63)));
        v.push(Node::I(I::I64ShrS));
        v.push(Node::I(I::I64Store(mem((lay.pc + 8) as u64, 3))));
        v.push(Node::I(I::LocalGet(0)));
        v.push(Node::I(I::LocalGet(self.l_ic)));
        v.push(Node::I(I::I64Store(mem(lay.icount as u64, 3))));
        v
    }
}

fn undef0(v: &Av) -> Av {
    match v {
        Av::Undef => Av::Int(0, IT::I128),
        v => v.clone(),
    }
}

/// Lower, filling the dispatch hole (u32::MAX) with the br_table over the
/// block labels.
fn lower_with_table(
    nodes: &[Node],
    holes: &[Vec<Node>],
    stack: &mut Vec<Label>,
    out: &mut Vec<I<'static>>,
    labels: &[Label],
    exit: Label,
) {
    for n in nodes {
        match n {
            Node::Hole(h) if *h == u32::MAX => {
                let depth = |l: Label| -> u32 {
                    for (k, &x) in stack.iter().rev().enumerate() {
                        if x == l {
                            return k as u32;
                        }
                    }
                    panic!("label");
                };
                let targets: Vec<u32> = labels.iter().map(|&l| depth(l)).collect();
                out.push(I::BrTable(targets.into(), depth(exit)));
            }
            Node::Block(l, body) => {
                out.push(I::Block(wasm_encoder::BlockType::Empty));
                stack.push(*l);
                lower_with_table(body, holes, stack, out, labels, exit);
                stack.pop();
                out.push(I::End);
            }
            Node::Loop(l, body) => {
                out.push(I::Loop(wasm_encoder::BlockType::Empty));
                stack.push(*l);
                lower_with_table(body, holes, stack, out, labels, exit);
                stack.pop();
                out.push(I::End);
            }
            Node::If(a, b) => {
                out.push(I::If(wasm_encoder::BlockType::Empty));
                stack.push(u32::MAX - 1);
                lower_with_table(a, holes, stack, out, labels, exit);
                if !b.is_empty() {
                    out.push(I::Else);
                    lower_with_table(b, holes, stack, out, labels, exit);
                }
                stack.pop();
                out.push(I::End);
            }
            Node::Hole(h) => lower_with_table(&holes[*h as usize], holes, stack, out, labels, exit),
            other => ir::lower(std::slice::from_ref(other), holes, stack, out),
        }
    }
}

/// The region module: imports the runtime's memory and helpers, exports
/// the region function "r" (st) -> exit code.
pub fn module(locals: &[W], body: Vec<I<'static>>) -> Vec<u8> {
    let mut types = TypeSection::new();
    for (_, params, results) in mach::HELPERS {
        types.ty().function(params.iter().map(|w| w.val()), results.iter().map(|w| w.val()));
    }
    let region_ty = mach::HELPERS.len() as u32;
    types.ty().function([ValType::I32], [ValType::I32]);
    let mut imports = ImportSection::new();
    imports.import(
        "env",
        "memory",
        EntityType::Memory(MemoryType {
            minimum: 1,
            maximum: None,
            memory64: false,
            shared: false,
            page_size_log2: None,
        }),
    );
    for (k, (name, _, _)) in mach::HELPERS.iter().enumerate() {
        imports.import("env", name, EntityType::Function(k as u32));
    }
    let mut funcs = FunctionSection::new();
    funcs.function(region_ty);
    let mut exports = ExportSection::new();
    exports.export("r", ExportKind::Func, mach::HELPERS.len() as u32);
    // Locals, run-length grouped (in order: deterministic).
    let mut groups: Vec<(u32, ValType)> = Vec::new();
    for w in locals {
        let v = w.val();
        match groups.last_mut() {
            Some((n, t)) if *t == v => *n += 1,
            _ => groups.push((1, v)),
        }
    }
    let mut f = Function::new(groups);
    for i in &body {
        f.instruction(i);
    }
    f.instruction(&I::End);
    let mut code = CodeSection::new();
    code.function(&f);
    let mut m = Module::new();
    m.section(&types);
    m.section(&imports);
    m.section(&funcs);
    m.section(&exports);
    m.section(&code);
    m.finish()
}
