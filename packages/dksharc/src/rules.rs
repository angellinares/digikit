//! The register contract of module DSP code (sdk/dsp/REGISTERS.md) checked against the analysis of one function.
//!
//! The classes come from [`crate::rules_table`], generated from `[dsp.registers]` of sdk/sdk.toml. The register names are the ones
//! [`crate::defuse::reg_name`] prints, so they match [`crate::DefUse`] exactly. Every problem is a [`Level::Warn`] for now; [`Level::Refuse`]
//! is for the day every module in the repository builds without a warning.

use crate::model::{Analysis, Block, Direction, Function, Insn, MemOp, Mode, Mode1Op, Terminator};
use crate::rules_table::{CLOBBER, KEEP, NEVER_WRITE, RETURN, STACK};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Warn,
    Refuse,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleProblem {
    pub level: Level,
    /// `never_write`, `stack`, `keep`, `unclassified` or `uncertain`.
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
        "unclassified" => "register not in the contract table",
        "uncertain" => "the register effect of the instruction is not modelled",
        _ => "",
    }
}

/// MODE1.PEYEN (bit 21): with it set, a compute instruction on R also writes the S register (the SIMD companion).
const PEYEN: u32 = 1 << 21;

/// What the rules know about MODE1 after each instruction, in pc order (not along the paths): pc -> (MODE1 holds its entry value,
/// MODE1 may have PEYEN set).
///
/// MODE1 holds its entry value at the entry. A copy `Rn = MODE1` while it does marks `Rn` as holding the entry value until `Rn` is
/// written again. A store of MODE1 to memory while it does lets a later load into MODE1 from memory restore it (same pc-order
/// simplification as the copy). A copy `MODE1 = Rn` of such an `Rn` is a restore: MODE1 holds its entry value again and PEYEN is not set. Any other
/// write of MODE1 ends the entry value. PEYEN may be set from a `BIT SET` or `BIT TGL` of the bit, a write of the whole register with
/// an unknown value or a value with the bit set, or a `POP STS` (or a write the analysis did not keep), until a restore.
/// `BIT CLR` and a `PUSH STS` do not set PEYEN.
fn mode1_states(body: &[&Insn]) -> BTreeMap<u32, (bool, bool)> {
    let mut sorted: Vec<&Insn> = body.to_vec();
    sorted.sort_by_key(|i| i.pc);
    let mut out = BTreeMap::new();
    let mut entry = true;
    let mut peyen = false;
    let mut copies: Vec<String> = vec![];
    let mut saved_in_memory = false;
    for insn in sorted {
        let d = &insn.defuse;
        let writes_mode1 = d.writes.iter().any(|r| r == "MODE1");
        let was_entry = entry;
        let copy_out = copies_out(insn, "MODE1").map(|r| r.to_string());
        if writes_mode1 || d.mode1.is_some() {
            let restore = writes_mode1
                && if insn.mem.is_empty() {
                    d.reads.iter().any(|r| copies.contains(r))
                } else {
                    saved_in_memory && loads(insn, "MODE1")
                };
            if restore {
                entry = true;
                peyen = false;
            } else {
                match d.mode1 {
                    Some(e) => match e.op {
                        Mode1Op::Set | Mode1Op::Toggle => {
                            entry = false;
                            peyen |= e.mask & PEYEN != 0;
                        }
                        Mode1Op::Full => {
                            entry = false;
                            peyen |= e.value.is_none_or(|v| v & PEYEN != 0);
                        }
                        Mode1Op::PopSts => {
                            entry = false;
                            peyen = true;
                        }
                        Mode1Op::Clear => entry = false,
                        Mode1Op::PushSts => {}
                    },
                    None => {
                        entry = false;
                        peyen = true;
                    }
                }
            }
        }
        if stores(insn, "MODE1") && was_entry {
            saved_in_memory = true;
        }
        // The entry state before this instruction fills a copy; a copy out does not write MODE1.
        let filled = copy_out.filter(|_| entry);
        copies.retain(|r| !d.writes.contains(r));
        if let Some(r) = filled {
            copies.push(r);
        }
        out.insert(insn.pc, (entry, peyen));
    }
    out
}

/// True when a write of Bn by `insn` also loads In (PRM p.6-25). Type 7d (`Bd = B2W(Bs)`, `Bd = W2B(Bs)`) does not load I
/// (PRM p.6-15: "the corresponding I register is not implicitly updated").
fn b_write_loads_i(insn: &Insn) -> bool {
    insn.form != "7d"
}

/// The registers an instruction writes. A write of Bn also loads In (the DAG sets the index register to the new base), so In is
/// added to the writes of the analysis. Type 7d is the exception ([`b_write_loads_i`]).
fn writes_of(insn: &Insn) -> Vec<String> {
    let mut v = insn.defuse.writes.clone();
    if !b_write_loads_i(insn) {
        return v;
    }
    for r in &insn.defuse.writes {
        if let Some(n) = r.strip_prefix('B').filter(|n| n.parse::<u8>().is_ok()) {
            let i = format!("I{n}");
            if !v.contains(&i) {
                v.push(i);
            }
        }
    }
    v
}

