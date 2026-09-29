//! The machine state as the translated code sees it. Registers, the PC and
//! the pending delayed transfer live in the abstract state (registers in
//! WebAssembly locals from region entry to exit); the special registers,
//! stacks and memory stay in the runtime's `St`, read inline or through
//! runtime helpers (which keep the undo log).

use super::eval::{Fail, Flow, Pe, R, fail};
use super::ir::{Node, W, mem};
use super::types::RTy;
use super::val::*;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use wasm_encoder::Instruction as I;

/// Registers whose known-bit mask stays a run-time value (ASTATX, ASTATY:
/// the CACC compare history is never known). Every other register must be
/// fully known where region code holds it.
pub const PARTIAL_REGS: [u32; 2] = [118, 119];
pub const NUREG: usize = 128;
pub const MODE1: u32 = 114;

/// Offsets into the runtime's `St` (and the scratch area's address),
/// reported by the runtime module (`jit_layout`).
#[derive(Clone, Debug, Default)]
pub struct Layout {
    pub r: u32,
    pub vb: u32,
    pub vm: u32,
    pub vsize: u32,
    pub pc: u32,
    pub icount: u32,
    pub limit: u32,
    pub un: u32,
    pub loops_n: u32,
    pub loops_a: u32,
    pub loop_size: u32,
    pub loop_start: u32,
    pub loop_end: u32,
    pub loop_rem: u32,
    pub loop_mode: u32,
    pub calls_n: u32,
    pub calls_a: u32,
    pub call_size: u32,
    pub status_n: u32,
    /// Absolute address of a 64-byte scratch area for helper results.
    pub scratch: u32,
}

impl Layout {
    pub const FIELDS: usize = 20;
    pub fn from_words(w: &[u32]) -> Layout {
        Layout {
            r: w[0],
            vb: w[1],
            vm: w[2],
            vsize: w[3],
            pc: w[4],
            icount: w[5],
            limit: w[6],
            un: w[7],
            loops_n: w[8],
            loops_a: w[9],
            loop_size: w[10],
            loop_start: w[11],
            loop_end: w[12],
            loop_rem: w[13],
            loop_mode: w[14],
            calls_n: w[15],
            calls_a: w[16],
            call_size: w[17],
            status_n: w[18],
            scratch: w[19],
        }
    }
}

/// The run configuration the code is translated for (rt.rs Cfg).
#[derive(Clone, Debug, PartialEq)]
pub struct CfgVals {
    pub has_concrete: bool,
    pub follow_loaded_calls: bool,
    pub continue_external_calls: bool,
    pub max_call_depth: i128,
    pub assume_nw32: bool,
    pub explicit_memory_model: bool,
    pub approx_recips: bool,
    pub data_memory_tainted: bool,
    pub dossier_bytes: i128,
    pub fast_mem: bool,
}

impl Default for CfgVals {
    fn default() -> Self {
        CfgVals {
            has_concrete: true,
            follow_loaded_calls: true,
            continue_external_calls: false,
            max_call_depth: 64,
            assume_nw32: true,
            explicit_memory_model: true,
            approx_recips: true,
            data_memory_tainted: false,
            dossier_bytes: 0,
            fast_mem: true,
        }
    }
}

/// A decoded instruction (sharc-decode output, symbols interned).
#[derive(Clone, Debug)]
pub struct DInsn {
    pub type_name: String,
    pub kind: &'static str,
    pub length_bytes: Option<u32>,
    pub fields: Vec<(String, i64)>,
}

pub trait InsnSource {
    fn insn(&self, pc: u32) -> Option<Rc<DInsn>>;
}

