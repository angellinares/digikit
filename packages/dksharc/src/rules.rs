//! The register contract of module DSP code (sdk/dsp/REGISTERS.md) checked against the analysis of one function.
//!
//! The classes come from [`crate::rules_table`], generated from `[dsp.registers]` of sdk/sdk.toml. The register names are the ones
//! [`crate::defuse::reg_name`] prints, so they match [`crate::DefUse`] exactly. Every problem is a [`Level::Warn`] for now; [`Level::Refuse`]
//! is for the day every module in the repository builds without a warning.

use crate::model::{Analysis, Direction, Function, Insn, MemOp, Terminator};
use crate::rules_table::{ENTRY, KEEP, NEVER_WRITE, RETURN};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Warn,
    Refuse,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleProblem {
    pub level: Level,
    /// `never_write`, `keep` or `uncertain`.
    pub rule: &'static str,
    /// Name of the function.
    pub func: String,
    /// Short-word pc of the entry of the function.
    pub entry: u32,
    /// Short-word pc of the instruction (for `keep`: the first write of the register).
    pub addr: u32,
    /// The register; for `uncertain` the reason the register effect is not known.
    pub reg: String,
}

/// One sentence for a rule, for a message.
pub fn reason(rule: &str) -> &'static str {
    match rule {
        "never_write" => "the stock interrupt code reads it at any instruction",
        "keep" => "save before the first write, restore before every return",
        "uncertain" => "the register effect of the instruction is not modelled",
        _ => "",
    }
}

/// The entry and return registers count as keep.
fn is_keep(reg: &str) -> bool {
    KEEP.contains(&reg) || reg == ENTRY || reg == RETURN
}

/// The access uses `reg` as its index or modify register (an address register is not the data that moves).
fn is_address(m: &MemOp, reg: &str) -> bool {
    m.i.is_some_and(|i| reg == format!("I{i}")) || m.m.is_some_and(|x| reg == format!("M{x}"))
}

/// The instruction stores `reg` to memory.
fn stores(insn: &Insn, reg: &str) -> bool {
    insn.defuse.reads.iter().any(|r| r == reg)
        && insn
            .mem
            .iter()
            .any(|m| m.dir == Direction::Store && !is_address(m, reg))
}

/// The instruction loads `reg` from memory.
fn loads(insn: &Insn, reg: &str) -> bool {
    insn.defuse.writes.iter().any(|r| r == reg)
        && insn
            .mem
            .iter()
            .any(|m| m.dir == Direction::Load && !is_address(m, reg))
}

/// The problems of one function of `a`.
///
/// - `never_write`: an instruction writes a register of the `never_write` class.
/// - `keep`: the function writes a register of the `keep` class (or the entry or return register) and the heuristic does not find the
///   save and the restore. The heuristic: some instruction at a lower pc than the first write stores the register to memory, and at
///   each return the last write at or before the return pc (in pc order, delay slots included) is a load of the register from
///   memory. It looks at pc order, not at the paths, so a restore in a branch that does not reach a return is not told apart; a save
///   by a move to another register is not a save. A function without a return needs no restore.
/// - `uncertain`: an instruction whose register effect the analysis does not know.
pub fn check(a: &Analysis, f: &Function) -> Vec<RuleProblem> {
    let body: Vec<&Insn> = a
        .insns
        .iter()
        .filter(|i| i.pc >= f.entry && i.pc < f.end)
        .collect();
    let problem = |rule: &'static str, addr: u32, reg: String| RuleProblem {
        level: Level::Warn,
        rule,
        func: f.name.clone(),
        entry: f.entry,
        addr,
        reg,
    };
    let mut out = vec![];
    let mut writes: BTreeMap<&str, Vec<&Insn>> = BTreeMap::new();
    for insn in &body {
        if let Some(why) = insn.defuse.uncertain.first() {
            out.push(problem("uncertain", insn.pc, why.clone()));
        }
        for r in &insn.defuse.writes {
            if NEVER_WRITE.contains(&r.as_str()) {
                out.push(problem("never_write", insn.pc, r.clone()));
            } else if is_keep(r) {
                writes.entry(r.as_str()).or_default().push(insn);
            }
        }
    }
    // The last instruction of each return, with its delay slots.
    let ends: Vec<u32> = f
        .blocks
        .iter()
        .filter(|b| {
            matches!(b.term, Terminator::Return { .. })
                || b.term_pc
                    .is_some_and(|p| f.effects.indirect_returns.contains(&p))
        })
        .map(|b| b.delay_slots.last().copied().or(b.term_pc).unwrap_or(b.end))
        .collect();
    for (reg, w) in writes {
        let first = w[0].pc;
        let saved = body.iter().any(|i| i.pc < first && stores(i, reg));
        let restored = ends
            .iter()
            .all(|&e| w.iter().rfind(|i| i.pc <= e).is_none_or(|i| loads(i, reg)));
        if !saved || !restored {
            out.push(problem("keep", first, reg.to_string()));
        }
    }
    out.sort_by(|x, y| (x.addr, &x.reg).cmp(&(y.addr, &y.reg)));
    out
}

/// The problems of every function of `a`.
pub fn check_all(a: &Analysis) -> Vec<RuleProblem> {
    a.functions.iter().flat_map(|f| check(a, f)).collect()
}
