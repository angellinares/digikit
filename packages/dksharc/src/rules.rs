//! The register contract of module DSP code (sdk/dsp/REGISTERS.md) checked against the analysis of one function.
//!
//! The classes come from [`crate::rules_table`], generated from `[dsp.registers]` of sdk/sdk.toml. The register names are the ones
//! [`crate::defuse::reg_name`] prints, so they match [`crate::DefUse`] exactly. Every problem is a [`Level::Warn`] for now; [`Level::Refuse`]
//! is for the day every module in the repository builds without a warning.

use crate::model::{Analysis, Block, Direction, Function, Insn, MemOp, Mode, Terminator};
use crate::rules_table::{KEEP, NEVER_WRITE, RETURN, STACK};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Warn,
    Refuse,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleProblem {
    pub level: Level,
    /// `never_write`, `stack`, `keep` or `uncertain`.
    pub rule: &'static str,
    /// Name of the function.
    pub func: String,
    /// Short-word pc of the entry of the function.
    pub entry: u32,
    /// Short-word pc of the instruction (for `keep`: the first write of the register; for a `stack` imbalance: the end of the return).
    pub addr: u32,
    /// The register; for `uncertain` the reason the register effect is not known.
    pub reg: String,
}

/// One sentence for a rule, for a message.
pub fn reason(rule: &str) -> &'static str {
    match rule {
        "never_write" => "the stock interrupt code reads it at any instruction",
        "stack" => "only balanced pushes and pops, the entry value at every return",
        "keep" => "save before the first write, restore before every return",
        "uncertain" => "the register effect of the instruction is not modelled",
        _ => "",
    }
}

/// The return register counts as keep (the entry register is free to use).
fn is_keep(reg: &str) -> bool {
    KEEP.contains(&reg) || reg == RETURN
}

/// The access uses `reg` as its index or modify register (an address register is not the data that moves).
fn is_address(m: &MemOp, reg: &str) -> bool {
    m.i.is_some_and(|i| reg == format!("I{i}")) || m.m.is_some_and(|x| reg == format!("M{x}"))
}

/// The access writes `reg` as an address register: its modify register is only read, its index register is written by a post-modify.
/// An index register of a pre-modify is not written, so a load into it (`I6 = DM(0, I6)`) is a load of data.
fn is_written_address(m: &MemOp, reg: &str) -> bool {
    m.m.is_some_and(|x| reg == format!("M{x}"))
        || (matches!(m.mode, Mode::PostModify | Mode::PostModifyImm)
            && m.i.is_some_and(|i| reg == format!("I{i}")))
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
    // RFRAME (`I6 = DM(0, I6)`) reloads I6 from the frame that the caller's CJUMP pushed.
    (reg == "I6" && insn.form == "25c_rframe")
        || (insn.defuse.writes.iter().any(|r| r == reg)
            && insn
                .mem
                .iter()
                .any(|m| m.dir == Direction::Load && !is_written_address(m, reg)))
}

/// A data register name (`R0` to `R15`).
fn is_data_reg(r: &str) -> bool {
    r.strip_prefix('R')
        .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// The instruction copies `reg` into a data register and writes nothing else (`R0 = MODE1`, `R0 = I6`): the data register.
fn copies_out<'a>(insn: &'a Insn, reg: &str) -> Option<&'a str> {
    let w = &insn.defuse.writes;
    (insn.mem.is_empty()
        && insn.defuse.reads.iter().any(|r| r == reg)
        && w.len() == 1
        && is_data_reg(&w[0]))
    .then(|| w[0].as_str())
}

/// The instruction copies the data register `rn` into `reg` (`MODE1 = R0`).
fn copies_in(insn: &Insn, reg: &str, rn: &str) -> bool {
    insn.mem.is_empty()
        && insn.defuse.reads.iter().any(|r| r == rn)
        && insn.defuse.writes.iter().any(|r| r == reg)
}

