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

use super::cfg::{self, CfgMeta};
use super::decode_view::Dec;
use super::ir::*;
use super::lower::{Env, Lower, Lowered, Plan, Refuse};
use super::{FlagWriter, Req, WinSpec};
use crate::decode::decode_at;
use crate::rt::{Loop, MODE1, St};

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
    /// MODE1 and ASTATX requirements.
    pub misc: Vec<Req>,
    /// The kernel tracks flags in pseudo registers (in `inputs`); the groups
    /// with writers write their last writer out (`glue::apply_groups`).
    pub flag_v: bool,
    pub flag_groups: [bool; 3],
    /// The kernel tracks the CACC compare history (`PSEUDO_CACC`).
    pub cacc: bool,
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
    /// `Some` for a CFG region (see `cfg`): `lp` is None, the kernel counts
    /// its own instructions and loop iterations.
    pub cfg: Option<CfgMeta>,
}

impl Region {
    pub fn len(&self) -> usize {
        self.insns.len()
    }
    pub fn is_empty(&self) -> bool {
        self.insns.is_empty()
    }
}

pub fn decode_one(s: &St, pc: u32) -> Result<Dec, Refuse> {
    let d = decode_at(|at| s.mem.read_sw(at), pc);
    Dec::from_decoded(&d).ok_or_else(|| Refuse(format!("undecodable instruction at {pc:#x}")))
}

fn env_of(s: &St, flag_v: bool) -> Env {
    let mut regs = [None; NREGS as usize];
    for (c, slot) in regs.iter_mut().enumerate() {
        if s.r[c].is_c() {
            *slot = Some(s.r[c].b);
        }
    }
    Env {
        regs,
        assume_nw32: s.cfg.assume_nw32,
        mode1: s.r[MODE1].b,
        flag_v,
        approx_recips: s.cfg.approx_recips,
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
    if !s.r[MODE1].is_c() {
        return Err(Refuse("MODE1 unknown".into()));
    }
    // Conditions need the kernel to track the flags they read.
    let flag_v = decs
        .iter()
        .any(|(_, d)| d.field("cond").is_some_and(|c| c != 0x1f));
    let env = env_of(s, flag_v);
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
        if r.first_read && c < NREGS as usize {
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
            .filter(|q| matches!(q, Req::NwPlain { .. }))
            .copied()
            .collect(),
        misc: reqs
            .iter()
            .filter(|q| matches!(q, Req::Mode1 { .. } | Req::FlagsKnown(_)))
            .copied()
            .collect(),
        flag_v: out.flag_v,
        flag_groups: out.flag_groups,
        cacc: out.cacc,
        inputs: regs.iter().map(|m| m.code).collect(),
        outputs: regs
            .iter()
            .filter(|m| m.written && (m.code as u32) < NREGS)
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
        cfg: None,
    })
}

/// A CFG region entered at PC: the code reachable from there, as far as the
/// lowering goes (see `cfg`).
pub fn build_cfg(s: &St, pc: u32) -> Result<Region, Refuse> {
    if !cfg!(sharc_gen) {
        return Err(Refuse("no generated core (SHARC_GEN_DIR)".into()));
    }
    if !s.r[MODE1].is_c() {
        return Err(Refuse("MODE1 unknown".into()));
    }
    let env = env_of(s, true);
    // An instruction that cannot be lowered where it stands (an address that
    // has no range, too many windows) becomes the end of the region.
    let mut deny = std::collections::BTreeSet::new();
    for _ in 0..80 {
        match build_cfg_with(s, pc, &env, &deny) {
            Err((e, Some(at))) if at != pc && !deny.contains(&at) => {
                let _ = e;
                deny.insert(at);
            }
            Err((e, _)) => return Err(e),
            Ok(r) => return Ok(r),
        }
    }
    Err(Refuse("too many instructions cannot be lowered".into()))
}

