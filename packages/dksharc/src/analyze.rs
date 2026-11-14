//! The analysis: a linear sweep of the text, then functions, DO loops, basic blocks and calls.

use crate::defuse::{self, Effects, reg_name, simd_companions};
use crate::forms::{self, DoCount, Fields, Flow};
use crate::input::Input;
use crate::model::*;
use sharc_rt::decode::{DecodeKind, Decoded, decode_at};
use std::collections::{BTreeMap, BTreeSet};

/// The two instructions after a delayed transfer.
const DELAY_SLOTS: usize = 2;

struct Swept {
    insn: Insn,
    decoded: Decoded,
    flow: Option<Flow>,
    fx: Effects,
}

/// The reported register effects of an instruction.
fn def_use(fx: &Effects, d: &Decoded, mem: &[MemOp]) -> DefUse {
    let names =
        |s: &std::collections::BTreeSet<u8>| s.iter().map(|c| reg_name(*c)).collect::<Vec<_>>();
    let modify_form = matches!(
        d.type_name,
        "16a" | "16b" | "7a" | "7b" | "7d" | "19a" | "19a_scaled" | "19a_bitrev"
    );
    let post = mem
        .iter()
        .any(|m| matches!(m.mode, Mode::PostModify | Mode::PostModifyImm));
    let post_modify = if post || modify_form {
        fx.writes
            .iter()
            .filter(|c| (16..32).contains(*c))
            .map(|c| reg_name(*c))
            .collect()
    } else {
        vec![]
    };
    DefUse {
        reads: names(&fx.reads),
        writes: names(&fx.writes),
        post_modify,
        conditional: Fields::new(d)
            .get("cond")
            .is_some_and(|c| c != forms::COND_ALWAYS),
        astat_bits: fx.astat,
        mode1: fx.mode1,
        stack: fx.stack,
        simd_writes: names(&simd_companions(&fx.writes)),
        simd_reads: names(&simd_companions(&fx.reads)),
        uncertain: fx.uncertain.clone(),
    }
}

/// Decode the whole text. A parcel that does not decode with confidence is reported and skipped (by the decoded length when the decoder
/// has one, else one parcel); no instruction is invented there.
fn sweep(input: &Input, out: &mut Analysis) -> Vec<Swept> {
    let base = input.base_sw;
    let n = input.text.len() as u32;
    let read = |pc: u32| -> Option<u16> {
        pc.checked_sub(base)
            .filter(|i| *i < n)
            .map(|i| input.text[i as usize])
    };
    let import_at: BTreeMap<u32, &str> = input
        .relocs
        .iter()
        .filter(|r| r.class == 2)
        .map(|r| (base + r.parcel, input.imports[r.import as usize].as_str()))
        .collect();
    let mut v = vec![];
    let mut p = 0u32;
    while p < n {
        let pc = base + p;
        let d = decode_at(read, pc);
        match (d.kind, d.length_bytes) {
            (DecodeKind::Confident, Some(len)) if len % 2 == 0 && (len as u32 / 2) + p <= n => {
                let parcels = len / 2;
                let import = import_at.get(&pc).map(|s| s.to_string());
                let mut mem = forms::mem_ops(&d);
                for m in &mut mem {
                    m.import = import.clone();
                }
                let fx = defuse::effects(&d);
                let defuse = def_use(&fx, &d, &mem);
                v.push(Swept {
                    insn: Insn {
                        pc,
                        parcels,
                        form: d.type_name.to_string(),
                        import,
                        mem,
                        defuse,
                        imm: matches!(d.type_name, "19a" | "19a_scaled")
                            .then(|| Fields::new(&d).get("data").map(|v| v as i32 as i64))
                            .flatten(),
                    },
                    flow: forms::flow(&d, pc),
                    decoded: d,
                    fx,
                });
                p += parcels as u32;
            }
            _ => {
                let skip = match (d.kind, d.length_bytes) {
                    (DecodeKind::Uncertain, Some(len)) if len >= 2 && (len as u32 / 2) + p <= n => {
                        len / 2
                    }
                    _ => 1,
                };
                out.undecoded.push(Undecoded {
                    pc,
                    decode: d.kind.as_str().to_string(),
                    form: (d.kind == DecodeKind::Uncertain).then(|| d.type_name.to_string()),
                    parcels: (0..3).filter_map(|k| read(pc + k)).collect(),
                });
                p += skip as u32;
            }
        }
    }
    v
}

fn name_at(symbols: &BTreeMap<u32, Vec<(&str, bool)>>, pc: u32) -> Option<(String, bool)> {
    let list = symbols.get(&pc)?;
    // A global symbol wins over a local one; then the first by name.
    list.iter()
        .find(|s| s.1)
        .or_else(|| list.first())
        .map(|s| (s.0.to_string(), s.1))
}