/// What an instruction does to the stack register (`sp`, index register number `idx`).
enum Sp {
    /// It does not write the register.
    No,
    /// A push or pop, or a modify by a known constant: the change of the register (in words).
    Delta(i64),
    /// An allowed kind of change whose size is not known here: a conditional one, or a modify by an immediate whose value the
    /// analysis did not keep (`Insn::imm`).
    Unknown,
    /// Any other write.
    Bad,
}

/// The kind of the change of `sp` by one instruction. Allowed: a memory access that post-modifies `sp` with M6 (+1) or M7 (-1)
/// or by an immediate (`MemOp::mode`, `MemOp::i`, `MemOp::m` and `MemOp::imm` of `Insn::mem`); a MODIFY with M6 or M7 (the form is one
/// of the register modifies and `DefUse::reads` holds M6 or M7); a MODIFY by an immediate (form `19a`). The registers an instruction
/// writes are `Insn::defuse.writes`.
fn sp_step(insn: &Insn, sp: &str, idx: u8) -> Sp {
    if !insn.defuse.writes.iter().any(|r| r == sp) {
        return Sp::No;
    }
    let posts: Vec<&MemOp> = insn
        .mem
        .iter()
        .filter(|m| m.i == Some(idx) && matches!(m.mode, Mode::PostModify | Mode::PostModifyImm))
        .collect();
    let step = if !posts.is_empty() {
        let mut sum = 0i64;
        for m in posts {
            match (m.mode, m.m, m.imm) {
                (Mode::PostModify, Some(6), _) => sum += 1,
                (Mode::PostModify, Some(7), _) => sum -= 1,
                (Mode::PostModifyImm, _, Some(d)) => sum += d,
                _ => return Sp::Bad,
            }
        }
        Sp::Delta(sum)
    } else {
        match insn.form.as_str() {
            "7a" | "7b" | "16a" | "16b" => {
                let reads = |m: &str| insn.defuse.reads.iter().any(|r| r == m);
                match (reads("M6"), reads("M7")) {
                    (true, false) => Sp::Delta(1),
                    (false, true) => Sp::Delta(-1),
                    _ => return Sp::Bad,
                }
            }
            "19a" | "19a_scaled" => match insn.imm {
                Some(d) => Sp::Delta(d),
                None => Sp::Unknown,
            },
            _ => return Sp::Bad,
        }
    };
    match step {
        Sp::Delta(_) if insn.defuse.conditional => Sp::Unknown,
        s => s,
    }
}

/// The words the caller's `DK_CALL` pushes (I6 and the return address) before the callee runs: a DK routine is entered with I7 this
/// far below the I7 of its caller (sdk/dsp/contract/call.inc).
const CALL_PUSHES: i64 = 2;

/// A step of the stack data flow: a change of the depth, the frame setup `I6 = I7` (records the depth), the frame exit `I7 = I6`
/// (restores the recorded depth), the first instruction `DK_ENTER` of a DK routine, or a `CJUMP` (`DK_CALL`).
///
/// The depth is counted from the entry of the function. `Enter` is the frame setup of a DK routine: the frame pointer I6 that the
/// caller's `CJUMP` set is the caller's I7, `CALL_PUSHES` words above the entry, so `Enter` moves the origin of the depth to the
/// caller's I7 (the entry is then at -`CALL_PUSHES`), records that origin as the frame and applies the modify. `RFRAME` (`I7 = I6`)
/// then returns to depth 0, the value the check expects at a return. `Call` is the frame setup for the callee: the callee's `RFRAME` returns I7 and I6 to their values at the
/// `CJUMP`, before its two delayed pushes, so the block of the call ends with the depth and frame it had at the `CJUMP`.
#[derive(Clone, Copy)]
enum St {
    Delta(i64),
    Save,
    Restore,
    Enter(i64),
    Call,
}

