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

/// `Site::pc` of an exit whose address the kernel stored in `CTX_DYNPC`.
pub const DYN_PC: u32 = u32::MAX;

/// `NodeKind::Return::jump_idx` of an indirect jump without delay slots.
pub const NO_JUMP: u16 = u16::MAX;

/// What a taken call or return leaves to the engine at the exit (see
/// `glue::sites::apply_tail`): the kernel runs the transfer and its delay
/// slots, the glue does the stack bookkeeping the interpreter does at the
/// branch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tail {
    #[default]
    None,
    /// A CALL (stack model, followed): pushes the return address (the
    /// instruction after the call and its slots) on the PC stack and on the
    /// followed-call stack, and lands on the target.
    Call(u32),
    /// An RTS (stack model): pops the PC stack, whose top (read at entry,
    /// `CTX_DYNPC`) is the target.
    Rts,
}

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
    /// A `DYN_PC` site: the address of the return instruction that made it
    /// (0 otherwise), for coverage reports.
    pub ret: u32,
    /// The call or return this exit completes.
    pub tail: Tail,
    /// How the exit arises, for the coverage report: "insn" (a guard or a
    /// stop at the instruction itself), "edge" (a taken branch or loop-back
    /// the kernel does not hold, or the end of a dormant loop), "call",
    /// "ret" (a return).
    pub kind: &'static str,
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
    /// An entry loop that does not contain the entry: it sits on the engine's
    /// loop stack (below the loops that do contain the entry) and the region
    /// reaches its end address. It is active at every node until its
    /// loop-back falls through, where the kernel leaves.
    pub dormant: bool,
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
        /// A call or return: the taken edge always leaves the kernel.
        tail: Tail,
        /// The compute of a `9a_rel`/`11a`, as the conditional `2a` it runs as.
        comp: Option<Dec>,
    },
    /// A delayed unconditional indirect JUMP (Mi, Ij) with two delay slots:
    /// the compiler's return (stack model only, cluster kernels only). The
    /// kernel runs it and its slots and leaves at the target address, which
    /// is only known at run time (`Site::pc == DYN_PC`).
    Return {
        jump_idx: u16,
        /// The compute of a `9a_abs`.
        comp: Option<Dec>,
    },
    // (`jump_idx` is `NO_JUMP` for a return that is not delayed.)
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
    /// A `Seq` instruction the kernel does not lower: it runs through a helper
    /// call (`Op::Helper`, the interpreter's one-instruction step) and the
    /// kernel goes on (`SHARC_FAST_HELPER`, see `lower::helper`).
    pub helper: bool,
}

/// `SHARC_FAST_HELPER=1` (or `=LO-HI`, hex addresses): instructions that
/// cannot be lowered for want of an address range become helper calls instead
/// of ending the region (only those in [LO, HI] with a range).
fn helper_range() -> &'static Option<Vec<(u32, u32)>> {
    static R: std::sync::OnceLock<Option<Vec<(u32, u32)>>> = std::sync::OnceLock::new();
    R.get_or_init(|| {
        let v = std::env::var("SHARC_FAST_HELPER").ok()?;
        // "1": everywhere; else a comma list of LO-HI hex ranges.
        let mut out = Vec::new();
        for part in v.split(',') {
            match part.split_once('-') {
                Some((a, b)) => out.push((
                    u32::from_str_radix(a, 16).ok()?,
                    u32::from_str_radix(b, 16).ok()?,
                )),
                None => out.push((0, u32::MAX)),
            }
        }
        Some(out)
    })
}

/// Entries whose first instruction left the kernel again and again (a guard on
/// a data-dependent address that never holds): from then on that instruction is
/// a helper call wherever a region is built (`mod.rs`, Decline::Exit0).
static FORCED: std::sync::Mutex<Option<std::collections::BTreeSet<u32>>> =
    std::sync::Mutex::new(None);

pub fn force_helper(pc: u32) {
    FORCED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(Default::default)
        .insert(pc);
}

