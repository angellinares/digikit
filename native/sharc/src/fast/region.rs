//! Region builder: decode the instructions of a hot loop body from the loaded
//! memory with the generic decoder, lower them (three passes, see
//! `lower`), and collect everything the caller needs to run the kernel:
//! the entry requirements, the memory windows, the flag writers and the
//! per-register write-back facts.
//!
//! The prototype shape is the body of an active DO loop entered at its top:
//! straight-line (no branches or calls), the region being the remaining
//! iterations. `build_straight` builds a one-instruction, loop-free region
//! (used by the per-form differential tests).

use super::decode_view::Dec;
use super::ir::*;
use super::lower::{Env, Lower, Plan, Refuse};
use super::{FlagWriter, Req, WinSpec};
use crate::decode::decode_at;
use crate::rt::{Loop, St};

#[derive(Clone, Copy, Debug)]
pub struct RInsn {
    pub pc: u32,
    pub len_sw: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct LoopSpec {
    pub start: i64,
    pub end: i64,
    pub mode: i64,
}

#[derive(Clone, Copy, Debug)]
pub struct RegMeta {
    pub code: u8,
    pub written: bool,
    pub first_write: Option<u32>,
}

pub struct Region {
    pub entry_pc: u32,
    /// `Some` for a DO loop body, `None` for a loop-free run of instructions.
    pub lp: Option<LoopSpec>,
    pub insns: Vec<RInsn>,
    pub last_len_bytes: u8,
    pub regs: Vec<RegMeta>,
    pub reqs: Vec<Req>,
    /// `reqs` in the order the caller checks them cheaply: registers that
    /// must be fully known, constants, address ranges.
    pub known: Vec<u8>,
    pub eqs: Vec<(u8, u32)>,
    pub nw: Vec<Req>,
    /// Registers the kernel reads (their values go into the context) and
    /// those it writes, with the instruction of the first write.
    pub inputs: Vec<u8>,
    pub outputs: Vec<(u8, u32)>,
    pub wins: Vec<WinSpec>,
    pub flags: Vec<FlagWriter>,
    pub kernel: Kernel,
    /// The code words the region was made from (re-checked when
    /// `Mem::dec_gen` moves).
    pub code_words: Vec<(u32, u16)>,
    pub verified_gen: u64,
    /// Form names, for reports.
    pub forms: Vec<&'static str>,
}

impl Region {
    pub fn len(&self) -> usize {
        self.insns.len()
    }
    pub fn is_empty(&self) -> bool {
        self.insns.is_empty()
    }
}

fn decode_one(s: &St, pc: u32) -> Result<Dec, Refuse> {
    let d = decode_at(|at| s.mem.read_sw(at), pc);
    Dec::from_decoded(&d).ok_or_else(|| Refuse(format!("undecodable instruction at {pc:#x}")))
}

fn env_of(s: &St) -> Env {
    let mut regs = [None; NREGS as usize];
    for (c, slot) in regs.iter_mut().enumerate() {
        if s.r[c].is_c() {
            *slot = Some(s.r[c].b);
        }
    }
    Env {
        regs,
        assume_nw32: s.cfg.assume_nw32,
        approx_recips: s.cfg.approx_recips,
        mode1: s.r[crate::rt::MODE1]
            .is_c()
            .then_some(s.r[crate::rt::MODE1].b),
    }
}

/// The DO loop at the top of the loop stack, if it starts at PC.
pub fn active_loop(s: &St, pc: u32) -> Option<Loop> {
    let top = *s.loops.items().last()?;
    (top.start_sw == pc as i64).then_some(top)
}

pub fn build_loop(s: &St, pc: u32) -> Result<Region, Refuse> {
    let lp = active_loop(s, pc).ok_or_else(|| Refuse("no active loop starts here".into()))?;
    let (start, end) = (lp.start_sw as u32, lp.end_sw as u32);
    if end < start || end - start > 512 {
        return Err(Refuse("loop body size".into()));
    }
    let mut decs = Vec::new();
    let mut at = start;
    loop {
        let d = decode_one(s, at)?;
        let len = d.len_sw;
        decs.push((at, d));
        if at == end {
            break;
        }
        at += len;
        if at > end {
            return Err(Refuse("loop end inside an instruction".into()));
        }
    }
    build(
        s,
        pc,
        Some(LoopSpec {
            start: lp.start_sw,
            end: lp.end_sw,
            mode: lp.mode,
        }),
        decs,
    )
}

/// A one-instruction region at PC with no loop (tests).
pub fn build_straight(s: &St, pc: u32) -> Result<Region, Refuse> {
    let d = decode_one(s, pc)?;
    build(s, pc, None, vec![(pc, d)])
}

fn build(s: &St, pc: u32, lp: Option<LoopSpec>, decs: Vec<(u32, Dec)>) -> Result<Region, Refuse> {
    if !cfg!(sharc_gen) {
        return Err(Refuse("no generated core (SHARC_GEN_DIR)".into()));
    }
    let env = env_of(s);
    let mut plan = Plan::default();
    let mut last = None;
    for pass in 0..3u8 {
        let mut l = Lower::new(env.clone(), plan.clone(), pass);
        for (i, (_, d)) in decs.iter().enumerate() {
            l.lower_insn(i as u32, d)?;
        }
        let out = l.finish()?;
        plan = out.next.clone();
        last = Some(out);
    }
    let out = last.unwrap();
    out.kernel
        .validate()
        .map_err(|e| Refuse(format!("kernel invalid: {e}")))?;
    let mut reqs = out.reqs.clone();
    let mut regs = Vec::new();
    for (c, r) in out.regs.iter().enumerate() {
        if r.var.is_none() {
            continue;
        }
        if r.first_read {
            reqs.push(Req::Known(c as u8));
        }
        regs.push(RegMeta {
            code: c as u8,
            written: r.written,
            first_write: r.first_write,
        });
    }
    for w in &out.wins {
        if let super::Base::Reg(c) = w.base
            && !reqs.contains(&Req::Known(c))
        {
            reqs.push(Req::Known(c));
        }
    }
    let mut code_words = Vec::new();
    for (at, d) in &decs {
        for k in 0..d.len_sw {
            code_words.push((at + k, s.mem.read_sw(at + k).unwrap_or(0)));
        }
    }
    let last_len_bytes = (decs.last().unwrap().1.len_sw * 2) as u8;
    Ok(Region {
        entry_pc: pc,
        lp,
        insns: decs
            .iter()
            .map(|(pc, d)| RInsn {
                pc: *pc,
                len_sw: d.len_sw,
            })
            .collect(),
        last_len_bytes,
        known: reqs
            .iter()
            .filter_map(|q| {
                if let Req::Known(c) = q {
                    Some(*c)
                } else {
                    None
                }
            })
            .collect(),
        eqs: reqs
            .iter()
            .filter_map(|q| {
                if let Req::Eq(c, k) = q {
                    Some((*c, *k))
                } else {
                    None
                }
            })
            .collect(),
        nw: reqs
            .iter()
            .filter(|q| matches!(q, Req::NwPlain { .. } | Req::Mode1Bit { .. }))
            .copied()
            .collect(),
        inputs: regs.iter().map(|m| m.code).collect(),
        outputs: regs
            .iter()
            .filter(|m| m.written)
            .map(|m| (m.code, m.first_write.unwrap_or(u32::MAX)))
            .collect(),
        regs,
        reqs,
        wins: out.wins,
        flags: out.flags,
        kernel: out.kernel,
        code_words,
        verified_gen: s.mem.dec_gen,
        forms: decs.iter().map(|(_, d)| d.form).collect(),
    })
}