/// The runtime helpers region modules import from the runtime (module
/// "env"), in import order: name, parameters, results.
pub const HELPERS: &[(&str, &[W], &[W])] = &[
    // (st, view, key) -> 0 None / 1 Some(V) in scratch / 2 Some(MR) in scratch
    ("jit_sv_get", &[W::I32, W::I32, W::I32], &[W::I32]),
    // (st, key, kind 1 V | 2 MR, from scratch) -> 0 ok / -1 trap
    ("jit_set_special", &[W::I32, W::I32, W::I32], &[W::I32]),
    // (st, akind, a, amask, width, signed) -> <0 trap / 0 None / 1<<32|b
    ("jit_dm_read", &[W::I32, W::I32, W::I64, W::I32, W::I32, W::I32], &[W::I64]),
    // (st, akind, a, amask, width, b, m) -> -1 trap / 0 false / 1 true
    ("jit_dm_write", &[W::I32, W::I32, W::I64, W::I32, W::I32, W::I32, W::I32], &[W::I32]),
    // (st, akind, a, amask) -> -1 trap / 0 None / 1 Some (two V in scratch)
    ("jit_read_px48", &[W::I32, W::I32, W::I64, W::I32], &[W::I32]),
    // (st, start, end, rem, mode) -> 0 / -1
    ("jit_push_loop", &[W::I32, W::I64, W::I64, W::I64, W::I64], &[W::I32]),
    // (st) -> 0 (Loop in scratch) / -1
    ("jit_pop_loop", &[W::I32], &[W::I32]),
    // (st, i, start, end, rem, mode) -> 0 / -1
    ("jit_set_loop", &[W::I32, W::I64, W::I64, W::I64, W::I64, W::I64], &[W::I32]),
    // (st, v) -> 0 / -1
    ("jit_push_call", &[W::I32, W::I64], &[W::I32]),
    // (st) -> 0 (value in scratch) / -1
    ("jit_pop_call", &[W::I32], &[W::I32]),
    // (st, i, v) -> 0 / -1
    ("jit_set_call", &[W::I32, W::I64, W::I64], &[W::I32]),
    // (st) -> 0 (three V from scratch) / -1
    ("jit_push_status", &[W::I32], &[W::I32]),
    // (st) -> 0 (three V into scratch) / -1
    ("jit_pop_status", &[W::I32], &[W::I32]),
    // (st, present, tpresent, target, call, slots, rfc, rpresent, rsw)
    ("jit_set_pending", &[W::I32, W::I32, W::I32, W::I64, W::I32, W::I64, W::I32, W::I32, W::I64], &[]),
    // (st): undo the current instruction's logged changes
    ("jit_rollback_log", &[W::I32], &[]),
    // (st, pc) -> the kind symbol of the instruction at pc, or -1 (none)
    ("jit_insn_kind", &[W::I32, W::I64], &[W::I32]),
    // 128-bit arithmetic, the result in scratch[0..16]:
    // (alo, ahi, blo, bhi) product; (lo, hi, n) left shift; (lo, hi, n,
    // signed) right shift.
    ("jit_mul128", &[W::I64, W::I64, W::I64, W::I64], &[]),
    ("jit_shl128", &[W::I64, W::I64, W::I32], &[]),
    ("jit_shr128", &[W::I64, W::I64, W::I32, W::I32], &[]),
    // Debugging (Ctx::trace): (st, code, b, m) a register after an
    // instruction; (st, pc) the instruction's end.
    ("jit_trace", &[W::I32, W::I32, W::I32, W::I32], &[]),
    ("jit_trace_end", &[W::I32, W::I64], &[]),
];

pub fn helper(name: &str) -> u32 {
    HELPERS.iter().position(|h| h.0 == name).expect("helper") as u32
}

pub struct Ctx<'a> {
    /// Emit jit_trace calls after every instruction (debugging).
    pub trace: bool,
    pub cfg: CfgVals,
    pub lay: Layout,
    pub loop_ends: Rc<Vec<i128>>,
    pub insns: &'a dyn InsnSource,
    pub syms: BTreeMap<String, u16>,
    pub fresh: RefCell<BTreeMap<String, u16>>,
}

impl<'a> Ctx<'a> {
    pub fn sym(&self, name: &str) -> u16 {
        if let Some(&s) = self.syms.get(name) {
            return s;
        }
        let mut f = self.fresh.borrow_mut();
        let n = f.len() as u16;
        *f.entry(name.to_string()).or_insert(60000u16.wrapping_add(n))
    }

    pub fn fields_none(&self) -> Av {
        st("Fields", vec![Av::Arr(Rc::new(vec![]))])
    }

    pub fn insn_none(&self) -> Av {
        let unk = self.sym("unknown") as i128;
        st(
            "Insn",
            vec![
                Av::Int(unk, IT::U16),
                self.fields_none(),
                none(),
                Av::Int(unk, IT::U16),
                Av::Int(0, IT::I128),
            ],
        )
    }

    /// An instruction as the core's `Insn` record.
    pub fn insn_av(&self, d: &DInsn) -> Av {
        let mut entries = Vec::new();
        for (key, value) in &d.fields {
            let (stem, range) = match key.find('[') {
                Some(i) => (&key[..i], &key[i..]),
                None => (key.as_str(), ""),
            };
            let (mut hi, mut lo) = (-1i128, -1i128);
            if let Some(inner) = range.strip_prefix('[').and_then(|x| x.strip_suffix(']'))
                && let Some((h, l)) = inner.split_once(':')
                && let (Ok(h), Ok(l)) = (h.parse::<i128>(), l.parse::<i128>())
            {
                hi = h;
                lo = l;
            }
            entries.push(st(
                "FieldEntry",
                vec![
                    Av::Int(self.sym(key) as i128, IT::U16),
                    Av::Int(self.sym(stem) as i128, IT::U16),
                    Av::Int(hi, IT::I8),
                    Av::Int(lo, IT::I8),
                    Av::Int(*value as i128, IT::I128),
                ],
            ));
        }
        st(
            "Insn",
            vec![
                Av::Int(self.sym(&d.type_name) as i128, IT::U16),
                st("Fields", vec![Av::Arr(Rc::new(entries))]),
                match d.length_bytes {
                    Some(n) => some(Av::Int(n as i128, IT::I128)),
                    None => none(),
                },
                Av::Int(self.sym(d.kind) as i128, IT::U16),
                Av::Int(0, IT::I128),
            ],
        )
    }
}