/// The frame setup or exit of a C function: a register copy that writes only I6 (from I7) or only I7 (from I6). `RFRAME` (form
/// `25c_rframe`, `I7 = I6; I6 = DM(0, I6)`) is a frame exit, and `CJUMP` (form `25a_direct`) is the frame setup for a callee.
fn frame_step(insn: &Insn) -> Option<St> {
    match insn.form.as_str() {
        "25c_rframe" => return Some(St::Restore),
        "25a_direct" => return Some(St::Call),
        _ => {}
    }
    if !insn.mem.is_empty() || insn.defuse.writes.len() != 1 || insn.defuse.conditional {
        return None;
    }
    let reads = |r: &str| insn.defuse.reads.iter().any(|x| x == r);
    match insn.defuse.writes[0].as_str() {
        "I6" if reads("I7") => Some(St::Save),
        "I7" if reads("I6") => Some(St::Restore),
        _ => None,
    }
}

/// The stack problems of one function: (pc, register). The writes that are not a push, a pop or a modify by a constant, and a return
/// where the net change of the register since the entry is not zero.
///
/// The net change is a data flow over the blocks of the function (the sum of the pushes, pops and modifies of each block, along the
/// edges of `Block::succ`). It is skipped for the function when a block holds a change of unknown size or an illegal write, when two
/// paths reach a block with different depths, or when an edge leaves the function. A call is assumed to leave the register as it
/// found it.
fn stack_problems(
    body: &[&Insn],
    f: &Function,
    rets: &[(&Block, u32)],
) -> Vec<(u32, &'static str)> {
    let mut out = vec![];
    for &sp in STACK {
        let Some(idx) = sp.strip_prefix('I').and_then(|n| n.parse::<u8>().ok()) else {
            continue;
        };
        let mut step: BTreeMap<u32, Option<St>> = BTreeMap::new();
        for insn in body {
            if sp == "I7" {
                if let Some(st) = frame_step(insn) {
                    step.insert(insn.pc, Some(st));
                    continue;
                }
            }
            match sp_step(insn, sp, idx) {
                Sp::No => {}
                // The first instruction of the routine, `MODIFY(I7, -N) (NW)`: `DK_ENTER`.
                Sp::Delta(d)
                    if sp == "I7" && insn.pc == f.entry && insn.form.starts_with("19a") =>
                {
                    step.insert(insn.pc, Some(St::Enter(d)));
                }
                Sp::Delta(d) => {
                    step.insert(insn.pc, Some(St::Delta(d)));
                }
                Sp::Unknown => {
                    step.insert(insn.pc, None);
                }
                Sp::Bad => {
                    out.push((insn.pc, sp));
                    step.insert(insn.pc, None);
                }
            }
        }
        // The state after one block: None when it holds a change of unknown size. A restore without a recorded frame is a problem.
        let bad_restore: std::cell::RefCell<Vec<u32>> = Default::default();
        let apply =
            |b: &Block, from: (i64, Option<i64>), report: bool| -> Option<(i64, Option<i64>)> {
                let (mut d, mut fr) = from;
                if b.start >= b.end {
                    return Some((d, fr));
                }
                let mut at_call = None;
                for (pc, s) in step.range(b.start..b.end) {
                    match (*s)? {
                        St::Delta(x) => d += x,
                        St::Save => fr = Some(d),
                        St::Enter(x) => {
                            // The new origin is the caller's I7, CALL_PUSHES above the current depth.
                            let origin = d + CALL_PUSHES;
                            d += x - origin;
                            fr = Some(0);
                        }
                        St::Call => at_call = Some((d, fr)),
                        St::Restore => match fr {
                            Some(x) => d = x,
                            None => {
                                if report {
                                    bad_restore.borrow_mut().push(*pc);
                                }
                                return None;
                            }
                        },
                    }
                }
                // The return edge of a call: the callee's RFRAME undoes the delayed pushes of the CJUMP.
                match at_call {
                    Some(state) if matches!(b.term, Terminator::Call { .. }) => Some(state),
                    _ => Some((d, fr)),
                }
            };
        let by_start: BTreeMap<u32, &Block> = f.blocks.iter().map(|b| (b.start, b)).collect();
        if !by_start.contains_key(&f.entry) {
            continue;
        }
        let mut depth: BTreeMap<u32, (i64, Option<i64>)> = BTreeMap::new();
        depth.insert(f.entry, (0, None));
        let mut work = vec![f.entry];
        let mut exact = true;
        while let Some(s) = work.pop() {
            let b = by_start[&s];
            let Some(after) = apply(b, depth[&s], true) else {
                exact = false;
                break;
            };
            for e in &b.succ {
                if !by_start.contains_key(&e.to) {
                    continue;
                }
                match depth.get(&e.to) {
                    Some(&x) if x != after => exact = false,
                    Some(_) => {}
                    None => {
                        depth.insert(e.to, after);
                        work.push(e.to);
                    }
                }
            }
            if !exact {
                break;
            }
        }
        for pc in bad_restore.borrow().iter().copied() {
            out.push((pc, sp));
        }
        if !exact {
            continue;
        }
        for (b, end) in rets {
            let net = match depth.get(&b.start).and_then(|&d| apply(b, d, false)) {
                Some((n, _)) => n,
                None => continue,
            };
            if net != 0 {
                out.push((*end, sp));
            }
        }
    }
    out
}