/// On by default; `SHARC_FAST_FLATAUTO=0` turns it off (A/B). An access that has no address range (no register
/// anchor either) is lowered as a flat guarded access (`Lower::flat_access`)
/// at that instruction only, instead of ending the region there; and a region
/// whose static windows keep declining at run time (a window needs every byte
/// of its hull present, or overlay-written for a store) is rebuilt with flat
/// accesses instead. Exact: a failing guard leaves before the instruction.
pub fn flat_auto() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("SHARC_FAST_FLATAUTO").map_or(true, |v| v != "0"))
}

static FLAT_PCS: std::sync::Mutex<Option<std::collections::BTreeSet<u32>>> =
    std::sync::Mutex::new(None);
static FLAT_WINS: std::sync::Mutex<Option<std::collections::BTreeSet<u32>>> =
    std::sync::Mutex::new(None);

/// Instructions lowered with a flat access (see `flat_auto`); true if new.
pub fn mark_flat_pc(pc: u32) -> bool {
    FLAT_PCS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(Default::default)
        .insert(pc)
}

static FLAT_DYN_PCS: std::sync::Mutex<Option<std::collections::BTreeSet<u32>>> =
    std::sync::Mutex::new(None);

/// Instructions whose guarded dynamic window kept failing at run time
/// (`SHARC_FAST_DYNFLAT`): lowered as flat accesses instead; true if new.
pub fn mark_flat_dyn(pc: u32) -> bool {
    FLAT_DYN_PCS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(Default::default)
        .insert(pc)
}

pub fn is_flat_dyn(pc: u32) -> bool {
    FLAT_DYN_PCS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|f| f.contains(&pc))
}

pub fn is_flat_pc(pc: u32) -> bool {
    FLAT_PCS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|f| f.contains(&pc))
}

/// Region roots whose windows are all flat accesses (see `flat_auto`); true if new.
pub fn mark_flat_win(root: u32) -> bool {
    FLAT_WINS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(Default::default)
        .insert(root)
}

pub fn is_flat_win(root: u32) -> bool {
    FLAT_WINS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|f| f.contains(&root))
}

pub fn is_forced(pc: u32) -> bool {
    FORCED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|f| f.contains(&pc))
}

pub fn helper_enabled() -> bool {
    helper_range().is_some()
}

/// Whether PC may become a helper call.
pub fn helper_at(pc: u32) -> bool {
    helper_range()
        .as_ref()
        .is_some_and(|r| r.iter().any(|&(lo, hi)| (lo..=hi).contains(&pc)))
}

/// The instructions the helper stub runs: a `3a` transfer of an R register to
/// or from DM/PM through an index and a modify register, unconditional and
/// with no compute, no long word.
pub fn helper_ok(d: &Dec) -> bool {
    super::helper::plan(d, &super::helper::NoRegs).is_some()
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
    /// Entries of called code that lies before its caller in address order
    /// and is no part of a cycle: its register ranges at the entry are the
    /// caller's (carried to the next lowering pass, `Plan::entry_abs`),
    /// not the unknown ranges of a loop-carried register.
    pub call_targets: BTreeSet<u32>,
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
        tail: Tail,
        comp: Option<Dec>,
    },
    Return {
        delayed: bool,
        comp: Option<Dec>,
    },
    Stop(String),
}

fn cond_supported(c: u32) -> bool {
    matches!(
        c,
        0x1f | 0x00..=0x08 | 0x0d | 0x10..=0x18 | 0x1d
    )
}

/// Why the instruction at PC cannot be a delay slot of the kernel.
fn slot_why(env: &Env, decode: &dyn Fn(u32) -> Result<Dec, Refuse>, pc: u32) -> String {
    match decode(pc) {
        Err(e) => e.0,
        Ok(d) => match lowerable(env, &d) {
            Err(e) => e,
            Ok(()) => format!("{} in a delay slot", d.form),
        },
    }
}

/// The condition code that is true exactly when COND is false.
fn complement(cond: u32) -> u32 {
    cond ^ 0x10
}