/// The abstract machine state. A register that is Undef holds its value at
/// region entry (its home local).
#[derive(Clone, Debug, Default)]
pub struct MState {
    pub r: Vec<Av>,
    /// The `_snapshot_uregs` view (the register file at the snapshot).
    pub old: Vec<Av>,
    pub pc: Av,
    pub pending: Av,
    /// A helper that logs undo records ran in this instruction.
    pub logged: bool,
}

impl MState {
    pub fn new() -> MState {
        MState {
            r: vec![Av::Undef; NUREG],
            old: vec![Av::Undef; NUREG],
            pc: Av::Undef,
            pending: none(),
            logged: false,
        }
    }
}

impl Default for Av {
    fn default() -> Self {
        Av::Undef
    }
}

/// Paths the evaluator handles itself instead of evaluating their Rust
/// body (state access, runtime helpers, generic helpers the parser skips).
const INTRINSICS: &[&str] = &[
    "rt::rv_get",
    "rt::s_set_r",
    "St::set_r",
    "St::snapshot",
    "St::old_of",
    "rt::sv_get",
    "rt::s_set_special",
    "rt::bnd::_dm_read",
    "rt::bnd::_dm_read_b",
    "rt::bnd::_dm_write",
    "rt::bnd::_dm_write_b",
    "rt::bnd::_read_px48",
    "rt::bnd::decode_at",
    "Cfg::provisional_get",
    "stk_len_loops",
    "stk_at_loops",
    "stk_push_loops",
    "stk_pop_loops",
    "stk_set_loops",
    "stk_len_call_stack",
    "stk_at_call_stack",
    "stk_push_call_stack",
    "stk_pop_call_stack",
    "stk_set_call_stack",
    "stk_len_status_stack",
    "stk_push_status_stack",
    "stk_pop_status_stack",
    "Tup::from_slice",
    "Tup::new",
    "tup_concat",
    "tup_index",
    "f64::from_bits",
    "f32::from_bits",
];

impl<'p> Pe<'p> {
    pub fn is_intrinsic(&self, key: &str) -> bool {
        INTRINSICS.contains(&key)
    }

    pub fn intrinsic_hints(&self, key: &str) -> Vec<Option<RTy>> {
        let int = || Some(RTy::Int(IT::I128));
        match key {
            "rt::rv_get" => vec![None, None, int()],
            "rt::s_set_r" => vec![None, int(), None],
            "St::set_r" => vec![Some(RTy::Int(IT::Usize)), None],
            "rt::bnd::_dm_read" | "rt::bnd::_dm_read_b" => vec![None, None, int(), None],
            "rt::bnd::_dm_write" | "rt::bnd::_dm_write_b" => vec![None, None, int(), None],
            "rt::bnd::decode_at" => vec![None, None, None, int()],
            "stk_at_loops" | "stk_at_call_stack" => vec![None, int()],
            "stk_set_loops" | "stk_set_call_stack" => vec![None, int(), int()],
            "stk_push_call_stack" => vec![None, int()],
            "f64::from_bits" => vec![Some(RTy::Int(IT::U64))],
            "f32::from_bits" => vec![Some(RTy::Int(IT::U32))],
            _ => vec![],
        }
    }