pub fn analyze(input: &Input) -> Analysis {
    let base = input.base_sw;
    let mut a = Analysis {
        base_sw: base,
        text_parcels: input.text.len() as u32,
        ..Analysis::default()
    };
    let swept = sweep(input, &mut a);
    let text_end = base + input.text.len() as u32;
    let by_pc: BTreeMap<u32, usize> = swept
        .iter()
        .enumerate()
        .map(|(i, s)| (s.insn.pc, i))
        .collect();

    // Delay slots: the next two instructions after a delayed transfer, in sweep order, when they are contiguous.
    let mut slot_of: BTreeMap<u32, u32> = BTreeMap::new();
    for (i, s) in swept.iter().enumerate() {
        let Some(fl) = &s.flow else { continue };
        if !fl.delayed() {
            continue;
        }
        let mut next = s.insn.pc + s.insn.parcels as u32;
        for k in 1..=DELAY_SLOTS {
            match swept.get(i + k) {
                Some(t) if t.insn.pc == next => {
                    if t.flow.is_some() || forms::do_loop(&t.decoded).is_some() {
                        a.diagnostics.push(format!(
                            "{:#x}: a control transfer in a delay slot of {:#x}",
                            t.insn.pc, s.insn.pc
                        ));
                    }
                    slot_of.insert(t.insn.pc, s.insn.pc);
                    next += t.insn.parcels as u32;
                }
                _ => a.diagnostics.push(format!(
                    "{:#x}: delayed transfer without {DELAY_SLOTS} decoded delay slots",
                    s.insn.pc
                )),
            }
        }
    }

    // Symbols by pc (only those on an instruction start).
    let mut symbols: BTreeMap<u32, Vec<(&str, bool)>> = BTreeMap::new();
    for s in &input.symbols {
        let pc = base + s.parcel;
        if !by_pc.contains_key(&pc) {
            a.diagnostics.push(format!(
                "symbol {} at {pc:#x} is not on an instruction start",
                s.name
            ));
            continue;
        }
        symbols.entry(pc).or_default().push((&s.name, s.global));
    }

    // Function starts.
    let mut starts: BTreeMap<u32, Vec<String>> = BTreeMap::new();
    let mut add_start = |pc: u32, why: &str, a: &mut Analysis| {
        if !by_pc.contains_key(&pc) {
            a.diagnostics.push(format!(
                "function start {pc:#x} ({why}) is not on an instruction start"
            ));
            return;
        }
        let w = starts.entry(pc).or_default();
        if !w.iter().any(|x| x == why) {
            w.push(why.to_string());
        }
    };
    if let Some(first) = swept.first() {
        add_start(first.insn.pc, "text_start", &mut a);
    }
    for (pc, list) in &symbols {
        if list.iter().any(|s| s.1) {
            add_start(*pc, "symbol", &mut a);
        }
    }
    for e in &input.entries {
        add_start(base + e.parcel, &format!("entry:{}", e.role), &mut a);
    }
    for s in &swept {
        if let Some(Flow::Call {
            target: Some(t), ..
        }) = &s.flow
            && s.insn.import.is_none()
            && (base..text_end).contains(t)
        {
            add_start(*t, "call", &mut a);
        }
    }
    let entries: Vec<u32> = starts.keys().copied().collect();
    let mut functions: Vec<Function> = entries
        .iter()
        .enumerate()
        .map(|(k, &entry)| {
            let (name, global) =
                name_at(&symbols, entry).unwrap_or_else(|| (format!("sub_{entry:06x}"), false));
            Function {
                name,
                entry,
                end: entries.get(k + 1).copied().unwrap_or(text_end),
                global,
                origin: starts[&entry].clone(),
                complete: true,
                blocks: vec![],
                effects: FunctionEffects::default(),
            }
        })
        .collect();
    let fn_of = |pc: u32| -> Option<usize> {
        let k = entries.partition_point(|e| *e <= pc);
        k.checked_sub(1)
            .filter(|k| pc < functions_end(&entries, *k, text_end))
    };

    // DO loops.
    let mut loops: Vec<DoLoop> = vec![];
    for s in &swept {
        let Some((count, off)) = forms::do_loop(&s.decoded) else {
            continue;
        };
        let do_pc = s.insn.pc;
        let start = do_pc + s.insn.parcels as u32;
        let end = (do_pc as i64 + off) as u32;
        if !by_pc.contains_key(&end) || end < start || !by_pc.contains_key(&start) {
            a.diagnostics.push(format!("DO at {do_pc:#x}: end {end:#x} is not an instruction at or after the start {start:#x}"));
            continue;
        }
        if slot_of.contains_key(&do_pc) {
            continue;
        }
        loops.push(DoLoop {
            do_pc,
            start,
            end,
            count: match count {
                DoCount::Lit(count) => LoopCount::Lit { count },
                DoCount::Reg(ureg) => LoopCount::Reg {
                    ureg,
                    name: forms::ureg_name(ureg),
                },
                DoCount::Until(term) => LoopCount::Until { term },
            },
            parent: None,
            depth: 0,
            function: fn_of(do_pc),
        });
    }
    for j in 0..loops.len() {
        let (s, e) = (loops[j].start, loops[j].end);
        let mut best: Option<usize> = None;
        let mut depth = 0;
        for i in 0..loops.len() {
            if i == j {
                continue;
            }
            let (os, oe) = (loops[i].start, loops[i].end);
            let inside = os <= s && e <= oe && (os, oe) != (s, e);
            let disjoint = oe < s || e < os;
            if !inside && !disjoint && !(s <= os && oe <= e) {
                a.diagnostics.push(format!(
                    "loops at {:#x} and {:#x} overlap",
                    loops[i].do_pc, loops[j].do_pc
                ));
            }
            if inside {
                depth += 1;
                if best.is_none_or(|b| (loops[b].end - loops[b].start) > (oe - os)) {
                    best = Some(i);
                }
            }
        }
        loops[j].parent = best;
        loops[j].depth = depth;
    }
    let loop_end_of: BTreeMap<u32, Vec<usize>> = {
        let mut m: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
        for (k, l) in loops.iter().enumerate() {
            m.entry(l.end).or_default().push(k);
        }
        m
    };

    // Calls.
    let mut calls = vec![];
    for s in &swept {
        let (target, delayed, conditional) = match &s.flow {
            Some(Flow::Call {
                target,
                delayed,
                conditional,
            }) => (
                match (&s.insn.import, target) {
                    (Some(sym), _) => CallTarget::Import {
                        symbol: sym.clone(),
                    },
                    (None, Some(t)) if (base..text_end).contains(t) => CallTarget::Local { pc: *t },
                    (None, Some(t)) => CallTarget::Outside { pc: *t },
                    (None, None) => continue,
                },
                *delayed,
                *conditional,
            ),
            Some(Flow::CallIndirect {
                i,
                m,
                delayed,
                conditional,
            }) => (
                CallTarget::Indirect { i: *i, m: *m },
                *delayed,
                *conditional,
            ),
            _ => continue,
        };
        calls.push(Call {
            pc: s.insn.pc,
            caller: fn_of(s.insn.pc),
            target,
            delayed,
            conditional,
        });
    }

    // Basic blocks.
    let mut leaders: BTreeSet<u32> = entries.iter().copied().collect();
    for l in &loops {
        leaders.insert(l.start);
    }
    for (i, s) in swept.iter().enumerate() {
        let after = |k: usize| swept.get(i + k).map(|t| t.insn.pc);
        if let Some(fl) = &s.flow {
            let last = if fl.delayed() { DELAY_SLOTS } else { 0 };
            if let Some(n) = after(last + 1) {
                leaders.insert(n);
            }
            let t = match fl {
                Flow::Jump { target, .. } => *target,
                _ => None,
            };
            if let Some(t) = t
                && by_pc.contains_key(&t)
            {
                if slot_of.contains_key(&t) {
                    a.diagnostics.push(format!(
                        "{:#x}: branch into the delay slot {t:#x}",
                        s.insn.pc
                    ));
                } else {
                    leaders.insert(t);
                }
            }
        }
        if loop_end_of.contains_key(&s.insn.pc)
            && let Some(n) = after(1)
        {
            leaders.insert(n);
        }
    }
    // A delay slot never starts a block of its own.
    for slot in slot_of.keys() {
        leaders.remove(slot);
    }

    for (fi, f) in functions.iter_mut().enumerate() {
        let lo = by_pc[&f.entry];
        let hi = swept.partition_point(|s| s.insn.pc < f.end);
        let mut k = lo;
        while k < hi {
            let start = swept[k].insn.pc;
            // Extend to the next leader or the end of a transfer with its slots.
            let mut term_pc = None;
            let mut term = Terminator::Fall;
            let mut slots = vec![];
            loop {
                let s = &swept[k];
                let end_here = loop_end_of.contains_key(&s.insn.pc);
                if let Some(fl) = &s.flow {
                    term_pc = Some(s.insn.pc);
                    term = match fl {
                        Flow::Jump {
                            target,
                            conditional,
                            delayed,
                        } => Terminator::Jump {
                            target: *target,
                            conditional: *conditional,
                            delayed: *delayed,
                        },
                        Flow::JumpIndirect {
                            i,
                            m,
                            conditional,
                            delayed,
                        } => Terminator::IndirectJump {
                            i: *i,
                            m: *m,
                            conditional: *conditional,
                            delayed: *delayed,
                        },
                        Flow::Call {
                            conditional,
                            delayed,
                            ..
                        }
                        | Flow::CallIndirect {
                            conditional,
                            delayed,
                            ..
                        } => Terminator::Call {
                            conditional: *conditional,
                            delayed: *delayed,
                        },
                        Flow::Return {
                            conditional,
                            delayed,
                        } => Terminator::Return {
                            conditional: *conditional,
                            delayed: *delayed,
                        },
                        Flow::Unmodelled => {
                            f.complete = false;
                            Terminator::Unmodelled {
                                form: s.insn.form.clone(),
                            }
                        }
                    };
                    if fl.delayed() {
                        for _ in 0..DELAY_SLOTS {
                            if k + 1 < hi && slot_of.get(&swept[k + 1].insn.pc) == Some(&s.insn.pc)
                            {
                                k += 1;
                                slots.push(swept[k].insn.pc);
                            }
                        }
                    }
                    k += 1;
                    break;
                }
                k += 1;
                if end_here || k >= hi || leaders.contains(&swept[k].insn.pc) {
                    break;
                }
            }
            let end = swept.get(k).filter(|_| k < hi).map_or(f.end, |s| s.insn.pc);
            let last_pc = swept[k - 1].insn.pc;
            let mut succ = vec![];
            let fall = (k < hi).then_some(end);
            let mut push = |to: Option<u32>, kind| {
                if let Some(to) = to {
                    succ.push(Edge { to, kind });
                }
            };
            match &term {
                Terminator::Fall => push(fall, EdgeKind::Fall),
                Terminator::Jump {
                    target,
                    conditional,
                    ..
                } => {
                    push(
                        target.filter(|t| by_pc.contains_key(t) && fn_of(*t) == Some(fi)),
                        EdgeKind::Taken,
                    );
                    if *conditional {
                        push(fall, EdgeKind::Fall);
                    }
                }
                Terminator::IndirectJump { conditional, .. }
                | Terminator::Return { conditional, .. } => {
                    if *conditional {
                        push(fall, EdgeKind::Fall);
                    }
                }
                Terminator::Call { .. } => push(fall, EdgeKind::ReturnSite),
                Terminator::Unmodelled { .. } => {}
            }
            // The loop-back: the last instruction of the block (its slots included) ends a DO loop.
            let ends_loop = std::iter::once(last_pc)
                .chain(slots.iter().copied())
                .find_map(|p| loop_end_of.get(&p));
            if let Some(ids) = ends_loop {
                if term_pc.is_some() {
                    a.diagnostics.push(format!(
                        "{last_pc:#x}: a loop ends on a control transfer or its delay slot"
                    ));
                }
                for id in ids {
                    succ.push(Edge {
                        to: loops[*id].start,
                        kind: EdgeKind::LoopBack,
                    });
                }
            }
            f.blocks.push(Block {
                start,
                end,
                term_pc,
                term,
                delay_slots: slots,
                succ,
            });
        }
    }

    // Instructions that did not decode make the function containing them incomplete.
    for u in &a.undecoded {
        if let Some(fi) = fn_of(u.pc) {
            functions[fi].complete = false;
        }
    }

    let fxs: Vec<(u32, &Effects)> = swept.iter().map(|s| (s.insn.pc, &s.fx)).collect();
    crate::fxsum::summarize(&mut functions, &fxs, &calls);
    a.summary = Summary {
        instructions: swept.len(),
        functions: functions.len(),
        blocks: functions.iter().map(|f| f.blocks.len()).sum(),
        loops: loops.len(),
        calls: calls.len(),
        import_calls: calls
            .iter()
            .filter(|c| matches!(c.target, CallTarget::Import { .. }))
            .count(),
        undecoded: a.undecoded.len(),
        memory_operands: swept.iter().map(|s| s.insn.mem.len()).sum(),
    };
    a.functions = functions;
    a.loops = loops;
    a.calls = calls;
    a.insns = swept.into_iter().map(|s| s.insn).collect();
    a
}

fn functions_end(entries: &[u32], k: usize, text_end: u32) -> u32 {
    entries.get(k + 1).copied().unwrap_or(text_end)
}
