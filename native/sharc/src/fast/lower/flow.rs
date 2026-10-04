//! Lowering of a CFG region (`fast::cfg::Structure`) to a CFG kernel.
//!
//! The kernel is a list of basic blocks. Registers live in variables across
//! blocks (each block flushes what it wrote; see `Lower::end_block`), the
//! interpreter's own loop state lives in extra variables: the retired
//! instruction count at the start of the current block, one remaining count
//! per loop, LCNTR and a few event flags. Every exit (a failed guard, a
//! `Leave`) names an exit site (`cfg::Site`) from which the glue rebuilds the
//! engine's PC, loop stack and PC stack.
//!
//! The budget is checked at the cut points (the entry and the targets of
//! backward edges): there the kernel proves that the longest path to the next
//! cut point fits; otherwise it leaves at the cut point, so the instruction
//! count never passes the engine's limit and an exit at a cut point is a
//! clean block boundary.

use super::*;
use crate::fast::cfg::{LoopCount, LoopDef, Node, NodeKind, Structure};
use std::collections::HashMap;

fn merge(a: &[Option<Abs>], b: &[Option<Abs>]) -> Vec<Option<Abs>> {
    a.iter()
        .zip(b)
        .map(|(x, y)| match (x, y) {
            (Some(p), Some(q)) => p.union(*q),
            _ => None,
        })
        .collect()
}

/// What flows into each label from the edges seen so far: the registers'
/// ranges and the ASTATX bits known on every path.
#[derive(Default)]
struct Joins {
    abs: HashMap<u32, Vec<Option<Abs>>>,
    fm: HashMap<u32, u32>,
}

impl Joins {
    fn edge(&mut self, pc: u32, abs: &[Option<Abs>], fm: u32) {
        match self.abs.get(&pc) {
            Some(old) => {
                let merged = merge(old, abs);
                self.abs.insert(pc, merged);
            }
            None => {
                self.abs.insert(pc, abs.to_vec());
            }
        }
        self.fm.entry(pc).and_modify(|x| *x &= fm).or_insert(fm);
    }
}

#[derive(Clone, Copy)]
enum Count {
    Lit(i64),
    /// The remaining iterations of the innermost loop active at entry.
    Entry,
    Unknown,
}

/// The range of a register over the iterations of a loop that adds D each
/// time, from its range A at the first iteration.
fn widen(a: Abs, d: i64, kind: Count) -> Option<Abs> {
    match kind {
        Count::Lit(n) => {
            let span = d.checked_mul(n - 1)?;
            Abs {
                lo: a.lo + span.min(0),
                hi: a.hi + span.max(0),
                ..a
            }
            .shifted(0)
        }
        Count::Entry if a.ts == 0 => Some(Abs { ts: d, ..a }),
        _ => None,
    }
}

struct Vars {
    icnt: Var,
    rem: Vec<Var>,
    lcntr: Var,
    lset: Var,
    stk26: Var,
    didpc: Var,
    taken: Vec<Var>,
    /// The loop (plus one) that last started at each loop-stack depth.
    slot_last: Vec<Var>,
}

impl Lower {
    fn cfg_x(&mut self) -> &mut CfgX {
        self.cfgx.as_mut().expect("cfg lowering")
    }

    /// An exit list with no fix-ups (registers are all in variables).
    fn empty_exit(&mut self) -> u32 {
        self.exits.push(Vec::new());
        (self.exits.len() - 1) as u32
    }

    /// End the current block for a branch: the retired count goes into the
    /// counter variable and the registers into theirs.
    fn close_block(&mut self, v: &Vars) {
        let n = self.cfg_x().nblock;
        if n > 0 {
            let cur = self.emit(Ty::I32, Op::GetVar(v.icnt));
            let k = self.ci(n);
            let sum = self.bin(Bin::Add, cur, k);
            self.body.push(Inst::Set(v.icnt, sum));
            self.cfg_x().nblock = 0;
        }
        self.end_block();
    }

