//! CFG regions: the shape beyond "the remaining iterations of one DO-loop
//! body". A region is the code reachable from an entry pc through forward and
//! backward branches, delayed branches (their two slots inline), DO loops
//! started inside the region and the loops already active at entry; it ends
//! wherever the lowering cannot go on (CALL, RTS, an indirect jump, an
//! unlowerable form, a branch out of the region): there the kernel leaves with
//! the engine state exactly as the interpreter has it before that instruction.
//!
//! This module finds the structure (`Structure`): nodes, loops, leaders, cut
//! points and their longest paths. `lower::flow` turns it into a kernel.
//! `glue::run_cfg` runs it and rebuilds the loop and PC stacks at the exit.
//!
//! Structural rules (anything else is refused or becomes an exit):
//!  * A loop's range [start, end] is nested in or disjoint from every other
//!    loop's, ends are distinct, and the body is decoded contiguously.
//!  * A branch whose target is in the same innermost loop (or in none) as the
//!    branch stays inside the kernel; any other branch is an exit at the
//!    target with the loops of the branch still active (what the interpreter
//!    has there).
//!  * No branch, delay slot or DO sits at a loop's end address (the
//!    interpreter skips the loop-back test while a transfer is pending), and
//!    no branch targets a delay slot.
//!  * The end of an active loop that is not modelled (not in the chain of
//!    loops that contain the entry) must not lie in the region.

use super::decode_view::Dec;
use super::lower::{Env, Lower, Plan, Refuse};
use crate::rt::{Int, St};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Where the interpreter resumes after the kernel leaves, and what it
/// has to be told: instructions retired in the current block before the exit
/// (the kernel's counter holds the count at the start of the block), the
/// loops active (indices into the loop table, outermost first) and a pending
/// delayed branch (jump index, delay slots left).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Site {
    pub pc: u32,
    pub off: u32,
    pub active: Vec<u16>,
    pub pending: Option<(u16, u8)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopCount {
    /// Active at entry: the remaining count comes from the engine.
    Entry,
    Lit(u32),
    Reg(u32),
}

#[derive(Clone, Debug)]
pub struct LoopDef {
    pub start: u32,
    pub end: u32,
    pub mode: i64,
    pub count: LoopCount,
    /// The address of the DO (in-region loops).
    pub do_pc: u32,
    /// Leaving this loop empties the loop stack.
    pub empties: bool,
    /// Loops below it on the engine's loop stack when it runs.
    pub depth: u32,
}

impl LoopDef {
    pub fn entry(&self) -> bool {
        self.count == LoopCount::Entry
    }
}

#[derive(Clone, Debug)]
pub enum NodeKind {
    Seq,
    Do(u16),
    /// A JUMP (not a call). `slots` are the two delay slot addresses of a
    /// delayed jump. `exit_edge`: the taken edge leaves the kernel.
    Jump {
        target: u32,
        cond: u32,
        delayed: bool,
        jump_idx: u16,
        exit_edge: bool,
    },
    Stop(String),
}

#[derive(Clone, Debug)]
pub struct Node {
    pub pc: u32,
    pub len: u32,
    pub dec: Dec,
    pub kind: NodeKind,
    /// Delay slot instructions (decoded) of a delayed jump.
    pub slots: Vec<(u32, Dec)>,
}

impl Node {
    /// Address after the node (and its delay slots).
    pub fn next(&self) -> u32 {
        match self.slots.last() {
            Some((p, d)) => p + d.len_sw,
            None => self.pc + self.len,
        }
    }
    /// Instructions retired when the node executes.
    pub fn weight(&self) -> u32 {
        match self.kind {
            NodeKind::Stop(_) => 0,
            _ => 1 + self.slots.len() as u32,
        }
    }
}