/// The problems of one function of `a`.
///
/// - `never_write`: an instruction writes a register of the `never_write` class.
/// - `stack`: an instruction writes a register of the `stack` class (I7) other than by a push or pop (a post-modify with M6 or M7) or a
///   MODIFY by a constant, or the net change of the register from the entry to a return is not zero. The net change is computed by a
///   data flow over the blocks; see [`stack_problems`] for when it is skipped.
/// - `keep`: the function writes a register of the `keep` class (or the return register) and the heuristic does not find the save and
///   the restore. The heuristic: some instruction at a lower pc than the first write stores the register to memory, or copies it into a
///   data register (`R0 = MODE1`); and at each return the last write at or before the return pc (in pc order, delay slots included)
///   is a load of the register from memory, or a copy of that data register back into it (`MODE1 = R0`) when no instruction between
///   the two copies writes the data register. It looks at pc order, not at the paths, so a restore in a branch that does not reach a
///   return is not told apart. A function without a return needs no restore.
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
            } else if r == "I6" && insn.form == "25a_direct" {
                // The CJUMP of a call sets I6 for the callee; the callee's RFRAME restores it.
            } else if is_keep(r) {
                writes.entry(r.as_str()).or_default().push(insn);
            }
        }
    }
    // Each return: its block and the last instruction, with its delay slots.
    let rets: Vec<(&Block, u32)> = f
        .blocks
        .iter()
        .filter(|b| {
            matches!(b.term, Terminator::Return { .. })
                || b.term_pc
                    .is_some_and(|p| f.effects.indirect_returns.contains(&p))
        })
        .map(|b| {
            (
                b,
                b.delay_slots.last().copied().or(b.term_pc).unwrap_or(b.end),
            )
        })
        .collect();
    let ends: Vec<u32> = rets.iter().map(|(_, e)| *e).collect();
    for (addr, reg) in stack_problems(&body, f, &rets) {
        out.push(problem("stack", addr, reg.to_string()));
    }
    for (reg, w) in writes {
        let first = w[0].pc;
        // A routine that ends in RFRAME finds the caller's I6 saved in its frame (pushed by the caller's CJUMP delay slot).
        let saved = body.iter().any(|i| {
            (i.pc < first && (stores(i, reg) || copies_out(i, reg).is_some()))
                || (reg == "I6" && i.form == "25c_rframe")
        });
        // `w` is a copy back of a data register that an earlier copy filled and no instruction wrote again.
        let copied_back = |w: &Insn| {
            body.iter().any(|s| {
                s.pc < first
                    && copies_out(s, reg).is_some_and(|rn| {
                        copies_in(w, reg, rn)
                            && s.pc < w.pc
                            && !body.iter().any(|x| {
                                x.pc > s.pc
                                    && x.pc < w.pc
                                    && x.defuse.writes.iter().any(|r| r == rn)
                            })
                    })
            })
        };
        let restored = ends.iter().all(|&e| {
            w.iter()
                .rfind(|i| i.pc <= e)
                .is_none_or(|i| loads(i, reg) || copied_back(*i))
        });
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
