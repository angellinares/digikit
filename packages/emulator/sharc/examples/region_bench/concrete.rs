//! Experimental firmware-independent, predecoded concrete handlers.
//!
//! Ordinary registers stay in a u32 array. The existing exact numeric/flag,
//! memory and loop-boundary helpers are reused; the timed candidate never
//! dispatches through the generic instruction or compute-field executor.
//! Unsupported forms, controls and unknown entry inputs refuse before running.
use generated::core_g as core;
use generated::syms::*;
use sharc_native::fast::decode_view::Dec;
use sharc_native::rt::{
    Fields, FlagUpdate, Insn, R, RegView, S_CONFIDENT, SYM_DYN, SpecView, St, Sym, Trap, V, VI, bnd,
};
use sharc_native::{
    EXIT_NEXT, EXIT_TRAP, Engine, addressing, decode, exec_insn, generated, sym_of,
};
use std::time::Instant;

type Res<T> = Result<T, String>;
fn err(t: sharc_native::rt::Trap) -> String {
    format!("semantic helper trap {}", t.0)
}
fn clean_journal(s: &St) -> bool {
    // jn/snap are private in sharc-rt; unchanged St::commit clears both.
    s.un == 0 && s.probe.is_none()
}
fn need(d: &Dec, key: &str) -> Res<u32> {
    d.field(key)
        .map(|x| x as u32)
        .ok_or_else(|| format!("missing {key}"))
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    None,
    Add,
    Sub,
    Mul,
    FloatPass,
    Pass,
    Inc,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Compute {
    kind: Kind,
    dst: usize,
    x: usize,
    y: usize,
}
impl Compute {
    fn decode(field: u32) -> Res<Self> {
        let kind = if field == 0 {
            Kind::None
        } else {
            if field & (1 << 22) != 0 {
                return Err("multifunction compute not implemented".into());
            }
            match ((field >> 20) & 3, (field >> 12) & 255) {
                (0, 0x81) => Kind::Add,
                (0, 0x82) => Kind::Sub,
                (1, 0x30) => Kind::Mul,
                (0, 0xa1) => Kind::FloatPass,
                (0, 0x21) => Kind::Pass,
                (0, 0x29) => Kind::Inc,
                (cu, op) => return Err(format!("unsupported compute cu={cu} opcode={op:X}")),
            }
        };
        Ok(Self {
            kind,
            dst: ((field >> 8) & 15) as usize,
            x: ((field >> 4) & 15) as usize,
            y: (field & 15) as usize,
        })
    }
    fn reads(&self) -> Vec<usize> {
        match self.kind {
            Kind::None => vec![],
            Kind::Add | Kind::Sub | Kind::Mul => vec![self.x, self.y],
            _ => vec![self.x],
        }
    }
    fn value<R: RegisterAccess>(&self, s: &mut St, r: &R) -> Res<Option<(u32, FlagUpdate)>> {
        if matches!(self.kind, Kind::None) {
            return Ok(None);
        }
        let a = V::c(r.get(s, self.x)? as i128);
        let b = if matches!(self.kind, Kind::Add | Kind::Sub | Kind::Mul) {
            V::c(r.get(s, self.y)? as i128)
        } else {
            V::c(0)
        };
        let (value, flags) = match self.kind {
            Kind::Add | Kind::Sub | Kind::Mul => {
                let (v, overflow, underflow, invalid) = match self.kind {
                    Kind::Add => {
                        let (v, o, i) = bnd::_float_binary_add(s, a, b);
                        (v, o, None, i)
                    }
                    Kind::Sub => {
                        let (v, o, i) = bnd::_float_binary_sub(s, a, b);
                        (v, o, None, i)
                    }
                    _ => bnd::_float_multiply(s, a, b),
                };
                let f = if matches!(self.kind, Kind::Mul) {
                    bnd::_astatx_mult_float(s, v, overflow, underflow, invalid)
                } else {
                    core::flags::_float_alu_updates(s, v, overflow, false, None, invalid, false)
                        .map_err(err)?
                };
                (v, f)
            }
            Kind::FloatPass => {
                let (_, v, _, f) = core::compute_alu::alu_float_pass(
                    s,
                    self.dst as i128,
                    self.x as i128,
                    self.y as i128,
                    a,
                    b,
                    RegView::CUR,
                    SpecView::CUR,
                )
                .map_err(err)?;
                (v, f)
            }
            Kind::Pass => (a, core::flags::_astatx_alu_logical(s, a).map_err(err)?),
            Kind::Inc => (
                V::c(a.b.wrapping_add(1) as i128),
                core::flags::_astatx_alu_arith(s, a, V::c(1), false, false).map_err(err)?,
            ),
            Kind::None => unreachable!(),
        };
        Ok(Some((value.b, flags)))
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Offset {
    Reg(usize),
    Imm(u32),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Transfer {
    index: usize,
    offset: Offset,
    reg: usize,
    store: bool,
    post: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Descriptor {
    compute: Compute,
    transfer: Option<Transfer>,
    mov: Option<(usize, usize)>,
}

fn form_field(s: &St, insn: Insn, key: Sym) -> Res<u32> {
    bnd::_field(s, insn.fields, key)
        .map(|value| value as u32)
        .map_err(err)
}

// This is deliberately limited to the already admitted form set. It consumes
// the generated field symbols directly, so repeated parsing measures owned
// descriptor construction rather than symbol-name lookup or memory decoding.
fn parse_descriptor(s: &St, insn: Insn) -> Res<Descriptor> {
    let (transfer, mov) = match insn.type_name {
        S_3A => {
            let bank = form_field(s, insn, S_G)? as usize * 8;
            (
                Some(Transfer {
                    index: bank + form_field(s, insn, S_I)? as usize,
                    offset: Offset::Reg(32 + bank + form_field(s, insn, S_M)? as usize),
                    reg: form_field(s, insn, S_UREG)? as usize,
                    store: form_field(s, insn, S_D)? != 0,
                    post: form_field(s, insn, S_U)? != 0,
                }),
                None,
            )
        }
        S_4A => {
            let bank = form_field(s, insn, S_G)? as usize * 8;
            let bits = (form_field(s, insn, S_DATA_5_5)? << 5) | form_field(s, insn, S_DATA_4_0)?;
            (
                Some(Transfer {
                    index: bank + form_field(s, insn, S_I)? as usize,
                    offset: Offset::Imm((((bits ^ 32) as i32 - 32).wrapping_mul(4)) as u32),
                    reg: form_field(s, insn, S_DREG)? as usize,
                    store: form_field(s, insn, S_D)? != 0,
                    post: form_field(s, insn, S_U)? != 0,
                }),
                None,
            )
        }
        S_3C => (
            Some(Transfer {
                index: form_field(s, insn, S_DMI)? as usize,
                offset: Offset::Reg(32 + form_field(s, insn, S_DMM)? as usize),
                reg: form_field(s, insn, S_DREG)? as usize,
                store: form_field(s, insn, S_D)? != 0,
                post: true,
            }),
            None,
        ),
        S_5A_MOVE | S_5B_MOVE => {
            let src = (form_field(s, insn, S_SRCUREGHIGH)? << 2)
                | (form_field(s, insn, S_SRCUREGLOW_1_1)? << 1)
                | form_field(s, insn, S_SRCUREGLOW_0_0)?;
            (
                None,
                Some((src as usize, form_field(s, insn, S_DSTUREG)? as usize)),
            )
        }
        S_2A => (None, None),
        other => return Err(format!("unmapped admitted form {other}")),
    };
    let compute_field = match insn.type_name {
        S_3C | S_5B_MOVE => 0,
        S_2A | S_3A | S_4A | S_5A_MOVE => {
            (form_field(s, insn, S_COMPUTE_22_16)? << 16) | form_field(s, insn, S_COMPUTE_15_0)?
        }
        _ => unreachable!(),
    };
    Ok(Descriptor {
        compute: Compute::decode(compute_field)?,
        transfer,
        mov,
    })
}
struct Op {
    pc: u32,
    len: u32,
    insn: Insn,
    handler: FormHandler,
    compute: Compute,
    transfer: Option<Transfer>,
    mov: Option<(usize, usize)>,
}

type FormHandler = fn(&mut St, Insn, Fields, Sym) -> R<()>;

// This intentionally uses the generated form-level handlers selected by the
// generic executor, rather than any of their specialized access subhandlers.
// It is a pre-resolved dispatch comparison; compiler routing is part of it.
fn resolve_form(name: Sym) -> Option<FormHandler> {
    match name {
        S_2A => Some(core::forms_compute::_type_2a),
        S_3A => Some(core::forms_move::_type_3a),
        S_3C => Some(core::forms_move::_type_3c),
        S_4A => Some(core::forms_move::_type_4a),
        S_5A_MOVE | S_5B_MOVE => Some(core::forms_move::_type_5a_move),
        _ => None,
    }
}

// Keep the transaction, validation, probe, and peripheral-timer ordering of
// sharc_native::exec_insn, including its call boundary. Form-table lookup and
// the wide form match become a preselected function-pointer call.
#[inline(never)]
fn exec_resolved_form(s: &mut St, insn: Insn, handler: FormHandler) -> R<()> {
    if s.cfg.core_timer {
        s.timer_written = false;
    }
    s.begin();
    match (|| {
        core::state::_sync_empty_loop_registers(s)?;
        if insn.kind != S_CONFIDENT || insn.length_bytes.is_none() {
            if insn.length_bytes.is_none() || !s.cfg.provisional_form(insn.type_name) {
                return Err(Trap(9));
            }
        }
        s.at_loaded_entry = false;
        handler(s, insn, insn.fields, insn.type_name)
    })() {
        Ok(()) => {
            if s.probe.is_some() {
                s.probe_note();
            }
            s.commit();
        }
        Err(t) => {
            s.rollback();
            return Err(t);
        }
    }
    if s.cfg.core_timer {
        s.begin();
        match core::sequencer::_core_timer_tick(s) {
            Ok(_) => s.commit_host(),
            Err(t) => {
                s.rollback();
                return Err(t);
            }
        }
    }
    Ok(())
}

impl Op {
    fn decode(s: &mut St, pc: u32) -> Res<Self> {
        let decoded = decode::decode_at(|a| s.mem.read_sw(a), pc);
        let d = Dec::from_decoded(&decoded).ok_or_else(|| format!("uncertain decode at {pc:X}"))?;
        if d.field("cond").is_some_and(|x| x != 31) {
            return Err(format!("conditional {}", d.form));
        }
        let mut mov = None;
        let transfer = match d.form {
            "3a" | "4a" => {
                if d.form == "3a" && need(&d, "l")? != 0 {
                    return Err("long-word transfer".into());
                }
                let bank = need(&d, "g")? as usize * 8;
                let index = bank + need(&d, "i")? as usize;
                let reg = need(&d, if d.form == "3a" { "ureg" } else { "dreg" })? as usize;
                let offset = if d.form == "3a" {
                    Offset::Reg(32 + bank + need(&d, "m")? as usize)
                } else {
                    let v = (need(&d, "data[5:5]")? << 5) | need(&d, "data[4:0]")?;
                    Offset::Imm((((v ^ 32) as i32 - 32).wrapping_mul(4)) as u32)
                };
                Some(Transfer {
                    index,
                    offset,
                    reg,
                    store: need(&d, "d")? != 0,
                    post: need(&d, "u")? != 0,
                })
            }
            "3c" => Some(Transfer {
                index: need(&d, "dmi")? as usize,
                offset: Offset::Reg(32 + need(&d, "dmm")? as usize),
                reg: need(&d, "dreg")? as usize,
                store: need(&d, "d")? != 0,
                post: true,
            }),
            "5a_move" | "5b_move" => {
                let src = (need(&d, "srcureghigh")? << 2)
                    | (need(&d, "srcureglow[1:1]")? << 1)
                    | need(&d, "srcureglow[0:0]")?;
                mov = Some((src as usize, need(&d, "dstureg")? as usize));
                None
            }
            "2a" => None,
            other => return Err(format!("unsupported form {other} at {pc:X}")),
        };
        if transfer.is_some_and(|t| {
            t.index >= 16 || t.reg >= 16 || matches!(t.offset,Offset::Reg(m) if m>=48)
        }) {
            return Err("special-register memory operand".into());
        }
        if mov.is_some_and(|(src, dst)| src >= 96 || dst >= 32) {
            return Err("special/control register move".into());
        }
        let field = if matches!(d.form, "3c" | "5b_move") {
            0
        } else {
            d.compute().ok_or("missing full compute field")?
        };
        let compute = Compute::decode(field)?;
        let insn = bnd::decode_at_with(s, pc as i128, sym_of).map_err(err)?;
        let handler = resolve_form(insn.type_name)
            .ok_or_else(|| format!("unmapped admitted form {} at {pc:X}", insn.type_name))?;
        let decoded_descriptor = Descriptor {
            compute,
            transfer,
            mov,
        };
        if parse_descriptor(s, insn)? != decoded_descriptor {
            return Err(format!(
                "field descriptor differs from decoded form at {pc:X}"
            ));
        }
        Ok(Self {
            pc,
            len: d.len_sw,
            insn,
            handler,
            compute,
            transfer,
            mov,
        })
    }
    fn step<R: RegisterAccess>(&self, s: &mut St, r: &mut R, terminal: bool) -> Res<()> {
        self.step_descriptor::<R, false>(
            s,
            r,
            Descriptor {
                compute: self.compute,
                transfer: self.transfer,
                mov: self.mov,
            },
            terminal,
        )
    }
    fn step_reparsed<R: RegisterAccess>(&self, s: &mut St, r: &mut R, terminal: bool) -> Res<()> {
        let descriptor = parse_descriptor(s, self.insn)?;
        self.step_descriptor::<R, false>(s, r, descriptor, terminal)
    }
    fn step_full_advance<R: RegisterAccess>(&self, s: &mut St, r: &mut R) -> Res<()> {
        self.step_descriptor::<R, true>(
            s,
            r,
            Descriptor {
                compute: self.compute,
                transfer: self.transfer,
                mov: self.mov,
            },
            false,
        )
    }
    fn step_descriptor<R: RegisterAccess, const FULL_ADVANCE: bool>(
        &self,
        s: &mut St,
        r: &mut R,
        descriptor: Descriptor,
        terminal: bool,
    ) -> Res<()> {
        if s.icount >= s.limit {
            return Err("instruction budget reached".into());
        }
        // Snapshot all sources before any destination or memory write.
        let computed = descriptor.compute.value(s, r)?;
        let moved = descriptor
            .mov
            .map(|(src, dst)| r.get(s, src).map(|v| (dst, v)))
            .transpose()?;
        let mut memory = None;
        if let Some(t) = descriptor.transfer {
            let iv = r.get(s, 16 + t.index)?;
            if addressing::normal_word_to_byte(iv as i128).is_some() {
                return Err("mapped normal-word index requires another scale".into());
            }
            let delta = match t.offset {
                Offset::Reg(m) => r.get(s, m)?.wrapping_mul(4),
                Offset::Imm(v) => v,
            };
            let addr = if t.post { iv } else { iv.wrapping_add(delta) };
            if addressing::normal_word_to_byte(addr as i128).is_some()
                || s.in_mmr_windows(addr as i128)
            {
                return Err("mapped/MMR data access".into());
            }
            let store = if t.store {
                Some(r.get(s, t.reg)?)
            } else {
                None
            };
            // L/B and MODE1 cannot be written by this admitted opcode set.
            let next = if t.post {
                Some(
                    core::memory::_post_modify_index(
                        s,
                        t.index as i128,
                        V::c(iv as i128),
                        V::c(delta as i128),
                        4,
                        SYM_DYN,
                    )
                    .map_err(err)?,
                )
            } else {
                None
            };
            if next.is_some_and(|v| !v.is_c()) {
                return Err("unknown post-modify result".into());
            }
            memory = Some((t, addr, store, next));
        }
        s.begin();
        s.at_loaded_entry = false;
        if s.cfg.core_timer {
            s.timer_written = false;
        }
        // Failed modeled accesses never become measurements; caller restores
        // the whole capture on refusal. Roll back the current memory journal.
        if let Some((t, addr, store, next)) = memory {
            let result = if let Some(value) = store {
                bnd::_dm_write(s, VI::V(V::c(addr as i128)), 4, V::c(value as i128), true)
                    .map_err(err)
                    .and_then(|ok| {
                        if ok {
                            Ok(None)
                        } else {
                            Err("unhandled memory store".into())
                        }
                    })
            } else {
                bnd::_dm_read(s, VI::V(V::c(addr as i128)), 4, false, true)
                    .map_err(err)
                    .and_then(|v| match v {
                        Some(v) if v.is_c() => Ok(Some(v.b)),
                        _ => Err("unknown memory load".into()),
                    })
            };
            match result {
                Ok(Some(v)) => r.put(s, t.reg, v)?,
                Ok(None) => (),
                Err(e) => {
                    s.rollback();
                    return Err(e);
                }
            }
            if let Some(v) = next {
                r.put(s, 16 + t.index, v.b)?;
            }
        }
        if let Some((dst, value)) = moved {
            r.put(s, dst, value)?;
        }
        if let Some((value, flags)) = computed {
            r.put(s, descriptor.compute.dst, value)?;
            let astat = bnd::_apply_flag_update(s, s.r[118], flags);
            s.set_r(118, astat).map_err(err)?;
            if matches!(descriptor.compute.kind, Kind::Mul) {
                core::compute::_apply_sticky(s, S_STKYX, astat, S_FLOAT_MULTIPLY).map_err(err)?;
            }
        }
        if FULL_ADVANCE {
            s.pc_sw = self.pc as i128;
            core::sequencer::_advance(s, self.insn).map_err(err)?;
        } else if terminal {
            // No admitted opcode can change MODE1/L/B or request a bank
            // transition. The pending-free loop path of _advance reads/writes
            // only special registers (>=96) and stack/control state. Ordinary
            // words can therefore stay cached through every loop boundary;
            // Program::run materializes them once after successful completion.
            // Refusals discard the trial rather than resume from cached state.
            s.pc_sw = self.pc as i128;
            core::sequencer::_advance(s, self.insn).map_err(err)?;
        } else {
            s.pc_sw = (self.pc + self.len) as i128;
            s.steps += 1;
        }
        s.commit(); // includes exactly one retired instruction
        Ok(())
    }
}
trait RegisterAccess {
    fn new(s: &St) -> Self;
    fn get(&self, s: &St, n: usize) -> Res<u32>;
    fn put(&mut self, s: &mut St, n: usize, value: u32) -> Res<()>;
    fn flush(&self, s: &mut St) -> Res<()>;
}

struct Words {
    words: [u32; 96],
    known: [bool; 96],
    written: [bool; 96],
}
impl RegisterAccess for Words {
    fn new(s: &St) -> Self {
        Self {
            words: std::array::from_fn(|n| s.r[n].b),
            known: std::array::from_fn(|n| s.r[n].is_c()),
            written: [false; 96],
        }
    }
    fn get(&self, _s: &St, n: usize) -> Res<u32> {
        if self.known[n] {
            Ok(self.words[n])
        } else {
            Err(format!("unknown numeric register {n}"))
        }
    }
    fn put(&mut self, _s: &mut St, n: usize, v: u32) -> Res<()> {
        self.words[n] = v;
        self.known[n] = true;
        self.written[n] = true;
        Ok(())
    }
    fn flush(&self, s: &mut St) -> Res<()> {
        for n in 0..96 {
            if self.written[n] {
                s.r[n] = V::c(self.words[n] as i128);
            }
        }
        Ok(())
    }
}

// Compared with Words, this changes architectural reads and journaled writes
// together, so it is not a pure cache-load measurement. Any refusal discards
// the captured trial; this path never resumes it.
#[derive(Default)]
struct LiveRegs;
impl RegisterAccess for LiveRegs {
    fn new(_s: &St) -> Self {
        Self
    }
    fn get(&self, s: &St, n: usize) -> Res<u32> {
        if s.r[n].is_c() {
            Ok(s.r[n].b)
        } else {
            Err(format!("unknown numeric register {n}"))
        }
    }
    fn put(&mut self, s: &mut St, n: usize, value: u32) -> Res<()> {
        s.set_r(n, V::c(value as i128)).map_err(err)
    }
    fn flush(&self, _s: &mut St) -> Res<()> {
        Ok(())
    }
}
struct Program {
    ops: Vec<Op>,
    trips: u64,
}
impl Program {
    fn decode(s: &mut St) -> Res<Self> {
        let lp = s.loops.items().last().copied().ok_or("no active DO loop")?;
        if lp.start_sw < 0
            || lp.end_sw < lp.start_sw
            || lp.remaining < 1
            || s.pc_sw != lp.start_sw as i128
        {
            return Err("not an active DO-loop head".into());
        }
        if s.probe.is_some() {
            return Err("active diagnostic probe".into());
        }
        if s.pending.is_some()
            || s.bank_pending_mask >= 0
            || s.bank_requested_mask >= 0
            || s.pc_stack_pending >= 0
            || s.pc_stack_requested >= 0
        {
            return Err("pending bank/stack/control transition".into());
        }
        if !s.cfg.assume_nw32
            || s.cfg.data_memory_tainted
            || !s.r[114].is_c()
            || s.r[114].b & (7 << 21) != 0
        {
            return Err("unsupported memory/SIMD/broadcast mode".into());
        }
        let mut ops = Vec::new();
        let mut pc = lp.start_sw as u32;
        loop {
            let op = Op::decode(s, pc)?;
            let next = pc.checked_add(op.len).ok_or("PC overflow")?;
            ops.push(op);
            if pc == lp.end_sw as u32 {
                break;
            }
            if next > lp.end_sw as u32 || ops.len() >= 128 {
                return Err("invalid/oversized loop extent".into());
            }
            pc = next;
        }
        // Only first reads need entry guards; later loads/computes may create
        // concrete values. Unknown loaded data is checked before committing.
        let mut known = std::array::from_fn::<_, 96, _>(|n| s.r[n].is_c());
        for op in &ops {
            let mut reads = op.compute.reads();
            if let Some((src, _)) = op.mov {
                reads.push(src);
            }
            if let Some(t) = op.transfer {
                reads.push(16 + t.index);
                if let Offset::Reg(m) = t.offset {
                    reads.push(m);
                }
                if t.store {
                    reads.push(t.reg);
                }
            }
            for n in reads {
                if !known[n] {
                    return Err(format!("unknown entry input register {n}"));
                }
            }
            if let Some(t) = op.transfer {
                if !t.store {
                    known[t.reg] = true;
                }
                if t.post {
                    known[16 + t.index] = true;
                }
            }
            if let Some((_, dst)) = op.mov {
                known[dst] = true;
            }
            if !matches!(op.compute.kind, Kind::None) {
                known[op.compute.dst] = true;
            }
        }
        Ok(Self {
            ops,
            trips: lp.remaining as u64,
        })
    }
    fn run_with<R: RegisterAccess>(&self, s: &mut St) -> Res<u64> {
        let before = s.icount;
        let mut r = R::new(s);
        for _ in 0..self.trips {
            for (n, op) in self.ops.iter().enumerate() {
                if s.pc_sw != op.pc as i128 {
                    return Err("unexpected loop/control PC".into());
                }
                op.step(s, &mut r, n + 1 == self.ops.len())?;
            }
        }
        r.flush(s)?;
        Ok(s.icount - before)
    }
    fn run(&self, s: &mut St) -> Res<u64> {
        self.run_with::<Words>(s)
    }
    // This control combines live architectural reads with journaled writes;
    // it does not isolate pure cache-load cost. Any refusal discards the trial.
    fn run_live(&self, s: &mut St) -> Res<u64> {
        self.run_with::<LiveRegs>(s)
    }
    // Rebuild the owned semantic descriptor from the fixed decoded fields for
    // every instruction. This is descriptor parsing, not generic execution.
    fn run_live_reparse(&self, s: &mut St) -> Res<u64> {
        let before = s.icount;
        let mut r = LiveRegs::new(s);
        for _ in 0..self.trips {
            for (n, op) in self.ops.iter().enumerate() {
                if s.pc_sw != op.pc as i128 {
                    return Err("reparse loop/control PC diverged".into());
                }
                op.step_reparsed(s, &mut r, n + 1 == self.ops.len())?;
            }
        }
        r.flush(s)?;
        Ok(s.icount - before)
    }
    // This keeps the same predecoded owned body and live registers, but uses
    // the generated sequencer for every instruction instead of only the loop
    // tail. The const specialization keeps this choice out of the body path.
    fn run_live_full_advance(&self, s: &mut St) -> Res<u64> {
        let before = s.icount;
        let mut r = LiveRegs::new(s);
        for _ in 0..self.trips {
            for op in &self.ops {
                if s.pc_sw != op.pc as i128 {
                    return Err("full advance loop/control PC diverged".into());
                }
                op.step_full_advance(s, &mut r)?;
            }
        }
        r.flush(s)?;
        Ok(s.icount - before)
    }
    fn generic(&self, s: &mut St) -> Res<u64> {
        let before = s.icount;
        for _ in 0..self.trips {
            for op in &self.ops {
                if s.pc_sw != op.pc as i128 {
                    return Err("generic loop PC diverged".into());
                }
                exec_insn(s, op.insn).map_err(err)?;
            }
        }
        Ok(s.icount - before)
    }
    fn resolved_generic(&self, s: &mut St) -> Res<u64> {
        let before = s.icount;
        for _ in 0..self.trips {
            for op in &self.ops {
                if s.pc_sw != op.pc as i128 {
                    return Err("resolved generic loop PC diverged".into());
                }
                exec_resolved_form(s, op.insn, op.handler).map_err(err)?;
            }
        }
        Ok(s.icount - before)
    }
}
fn refuse(pc: u32, reason: &str) {
    println!(
        "{{\"region\":\"{pc:X}\",\"status\":\"refused\",\"reason\":\"{}\"}}",
        super::json_text(reason)
    );
}
fn finish_generic(e: &mut Engine, mut plan: super::CallPlan) {
    plan.timer = None;
    super::finish_call(e, plan, EXIT_NEXT);
}

fn boundary_diag(
    expected: &Engine,
    candidate: &Engine,
    expected_export: &[u8],
    ran: u64,
    insns: u64,
) -> String {
    let registers: Vec<String> = expected
        .s
        .r
        .iter()
        .zip(candidate.s.r.iter())
        .enumerate()
        .filter_map(|(n, (a, b))| {
            (a != b).then(|| format!("r{n}=({:08X}/{:08X})->({:08X}/{:08X})", a.b, a.m, b.b, b.m))
        })
        .take(16)
        .collect();
    let actual_export = candidate.export();
    let byte = expected_export
        .iter()
        .zip(actual_export.iter())
        .position(|(a, b)| a != b)
        .map(|n| format!("{n}:{:02X}->{:02X}", expected_export[n], actual_export[n]))
        .or_else(|| {
            (expected_export.len() != actual_export.len())
                .then(|| format!("length {}->{}", expected_export.len(), actual_export.len()))
        })
        .unwrap_or_else(|| "same".into());
    format!(
        "boundary mismatch ran={ran}/{insns} pc={:X}->{:X} pending={:?}->{:?} at_loaded={}{} loops={:?}->{:?} call={:?}->{:?} pc_stack={:?}->{:?} status={:?}->{:?} regs=[{}] export={byte} host={} || {}",
        expected.s.pc_sw,
        candidate.s.pc_sw,
        expected.s.pending,
        candidate.s.pending,
        expected.s.at_loaded_entry,
        candidate.s.at_loaded_entry,
        expected.s.loops.items(),
        candidate.s.loops.items(),
        expected.s.call_stack.items(),
        candidate.s.call_stack.items(),
        expected.s.pc_stack.items(),
        candidate.s.pc_stack.items(),
        expected.s.status_stack.items(),
        candidate.s.status_stack.items(),
        registers.join(","),
        super::host_state(expected),
        super::host_state(candidate),
    )
}

// Small synthetic RAM keeps random differential tests independent of the
// multi-megabyte captured overlay. Each complete decoded instruction is checked.
fn forms(p: &Program, image: &[u8], mode: u32, trials: usize) -> Res<usize> {
    let mut a = Engine::from_image(image).map_err(|e| format!("image error {e}"))?;
    let mut b = Engine::from_image(image).map_err(|e| format!("image error {e}"))?;
    let mut c = Engine::from_image(image).map_err(|e| format!("image error {e}"))?;
    let mut d = Engine::from_image(image).map_err(|e| format!("image error {e}"))?;
    let mut reparse = Engine::from_image(image).map_err(|e| format!("image error {e}"))?;
    let mut full_advance = Engine::from_image(image).map_err(|e| format!("image error {e}"))?;
    let corners = [
        0, 0x80000000, 1, 0x80000001, 0x007fffff, 0x00800000, 0x3f800000, 0xbf800000, 0x7f800000,
        0xff800000, 0x7fc12345, 0x7f812345, 0x7f7fffff, 0xff7fffff,
    ];
    let mut seed = 0x713ab98du32;
    let mut count = 0;
    for op in &p.ops {
        for trial in 0..trials {
            let mut values = [V::c(0); 128];
            for (n, v) in values.iter_mut().enumerate() {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let bits = if trial < corners.len() {
                    corners[(trial + n) % corners.len()]
                } else {
                    seed
                };
                *v = V::c(bits as i128);
            }
            values[114] = V::c(mode as i128);
            values[118] = V::partial(0x00ff_ffff, values[118].b as i128);
            for n in 0..16 {
                values[16 + n] = V::c(0x200000 + (n as i128) * 0x1000);
                values[32 + n] = V::c((n % 3) as i128 - 1);
                values[48 + n] = V::c(0);
                values[64 + n] = values[16 + n];
            }
            for engine in [
                &mut a,
                &mut b,
                &mut c,
                &mut d,
                &mut reparse,
                &mut full_advance,
            ] {
                engine.s.mem.reset();
                engine.s.r = values;
                engine.s.bank_pending_mask = -1;
                engine.s.bank_requested_mask = -1;
                engine.s.pc_stack_pending = -1;
                engine.s.pc_stack_requested = -1;
                engine.s.loops.clear();
                engine.s.call_stack.clear();
                engine.s.pc_stack.clear();
                engine.s.status_stack.clear();
                engine.s.pc_sw = op.pc as i128;
                engine.s.pending = None;
                engine.s.steps = 0;
                engine.s.icount = 0;
                engine.s.limit = 100;
                engine.s.trap = None;
                engine.s.at_loaded_entry = false;
                engine.s.timer_written = false;
                engine.set_option(12, 1);
                engine.set_option(21, 0);
                engine.set_option(22, 1);
                engine.s.in_block = true;
                engine.s.sync_snapshot();
                if let Some(t) = op.transfer {
                    let iv = values[16 + t.index].b;
                    let delta = match t.offset {
                        Offset::Reg(m) => values[m].b.wrapping_mul(4),
                        Offset::Imm(v) => v,
                    };
                    let addr = if t.post { iv } else { iv.wrapping_add(delta) };
                    assert_eq!(engine.poke(addr as u64, &seed.to_le_bytes(), 4), 1);
                }
            }
            exec_insn(&mut a.s, op.insn).map_err(err)?;
            let expected_export = a.export();
            exec_resolved_form(&mut d.s, op.insn, op.handler).map_err(err)?;
            let mut words = Words::new(&b.s);
            op.step(&mut b.s, &mut words, false)?;
            words.flush(&mut b.s)?;
            let mut live_regs = LiveRegs::new(&c.s);
            op.step(&mut c.s, &mut live_regs, false)?;
            let mut reparse_regs = LiveRegs::new(&reparse.s);
            op.step_reparsed(&mut reparse.s, &mut reparse_regs, false)?;
            let mut full_advance_regs = LiveRegs::new(&full_advance.s);
            op.step_full_advance(&mut full_advance.s, &mut full_advance_regs)?;
            if expected_export != b.export()
                || expected_export != c.export()
                || expected_export != d.export()
                || expected_export != reparse.export()
                || expected_export != full_advance.export()
                || a.s.icount != b.s.icount
                || a.s.icount != c.s.icount
                || a.s.icount != d.s.icount
                || a.s.icount != reparse.s.icount
                || a.s.icount != full_advance.s.icount
                || a.s.steps != b.s.steps
                || a.s.steps != c.s.steps
                || a.s.steps != d.s.steps
                || a.s.steps != reparse.s.steps
                || a.s.steps != full_advance.s.steps
                || a.s.trap != b.s.trap
                || a.s.trap != c.s.trap
                || a.s.trap != d.s.trap
                || a.s.trap != reparse.s.trap
                || a.s.trap != full_advance.s.trap
                || !clean_journal(&a.s)
                || !clean_journal(&b.s)
                || !clean_journal(&c.s)
                || !clean_journal(&d.s)
                || !clean_journal(&reparse.s)
                || !clean_journal(&full_advance.s)
            {
                return Err(format!(
                    "random form mismatch at {:X}, trial {trial}",
                    op.pc
                ));
            }
            count += 1;
        }
    }
    Ok(count)
}

pub fn compare(args: &[String]) {
    if args.len() < 3 {
        eprintln!(
            "usage: concrete-compare IMAGE STATES PC [--calls N --warm N --reps N --trials N]"
        );
        return;
    }
    let pc = u32::from_str_radix(args[2].trim_start_matches("0x"), 16).expect("hex PC");
    let (calls, warm, reps, trials) = (
        super::flag(args, "--calls", 2) as usize,
        super::flag(args, "--warm", 1) as usize,
        super::flag(args, "--reps", 1) as usize,
        super::flag(args, "--trials", 100) as usize,
    );
    if calls == 0 || reps == 0 {
        refuse(pc, "positive calls/reps required");
        return;
    }
    let dir = &args[1];
    let meta_path = format!("{dir}/r_{pc:X}.meta");
    let state_path = format!("{dir}/r_{pc:X}.state");
    if !std::path::Path::new(&meta_path).is_file() || !std::path::Path::new(&state_path).is_file() {
        refuse(pc, "missing captured entry");
        return;
    }
    let clock = super::meta(&meta_path)["clock"].parse().expect("clock");
    let image = std::fs::read(&args[0]).expect("image");
    let state = std::fs::read(state_path).expect("state");
    let mut concrete = super::open(&image, &state, clock);
    let p = match Program::decode(&mut concrete.s) {
        Ok(p) => p,
        Err(e) => {
            refuse(pc, &e);
            return;
        }
    };
    let form_trials = match forms(&p, &image, concrete.s.r[114].b, trials) {
        Ok(n) => n,
        Err(e) => {
            refuse(pc, &e);
            return;
        }
    };
    let mut aot = super::open(&image, &state, clock);
    let mut generic = super::open(&image, &state, clock);
    let mut resolved_generic = super::open(&image, &state, clock);
    let mut state_regs = super::open(&image, &state, clock);
    let mut state_regs_reparse = super::open(&image, &state, clock);
    let mut state_regs_full_advance = super::open(&image, &state, clock);
    let Some(entry) = aot.dispatch.get_entry(pc as i128) else {
        refuse(pc, "missing AOT entry");
        return;
    };
    super::reset_capture(&mut aot, &state, clock);
    let plan = super::begin_call(&mut aot);
    let code = (entry.f)(&mut aot.s);
    let (_, insns) = super::finish_call(&mut aot, plan, code);
    if code == EXIT_TRAP || insns != p.trips * p.ops.len() as u64 || !clean_journal(&aot.s) {
        refuse(pc, "AOT work differs from complete loop boundary");
        return;
    }
    let expected = aot.export();
    let host = super::host_state(&aot);
    let aot_steps = aot.s.steps;
    for mode in 0..6 {
        let e = match mode {
            0 => &mut concrete,
            1 => &mut generic,
            2 => &mut resolved_generic,
            3 => &mut state_regs,
            4 => &mut state_regs_reparse,
            _ => &mut state_regs_full_advance,
        };
        super::reset_capture(e, &state, clock);
        let plan = super::begin_call(e);
        let ran = match mode {
            0 => p.run(&mut e.s),
            1 => p.generic(&mut e.s),
            2 => p.resolved_generic(&mut e.s),
            3 => p.run_live(&mut e.s),
            4 => p.run_live_reparse(&mut e.s),
            _ => p.run_live_full_advance(&mut e.s),
        };
        let ran = match ran {
            Ok(n) => n,
            Err(e) => {
                refuse(pc, &e);
                return;
            }
        };
        if matches!(mode, 1 | 2) {
            finish_generic(e, plan);
        } else {
            super::finish_call(e, plan, EXIT_NEXT);
        }
        if ran != insns
            || e.export() != expected
            || super::host_state(e) != host
            || e.s.steps != insns as i128
            || !clean_journal(&e.s)
        {
            refuse(
                pc,
                &format!(
                    "{} {}",
                    [
                        "concrete",
                        "generic",
                        "resolved_generic",
                        "state_regs",
                        "state_regs_reparse",
                        "state_regs_full_advance",
                    ][mode],
                    boundary_diag(&aot, e, &expected, ran, insns),
                ),
            );
            return;
        }
    }
    println!(
        "{{\"region\":\"{pc:X}\",\"status\":\"proved\",\"ops\":{},\"trips\":{},\"insns\":{insns},\"form_trials\":{form_trials},\"pc_after\":{},\"aot_steps\":{},\"generic_steps\":{},\"resolved_generic_steps\":{},\"concrete_steps\":{},\"state_regs_steps\":{},\"state_regs_reparse_steps\":{},\"state_regs_full_advance_steps\":{},\"resolved_generic_progress\":\"pre-resolved generated form handler passed seeded and captured-boundary gates\",\"state_regs_reparse_progress\":\"field and compute descriptor parsing passed seeded and captured-boundary gates\",\"state_regs_full_advance_progress\":\"per-instruction sequencer advance passed seeded and captured-boundary gates\",\"accessible_journal_checks\":\"un_zero_probe_none; jn_snap_zero_via_unchanged_St_commit_invariant\",\"access_comparison\":\"combined register reads plus journaled writes; not pure cache-load cost\"}}",
        p.ops.len(),
        p.trips,
        aot.s.pc_sw,
        aot.s.steps,
        generic.s.steps,
        resolved_generic.s.steps,
        concrete.s.steps,
        state_regs.s.steps,
        state_regs_reparse.s.steps,
        state_regs_full_advance.s.steps
    );
    for rep in 0..reps {
        let mut times: [Vec<u64>; 7] = std::array::from_fn(|_| Vec::new());
        let mut resets: [Vec<u64>; 7] = std::array::from_fn(|_| Vec::new());
        for i in 0..warm + calls {
            for j in 0..7 {
                let mode = (rep + i + j) % 7;
                let e = match mode {
                    0 => &mut aot,
                    1 => &mut generic,
                    2 => &mut resolved_generic,
                    3 => &mut concrete,
                    4 => &mut state_regs,
                    5 => &mut state_regs_reparse,
                    _ => &mut state_regs_full_advance,
                };
                let t = Instant::now();
                super::reset_capture(e, &state, clock);
                let reset = t.elapsed().as_nanos() as u64;
                let plan = super::begin_call(e);
                let t = Instant::now();
                let result = match mode {
                    0 => {
                        let c = (entry.f)(&mut e.s);
                        let (_, k) = super::finish_call(e, plan, c);
                        if c != code {
                            Err("AOT exit varied".into())
                        } else {
                            Ok(k)
                        }
                    }
                    1 => {
                        let r = p.generic(&mut e.s);
                        finish_generic(e, plan);
                        r
                    }
                    2 => {
                        let r = p.resolved_generic(&mut e.s);
                        finish_generic(e, plan);
                        r
                    }
                    3 => {
                        let r = p.run(&mut e.s);
                        super::finish_call(e, plan, EXIT_NEXT);
                        r
                    }
                    4 => {
                        let r = p.run_live(&mut e.s);
                        super::finish_call(e, plan, EXIT_NEXT);
                        r
                    }
                    5 => {
                        let r = p.run_live_reparse(&mut e.s);
                        super::finish_call(e, plan, EXIT_NEXT);
                        r
                    }
                    _ => {
                        let r = p.run_live_full_advance(&mut e.s);
                        super::finish_call(e, plan, EXIT_NEXT);
                        r
                    }
                };
                let elapsed = t.elapsed().as_nanos() as u64;
                if result != Ok(insns)
                    || e.export() != expected
                    || super::host_state(e) != host
                    || e.s.steps != if mode == 0 { aot_steps } else { insns as i128 }
                    || !clean_journal(&e.s)
                {
                    refuse(pc, "sample failed complete state/boundary check");
                    return;
                }
                if i >= warm {
                    times[mode].push(elapsed);
                    resets[mode].push(reset);
                }
            }
        }
        for mode in 0..7 {
            times[mode].sort_unstable();
            resets[mode].sort_unstable();
            let ns = times[mode].iter().sum::<u64>() as f64 / calls as f64;
            let reset = resets[mode].iter().sum::<u64>() as f64 / calls as f64;
            println!(
                "{{\"region\":\"{pc:X}\",\"status\":\"measured\",\"mode\":\"{}\",\"rep\":{rep},\"calls\":{calls},\"insns_per_call\":{insns},\"ns_per_call\":{ns:.3},\"ns_per_insn\":{:.4},\"median_ns\":{},\"reset_mean_ns\":{reset:.1}}}",
                [
                    "aot",
                    "generic",
                    "resolved_generic",
                    "concrete",
                    "state_regs",
                    "state_regs_reparse",
                    "state_regs_full_advance",
                ][mode],
                ns / insns as f64,
                times[mode][calls / 2]
            );
        }
    }
}