pub struct Structure {
    pub entry: u32,
    pub nodes: BTreeMap<u32, Node>,
    pub loops: Vec<LoopDef>,
    /// Loops active at each node (indices, outermost first).
    pub ctx: HashMap<u32, Vec<u16>>,
    pub leaders: BTreeSet<u32>,
    /// Cut points (budget checks) and the longest path from each.
    pub cuts: HashMap<u32, u32>,
    pub n_jumps: u16,
    /// Delay slot addresses.
    pub slot_pcs: BTreeSet<u32>,
    /// The entry loops' indices into the engine's loop stack (top = 0).
    pub entry_chain: Vec<usize>,
    /// Backward branches inside the kernel: (address of the last instruction
    /// of the branch with its slots, target).
    pub back_edges: Vec<(u32, u32)>,
}

pub const MAX_NODES: usize = 400;
/// Branches to targets farther than this from the entry are exits.
const REACH: i64 = 0x1000;

fn signed(v: i64, bits: u32) -> i64 {
    let m = 1i64 << (bits - 1);
    (v ^ m) - m
}

/// Whether the single instruction lowers (a scratch lowering), else why not.
fn lowerable(env: &Env, d: &Dec) -> Result<(), String> {
    let mut e = env.clone();
    e.flag_v = true;
    let mut l = Lower::new(e, Plan::default(), 0);
    l.lower_insn(0, d).map_err(|e| e.0)
}

enum Cls {
    Seq,
    Do,
    Jump {
        target: u32,
        cond: u32,
        delayed: bool,
    },
    Stop(String),
}

fn cond_supported(c: u32) -> bool {
    matches!(
        c,
        0x1f | 0x00..=0x08 | 0x0d | 0x10..=0x18 | 0x1d
    )
}

fn classify(env: &Env, pc: u32, d: &Dec) -> Cls {
    match d.form {
        "8a_abs" | "8a_rel" => {
            if d.field("b") != Some(0) {
                return Cls::Stop("call".into());
            }
            if d.field("a") != Some(0) || d.field("ci") != Some(0) {
                return Cls::Stop("loop abort or CI modifier".into());
            }
            let Some(cond) = d.field("cond") else {
                return Cls::Stop("jump without a condition".into());
            };
            let cond = cond as u32;
            if !cond_supported(cond) {
                return Cls::Stop(format!("jump condition {cond:#x}"));
            }
            let target = if d.form == "8a_abs" {
                let hi = d.get("addr[23:16]");
                let lo = d.get("addr[15:0]");
                match (hi, lo) {
                    (Some(h), Some(l)) => ((h << 16) | l) as u32,
                    _ => return Cls::Stop("jump target".into()),
                }
            } else {
                let hi = d.get("reladdr[23:16]");
                let lo = d.get("reladdr[15:0]");
                match (hi, lo) {
                    (Some(h), Some(l)) => {
                        ((pc as i64 + signed((h << 16) | l, 24)) & 0xff_ffff) as u32
                    }
                    _ => return Cls::Stop("jump target".into()),
                }
            };
            Cls::Jump {
                target,
                cond,
                delayed: d.field("j") == Some(1),
            }
        }
        "12a_imm" => {
            // A zero count is a trap in the interpreter: stop before it.
            let count = match (d.get("data[15:8]"), d.get("data[7:0]")) {
                (Some(h), Some(l)) => (h << 8) | l,
                _ => 0,
            };
            if count == 0 {
                Cls::Stop("zero-count DO".into())
            } else {
                Cls::Do
            }
        }
        "12a_ureg" => match d.get("ureg[6:0]") {
            Some(code) if (code as u32) < super::ir::NREGS => Cls::Do,
            _ => Cls::Stop("DO count register".into()),
        },
        _ => match lowerable(env, d) {
            Ok(()) => Cls::Seq,
            Err(why) => Cls::Stop(why),
        },
    }
}