fn build_cfg_with(
    s: &St,
    pc: u32,
    env: &Env,
    deny: &std::collections::BTreeSet<u32>,
) -> Result<Region, (Refuse, Option<u32>)> {
    let st =
        cfg::build_structure(s, env, pc, &|at| decode_one(s, at), deny).map_err(|e| (e, None))?;
    let mut plan = Plan::default();
    let mut last = None;
    for pass in 0..3u8 {
        let mut l = Lower::new(env.clone(), plan.clone(), pass);
        if let Err(e) = l.lower_cfg(&st) {
            return Err((e, l.fail_pc));
        }
        let sites = l.cfgx.as_ref().map(|x| x.sites.clone()).unwrap_or_default();
        let out = l.finish().map_err(|e| (e, None))?;
        plan = out.next.clone();
        last = Some((out, sites));
    }
    let (out, sites) = last.unwrap();
    assemble_cfg(s, pc, &st, out, sites).map_err(|e| (e, None))
}

fn assemble_cfg(
    s: &St,
    pc: u32,
    st: &cfg::Structure,
    out: Lowered,
    sites: Vec<cfg::Site>,
) -> Result<Region, Refuse> {
    out.kernel
        .validate()
        .map_err(|e| Refuse(format!("kernel invalid: {e}")))?;
    let mut reqs = out.reqs.clone();
    let mut regs = Vec::new();
    for (c, r) in out.regs.iter().enumerate() {
        if r.var.is_none() {
            continue;
        }
        // Every register the kernel touches is known at entry, so the exit
        // writes back values for all that were written (unchanged on the
        // paths that did not write them).
        if c < NREGS as usize {
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
    let mut insns = Vec::new();
    let mut forms = Vec::new();
    let mut note = |at: u32, d: &Dec| {
        for k in 0..d.len_sw {
            code_words.push((at + k, s.mem.read_sw(at + k).unwrap_or(0)));
        }
    };
    for n in st.nodes.values() {
        if matches!(n.kind, cfg::NodeKind::Stop(_)) {
            continue;
        }
        note(n.pc, &n.dec);
        insns.push(RInsn {
            pc: n.pc,
            len_sw: n.len,
        });
        forms.push(n.dec.form);
        for (p, d) in &n.slots {
            note(*p, d);
        }
    }
    // Stop nodes: the word(s) that stopped the region matter too (a change
    // may make the instruction lowerable, or no longer decode).
    for n in st.nodes.values() {
        if matches!(n.kind, cfg::NodeKind::Stop(_)) {
            code_words.push((n.pc, s.mem.read_sw(n.pc).unwrap_or(0)));
        }
    }
    let mut jumps = vec![(0u32, 0u32); st.n_jumps as usize];
    for n in st.nodes.values() {
        if let cfg::NodeKind::Jump {
            target,
            delayed: true,
            jump_idx,
            ..
        } = n.kind
        {
            jumps[jump_idx as usize] = (target, n.pc);
        }
    }
    let n_entry = st.loops.iter().take_while(|l| l.entry()).count();
    // The deepest nesting of in-region loops (for the stack-depth checks).
    let mut deepest = 0u32;
    for l in &st.loops {
        if l.entry() {
            continue;
        }
        let d = st
            .loops
            .iter()
            .filter(|o| !o.entry() && o.start <= l.start && l.end <= o.end)
            .count() as u32;
        deepest = deepest.max(d);
    }
    let meta = CfgMeta {
        sites,
        loops: st.loops.clone(),
        n_entry,
        jumps,
        deepest_in_region: deepest,
        stops: st
            .nodes
            .values()
            .filter_map(|n| match &n.kind {
                cfg::NodeKind::Stop(why) => Some((n.pc, why.clone())),
                _ => None,
            })
            .collect(),
    };
    Ok(Region {
        entry_pc: pc,
        lp: None,
        last_len_bytes: 0,
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
            .filter(|q| matches!(q, Req::NwPlain { .. }))
            .copied()
            .collect(),
        misc: reqs
            .iter()
            .filter(|q| matches!(q, Req::Mode1 { .. } | Req::FlagsKnown(_)))
            .copied()
            .collect(),
        flag_v: true,
        flag_groups: out.flag_groups,
        cacc: out.cacc,
        inputs: regs.iter().map(|m| m.code).collect(),
        outputs: regs
            .iter()
            .filter(|m| m.written && (m.code as u32) < NREGS)
            .map(|m| (m.code, 0))
            .collect(),
        regs,
        reqs,
        wins: out.wins,
        flags: Vec::new(),
        kernel: out.kernel,
        code_words,
        verified_gen: s.mem.dec_gen,
        insns,
        forms,
        cfg: Some(meta),
    })
}