    pub fn intrinsic(&mut self, key: &str, a: Vec<Av>, hint: Option<&RTy>) -> R<Flow> {
        let lay = self.cx.lay.clone();
        let v = match key {
            "rt::rv_get" => self.reg_read(&a[1], &a[2])?,
            "St::old_of" => {
                let c = a[1].as_int().ok_or_else(|| Fail("old_of index".into()))?;
                self.reg_get(c as u32, true)?
            }
            "rt::s_set_r" | "St::set_r" => {
                let (code, v) = if key == "St::set_r" { (&a[1], &a[2]) } else { (&a[1], &a[2]) };
                let Some(c) = code.as_int() else {
                    return fail("register write with a run-time number");
                };
                if !(0..NUREG as i128).contains(&c) {
                    self.code.push(Node::Br(self.trap));
                    return Ok(Flow::Div);
                }
                self.reg_write(c as u32, v.clone())?;
                ok(Av::Unit)
            }
            "St::snapshot" => {
                self.ms.old = self.ms.r.clone();
                Av::Unit
            }
            "rt::sv_get" => self.sv_get(&a[1], &a[2])?,
            "rt::s_set_special" => {
                self.set_special(&a[1], &a[2])?;
                ok(Av::Unit)
            }
            "rt::bnd::_dm_read" | "rt::bnd::_dm_read_b" => {
                let (akind, am) = self.push_address(&a[1])?;
                let _ = (akind, am);
                let width = a[2].as_int().ok_or_else(|| Fail("read width".into()))?;
                let signed = a[3].as_bool().ok_or_else(|| Fail("read signedness".into()))?;
                self.emit(I::I32Const(width as i32));
                self.emit(I::I32Const(signed as i32));
                self.emit(I::Call(helper("jit_dm_read")));
                let r = self.local(W::I64);
                self.emit(I::LocalTee(r));
                self.emit(I::I64Const(0));
                self.emit(I::I64LtS);
                self.code.push(Node::BrIf(self.trap));
                // Some when bit 32 is set.
                self.emit(I::LocalGet(r));
                self.emit(I::I64Const(32));
                self.emit(I::I64ShrU);
                self.emit(I::I32WrapI64);
                let disc = self.def(Kind::Bool, 0, 1);
                self.emit(I::LocalGet(r));
                self.emit(I::I32WrapI64);
                let b = self.def(Kind::Int(IT::U32, Rep::I32), 0, u32::MAX as i128);
                let v = st("V", vec![Av::D(b), Av::Int(u32::MAX as i128, IT::U32)]);
                let mut d = disc;
                d.k = Kind::Int(IT::U32, Rep::I32);
                d.vset = Some(Rc::new(vec![0, 1]));
                ok(Av::DEnum("Option".into(), Box::new(d), Rc::new(vec![vec![], vec![v]])))
            }
            "rt::bnd::_dm_write" | "rt::bnd::_dm_write_b" => {
                self.push_address(&a[1])?;
                let width = a[2].as_int().ok_or_else(|| Fail("write width".into()))?;
                self.emit(I::I32Const(width as i32));
                let (b, m) = v_parts(&a[3])?;
                self.push_as(&b, Kind::Int(IT::U32, Rep::I32))?;
                self.push_as(&m, Kind::Int(IT::U32, Rep::I32))?;
                self.emit(I::Call(helper("jit_dm_write")));
                self.ms.logged = true;
                let r = self.local(W::I32);
                self.emit(I::LocalTee(r));
                self.emit(I::I32Const(0));
                self.emit(I::I32LtS);
                self.code.push(Node::BrIf(self.trap));
                self.emit(I::LocalGet(r));
                ok(self.def_bool())
            }
            "rt::bnd::_read_px48" => {
                self.push_address(&a[1])?;
                self.emit(I::Call(helper("jit_read_px48")));
                let r = self.local(W::I32);
                self.emit(I::LocalTee(r));
                self.emit(I::I32Const(0));
                self.emit(I::I32LtS);
                self.code.push(Node::BrIf(self.trap));
                self.emit(I::LocalGet(r));
                let mut d = self.def(Kind::Int(IT::U32, Rep::I32), 0, 1);
                d.vset = Some(Rc::new(vec![0, 1]));
                let v1 = self.load_scratch_v(0)?;
                let v2 = self.load_scratch_v(8)?;
                ok(Av::DEnum(
                    "Option".into(),
                    Box::new(d),
                    Rc::new(vec![vec![], vec![Av::Tuple(Rc::new(vec![v1, v2]))]]),
                ))
            }
            "rt::bnd::decode_at" => {
                let Some(pc) = a[3].as_int() else {
                    // Only the kind is known: an instruction record whose
                    // other fields are absent (reading one fails).
                    self.emit(I::LocalGet(0));
                    self.push_as(&a[3], Kind::Int(IT::I128, Rep::I64))?;
                    self.emit(I::Call(helper("jit_insn_kind")));
                    let k = self.local(W::I32);
                    self.emit(I::LocalTee(k));
                    self.emit(I::I32Const(0));
                    self.emit(I::I32LtS);
                    self.code.push(Node::BrIf(self.trap));
                    self.emit(I::LocalGet(k));
                    let kind = self.def_int(IT::U16, Rep::I32, 0, 65535);
                    return Ok(Flow::V(ok(st("Insn", vec![Av::Undef, Av::Undef, Av::Undef, kind, Av::Undef]))));
                };
                match u32::try_from(pc).ok().and_then(|p| self.cx.insns.insn(p)) {
                    Some(d) if d.length_bytes.is_some() => ok(self.cx.insn_av(&d)),
                    _ => {
                        self.code.push(Node::Br(self.trap));
                        return Ok(Flow::Div);
                    }
                }
            }
            "Cfg::provisional_get" => none(),
            "stk_len_loops" => self.load_len(lay.loops_n, 16)?,
            "stk_len_call_stack" => self.load_len(lay.calls_n, 256)?,
            "stk_len_status_stack" => self.load_len(lay.status_n, 16)?,
            "stk_at_loops" => {
                let Some(addr) = self.stack_elem(lay.loops_n, lay.loops_a, lay.loop_size, &a[1])? else {
                    return Ok(Flow::Div);
                };
                let f = |pe: &mut Self, off: u32, vset: Option<Rc<Vec<i128>>>| -> Av {
                    pe.emit(I::LocalGet(addr));
                    pe.emit(I::I64Load(mem((lay.loops_a + off) as u64, 3)));
                    // 32-bit machine quantities (PCs, counts) stored as i64.
                    let mut d = pe.def(Kind::Int(IT::I64, Rep::I64), -(1i128 << 40), 1i128 << 40);
                    d.vset = vset;
                    Av::D(d)
                };
                let s = f(self, lay.loop_start, None);
                let e = f(self, lay.loop_end, Some(self.cx.loop_ends.clone()));
                let r = f(self, lay.loop_rem, None);
                let m = f(self, lay.loop_mode, None);
                ok(st("Loop", vec![s, e, r, m]))
            }
            "stk_at_call_stack" => {
                let Some(addr) = self.stack_elem(lay.calls_n, lay.calls_a, lay.call_size, &a[1])? else {
                    return Ok(Flow::Div);
                };
                self.emit(I::LocalGet(addr));
                self.emit(I::I64Load(mem(lay.calls_a as u64, 3)));
                // Return addresses: PCs.
                ok(self.def_int(IT::I128, Rep::I64, -(1i128 << 40), 1i128 << 40))
            }
            "stk_push_loops" | "stk_set_loops" => {
                let lp = if key == "stk_push_loops" { &a[1] } else { &a[2] };
                let items = match lp {
                    Av::Struct(n, it) if &**n == "Loop" => it.clone(),
                    _ => return fail("loop record"),
                };
                self.emit(I::LocalGet(0));
                if key == "stk_set_loops" {
                    self.push_as(&a[1], Kind::Int(IT::I64, Rep::I64))?;
                }
                for x in items.iter() {
                    self.push_as(x, Kind::Int(IT::I64, Rep::I64))?;
                }
                self.emit(I::Call(helper(if key == "stk_push_loops" { "jit_push_loop" } else { "jit_set_loop" })));
                self.ms.logged = true;
                self.trap_if_negative();
                ok(Av::Unit)
            }
            "stk_pop_loops" => {
                self.emit(I::LocalGet(0));
                self.emit(I::Call(helper("jit_pop_loop")));
                self.ms.logged = true;
                self.trap_if_negative();
                let mut items = Vec::new();
                for k in 0..4u32 {
                    self.emit(I::I32Const(lay.scratch as i32));
                    self.emit(I::I64Load(mem((8 * k) as u64, 3)));
                    items.push(self.def_int(IT::I64, Rep::I64, -(1i128 << 40), 1i128 << 40));
                }
                if let Av::D(d) = &mut items[1] {
                    d.vset = Some(self.cx.loop_ends.clone());
                }
                ok(st("Loop", items))
            }
            "stk_push_call_stack" | "stk_set_call_stack" => {
                self.emit(I::LocalGet(0));
                if key == "stk_set_call_stack" {
                    self.push_as(&a[1], Kind::Int(IT::I64, Rep::I64))?;
                    self.push_as(&a[2], Kind::Int(IT::I128, Rep::I64))?;
                } else {
                    self.push_as(&a[1], Kind::Int(IT::I128, Rep::I64))?;
                }
                self.emit(I::Call(helper(if key == "stk_push_call_stack" { "jit_push_call" } else { "jit_set_call" })));
                self.ms.logged = true;
                self.trap_if_negative();
                ok(Av::Unit)
            }
            "stk_pop_call_stack" => {
                self.emit(I::LocalGet(0));
                self.emit(I::Call(helper("jit_pop_call")));
                self.ms.logged = true;
                self.trap_if_negative();
                self.emit(I::I32Const(lay.scratch as i32));
                self.emit(I::I64Load(mem(0, 3)));
                ok(self.def_int(IT::I128, Rep::I64, -(1i128 << 40), 1i128 << 40))
            }
            "stk_push_status_stack" => {
                let items = match &a[1] {
                    Av::Tuple(it) => it.clone(),
                    _ => return fail("status triple"),
                };
                for (k, x) in items.iter().enumerate() {
                    let (b, m) = v_parts(x)?;
                    self.emit(I::I32Const(lay.scratch as i32));
                    self.push_as(&b, Kind::Int(IT::U32, Rep::I32))?;
                    self.emit(I::I32Store(mem((8 * k) as u64, 2)));
                    self.emit(I::I32Const(lay.scratch as i32));
                    self.push_as(&m, Kind::Int(IT::U32, Rep::I32))?;
                    self.emit(I::I32Store(mem((8 * k + 4) as u64, 2)));
                }
                self.emit(I::LocalGet(0));
                self.emit(I::Call(helper("jit_push_status")));
                self.ms.logged = true;
                self.trap_if_negative();
                ok(Av::Unit)
            }
            "stk_pop_status_stack" => {
                self.emit(I::LocalGet(0));
                self.emit(I::Call(helper("jit_pop_status")));
                self.ms.logged = true;
                self.trap_if_negative();
                let mut vs = Vec::new();
                for k in 0..3 {
                    vs.push(self.load_scratch_v(8 * k)?);
                }
                ok(Av::Tuple(Rc::new(vs)))
            }
            "Tup::new" => Av::Tup(Rc::new(vec![])),
            "Tup::from_slice" => match &a[0] {
                Av::Arr(items) => Av::Tup(Rc::new(items.iter().take(8).cloned().collect())),
                other => return fail(format!("Tup::from_slice of {other:?}")),
            },
            "tup_concat" => match (&a[0], &a[1]) {
                (Av::Tup(x), Av::Tup(y)) => {
                    let mut v = (**x).clone();
                    v.extend(y.iter().cloned());
                    v.truncate(8);
                    Av::Tup(Rc::new(v))
                }
                _ => return fail("tup_concat"),
            },
            "tup_index" => {
                let items = match &a[0] {
                    Av::Arr(v) | Av::Tup(v) => v.clone(),
                    _ => return fail("tup_index"),
                };
                let Some(i) = a[1].as_int() else {
                    return fail("tup_index at a run-time index");
                };
                let n = items.len() as i128;
                let j = if i < 0 { i + n } else { i };
                if j < 0 || j >= n {
                    self.code.push(Node::Br(self.trap));
                    return Ok(Flow::Div);
                }
                ok(items[j as usize].clone())
            }
            "f64::from_bits" => match &a[0] {
                Av::Int(x, _) => Av::F64(f64::from_bits(*x as u64)),
                v => {
                    self.push_as(v, Kind::Int(IT::U64, Rep::I64))?;
                    self.emit(I::F64ReinterpretI64);
                    Av::D(self.def(Kind::F64, 0, 0))
                }
            },
            "f32::from_bits" => match &a[0] {
                Av::Int(x, _) => Av::F32(f32::from_bits(*x as u32)),
                v => {
                    self.push_as(v, Kind::Int(IT::U32, Rep::I32))?;
                    self.emit(I::F32ReinterpretI32);
                    Av::D(self.def(Kind::F32, 0, 0))
                }
            },
            _ => return fail(format!("intrinsic {key}")),
        };
        let _ = hint;
        Ok(Flow::V(v))
    }