pub fn build_structure(
    s: &St,
    env: &Env,
    entry: u32,
    decode: &dyn Fn(u32) -> Result<Dec, Refuse>,
    deny: &BTreeSet<u32>,
) -> Result<Structure, Refuse> {
    let refuse = |m: String| Err(Refuse(m));
    let mut nodes: BTreeMap<u32, Node> = BTreeMap::new();
    let mut slot_pcs: BTreeSet<u32> = BTreeSet::new();
    let mut work = vec![entry];
    let mut jump_idx = 0u16;
    while let Some(pc) = work.pop() {
        if nodes.contains_key(&pc) || slot_pcs.contains(&pc) {
            continue;
        }
        if nodes.len() >= MAX_NODES {
            // Frontier: leave before this address.
            nodes.insert(pc, stop_node(pc, "region size"));
            continue;
        }
        let dec = match decode(pc) {
            Ok(d) => d,
            Err(e) => {
                nodes.insert(pc, stop_node(pc, &e.0));
                continue;
            }
        };
        let len = dec.len_sw;
        if deny.contains(&pc) {
            nodes.insert(pc, stop_node(pc, "cannot be lowered in this region"));
            continue;
        }
        let mut node = Node {
            pc,
            len,
            dec: dec.clone(),
            kind: NodeKind::Seq,
            slots: Vec::new(),
        };
        match classify(env, pc, &dec) {
            Cls::Stop(why) => node.kind = NodeKind::Stop(why),
            Cls::Seq => work.push(pc + len),
            Cls::Do => {
                node.kind = NodeKind::Do(0);
                work.push(pc + len);
            }
            Cls::Jump {
                target,
                cond,
                delayed,
            } => {
                let mut ok = true;
                let mut next = pc + len;
                if delayed {
                    for _ in 0..2 {
                        match decode(next) {
                            Ok(sd)
                                if !matches!(
                                    sd.form,
                                    "8a_abs" | "8a_rel" | "12a_imm" | "12a_ureg"
                                ) && lowerable(env, &sd).is_ok() =>
                            {
                                let l = sd.len_sw;
                                node.slots.push((next, sd));
                                next += l;
                            }
                            _ => {
                                ok = false;
                                break;
                            }
                        }
                    }
                }
                if !ok {
                    node.slots.clear();
                    node.kind = NodeKind::Stop("delay slot".into());
                } else {
                    let near = (target as i64 - entry as i64).abs() < REACH;
                    node.kind = NodeKind::Jump {
                        target,
                        cond,
                        delayed,
                        jump_idx: if delayed {
                            jump_idx += 1;
                            jump_idx - 1
                        } else {
                            0
                        },
                        // Fixed up below once the loops are known.
                        exit_edge: !near,
                    };
                    for (p, _) in &node.slots {
                        slot_pcs.insert(*p);
                    }
                    if near {
                        work.push(target);
                    }
                    if cond != 0x1f {
                        work.push(next);
                    }
                }
            }
        }
        nodes.insert(pc, node);
    }
    if jump_idx as u32 > super::ir::MAX_CFG_JUMPS {
        return refuse("too many delayed branches".into());
    }
    if nodes.keys().any(|p| slot_pcs.contains(p)) {
        return refuse("a delay slot is also reached as an instruction".into());
    }
    // The entry must be an instruction the kernel runs.
    match nodes.get(&entry) {
        Some(Node {
            kind: NodeKind::Stop(why),
            ..
        }) => return refuse(format!("the entry instruction cannot run here: {why}")),
        None => return refuse("entry not decoded".into()),
        _ => {}
    }

    // -- loops -----------------------------------------------------------------
    let mut loops: Vec<LoopDef> = Vec::new();
    // Entry loops: the chain from the top of the engine's loop stack whose
    // ranges contain the entry pc.
    let stack: Vec<_> = s.loops.items().to_vec();
    let mut chain: Vec<usize> = Vec::new();
    for (i, l) in stack.iter().enumerate().rev() {
        let (a, b) = (l.start_sw, l.end_sw);
        if a < 0 || a > b || !(a <= entry as i64 && entry as i64 <= b) {
            break;
        }
        if let Some(&inner) = chain.last()
            && !(a <= stack[inner].start_sw && stack[inner].end_sw <= b)
        {
            break;
        }
        chain.push(i);
    }
    let irrelevant_deeper = stack.len() - chain.len();
    // The stack entries below the chain must not end inside the region.
    for l in &stack[..irrelevant_deeper] {
        if nodes.contains_key(&(l.end_sw as u32)) || slot_pcs.contains(&(l.end_sw as u32)) {
            return refuse("the end of an outer active loop lies in the region".into());
        }
    }
    // Entry loops outer to inner.
    for &i in chain.iter().rev() {
        let l = stack[i];
        loops.push(LoopDef {
            start: l.start_sw as u32,
            end: l.end_sw as u32,
            mode: l.mode,
            count: LoopCount::Entry,
            do_pc: 0,
            empties: false,
            depth: 0,
        });
    }
    let n_entry = loops.len();
    // In-region loops from the DO nodes. A DO whose loop cannot be modelled
    // (its body is not decoded, it overlaps another loop) becomes the end of
    // the region instead.
    let entry_loops = loops.clone();
    loops = loop {
        match derive_loops(&nodes, &entry_loops) {
            Ok(l) => break l,
            Err((why, Some(do_pc))) => {
                nodes.get_mut(&do_pc).unwrap().kind = NodeKind::Stop(why);
            }
            Err((why, None)) => return refuse(why),
        }
    };
    for (i, l) in loops.iter().enumerate() {
        if !l.entry() {
            nodes.get_mut(&l.do_pc).unwrap().kind = NodeKind::Do(i as u16);
        }
    }
    // Context of every node: the loops whose range holds it.
    let mut ctx: HashMap<u32, Vec<u16>> = HashMap::new();
    let mut order: Vec<usize> = (0..loops.len()).collect();
    order.sort_by_key(|&i| (loops[i].start, std::cmp::Reverse(loops[i].end)));
    for pc in nodes.keys().copied().chain(slot_pcs.iter().copied()) {
        let v: Vec<u16> = order
            .iter()
            .filter(|&&i| loops[i].start <= pc && pc <= loops[i].end)
            .map(|&i| i as u16)
            .collect();
        ctx.insert(pc, v);
    }
    // `empties`.
    for i in 0..loops.len() {
        let ancestors = loops
            .iter()
            .enumerate()
            .filter(|&(j, o)| j != i && o.start <= loops[i].start && loops[i].end <= o.end)
            .count();
        loops[i].empties = irrelevant_deeper == 0 && ancestors == 0;
        loops[i].depth = (irrelevant_deeper + ancestors) as u32;
    }
    // No transfer, slot or DO at a loop end.
    for l in &loops {
        if slot_pcs.contains(&l.end) {
            return refuse("a delay slot at a loop end".into());
        }
    }
    // Branch edges: inside the kernel only within one context.
    let pcs: Vec<u32> = nodes.keys().copied().collect();
    for pc in &pcs {
        let (target, cond) = match nodes[pc].kind {
            NodeKind::Jump { target, cond, .. } => (target, cond),
            _ => continue,
        };
        let _ = cond;
        if loops.iter().any(|l| l.end == *pc) {
            return refuse("a branch at a loop end".into());
        }
        let n_slots_at_end = nodes[pc]
            .slots
            .iter()
            .any(|(p, _)| loops.iter().any(|l| l.end == *p));
        if n_slots_at_end {
            return refuse("a delay slot at a loop end".into());
        }
        if slot_pcs.contains(&target) {
            return refuse("a branch into a delay slot".into());
        }
        let inside = nodes.contains_key(&target)
            && ctx.get(&target) == ctx.get(pc)
            && !matches!(nodes[&target].kind, NodeKind::Stop(_));
        if let NodeKind::Jump { exit_edge, .. } = &mut nodes.get_mut(pc).unwrap().kind {
            *exit_edge = !inside;
        }
    }
    // Leaders and cut points.
    let mut leaders: BTreeSet<u32> = BTreeSet::new();
    leaders.insert(entry);
    for n in nodes.values() {
        if let NodeKind::Jump {
            target, exit_edge, ..
        } = n.kind
            && !exit_edge
        {
            leaders.insert(target);
        }
    }
    for l in &loops {
        if nodes.contains_key(&l.start) {
            leaders.insert(l.start);
        }
    }
    // The first instruction after an unconditional terminator that is
    // reached some other way is already a target; nothing to add.
    let mut back_edges: Vec<(u32, u32)> = Vec::new();
    for n in nodes.values() {
        if let NodeKind::Jump {
            target, exit_edge, ..
        } = n.kind
            && !exit_edge
            && target <= n.pc
        {
            back_edges.push((n.next() - 1, target));
        }
    }
    let mut cuts: BTreeSet<u32> = BTreeSet::new();
    cuts.insert(entry);
    for l in &loops {
        if nodes.contains_key(&l.start) {
            cuts.insert(l.start);
        }
    }
    for n in nodes.values() {
        if let NodeKind::Jump {
            target, exit_edge, ..
        } = n.kind
            && !exit_edge
            && target <= n.pc
        {
            cuts.insert(target);
        }
    }
    // Longest path (in retired instructions) from each cut to the next cut
    // or the end of the kernel.
    let mut memo: HashMap<u32, u32> = HashMap::new();
    let mut maxlen: HashMap<u32, u32> = HashMap::new();
    for &c in &cuts {
        let len = path_len(c, &nodes, &loops, &cuts, &mut memo, 0)?;
        maxlen.insert(c, len);
    }
    let _ = (Int::default(), n_entry);
    Ok(Structure {
        entry,
        nodes,
        loops,
        ctx,
        leaders,
        cuts: maxlen,
        n_jumps: jump_idx,
        slot_pcs,
        entry_chain: chain,
        back_edges,
    })
}