/// A CJUMP (`DK_CALL`): direct or pc-relative.
fn is_cjump(insn: &Insn) -> bool {
    matches!(insn.form.as_str(), "25a_direct" | "25a_pcrel")
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
    // A write of Bn also loads In: never a push, pop or modify. Type 7d does not load In.
    if b_write_loads_i(insn) && insn.defuse.writes.iter().any(|r| *r == format!("B{idx}")) {
        return Sp::Bad;
    }
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
/// `25c_rframe`, `I7 = I6; I6 = DM(0, I6)`) is a frame exit, and `CJUMP` (form `25a_direct` or `25a_pcrel`) is the frame setup for a callee.
fn frame_step(insn: &Insn) -> Option<St> {
    if is_cjump(insn) {
        return Some(St::Call);
    }
    if insn.form == "25c_rframe" {
        return Some(St::Restore);
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

/// The stack problems of one function: (pc, register), then the places where the check is skipped: (pc, reason). The writes that are
/// not a push, a pop or a modify by a constant, and a return where the net change of the register since the entry is not zero.
///
/// The net change is a data flow over the blocks of the function (the sum of the pushes, pops and modifies of each block, along the
/// edges of `Block::succ`). It is skipped for the function when a block holds a change of unknown size or an illegal write, when two
/// paths reach a block with different depths, or when an edge leaves the function. A change of unknown size and a merge with different
/// depths are reported as skipped (`uncertain`). A call is assumed to leave the register as it found it.
#[allow(clippy::type_complexity)]
fn stack_problems(
    body: &[&Insn],
    f: &Function,
    rets: &[(&Block, u32)],
) -> (Vec<(u32, &'static str)>, Vec<(u32, &'static str)>) {
    let mut out = vec![];
    let mut skipped: Vec<(u32, &'static str)> = vec![];
    for &sp in STACK {
        let Some(idx) = sp.strip_prefix('I').and_then(|n| n.parse::<u8>().ok()) else {
            continue;
        };
        let mut step: BTreeMap<u32, Option<St>> = BTreeMap::new();
        for insn in body {
            if sp == "I7"
                && let Some(st) = frame_step(insn)
            {
                step.insert(insn.pc, Some(st));
                continue;
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
                    skipped.push((insn.pc, "stack step of unknown size"));
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
        let mut mismatch: Option<u32> = None;
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
                    Some(&x) if x != after => {
                        exact = false;
                        mismatch.get_or_insert(e.to);
                    }
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
        if let Some(at) = mismatch {
            skipped.push((at, "stack depth differs at a merge"));
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
    (out, skipped)
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
///   return is not told apart. MODE1 has its own rule: it passes when it holds its entry value at every return, by the value
///   tracking of [`mode1_states`] (a save, clear, restore window may repeat).
/// - Write sets: a write of Bn also writes In (so a B7 write is a `stack` problem on I7, a restored B8 also writes I8 for `keep`).
///   Type 7d (`Bd = B2W(Bs)`, `Bd = W2B(Bs)`) writes Bn only (PRM p.6-15).
///   While MODE1 may have PEYEN set (from an instruction that may set it until a restore of the entry value), the SIMD companions
///   (`DefUse::simd_writes`, S0-S15 for R0-R15) count as writes for `never_write` and `keep`; the order is the pc order, not the paths.
/// - `unclassified`: an instruction writes a register that is in no class of the table.
/// - `uncertain`: an instruction whose register effect the analysis does not know; a stack check that is skipped (a stack step of
///   unknown size, a merge with different depths); a write of PCSTK or PCSTKP (push and pop are not told apart); a function with no
///   recognised return (the restore check is not done).
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
    let mut writes: BTreeMap<String, Vec<&Insn>> = BTreeMap::new();
    let mode1 = mode1_states(&body);
    for insn in &body {
        if let Some(why) = insn.defuse.uncertain.first() {
            out.push(problem("uncertain", insn.pc, why.clone()));
        }
        let mut names = writes_of(insn);
        // While MODE1 may have PEYEN set (in pc order), the SIMD companions are written too.
        if mode1.get(&insn.pc).is_some_and(|s| s.1) {
            for r in &insn.defuse.simd_writes {
                if !names.contains(r) {
                    names.push(r.clone());
                }
            }
        }
        for r in &names {
            if NEVER_WRITE.contains(&r.as_str()) {
                out.push(problem("never_write", insn.pc, r.clone()));
            } else if r == "I6" && is_cjump(insn) {
                // The CJUMP of a call sets I6 for the callee; the callee's RFRAME restores it.
            } else if STACK.contains(&r.as_str()) {
                // I7 is checked by `stack_problems`. A write of PCSTK or PCSTKP does not tell a push from a pop.
                if !r.starts_with('I') {
                    out.push(problem(
                        "uncertain",
                        insn.pc,
                        format!("{r} written: push or pop not told apart"),
                    ));
                }
            } else if is_keep(r) {
                writes.entry(r.clone()).or_default().push(insn);
            } else if !CLOBBER.contains(&r.as_str()) {
                out.push(problem("unclassified", insn.pc, r.clone()));
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
    if rets.is_empty() {
        out.push(problem(
            "uncertain",
            f.entry,
            "no return recognised; restore check not done".to_string(),
        ));
    }
    let (stack, skipped) = stack_problems(&body, f, &rets);
    for (addr, reg) in stack {
        out.push(problem("stack", addr, reg.to_string()));
    }
    for (addr, why) in skipped {
        out.push(problem("uncertain", addr, why.to_string()));
    }
    for (reg, w) in writes {
        let reg = reg.as_str();
        let first = w[0].pc;
        if reg == "MODE1" {
            // MODE1 passes when it holds its entry value at every return; see `mode1_states`.
            let at_entry = ends
                .iter()
                .all(|&e| mode1.range(..=e).next_back().is_none_or(|(_, s)| s.0));
            if !at_entry {
                out.push(problem("keep", first, reg.to_string()));
            }
            continue;
        }
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
                .is_none_or(|i| loads(i, reg) || copied_back(i))
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