    fn trap_if_negative(&mut self) {
        self.emit(I::I32Const(0));
        self.emit(I::I32LtS);
        self.code.push(Node::BrIf(self.trap));
    }

    fn load_scratch_v(&mut self, off: u32) -> R<Av> {
        let s = self.cx.lay.scratch as i32;
        self.emit(I::I32Const(s));
        self.emit(I::I32Load(mem(off as u64, 2)));
        let b = self.def(Kind::Int(IT::U32, Rep::I32), 0, u32::MAX as i128);
        self.emit(I::I32Const(s));
        self.emit(I::I32Load(mem((off + 4) as u64, 2)));
        let m = self.def(Kind::Int(IT::U32, Rep::I32), 0, u32::MAX as i128);
        Ok(st("V", vec![Av::D(b), Av::D(m)]))
    }

    /// A stack length (usize in St) as an Int.
    fn load_len(&mut self, off: u32, cap: i128) -> R<Av> {
        self.emit(I::LocalGet(0));
        self.emit(I::I32Load(mem(off as u64, 2)));
        self.emit(I::I64ExtendI32U);
        Ok(self.def_int(IT::I128, Rep::I64, 0, cap))
    }

    /// The address (St-relative base in a local) of stack element I
    /// (negative from the top), after a bounds check that traps. None when
    /// the access statically traps.
    fn stack_elem(&mut self, n_off: u32, _a_off: u32, size: u32, i: &Av) -> R<Option<u32>> {
        let Some(i) = i.as_int() else {
            return fail("stack index at run time");
        };
        let n = self.local(W::I32);
        self.emit(I::LocalGet(0));
        self.emit(I::I32Load(mem(n_off as u64, 2)));
        self.emit(I::LocalSet(n));
        // j = i < 0 ? n + i : i; trap unless 0 <= j < n.
        let j = self.local(W::I32);
        if i < 0 {
            self.emit(I::LocalGet(n));
            self.emit(I::I32Const(i as i32));
            self.emit(I::I32Add);
        } else {
            self.emit(I::I32Const(i as i32));
        }
        self.emit(I::LocalTee(j));
        self.emit(I::LocalGet(n));
        self.emit(I::I32GeU);
        self.code.push(Node::BrIf(self.trap));
        let addr = self.local(W::I32);
        self.emit(I::LocalGet(0));
        self.emit(I::LocalGet(j));
        self.emit(I::I32Const(size as i32));
        self.emit(I::I32Mul);
        self.emit(I::I32Add);
        self.emit(I::LocalSet(addr));
        Ok(Some(addr))
    }