/// The loops of the region: the entry loops, then one per DO node, checked
/// for nesting and for a body that is decoded and ends in a plain
/// instruction. On a loop that cannot be modelled: the reason and the DO to
/// turn into a stop (None: the region is refused).
fn derive_loops(
    nodes: &BTreeMap<u32, Node>,
    entry_loops: &[LoopDef],
) -> Result<Vec<LoopDef>, (String, Option<u32>)> {
    let mut loops: Vec<LoopDef> = entry_loops.to_vec();
    for n in nodes.values() {
        if !matches!(n.kind, NodeKind::Do(_)) {
            continue;
        }
        let pc = n.pc;
        let count = if n.dec.form == "12a_imm" {
            let hi = n.dec.get("data[15:8]").unwrap_or(0);
            let lo = n.dec.get("data[7:0]").unwrap_or(0);
            LoopCount::Lit(((hi << 8) | lo) as u32)
        } else {
            LoopCount::Reg(n.dec.get("ureg[6:0]").unwrap_or(0) as u32)
        };
        let off = reladdr23(&n.dec).map_err(|e| (e.0, Some(pc)))?;
        let start = pc + n.len;
        let end = (pc as i64 + off) as u32;
        if end < start {
            return Err((format!("DO at {pc:#x}: end before start"), Some(pc)));
        }
        loops.push(LoopDef {
            start,
            end,
            mode: n.dec.get("mode").unwrap_or(0),
            count,
            do_pc: pc,
            empties: false,
            depth: 0,
        });
    }
    if loops.len() > super::ir::MAX_CFG_LOOPS as usize {
        return Err(("too many loops".into(), None));
    }
    let n_entry = entry_loops.len();
    // Proper nesting, distinct ends: the later loop gives way.
    for j in n_entry..loops.len() {
        for i in 0..j {
            let (a, b) = (&loops[i], &loops[j]);
            let disjoint = a.end < b.start || b.end < a.start;
            let a_in_b = b.start <= a.start && a.end <= b.end;
            let b_in_a = a.start <= b.start && b.end <= a.end;
            if !(disjoint || a_in_b || b_in_a) || a.end == b.end {
                return Err((
                    format!("loop at {:#x} overlaps another loop", b.start),
                    Some(b.do_pc),
                ));
            }
        }
    }
    // Bodies decoded contiguously, ending in a plain instruction.
    for l in &loops[n_entry..] {
        let bad = |why: String| Err((format!("loop at {:#x}: {why}", l.start), Some(l.do_pc)));
        let Some(en) = nodes.get(&l.end) else {
            return bad("end not decoded".into());
        };
        if !matches!(en.kind, NodeKind::Seq) {
            return bad("end is not a plain instruction".into());
        }
        let mut p = l.start;
        while p < l.end {
            let Some(n) = nodes.get(&p) else {
                return bad("body not decoded".into());
            };
            if let NodeKind::Stop(why) = &n.kind {
                return bad(format!("{why} inside"));
            }
            p = n.next();
        }
        if p != l.end {
            return bad("end inside an instruction".into());
        }
    }
    // An entry loop's end must be a plain instruction or a stop.
    for l in &loops[..n_entry] {
        if let Some(en) = nodes.get(&l.end)
            && !matches!(en.kind, NodeKind::Seq | NodeKind::Stop(_))
        {
            return Err((
                format!(
                    "the end of the loop at {:#x} is not a plain instruction",
                    l.start
                ),
                None,
            ));
        }
    }
    Ok(loops)
}

