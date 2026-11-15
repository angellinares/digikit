//! Per-function summaries of the register effects: the registers written on any path from the entry to a return, and the net
//! effect on MODE1.
//!
//! A path is a path of the block graph of the function ([`Block::succ`]). A call adds the summary of a local callee (iterated to a
//! fixed point, so recursion converges); a call to an import, through registers or outside the text has unknown effects and is
//! listed, never assumed harmless. An indirect jump or a jump out of the function ends a path and is listed.

use crate::defuse::{Effects, reg_name, simd_companions};
use crate::defuse_table::MODE1;
use crate::model::*;
use std::collections::{BTreeMap, BTreeSet};

const MAX_ROUNDS: usize = 12;
const MAX_STS: usize = 8;

/// The state of one MODE1 bit relative to the value at function entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rel {
    Same,
    Inv,
    Zero,
    One,
    Top,
}

impl Rel {
    fn join(self, o: Rel) -> Rel {
        if self == o { self } else { Rel::Top }
    }
    fn toggle(self) -> Rel {
        match self {
            Rel::Same => Rel::Inv,
            Rel::Inv => Rel::Same,
            Rel::Zero => Rel::One,
            Rel::One => Rel::Zero,
            Rel::Top => Rel::Top,
        }
    }
}

type Bits = [Rel; 32];

#[derive(Clone, Debug, PartialEq, Eq)]
struct Mode1State {
    bits: Bits,
    /// Snapshots of PUSH STS.
    sts: Vec<Bits>,
    /// Registers that hold a copy of MODE1 (`Rn = MODE1`), with the state at the copy.
    saved: BTreeMap<u8, Bits>,
    /// A join of different status stack depths, or a POP STS of an empty stack: MODE1 after it is unknown.
    sts_bad: bool,
}

impl Mode1State {
    fn entry() -> Self {
        Mode1State {
            bits: [Rel::Same; 32],
            sts: vec![],
            saved: BTreeMap::new(),
            sts_bad: false,
        }
    }

    fn join(&self, o: &Mode1State) -> Mode1State {
        let mut bits = self.bits;
        for (b, ob) in bits.iter_mut().zip(o.bits) {
            *b = b.join(ob);
        }
        let (sts, sts_bad) = if self.sts.len() == o.sts.len() {
            let v = self
                .sts
                .iter()
                .zip(&o.sts)
                .map(|(a, b)| {
                    let mut x = *a;
                    for (xi, bi) in x.iter_mut().zip(b) {
                        *xi = xi.join(*bi);
                    }
                    x
                })
                .collect();
            (v, self.sts_bad || o.sts_bad)
        } else {
            (vec![], true)
        };
        let saved = self
            .saved
            .iter()
            .filter(|(k, v)| o.saved.get(k) == Some(v))
            .map(|(k, v)| (*k, *v))
            .collect();
        Mode1State {
            bits,
            sts,
            saved,
            sts_bad,
        }
    }

    fn top(&mut self) {
        self.bits = [Rel::Top; 32];
    }

    fn step(&mut self, fx: &Effects) {
        // `Rn = MODE1` and `MODE1 = Rn`: a saved copy restores the state it holds.
        let mut restored_from: Option<Bits> = None;
        if let Some((src, dst)) = fx.moved
            && dst == MODE1
        {
            restored_from = self.saved.get(&src).copied();
        }
        if let Some(m) = &fx.mode1 {
            match m.op {
                Mode1Op::Full => match (restored_from, m.value) {
                    (Some(b), _) => self.bits = b,
                    (None, Some(v)) => {
                        for (i, b) in self.bits.iter_mut().enumerate() {
                            *b = if v >> i & 1 == 1 { Rel::One } else { Rel::Zero };
                        }
                    }
                    (None, None) => self.top(),
                },
                Mode1Op::Set | Mode1Op::Clear | Mode1Op::Toggle => {
                    for (i, b) in self.bits.iter_mut().enumerate() {
                        if m.mask >> i & 1 == 1 {
                            *b = match m.op {
                                Mode1Op::Set => Rel::One,
                                Mode1Op::Clear => Rel::Zero,
                                _ => b.toggle(),
                            };
                        }
                    }
                }
                Mode1Op::PushSts => {
                    if self.sts.len() < MAX_STS {
                        self.sts.push(self.bits);
                    } else {
                        self.sts_bad = true;
                    }
                    // MODE1 is masked by MMASK, which is not known here.
                    self.top();
                }
                Mode1Op::PopSts => match self.sts.pop() {
                    Some(b) => self.bits = b,
                    None => {
                        self.sts_bad = true;
                        self.top();
                    }
                },
            }
        }
        // A write to a register that holds a copy ends the copy; the copy instruction itself starts one.
        for w in &fx.writes {
            self.saved.remove(w);
        }
        if let Some((src, dst)) = fx.moved
            && src == MODE1
            && dst < 16
            && !self.sts_bad
        {
            self.saved.insert(dst, self.bits);
        }
    }
}