    /// Push (st, akind, a, amask) for a VI address argument.
    fn push_address(&mut self, a: &Av) -> R<(i32, i32)> {
        self.emit(I::LocalGet(0));
        match a {
            Av::Enum(n, 0, p) if &**n == "VI" => {
                let (b, m) = v_parts(&p[0])?;
                self.emit(I::I32Const(0));
                self.push_as(&b, Kind::Int(IT::U32, Rep::I32))?;
                self.emit(I::I64ExtendI32U);
                self.push_as(&m, Kind::Int(IT::U32, Rep::I32))?;
                Ok((0, 0))
            }
            Av::Enum(n, 1, p) if &**n == "VI" => {
                let (_, r) = super::ops::int_info(&p[0]);
                if r.0 < i64::MIN as i128 || r.1 > i64::MAX as i128 {
                    return fail("memory address outside 64 bits");
                }
                self.emit(I::I32Const(1));
                self.push_as(&p[0], Kind::Int(IT::I128, Rep::I64))?;
                self.emit(I::I32Const(0));
                Ok((1, 0))
            }
            _ => fail(format!("memory address {a:?}")),
        }
    }

    // ------------------------------------------------------------ registers

    /// The Av of register C at region entry (its home local).
    pub fn entry_reg(&mut self, c: u32) -> R<Av> {
        let (lb, lm) = self.home(c);
        let b = Av::D(Dv {
            l: lb,
            l2: lb,
            k: Kind::Int(IT::U32, Rep::I32),
            lo: 0,
            hi: u32::MAX as i128,
            vset: None,
        });
        let m = if PARTIAL_REGS.contains(&c) {
            Av::D(Dv {
                l: lm,
                l2: lm,
                k: Kind::Int(IT::U32, Rep::I32),
                lo: 0,
                hi: u32::MAX as i128,
                vset: None,
            })
        } else {
            self.needs_known.insert(c);
            Av::Int(u32::MAX as i128, IT::U32)
        };
        Ok(st("V", vec![b, m]))
    }