/// The compute of a transfer with a compute field: it runs when the
/// condition holds (`e` = 0) or, with ELSE, when it does not.
fn flow_compute(d: &Dec, cond: u32) -> Option<Dec> {
    if d.field("e") == Some(1) {
        // ELSE with no condition: the compute never runs.
        (cond != 0x1f)
            .then(|| d.compute_part(complement(cond)))
            .flatten()
    } else {
        d.compute_part(cond)
    }
}

fn classify(
    env: &Env,
    s: &St,
    pc: u32,
    d: &Dec,
    decode: &dyn Fn(u32) -> Result<Dec, Refuse>,
) -> Cls {
    let stack_model = s.cfg.stack_model;
    // The compiler's call and return sequence, in cluster kernels of the
    // stack model (where CJUMP pushes no PC stack entry, see
    // `lower::callret`; without the stack model it is a call and stays a stop).
    let seq = stack_model;
    // A call the kernel can end in: the engine follows it (the generic
    // core's `_advance`), and the bookkeeping at the exit is `Tail::Call`.
    let call_ok = stack_model && s.cfg.follow_loaded_calls && s.cfg.max_call_depth > 0;
    match d.form {
        "25a_direct" if seq => match (d.get("addr[23:16]"), d.get("addr[15:0]")) {
            (Some(h), Some(l)) => Cls::Jump {
                target: ((h << 16) | l) as u32,
                cond: 0x1f,
                delayed: true,
                tail: Tail::None,
                comp: None,
            },
            _ => Cls::Stop("call target".into()),
        },
        "9b_abs" | "9a_abs" if seq => {
            if d.field("b") == Some(0)
                && d.field("a") == Some(0)
                && d.field("ci") == Some(0)
                && d.field("cond") == Some(0x1f)
            {
                Cls::Return {
                    delayed: d.field("j") == Some(1),
                    comp: if d.form == "9a_abs" {
                        flow_compute(d, 0x1f)
                    } else {
                        None
                    },
                }
            } else {
                Cls::Stop("indirect jump shape".into())
            }
        }
        "8a_abs" | "8a_rel" | "9a_rel" => {
            let call = d.field("b") == Some(1);
            if call && !call_ok {
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
            let target = match d.form {
                "8a_abs" => match (d.get("addr[23:16]"), d.get("addr[15:0]")) {
                    (Some(h), Some(l)) => ((h << 16) | l) as u32,
                    _ => return Cls::Stop("jump target".into()),
                },
                "8a_rel" => match (d.get("reladdr[23:16]"), d.get("reladdr[15:0]")) {
                    (Some(h), Some(l)) => {
                        ((pc as i64 + signed((h << 16) | l, 24)) & 0xff_ffff) as u32
                    }
                    _ => return Cls::Stop("jump target".into()),
                },
                _ => match (d.get("reladdr[5:5]"), d.get("reladdr[4:0]")) {
                    (Some(h), Some(l)) => {
                        ((pc as i64 + signed((h << 5) | l, 6)) & 0xff_ffff) as u32
                    }
                    _ => return Cls::Stop("jump target".into()),
                },
            };
            let comp = if d.form == "9a_rel" {
                flow_compute(d, cond)
            } else {
                None
            };
            let tail = if call {
                // The target must be code (`follow_loaded_calls` decodes it).
                if decode(target).is_err() {
                    return Cls::Stop("call".into());
                }
                Tail::Call(0)
            } else {
                Tail::None
            };
            Cls::Jump {
                target,
                cond,
                delayed: d.field("j") == Some(1),
                tail,
                comp,
            }
        }
        "11a" | "11c" => {
            // RTS (the stack model's `_take_return`); RTI and loop re-entry
            // stay with the interpreter.
            if !stack_model || d.field("x") != Some(0) || d.field("lr") != Some(0) {
                return Cls::Stop(format!("form {}", d.form));
            }
            let Some(cond) = d.field("cond") else {
                return Cls::Stop("return without a condition".into());
            };
            let cond = cond as u32;
            if !cond_supported(cond) {
                return Cls::Stop(format!("return condition {cond:#x}"));
            }
            let comp = if d.form == "11a" {
                flow_compute(d, cond)
            } else {
                None
            };
            Cls::Jump {
                target: DYN_PC,
                cond,
                delayed: d.field("j") == Some(1),
                tail: Tail::Rts,
                comp,
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
    helpers: &BTreeSet<u32>,
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
            helper: false,
        };
        match classify(env, s, pc, &dec, decode) {
            Cls::Stop(why)
                if helper_at(pc)
                    && helper_ok(&dec)
                    && (helpers.contains(&pc) || why.contains("address range unknown")) =>
            {
                node.helper = true;
                work.push(pc + len);
            }
            Cls::Stop(why) => node.kind = NodeKind::Stop(why),
            Cls::Return { delayed, comp } => {
                let mut next = pc + len;
                let mut ok = true;
                let mut why = String::new();
                for _ in 0..if delayed { 2 } else { 0 } {
                    match decode(next) {
                        Ok(sd)
                            if !matches!(sd.form, "8a_abs" | "8a_rel" | "12a_imm" | "12a_ureg")
                                && lowerable(env, &sd).is_ok() =>
                        {
                            let l = sd.len_sw;
                            node.slots.push((next, sd));
                            next += l;
                        }
                        _ => {
                            ok = false;
                            why = slot_why(env, decode, next);
                            break;
                        }
                    }
                }
                if ok && delayed && jump_idx as u32 >= super::ir::MAX_CFG_JUMPS {
                    ok = false;
                    why = "too many delayed branches".into();
                }
                if ok {
                    node.kind = NodeKind::Return {
                        jump_idx: if delayed { jump_idx } else { NO_JUMP },
                        comp,
                    };
                    if delayed {
                        jump_idx += 1;
                    }
                    for (p, _) in &node.slots {
                        slot_pcs.insert(*p);
                    }
                } else {
                    node.slots.clear();
                    node.kind = NodeKind::Stop(format!("delay slot ({why})"));
                }
            }
            Cls::Seq => {
                node.helper = helpers.contains(&pc) || (is_forced(pc) && helper_ok(&dec));
                work.push(pc + len);
            }
            Cls::Do => {
                node.kind = NodeKind::Do(0);
                work.push(pc + len);
            }
            Cls::Jump {
                target,
                cond,
                delayed,
                tail,
                comp,
            } => {
                let mut ok = true;
                let mut why = String::new();
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
                                why = slot_why(env, decode, next);
                                break;
                            }
                        }
                    }
                }
                if ok && delayed && jump_idx as u32 >= super::ir::MAX_CFG_JUMPS {
                    // Past the delayed-branch budget a jump or call ends the
                    // region here instead of refusing the whole region.
                    ok = false;
                    why = "too many delayed branches".into();
                }
                if !ok {
                    node.slots.clear();
                    node.kind = NodeKind::Stop(format!("delay slot ({why})"));
                } else {
                    let near = tail == Tail::None && (target as i64 - entry as i64).abs() < REACH;
                    // A call returns to the instruction after its slots.
                    let tail = match tail {
                        Tail::Call(_) => Tail::Call(next),
                        t => t,
                    };
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
                        tail,
                        comp,
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
    let mut irrelevant_deeper = stack.len() - chain.len();
    // The loop just below the chain does not contain the entry. It matters
    // only when its end lies in the region (then it is modelled, active at
    // every node); otherwise it never ends there, and it blocks every loop
    // below it (only the top loop's end is tested).
    let mut dormant: Option<usize> = None;
    if irrelevant_deeper > 0 {
        let l = stack[irrelevant_deeper - 1];
        let end = l.end_sw as u32;
        if nodes.contains_key(&end) || slot_pcs.contains(&end) {
            if l.start_sw < 0 || l.start_sw > l.end_sw {
                return refuse("the end of an outer active loop lies in the region".into());
            }
            dormant = Some(irrelevant_deeper - 1);
            irrelevant_deeper -= 1;
        }
    }
    // Entry loops outer to inner.
    for &i in dormant.iter().chain(chain.iter().rev()) {
        let l = stack[i];
        loops.push(LoopDef {
            start: l.start_sw as u32,
            end: l.end_sw as u32,
            mode: l.mode,
            count: LoopCount::Entry,
            do_pc: 0,
            empties: false,
            depth: 0,
            dormant: dormant == Some(i),
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
    // The dormant loop is the outermost, at every node.
    order.sort_by_key(|&i| {
        (
            !loops[i].dormant,
            loops[i].start,
            std::cmp::Reverse(loops[i].end),
        )
    });
    for pc in nodes.keys().copied().chain(slot_pcs.iter().copied()) {
        let v: Vec<u16> = order
            .iter()
            .filter(|&&i| loops[i].dormant || (loops[i].start <= pc && pc <= loops[i].end))
            .map(|&i| i as u16)
            .collect();
        ctx.insert(pc, v);
    }
    // A return pops the PC stack, whose top is the return address only when
    // no loop is active (`CTX_DYNPC` is read from it at entry).
    for pc in nodes.keys().copied().collect::<Vec<_>>() {
        let n = &nodes[&pc];
        if matches!(
            n.kind,
            NodeKind::Jump {
                tail: Tail::Rts,
                ..
            }
        ) && ctx.get(&pc).is_some_and(|v| !v.is_empty())
        {
            let n = nodes.get_mut(&pc).unwrap();
            for (p, _) in n.slots.drain(..) {
                slot_pcs.remove(&p);
            }
            n.kind = NodeKind::Stop("return inside a loop".into());
        }
    }
    // At the dormant loop's end no other loop may hold the address (the
    // engine tests the top loop's end only).
    for d in loops.iter().filter(|l| l.dormant) {
        let others = ctx.get(&d.end).map_or(0, |v| {
            v.iter()
                .filter(|&&i| {
                    !loops[i as usize].dormant && {
                        let o = &loops[i as usize];
                        o.start <= d.end && d.end <= o.end
                    }
                })
                .count()
        });
        if others > 0 {
            return refuse("the end of an outer active loop lies in the region".into());
        }
    }
    // `empties`.
    for i in 0..loops.len() {
        // Loops below it on the engine's stack: the dormant loop is below
        // every other loop of the region.
        let ancestors = loops
            .iter()
            .enumerate()
            .filter(|&(j, o)| {
                j != i && (o.dormant || (o.start <= loops[i].start && loops[i].end <= o.end))
            })
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
        if matches!(nodes[pc].kind, NodeKind::Return { .. }) {
            if loops.iter().any(|l| l.end == *pc) {
                return refuse("a branch at a loop end".into());
            }
            if nodes[pc]
                .slots
                .iter()
                .any(|(p, _)| loops.iter().any(|l| l.end == *p))
            {
                return refuse("a delay slot at a loop end".into());
            }
        }
    }
    for pc in &pcs {
        let (target, cond, tail) = match nodes[pc].kind {
            NodeKind::Jump {
                target, cond, tail, ..
            } => (target, cond, tail),
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
        if tail == Tail::None && slot_pcs.contains(&target) {
            return refuse("a branch into a delay slot".into());
        }
        let inside = tail == Tail::None
            && nodes.contains_key(&target)
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
    // Calls that do not close a cycle: the target does not reach the call.
    let mut call_targets: BTreeSet<u32> = BTreeSet::new();
    let mut cyclic: BTreeSet<u32> = loops.iter().map(|l| l.start).collect();
    for n in nodes.values() {
        if let NodeKind::Jump {
            target, exit_edge, ..
        } = n.kind
            && !exit_edge
            && target <= n.pc
        {
            let is_call = n.dec.form == "25a_direct";
            if is_call && !reaches(target, n.pc, &nodes, &loops) {
                call_targets.insert(target);
            } else {
                cyclic.insert(target);
            }
        }
    }
    call_targets.retain(|t| !cyclic.contains(t));
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
        call_targets,
    })
}

/// Whether the node at FROM reaches the node at TO through the kernel's
/// own edges.
fn reaches(from: u32, to: u32, nodes: &BTreeMap<u32, Node>, loops: &[LoopDef]) -> bool {
    let mut seen: BTreeSet<u32> = BTreeSet::new();
    let mut work = vec![from];
    while let Some(pc) = work.pop() {
        if pc == to {
            return true;
        }
        if !seen.insert(pc) {
            continue;
        }
        if let Some(n) = nodes.get(&pc) {
            work.extend(successors(n, nodes, loops));
        }
    }
    false
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
            dormant: false,
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
            // A loop started later is above the dormant one on the engine's
            // stack, so it cannot hold the dormant loop's range.
            if !(disjoint || a_in_b || b_in_a) || a.end == b.end || (a.dormant && a_in_b) {
                return Err((
                    format!("loop at {:#x} overlaps another loop", b.start),
                    Some(b.do_pc),
                ));
            }
        }
    }
    // Bodies: the end is a plain instruction or not decoded (the loop-back
    // is then left to the interpreter: the kernel leaves at an instruction
    // it cannot run inside the loop, the glue rebuilds the loop). No decoded
    // instruction of the body may run across the end.
    for l in &loops[n_entry..] {
        let bad = |why: String| Err((format!("loop at {:#x}: {why}", l.start), Some(l.do_pc)));
        if let Some(en) = nodes.get(&l.end)
            && !matches!(en.kind, NodeKind::Seq | NodeKind::Stop(_))
        {
            return bad("end is not a plain instruction".into());
        }
        for n in nodes.range(l.start..=l.end).map(|(_, n)| n) {
            if n.pc != l.end && n.next() > l.end + 1 && !matches!(n.kind, NodeKind::Stop(_)) {
                return bad("end inside an instruction".into());
            }
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
        helper: false,
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
        NodeKind::Stop(_) | NodeKind::Return { .. } => {}
        NodeKind::Seq | NodeKind::Do(_) => {
            let ending = loops.iter().find(|l| l.end == n.pc);
            if let Some(l) = ending
                && nodes.contains_key(&l.start)
            {
                v.push(l.start);
            }
            // The kernel leaves where the dormant loop falls through.
            if !ending.is_some_and(|l| l.dormant) {
                v.push(n.next());
            }
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
    /// The symbolic trip count of a region without an entry loop: a
    /// register at entry plus a constant (`lower::Lowered::reg_n`). The
    /// glue evaluates it as the window iteration count.
    pub reg_n: Option<(u8, i64)>,
    /// Delayed branches: (target, jump address).
    pub jumps: Vec<(u32, u32)>,
    /// What each delayed branch of `jumps` is (a call, a return, a jump).
    pub jump_tail: Vec<Tail>,
    /// The region has a return exit: the glue reads the return address from
    /// the PC stack at entry (`CTX_DYNPC`), and declines when it cannot.
    pub has_rts: bool,
    /// The region has a call exit: the glue checks that the stacks have room.
    pub has_call: bool,
    /// The deepest nesting of loops started inside the region.
    pub deepest_in_region: u32,
    /// Where the region ends (instructions that are not part of it) and why.
    pub stops: Vec<(u32, String)>,
    /// Which loop-event context slots the kernel stores (a kernel that can
    /// start or end a loop stores LCNTR, the DO flag, the STKYX event, or a
    /// stale-slot record). The others hold what an earlier call of another
    /// region left there and must be read as zero: the DO flag, LCNTR with its
    /// set flag, the STKYX event, the stale-slot records.
    pub st_didpc: bool,
    pub st_lcntr: bool,
    pub st_stk26: bool,
    /// One bit per `CTX_SLOT_LAST` slot the kernel stores. A slot the kernel
    /// does not store is stale (a deeper loop of another region wrote it) and
    /// must be read as zero, whether or not another slot is stored.
    pub st_slots: u32,
}