    /// Forget everything a block that ended without a flush computed.
    fn reset_block(&mut self) {
        for c in 0..NALL {
            self.cur[c] = None;
            self.written_iter[c] = false;
        }
        self.conv.clear();
        self.exit_id = None;
        self.lw = [None; 3];
        self.cfg_x().nblock = 0;
    }

    fn set_ctx(&mut self, pc: u32, active: Vec<u16>, pending: Option<(u16, u8)>) {
        let x = self.cfg_x();
        x.pc = pc;
        x.active = active;
        x.pending = pending;
    }

    fn set_var(&mut self, var: Var, c: u32) {
        let v = self.ci(c);
        self.body.push(Inst::Set(var, v));
    }

    /// A stub leaving the kernel at PC (taken branch out of the kernel, a
    /// loop-back to an address the kernel does not hold).
    fn stub(&mut self, stubs: &mut Vec<(u32, u32)>, pc: u32, active: Vec<u16>) -> u32 {
        let k = self.site_at(pc, 0, active);
        let l = self.new_label();
        stubs.push((l, k));
        l
    }

    pub fn lower_cfg(&mut self, st: &Structure) -> LR<()> {
        self.cfgx = Some(CfgX {
            pc: st.entry,
            nblock: 0,
            active: Vec::new(),
            pending: None,
            sites: Vec::new(),
        });
        self.cfg_shape = true;
        self.var_abs = (0..NREGS as usize).map(|c| self.init_abs_pub(c)).collect();
        self.loop_stride_out = vec![[None; NREGS as usize]; st.loops.len()];
        self.loop_back_seen_out = vec![false; st.loops.len()];
        self.loop_in = vec![Vec::new(); st.loops.len()];
        self.loop_head = vec![Vec::new(); st.loops.len()];
        let budget = self.new_val(Ty::I32, None);
        self.pre.push(Inst::Def(budget, Op::Ctx32(CTX_BUDGET)));
        let icnt = self.new_var(Ty::I32, ExtraInit::Const(0), Some(CTX_ICNT));
        let mut rem = Vec::new();
        for (i, l) in st.loops.iter().enumerate() {
            let init = if l.entry() {
                ExtraInit::Ctx(CTX_LOOP_REM_IN + 4 * i as u32)
            } else {
                ExtraInit::Const(0)
            };
            rem.push(self.new_var(Ty::I32, init, Some(CTX_LOOP_REM_OUT + 4 * i as u32)));
        }
        let lcntr = self.new_var(Ty::I32, ExtraInit::Const(0), Some(CTX_LCNTR));
        let lset = self.new_var(Ty::I32, ExtraInit::Const(0), Some(CTX_LCNTR + 4));
        let stk26 = self.new_var(Ty::I32, ExtraInit::Const(0), Some(CTX_STK26));
        let didpc = self.new_var(Ty::I32, ExtraInit::Const(0), Some(CTX_DIDPC));
        let taken: Vec<Var> = (0..st.n_jumps)
            .map(|j| self.new_var(Ty::I32, ExtraInit::Const(0), Some(CTX_TAKEN + 4 * j as u32)))
            .collect();
        let slot_last: Vec<Var> = (0..MAX_SLOTS)
            .map(|d| self.new_var(Ty::I32, ExtraInit::Const(0), Some(CTX_SLOT_LAST + 4 * d)))
            .collect();
        let v = Vars {
            slot_last,
            icnt,
            rem,
            lcntr,
            lset,
            stk26,
            didpc,
            taken,
        };
        let mut label_of: HashMap<u32, u32> = HashMap::new();
        for &pc in &st.leaders {
            let l = self.new_label();
            label_of.insert(pc, l);
        }
        let mut stubs: Vec<(u32, u32)> = Vec::new();
        let mut joins = Joins::default();
        // The block is entered by the jump below.
        self.body.push(Inst::Jump(label_of[&st.entry]));
        let mut open = false; // the previous node falls into the next
        let mut idx = 0u32;
        for (&pc, node) in &st.nodes {
            idx += 1;
            let active = st.ctx.get(&pc).cloned().unwrap_or_default();
            if st.leaders.contains(&pc) {
                if open {
                    self.close_block(&v);
                    let cur = self.var_abs.clone();
                    let fm = self.fm_static;
                    joins.edge(pc, &cur, fm);
                } else {
                    // The previous block ended in a jump or an exit: nothing
                    // it computed is visible here.
                    self.reset_block();
                }
                if let Some(a) = joins.abs.get(&pc) {
                    self.var_abs = a.clone();
                }
                self.fm_static = joins.fm.get(&pc).copied().unwrap_or(0);
                // A register written on a cycle through a backward branch has
                // no known range here.
                for &(last, target) in &st.back_edges {
                    if target == pc {
                        let w = self.written_in(pc, last);
                        self.blank(w);
                    }
                }
                // A loop starts: registers its body steps get the range of
                // all its iterations (see `loop_enter`).
                for (i, l) in st.loops.iter().enumerate() {
                    if l.start == pc {
                        self.loop_enter(st, i);
                    }
                }
                self.body.push(Inst::Label(label_of[&pc]));
                if let Some(&len) = st.cuts.get(&pc) {
                    self.set_ctx(pc, active.clone(), None);
                    self.begin_insn(idx);
                    let ic = self.emit(Ty::I32, Op::GetVar(v.icnt));
                    let k = self.ci(len);
                    let sum = self.bin(Bin::Add, ic, k);
                    let ok = self.bin(Bin::LeU, sum, budget);
                    self.guard(ok);
                }
            } else if !open {
                // Not reached (it was found through an edge that turned out
                // to leave the kernel).
                continue;
            }
            open = true;
            self.set_ctx(pc, active.clone(), None);
            if let Err(e) = self.lower_node(
                st, node, idx, &v, &label_of, &mut stubs, &mut joins, &mut open,
            ) {
                // A node inside a loop started in the region takes the DO of
                // the outermost such loop with it.
                let at = st
                    .ctx
                    .get(&pc)
                    .and_then(|chain| chain.iter().find(|&&i| !st.loops[i as usize].entry()))
                    .map_or(pc, |&i| st.loops[i as usize].do_pc);
                self.fail_pc = Some(at);
                return Err(Refuse(format!("{pc:#x}: {}", e.0)));
            }
        }
        if open {
            return refuse("the region falls off its last instruction");
        }
        for (l, k) in stubs {
            self.body.push(Inst::Label(l));
            let exit = self.empty_exit();
            self.body.push(Inst::Leave { k, exit });
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn lower_node(
        &mut self,
        st: &Structure,
        node: &Node,
        idx: u32,
        v: &Vars,
        label_of: &HashMap<u32, u32>,
        stubs: &mut Vec<(u32, u32)>,
        joins: &mut Joins,
        open: &mut bool,
    ) -> LR<()> {
        let pc = node.pc;
        let active = st.ctx.get(&pc).cloned().unwrap_or_default();
        match &node.kind {
            NodeKind::Stop(_) => {
                self.begin_insn(idx);
                let exit = self.exit_for_insn();
                let k = self.site_k();
                self.body.push(Inst::Leave { k, exit });
                *open = false;
            }
            NodeKind::Seq => {
                self.lower_insn(idx, &node.dec)?;
                self.note_written(st, pc);
                self.cfg_x().nblock += 1;
                if let Some((i, l)) = st.loops.iter().enumerate().find(|(_, l)| l.end == pc) {
                    self.loop_back(st, i, l, v, label_of, stubs, &active)?;
                }
            }
            NodeKind::Do(li) => {
                let l = &st.loops[*li as usize];
                self.begin_insn(idx);
                let count = match l.count {
                    LoopCount::Lit(n) => self.ci(n),
                    LoopCount::Reg(code) => {
                        let c = self.rd_i(code)?;
                        let z = self.ci(0);
                        let ok = self.bin(Bin::Ne, c, z);
                        self.guard(ok);
                        c
                    }
                    LoopCount::Entry => unreachable!("entry loop DO"),
                };
                self.body.push(Inst::Set(v.rem[*li as usize], count));
                if (l.depth as usize) < v.slot_last.len() {
                    self.set_var(v.slot_last[l.depth as usize], *li as u32 + 1);
                }
                self.body.push(Inst::Set(v.lcntr, count));
                self.set_var(v.lset, 1);
                self.set_var(v.stk26, 1);
                self.set_var(v.didpc, 1);
                self.cfg_x().nblock += 1;
            }
            NodeKind::Jump {
                target,
                cond,
                delayed,
                jump_idx,
                exit_edge,
            } => {
                self.begin_insn(idx);
                let cv = if *cond == 0x1f {
                    None
                } else {
                    Some(self.cond_value(*cond)?)
                };
                if *delayed {
                    let one = self.ci(1);
                    self.body
                        .push(Inst::Set(v.taken[*jump_idx as usize], cv.unwrap_or(one)));
                }
                self.cfg_x().nblock += 1;
                for (n, (spc, sdec)) in node.slots.iter().enumerate() {
                    let left = 2 - n as u8;
                    let sctx = st.ctx.get(spc).cloned().unwrap_or_default();
                    self.set_ctx(*spc, sctx, Some((*jump_idx, left)));
                    // A slot is lowered as the ordinary instruction it is.
                    self.lower_insn(idx, sdec)?;
                    self.note_written(st, *spc);
                    self.cfg_x().nblock += 1;
                }
                self.set_ctx(pc, active.clone(), None);
                self.close_block(v);
                let dest = if *exit_edge {
                    self.stub(stubs, *target, active.clone())
                } else {
                    let cur = self.var_abs.clone();
                    let fm = self.fm_static;
                    joins.edge(*target, &cur, fm);
                    label_of[target]
                };
                match cv {
                    Some(c) => self.body.push(Inst::BrIf { c, target: dest }),
                    None => {
                        self.body.push(Inst::Jump(dest));
                        *open = false;
                    }
                }
            }
        }
        Ok(())
    }

    /// Registers written by the instruction just lowered count for every loop
    /// that holds it.
    fn note_written(&mut self, _st: &Structure, pc: u32) {
        let w = std::mem::take(&mut self.written_mask);
        *self.node_written.entry(pc).or_default() |= w;
    }

    /// The registers written by the instructions at addresses LO..=HI, as
    /// the previous pass saw them (all of them in the first pass).
    fn written_in(&self, lo: u32, hi: u32) -> u64 {
        if self.plan.node_written.is_empty() {
            return u64::MAX;
        }
        self.plan
            .node_written
            .range(lo..=hi)
            .fold(0, |a, (_, m)| a | m)
    }

    fn blank(&mut self, mask: u64) {
        for c in 0..NREGS as usize {
            if mask >> c & 1 == 1 {
                self.var_abs[c] = None;
            }
        }
    }

    /// How many iterations of loop I the abstract values can tell: its
    /// literal count, the symbolic remaining count of the innermost loop
    /// active at entry, or unknown.
    fn loop_kind(&self, st: &Structure, i: usize) -> Count {
        let n_entry = st.loops.iter().take_while(|l| l.entry()).count();
        match st.loops[i].count {
            LoopCount::Lit(n) => Count::Lit(n as i64),
            LoopCount::Entry if i + 1 == n_entry => Count::Entry,
            _ => Count::Unknown,
        }
    }

    /// At the head of loop I: the registers its body steps are widened to the
    /// range over all iterations (with the step the previous pass measured;
    /// in the first passes they keep their value at entry so the step can be
    /// measured).
    fn loop_enter(&mut self, st: &Structure, i: usize) {
        let l = &st.loops[i];
        let in_abs = self.var_abs.clone();
        let w = self.written_in(l.start, l.end);
        // Passes 0 and 1 measure (modifier registers are baked from pass 1
        // on); pass 2 widens with what pass 1 measured and checks it.
        let first = self.pass < 2;
        // A loop whose loop-back the kernel never reaches does not iterate
        // in it: the head is only entered once.
        let iterates = self.plan.loop_back_seen.get(i).copied().unwrap_or(true);
        let kind = self.loop_kind(st, i);
        for c in 0..NREGS as usize {
            if w >> c & 1 == 0 {
                continue;
            }
            self.var_abs[c] = if first || !iterates {
                in_abs[c]
            } else {
                let d = self.plan.loop_stride.get(i).and_then(|a| a[c]);
                match (d, in_abs[c]) {
                    (Some(0), a) => a,
                    (Some(d), Some(a)) => widen(a, d, kind),
                    _ => None,
                }
            };
        }
        if std::env::var_os("SHARC_FAST_DEBUG_ABS").is_some() {
            for c in 0..NREGS as usize {
                if w >> c & 1 == 1 {
                    eprintln!(
                        "abs: pass {} loop {i} enter r{c}: in {:?} stride {:?} -> head {:?}",
                        self.pass,
                        in_abs[c],
                        self.plan.loop_stride.get(i).and_then(|a| a[c]),
                        self.var_abs[c]
                    );
                }
            }
        }
        self.loop_in[i] = in_abs;
        self.loop_head[i] = self.var_abs.clone();
    }

    /// The loop-back test after the last instruction of loop I.
    #[allow(clippy::too_many_arguments)]
    fn loop_back(
        &mut self,
        st: &Structure,
        i: usize,
        l: &LoopDef,
        v: &Vars,
        label_of: &HashMap<u32, u32>,
        stubs: &mut Vec<(u32, u32)>,
        active: &[u16],
    ) -> LR<()> {
        self.close_block(v);
        self.loop_back_seen_out[i] = true;
        // The step of each register over one iteration, from the values at
        // the loop's head and now.
        let w = self.written_in(l.start, l.end);
        let kind = self.loop_kind(st, i);
        let mut exit_abs = self.var_abs.clone();
        for c in 0..NREGS as usize {
            if w >> c & 1 == 0 {
                continue;
            }
            let measured = match (self.var_abs[c], self.loop_head[i].get(c).copied().flatten()) {
                (Some(e), Some(h)) => e.step_from(h),
                _ => None,
            };
            if std::env::var_os("SHARC_FAST_DEBUG_ABS").is_some() {
                eprintln!(
                    "abs: pass {} loop {i} back r{c}: end {:?} head {:?} -> step {measured:?}",
                    self.pass,
                    self.var_abs[c],
                    self.loop_head[i].get(c)
                );
            }
            self.loop_stride_out[i][c] = measured;
            if self.pass >= 2 && self.plan.loop_stride.get(i).map(|a| a[c]) != Some(measured) {
                return refuse(format!("loop at {:#x}: register step not stable", l.start));
            }
            exit_abs[c] = match (measured, self.loop_in[i].get(c).copied().flatten()) {
                (Some(0), a) => a,
                (Some(d), Some(a)) => match kind {
                    Count::Lit(n) => a.shifted(d * n),
                    Count::Entry if a.nf == 0 && a.ts == 0 => Some(Abs { nf: d, ..a }),
                    _ => None,
                },
                _ => None,
            };
        }
        let rem = self.emit(Ty::I32, Op::GetVar(v.rem[i]));
        let one = self.ci(1);
        let r = self.bin(Bin::Sub, rem, one);
        self.body.push(Inst::Set(v.rem[i], r));
        let z = self.ci(0);
        let again = self.bin(Bin::Ne, r, z);
        let dest = match label_of.get(&l.start) {
            Some(&lb) => lb,
            None => self.stub(stubs, l.start, active.to_vec()),
        };
        self.body.push(Inst::BrIf {
            c: again,
            target: dest,
        });
        // The loop ends: the interpreter pops it and the PC stack entry.
        self.set_var(v.didpc, 1);
        if l.empties {
            self.set_var(v.stk26, 2);
        }
        self.var_abs = exit_abs;
        Ok(())
    }
}