    /// The home locals of register C (value, mask).
    pub fn home(&mut self, c: u32) -> (u32, u32) {
        if let Some(&h) = self.homes.get(&c) {
            return h;
        }
        let lb = self.local(W::I32);
        let lm = self.local(W::I32);
        self.homes.insert(c, (lb, lm));
        (lb, lm)
    }

    pub fn reg_get(&mut self, c: u32, old: bool) -> R<Av> {
        let v = if old { self.ms.old[c as usize].clone() } else { self.ms.r[c as usize].clone() };
        if let Av::Undef = v {
            return self.entry_reg(c);
        }
        Ok(v)
    }

    fn reg_read(&mut self, view: &Av, code: &Av) -> R<Av> {
        let view = match view {
            Av::Struct(n, it) if &**n == "RegView" => it[0].as_int().ok_or_else(|| Fail("run-time register view".into()))?,
            _ => return fail(format!("register view {view:?}")),
        };
        let Some(c) = code.as_int() else {
            return fail("register read with a run-time number");
        };
        if !(0..NUREG as i128).contains(&c) {
            return Ok(st("V", vec![Av::Int(0, IT::U32), Av::Int(0, IT::U32)]));
        }
        let mut c = c as u32;
        if view & 2 != 0 && c < 16 {
            c += 80;
        }
        self.reg_get(c, view & 1 != 0)
    }

    pub fn reg_write(&mut self, c: u32, v: Av) -> R<()> {
        let (b, m) = v_parts(&v)?;
        let m = if PARTIAL_REGS.contains(&c) {
            m
        } else {
            match m {
                Av::Int(..) => m,
                _ => {
                    // A value that may not be fully known: block code keeps
                    // masks constant, so it leaves (the interpreter runs
                    // the instruction).
                    self.push_as(&m, Kind::Int(IT::U32, Rep::I32))?;
                    self.emit(I::I32Const(-1));
                    self.emit(I::I32Ne);
                    self.code.push(Node::BrIf(self.trap));
                    Av::Int(u32::MAX as i128, IT::U32)
                }
            }
        };
        self.ms.r[c as usize] = st("V", vec![b, m]);
        Ok(())
    }

    // ----------------------------------------------------- special registers

    fn sv_get(&mut self, view: &Av, key: &Av) -> R<Av> {
        let view = match view {
            Av::Struct(n, it) if &**n == "SpecView" => it[0].as_int().ok_or_else(|| Fail("spec view".into()))?,
            _ => return fail(format!("special view {view:?}")),
        };
        // NONE and EMPTY: nothing (PEY_EMPTY: MRF -> Unknown).
        let key = key.as_int().ok_or_else(|| Fail("special key at run time".into()))?;
        let mrf = self.cx.sym("MRF") as i128;
        match view {
            0 | 2 => return Ok(none()),
            4 => {
                return Ok(if key == mrf { some(spec_v(v_unknown())) } else { none() });
            }
            _ => {}
        }
        self.emit(I::LocalGet(0));
        self.emit(I::I32Const(view as i32));
        self.emit(I::I32Const(key as i32));
        self.emit(I::Call(helper("jit_sv_get")));
        let status = self.def(Kind::Int(IT::U32, Rep::I32), 0, 2);
        // Both payloads are read (the one the status does not name is
        // never used): Spec::V's V at 0, Spec::M's MR (mask, bits) at 0/16.
        let v = self.load_scratch_v(0)?;
        let mr = self.load_scratch_mr()?;
        self.emit(I::LocalGet(status.l));
        self.emit(I::I32Const(0));
        self.emit(I::I32Ne);
        let mut disc = self.def(Kind::Int(IT::U32, Rep::I32), 0, 1);
        disc.vset = Some(Rc::new(vec![0, 1]));
        self.emit(I::LocalGet(status.l));
        self.emit(I::I32Const(2));
        self.emit(I::I32Eq);
        let mut sdisc = self.def(Kind::Int(IT::U32, Rep::I32), 0, 1);
        sdisc.vset = Some(Rc::new(vec![0, 1]));
        let spec = Av::DEnum("Spec".into(), Box::new(sdisc), Rc::new(vec![vec![v], vec![mr]]));
        Ok(Av::DEnum("Option".into(), Box::new(disc), Rc::new(vec![vec![], vec![spec]])))
    }