struct FnFx<'a> {
    /// The effects of the instructions by pc.
    insns: &'a [(u32, &'a Effects)],
    calls: BTreeMap<u32, &'a Call>,
}

#[derive(Clone, Default)]
struct Summary {
    writes: BTreeSet<u8>,
    mode1_changed: u32,
    mode1_unknown: u32,
    exact: bool,
    unknown_calls: BTreeSet<String>,
    complete: bool,
}

fn block_insns<'a>(ctx: &'a FnFx<'a>, b: &Block) -> &'a [(u32, &'a Effects)] {
    let lo = ctx.insns.partition_point(|i| i.0 < b.start);
    let hi = ctx.insns.partition_point(|i| i.0 < b.end);
    &ctx.insns[lo..hi]
}

fn analyse(
    ctx: &FnFx,
    fi: usize,
    f: &Function,
    summaries: &[Summary],
    entries: &BTreeMap<u32, usize>,
) -> (FunctionEffects, Summary, Vec<(u32, Mode1Effect)>) {
    let nb = f.blocks.len();
    let by_start: BTreeMap<u32, usize> = f
        .blocks
        .iter()
        .enumerate()
        .map(|(i, b)| (b.start, i))
        .collect();
    let mut fe = FunctionEffects::default();
    let mut sum = Summary {
        exact: true,
        ..Summary::default()
    };
    let mut mode1_writes = vec![];
    if nb == 0 {
        fe.complete = false;
        return (fe, sum, mode1_writes);
    }
    // Reachability from the entry, path ends, and the blocks that reach a path end.
    let succ: Vec<Vec<usize>> = f
        .blocks
        .iter()
        .map(|b| {
            b.succ
                .iter()
                .filter_map(|e| by_start.get(&e.to).copied())
                .collect()
        })
        .collect();
    let entry = by_start.get(&f.entry).copied().unwrap_or(0);
    let mut reach = vec![false; nb];
    let mut stack = vec![entry];
    while let Some(b) = stack.pop() {
        if std::mem::replace(&mut reach[b], true) {
            continue;
        }
        stack.extend(succ[b].iter().copied());
    }
    let mut ends = vec![];
    let mut other_exits = BTreeSet::new();
    for (i, b) in f.blocks.iter().enumerate() {
        if !reach[i] {
            continue;
        }
        match &b.term {
            Terminator::Return { .. } => {
                ends.push(i);
                fe.returns = true;
            }
            Terminator::Jump { target, .. }
                if target.is_none_or(|t| !by_start.contains_key(&t)) =>
            {
                ends.push(i);
                other_exits.insert(b.term_pc.unwrap_or(b.start));
            }
            // The two return idioms of the module contract: `JUMP (M13, I13)` (machine and effect entries, the adapters' routine
            // ABI) and `JUMP (M14, I12)` (the C calling convention). Every other indirect jump is not analysed.
            Terminator::IndirectJump { i: ri, m: rm, .. } => {
                ends.push(i);
                if matches!((*ri, *rm), (13, 13) | (12, 14)) {
                    fe.returns = true;
                    fe.indirect_returns.push(b.term_pc.unwrap_or(b.start));
                } else {
                    other_exits.insert(b.term_pc.unwrap_or(b.start));
                }
            }
            _ => {}
        }
    }
    let mut pred: Vec<Vec<usize>> = vec![vec![]; nb];
    for (i, s) in succ.iter().enumerate() {
        for t in s {
            pred[*t].push(i);
        }
    }
    let mut on_path = vec![false; nb];
    let mut work: Vec<usize> = ends.clone();
    if ends.is_empty() {
        // No path end: the function loops forever or ends through an unmodelled transfer. Take everything reachable.
        work = (0..nb).filter(|i| reach[*i]).collect();
    }
    while let Some(b) = work.pop() {
        if std::mem::replace(&mut on_path[b], true) {
            continue;
        }
        work.extend(pred[b].iter().copied().filter(|p| reach[*p]));
    }
    fe.other_exits = other_exits.into_iter().collect();
    let mut complete = f.complete
        && ends
            .iter()
            .all(|e| !matches!(f.blocks[*e].term, Terminator::Unmodelled { .. }));
    if f.blocks
        .iter()
        .enumerate()
        .any(|(i, b)| reach[i] && matches!(b.term, Terminator::Unmodelled { .. }))
    {
        complete = false;
    }
    if !fe.other_exits.is_empty() || !fe.returns {
        complete = false;
    }
    // Writes and calls of the blocks on a path.
    let mut unknown: BTreeSet<String> = BTreeSet::new();
    let mut uncertain: BTreeSet<u32> = BTreeSet::new();
    let mut callee_complete = true;
    let mut callee_mode1: BTreeMap<u32, (u32, u32, bool, BTreeSet<u8>)> = BTreeMap::new();
    for (i, b) in f.blocks.iter().enumerate() {
        if !on_path[i] || !reach[i] {
            continue;
        }
        for (pc, fx) in block_insns(ctx, b) {
            sum.writes.extend(fx.writes.iter().copied());
            if !fx.uncertain.is_empty() {
                uncertain.insert(*pc);
            }
            if let Some(c) = ctx.calls.get(pc) {
                match &c.target {
                    CallTarget::Local { pc: t } => {
                        if let Some(ci) = entries.get(t) {
                            let s = &summaries[*ci];
                            sum.writes.extend(s.writes.iter().copied());
                            unknown.extend(s.unknown_calls.iter().cloned());
                            callee_complete &= s.complete;
                            callee_mode1.insert(
                                *pc,
                                (s.mode1_changed, s.mode1_unknown, s.exact, s.writes.clone()),
                            );
                        } else {
                            unknown.insert(format!("local:{t:#x}"));
                        }
                    }
                    other => {
                        let label = match other {
                            CallTarget::Import { symbol } => format!("import:{symbol}"),
                            CallTarget::Indirect { i, m } => format!("indirect:I{i},M{m}"),
                            CallTarget::Outside { pc } => format!("outside:{pc:#x}"),
                            CallTarget::Local { .. } => unreachable!(),
                        };
                        unknown.insert(label);
                    }
                }
            }
            if let Some(m) = &fx.mode1 {
                mode1_writes.push((*pc, *m));
            }
        }
    }
    let _ = fi;
    // MODE1 data flow over the reachable blocks.
    let mut ins: Vec<Option<Mode1State>> = vec![None; nb];
    ins[entry] = Some(Mode1State::entry());
    let mut outs: Vec<Option<Mode1State>> = vec![None; nb];
    let mut inexact = !unknown.is_empty();
    let mut queue: Vec<usize> = vec![entry];
    let mut rounds = 0usize;
    while let Some(b) = queue.pop() {
        rounds += 1;
        if rounds > 200 * nb.max(1) {
            inexact = true;
            break;
        }
        let Some(mut st) = ins[b].clone() else {
            continue;
        };
        for (pc, fx) in block_insns(ctx, &f.blocks[b]) {
            st.step(fx);
            if let Some((cm, cu, cexact, cw)) = callee_mode1.get(pc) {
                for (i, bit) in st.bits.iter_mut().enumerate() {
                    if (cm | cu) >> i & 1 == 1 {
                        *bit = Rel::Top;
                    }
                }
                inexact |= !cexact;
                st.saved.retain(|r, _| !cw.contains(r));
            } else if ctx.calls.contains_key(pc) {
                // An unknown callee: its effect on MODE1 and on the saved copies is not known.
                st.saved.clear();
            }
        }
        if outs[b].as_ref() == Some(&st) {
            continue;
        }
        outs[b] = Some(st.clone());
        for s in &succ[b] {
            let joined = match &ins[*s] {
                None => st.clone(),
                Some(old) => old.join(&st),
            };
            if ins[*s].as_ref() != Some(&joined) {
                ins[*s] = Some(joined);
                queue.push(*s);
            }
        }
    }
    let mut at_end: Option<Mode1State> = None;
    let exits: Vec<usize> = if ends.is_empty() {
        (0..nb).filter(|i| reach[*i]).collect()
    } else {
        ends.clone()
    };
    for e in exits {
        if let Some(s) = &outs[e] {
            at_end = Some(match at_end {
                None => s.clone(),
                Some(a) => a.join(s),
            });
        }
    }
    let end = at_end.unwrap_or_else(Mode1State::entry);
    let mut changed = 0u32;
    let mut unknown_bits = 0u32;
    for (i, b) in end.bits.iter().enumerate() {
        if *b != Rel::Same {
            changed |= 1 << i;
        }
        if *b == Rel::Top {
            unknown_bits |= 1 << i;
        }
    }
    if end.sts_bad {
        inexact = true;
    }
    if !end.sts.is_empty() {
        // A PUSH STS without its POP: the entry MODE1 is not back.
        changed = u32::MAX;
        unknown_bits = u32::MAX;
    }
    sum.mode1_changed = changed;
    sum.mode1_unknown = unknown_bits;
    sum.exact = !inexact;
    complete = complete && unknown.is_empty() && uncertain.is_empty() && callee_complete;
    sum.unknown_calls = unknown.clone();
    sum.complete = complete;
    fe.writes = sum.writes.iter().map(|c| reg_name(*c)).collect();
    // A name sort that follows the register file order is wanted: codes are in that order already.
    let comp = simd_companions(&sum.writes);
    fe.simd_writes = comp.iter().map(|c| reg_name(*c)).collect();
    fe.unknown_calls = unknown.into_iter().collect();
    fe.uncertain_pcs = uncertain.into_iter().collect();
    fe.complete = complete;
    fe.mode1 = Mode1Summary {
        writes: mode1_writes
            .iter()
            .map(|(pc, m)| Mode1Write {
                pc: *pc,
                op: m.op,
                mask: m.mask,
            })
            .collect(),
        changed_mask: changed,
        unknown_mask: unknown_bits,
        restored: changed == 0 && !inexact,
        exact: !inexact,
    };
    (fe, sum, mode1_writes)
}

/// Fill [`Function::effects`] of every function. `insns` are the effects of the decoded instructions sorted by pc.
pub fn summarize(functions: &mut [Function], insns: &[(u32, &Effects)], calls: &[Call]) {
    let ctx = FnFx {
        insns,
        calls: calls.iter().map(|c| (c.pc, c)).collect(),
    };
    let entries: BTreeMap<u32, usize> = functions
        .iter()
        .enumerate()
        .map(|(i, f)| (f.entry, i))
        .collect();
    let mut summaries = vec![Summary::default(); functions.len()];
    let mut results: Vec<Option<FunctionEffects>> = vec![None; functions.len()];
    let mut converged = false;
    for _ in 0..MAX_ROUNDS {
        let mut changed = false;
        for fi in 0..functions.len() {
            let (fe, sum, _) = analyse(&ctx, fi, &functions[fi], &summaries, &entries);
            if sum.writes != summaries[fi].writes
                || sum.mode1_changed != summaries[fi].mode1_changed
                || sum.mode1_unknown != summaries[fi].mode1_unknown
                || sum.exact != summaries[fi].exact
                || sum.unknown_calls != summaries[fi].unknown_calls
                || sum.complete != summaries[fi].complete
            {
                changed = true;
            }
            summaries[fi] = sum;
            results[fi] = Some(fe);
        }
        if !changed {
            converged = true;
            break;
        }
    }
    for (f, r) in functions.iter_mut().zip(results) {
        let mut fe = r.unwrap_or_default();
        if !converged {
            fe.complete = false;
            fe.mode1.exact = false;
            fe.mode1.restored = false;
        }
        f.effects = fe;
    }
}