fn stop_node(pc: u32, why: &str) -> Node {
    Node {
        pc,
        len: 1,
        dec: Dec::nop(),
        kind: NodeKind::Stop(why.to_string()),
        slots: Vec::new(),
    }
}

fn reladdr23(d: &Dec) -> Result<i64, Refuse> {
    let hi = d.get("reladdr[22:16]").ok_or(Refuse("DO reladdr".into()))?;
    let lo = d.get("reladdr[15:0]").ok_or(Refuse("DO reladdr".into()))?;
    Ok(signed((hi << 16) | lo, 23))
}

/// Successor addresses of a node, ignoring cut points (the loop-back edge of
/// the node at a loop's end included).
pub fn successors(n: &Node, nodes: &BTreeMap<u32, Node>, loops: &[LoopDef]) -> Vec<u32> {
    let mut v = Vec::new();
    match &n.kind {
        NodeKind::Stop(_) => {}
        NodeKind::Seq | NodeKind::Do(_) => {
            if let Some(l) = loops.iter().find(|l| l.end == n.pc)
                && nodes.contains_key(&l.start)
            {
                v.push(l.start);
            }
            v.push(n.next());
        }
        NodeKind::Jump {
            target,
            cond,
            exit_edge,
            ..
        } => {
            if !exit_edge {
                v.push(*target);
            }
            if *cond != 0x1f {
                v.push(n.next());
            }
        }
    }
    v.retain(|p| nodes.contains_key(p));
    v
}