    fn load_scratch_mr(&mut self) -> R<Av> {
        let s = self.cx.lay.scratch as i32;
        let mut half = |pe: &mut Self, off: u64| -> u32 {
            pe.emit(I::I32Const(s));
            pe.emit(I::I64Load(mem(off, 3)));
            let l = pe.local(W::I64);
            pe.emit(I::LocalSet(l));
            l
        };
        let (ml, mh, bl, bh) = (half(self, 0), half(self, 8), half(self, 16), half(self, 24));
        let r = (0, (1i128 << 80) - 1);
        let mask = self.wresult(IT::I128, ml, mh, r);
        let bits = self.wresult(IT::I128, bl, bh, r);
        Ok(st("MR", vec![mask, bits]))
    }

    fn set_special(&mut self, key: &Av, v: &Av) -> R<()> {
        let key = key.as_int().ok_or_else(|| Fail("special key at run time".into()))?;
        let s = self.cx.lay.scratch as i32;
        if let Av::Enum(n, 1, p) = v
            && &**n == "Spec"
        {
            // Spec::M(MR { mask, bits }): two 128-bit values.
            let Av::Struct(_, f) = &p[0] else {
                return fail("MR record");
            };
            let (f0, f1) = (f[0].clone(), f[1].clone());
            for (k, x) in [f0, f1].iter().enumerate() {
                let (lo, hi) = self.wparts(x)?;
                self.emit(I::I32Const(s));
                self.wpush(lo);
                self.emit(I::I64Store(mem(16 * k as u64, 3)));
                self.emit(I::I32Const(s));
                self.wpush(hi);
                self.emit(I::I64Store(mem(16 * k as u64 + 8, 3)));
            }
            self.emit(I::LocalGet(0));
            self.emit(I::I32Const(key as i32));
            self.emit(I::I32Const(2));
            self.emit(I::Call(helper("jit_set_special")));
            self.ms.logged = true;
            self.trap_if_negative();
            return Ok(());
        }
        let inner = match v {
            Av::Enum(n, 0, p) if &**n == "Spec" => p[0].clone(),
            _ => return fail(format!("special value {v:?}")),
        };
        let (b, m) = v_parts(&inner)?;
        self.emit(I::I32Const(s));
        self.push_as(&b, Kind::Int(IT::U32, Rep::I32))?;
        self.emit(I::I32Store(mem(0, 2)));
        self.emit(I::I32Const(s));
        self.push_as(&m, Kind::Int(IT::U32, Rep::I32))?;
        self.emit(I::I32Store(mem(4, 2)));
        self.emit(I::LocalGet(0));
        self.emit(I::I32Const(key as i32));
        self.emit(I::I32Const(1));
        self.emit(I::Call(helper("jit_set_special")));
        self.ms.logged = true;
        self.trap_if_negative();
        Ok(())
    }

    // ------------------------------------------------------------ St fields

    pub fn st_field(&mut self, f: &str) -> R<Av> {
        match f {
            "pc_sw" => Ok(self.ms.pc.clone()),
            "pending" => Ok(self.ms.pending.clone()),
            "cfg" => Ok(Av::Cfg),
            _ => fail(format!("state field s.{f}")),
        }
    }

    pub fn st_store(&mut self, f: &str, v: Av) -> R<()> {
        match f {
            "pc_sw" => {
                self.ms.pc = match v {
                    Av::Int(x, _) => Av::Int(x, IT::I128),
                    v => v,
                };
                Ok(())
            }
            "pending" => {
                self.ms.pending = v;
                Ok(())
            }
            _ => fail(format!("state store s.{f}")),
        }
    }

    pub fn cfg_field(&mut self, f: &str) -> R<Av> {
        let c = &self.cx.cfg;
        Ok(match f {
            "has_concrete" => Av::Bool(c.has_concrete),
            "follow_loaded_calls" => Av::Bool(c.follow_loaded_calls),
            "continue_external_calls" => Av::Bool(c.continue_external_calls),
            "max_call_depth" => Av::Int(c.max_call_depth, IT::I128),
            "assume_nw32" => Av::Bool(c.assume_nw32),
            "explicit_memory_model" => Av::Bool(c.explicit_memory_model),
            "approx_recips" => Av::Bool(c.approx_recips),
            "data_memory_tainted" => Av::Bool(c.data_memory_tainted),
            "dossier_bytes" => Av::Int(c.dossier_bytes, IT::I128),
            "fast_mem" => Av::Bool(c.fast_mem),
            "block_ok" => Av::Bool(true),
            "provisional_interp" | "provisional_forms" => Av::Arr(Rc::new(vec![])),
            _ => return fail(format!("cfg field {f}")),
        })
    }
}

pub fn v_parts(v: &Av) -> R<(Av, Av)> {
    match v {
        Av::Struct(n, it) if &**n == "V" => Ok((it[0].clone(), it[1].clone())),
        _ => fail(format!("expected a V, got {v:?}")),
    }
}

fn v_unknown() -> Av {
    st("V", vec![Av::Int(0, IT::U32), Av::Int(0, IT::U32)])
}

fn spec_v(v: Av) -> Av {
    Av::Enum("Spec".into(), 0, Rc::new(vec![v]))
}