fn path_len(
    pc: u32,
    nodes: &BTreeMap<u32, Node>,
    loops: &[LoopDef],
    cuts: &BTreeSet<u32>,
    memo: &mut HashMap<u32, u32>,
    depth: u32,
) -> Result<u32, Refuse> {
    if let Some(&v) = memo.get(&pc) {
        return Ok(v);
    }
    if depth > 4000 {
        return Err(Refuse("path too deep".into()));
    }
    let n = &nodes[&pc];
    let mut best = 0;
    for s in successors(n, nodes, loops) {
        if cuts.contains(&s) {
            continue;
        }
        best = best.max(path_len(s, nodes, loops, cuts, memo, depth + 1)?);
    }
    let v = n.weight() + best;
    memo.insert(pc, v);
    Ok(v)
}

/// What `glue::run_cfg` needs to know about a built CFG region.
pub struct CfgMeta {
    pub sites: Vec<Site>,
    /// Entry loops (outer to inner) first, then the loops started inside.
    pub loops: Vec<LoopDef>,
    pub n_entry: usize,
    /// Delayed branches: (target, jump address).
    pub jumps: Vec<(u32, u32)>,
    /// The deepest nesting of loops started inside the region.
    pub deepest_in_region: u32,
    /// Where the region ends (instructions that are not part of it) and why.
    pub stops: Vec<(u32, String)>,
}
