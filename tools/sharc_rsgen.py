"""Generate the firmware part of the native SHARC+ core: block code.

tools/sharc_transpile.py translates tools/sharc_core into Rust (the
``core_i``/``core_g`` modules). This generator writes what depends on the
image, under out/ (it embeds firmware):

- ``blocks_NN.rs``: region functions. Each basic block (``bblocks`` in the
  program database, or a PC the interpreter was seen entering, ``--entries``)
  becomes a body: its instructions run through the transpiled
  ``forms._execute`` partially evaluated in block mode (Translator.blk):
  the decoded fields, the PC, "no delayed transfer pending" and MODE1 are
  constants, and the registers live in the region's local register file
  (rt.rs Rf) from entry to exit, so LLVM keeps them in host registers.
  Registers the region reads must be known at entry (their known-bit
  masks become constants); a value that would become unknown, or a trap in
  the core, undoes the instruction (its written registers from copies, the
  rest from the state's undo log) and leaves it to the one-instruction
  interpreter. Bodies whose blocks follow each other often (``--transitions``)
  share a region and pass control directly, within ``--region-insns``
  instructions and ``--region-regs`` registers (a region's registers are
  live across it).
- ``spec_NN.rs``: the specialised core functions the bodies call.
- ``insns.rs``: the instruction statics the code still names.
- ``image.rs``: includes the above, the PC -> block table, the image's DO
  loop ends and hash; ``insns.bin`` holds every decoded instruction for the
  interpreter. ``--decode-range START:END`` adds short-word PCs absent from
  the program database, such as a loader entry and its following zero fill.
  It uses the ordinary loaded-memory decoder and adds no execution shortcuts.

Which blocks to generate, and for which MODE1 values, comes from where the
interpreter ran (``sharc-frames --coverage``: the executed set, not the whole
image). It also (re)writes the transpiled core next to them, so the interned
string numbers the block code uses are the core's.

    sharc-frames PACK --coverage COV           # interpreter only
    sharc-frames PACK --interp-coverage I --transitions T   # a first build
    uv run python tools/sharc_rsgen.py dt2-1.16 --coverage COV \\
        --entries I.entries --transitions T --out out/native/opt/gen
    SHARC_GEN_DIR=$PWD/out/native/opt/gen cargo build --release \\
        --manifest-path native/sharc/Cargo.toml
"""

from __future__ import annotations

import argparse
import contextlib
import json
import os
import re
import sqlite3
import sys
import time
from dataclasses import dataclass

HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:
    sys.path.insert(0, HERE)
ROOT = os.path.dirname(HERE)

DEFAULT_OUT = os.path.join(ROOT, "out", "native", "gen", "tx")


def decode_range(text: str) -> tuple[int, int]:
    """An explicit, bounded half-open range of short-word execution PCs."""
    try:
        start, end = (int(part, 0) for part in text.split(":"))
    except ValueError as exc:
        raise argparse.ArgumentTypeError(
            "expected START:END, e.g. 0x1c1338:0x1c13e6"
        ) from exc
    if not 0 <= start < end <= 1 << 24 or end - start > 1 << 20:
        raise argparse.ArgumentTypeError(
            "range must fit 24-bit PCs and contain at most 1048576 short words"
        )
    return start, end


@dataclass
class Block:
    start: int
    end: int
    insns: list  # [(pc, Instruction)]


def ident(pc: int) -> str:
    return "%X" % pc


def field_entry(syms, key: str, value: int) -> str:
    """rt::FieldEntry(key, stem, hi, lo, value) for decoded field KEY."""
    stem, _, rng = key.partition("[")
    hi = lo = -1
    if rng.endswith("]") and ":" in rng:
        h, _, lo_text = rng[:-1].partition(":")
        if h.isdigit() and lo_text.isdigit():
            hi, lo = int(h), int(lo_text)
    return "FieldEntry(%s, %s, %d, %d, %d)" % (
        syms.ident(key),
        syms.ident(stem),
        hi,
        lo,
        value,
    )


def split_compute(fields: dict[str, int]) -> dict[str, int]:
    """encoding._split_compute_fields, applied at generation time to every
    instruction with a whole ``compute`` field (the runtime's
    _split_compute_fields is then the identity)."""
    if "compute" not in fields:
        return dict(fields)
    out = dict(fields)
    out["compute[22:16]"] = fields["compute"] >> 16
    out["compute[15:0]"] = fields["compute"] & 0xFFFF
    return out


def insn_static(syms, pc: int, insn) -> str:
    fields = split_compute(insn.fields)
    entries = ", ".join(field_entry(syms, k, v) for k, v in fields.items())
    length = "None" if insn.length_bytes is None else "Some(%d)" % insn.length_bytes
    return (
        "static F_%s: Fields = Fields { kv: &[%s] };\n"
        "static I_%s: Insn = Insn { type_name: %s, fields: &F_%s, "
        "length_bytes: %s, kind: %s, offset: %d };"
        % (
            ident(pc),
            entries,
            ident(pc),
            syms.ident(insn.type_name or "unknown"),
            ident(pc),
            length,
            syms.ident(insn.kind),
            insn.offset,
        )
    )


@dataclass
class Body:
    """One block specialised on one MODE1 value (None: not a fact), as a
    body of its region's function."""

    block: Block
    mode1: int | None
    idx: int
    # (pc, insn, variant path, (reads, snapshot reads, writes),
    #  pending known none, MODE1 fact after it (None: unknown))
    steps: list
    stopped: tuple | None
    read_any: set
    writes: set


def plan_body(block: Block, tr, mode1: int | None, idx: int) -> Body | None:
    """Specialise BLOCK's instructions on their decode, their PC, "no
    delayed transfer pending" until an instruction may start one, and MODE1
    while it is known (tracking the value an instruction leaves)."""
    import sharc_transpile as tp

    steps = []
    pending_none = True
    stopped = None
    entry_mode1 = mode1
    for i, (pc, insn) in enumerate(block.insns):
        facts: dict = {"pc_sw": pc}
        if pending_none:
            facts["pending"] = None
        if mode1 is not None:
            facts["MODE1"] = mode1
        try:
            path = tr.variant("sharc_core.forms._execute", {"insn": insn}, facts)
        except tp.TranspileError as exc:
            # The one-instruction interpreter runs it (and what follows).
            tr.block_failures.append((pc, str(exc)))
            if i == 0:
                return None
            stopped = (pc, insn, exc)
            break
        if tr.variant_memwrites.get(path, True) and not tr.variant_memreads.get(
            path, True
        ):
            # No memory read: its writes need no undo (see MEM_READS).
            with contextlib.suppress(tp.TranspileError):
                path = tr.variant(
                    "sharc_core.forms._execute",
                    {"insn": insn},
                    {**facts, "nolog": True},
                )
        effects = tr.variant_effects.get(path, {"pending", "MODE1"})
        if "pending" in effects:
            pending_none = False
        if "MODE1" in effects:
            mode1 = tr.variant_facts_out.get(path, {}).get("MODE1")
        steps.append((pc, insn, path, tr.variant_regs[path], "pending" in facts, mode1))
    read_any = set().union(*(r | o for _p, _i, _x, (r, o, _w), _n, _m in steps))
    writes = set().union(*(w for _p, _i, _x, (_r, _o, w), _n, _m in steps))
    return Body(block, entry_mode1, idx, steps, stopped, read_any, writes)


def route(
    body: Body,
    mode1_after: int | None,
    by_start: dict,
    succ: set | None,
    indent: str,
    chain: dict | None = None,
) -> list[str]:
    """Where control goes when an instruction of BODY leaves the PC off its
    straight line: another body of the region (the one for the MODE1 value
    known here, or chosen by the run-time MODE1), a block of another region
    seen to follow it (CHAIN: pc -> index, a direct call after the
    write-back), else the dispatcher."""
    arms = []
    starts = sorted(by_start)
    if succ is not None:
        starts = [p for p in starts if p in succ or p == body.block.start]
        if chain is not None:
            for pc in sorted(succ):
                if pc in by_start:
                    continue
                k = chain.setdefault(pc, len(chain))
                arms.append(
                    "%s    %#x => break 'r crate::EXIT_CHAIN + %d," % (indent, pc, k)
                )
    for pc in starts:
        targets = by_start[pc]  # [Body]
        if pc == body.block.start and (body.mode1 is None or body.mode1 == mode1_after):
            # Back to its own start (a DO loop body): no entry checks again.
            arms.append("%s    %#x => continue 'b%d," % (indent, pc, body.idx))
            continue
        exact = [t for t in targets if t.mode1 is None]
        if exact:
            t = exact[0]
            arms.append("%s    %#x => { at = %d; continue 'r; }" % (indent, pc, t.idx))
            continue
        if mode1_after is not None:
            same = [t for t in targets if t.mode1 == mode1_after]
            if same:
                arms.append(
                    "%s    %#x => { at = %d; continue 'r; }" % (indent, pc, same[0].idx)
                )
            continue
        sel = ", ".join("%#x => %d" % (t.mode1, t.idx) for t in targets)
        arms.append(
            "%s    %#x => { let m = rf.r[%d]; if m.m != u32::MAX { break 'r crate::EXIT_NEXT; } "
            "at = match m.b { %s, _ => break 'r crate::EXIT_NEXT }; continue 'r; }"
            % (indent, pc, MODE1, sel)
        )
    if not arms:
        return [indent + "break 'r crate::EXIT_NEXT;"]
    return (
        [indent + "match rf.pc as u32 {"]
        + arms
        + [indent + "    _ => break 'r crate::EXIT_NEXT,", indent + "}"]
    )


TR = None  # the translator of the current generation (tr_logs)


def tr_logs(path: str) -> bool:
    """Whether the instruction's code writes the undo log."""
    return TR is None or TR.variant_logs.get(path, True)


def body_lines(
    body: Body, by_start: dict, succ: set | None, chain: dict | None = None
) -> list[str]:
    import sharc_transpile as tp

    ind = "            "
    n = len(body.steps)
    known = sorted(body.read_any - tp.PARTIAL_REGS)
    lines = [
        "        %d => {" % body.idx,
        "            // %#x%s"
        % (
            body.block.start,
            "" if body.mode1 is None else " with MODE1 = %#x" % body.mode1,
        ),
    ]
    if known:
        # Registers read must be known (their masks become constants).
        lines.append(
            "            if (%s) != u32::MAX {"
            % " & ".join("rf.r[%d].m" % c for c in known)
        )
        lines.append("                break 'r crate::EXIT_BUDGET;")
        lines.append("            }")
    lines += [
        "            'b%d: loop {" % body.idx,
        "            // Entered at the start with no delayed transfer pending (the",
        "            // dispatcher single-steps otherwise).",
        "            if s.icount + ic + %d > s.limit || rf.pending.is_some() {" % n,
        "                break 'r crate::EXIT_BUDGET;",
        "            }",
    ]
    for i, (pc, insn, path, (_r, o, w), no_pending, m_after) in enumerate(body.steps):
        lines.append(ind + "// %#x %s" % (pc, insn.type_name))
        for c in sorted(o):
            lines.append(ind + "rf.o[%d] = rf.r[%d];" % (c, c))
        for c in sorted(w):
            lines.append(ind + "let sv%d = rf.r[%d];" % (c, c))
        # What a trap restores besides registers (the undo log holds the
        # rest): the PC is this instruction's, the pending transfer is
        # known to be none unless a delay slot.
        if not no_pending:
            lines.append(ind + "let pd0 = rf.pending;")
        if TR is not None and path in TR.variant_inline:
            # The instruction only moves the PC.
            lines.append(ind + TR.variant_inline[path])
            lines.append(ind + "if false {")
            lines.append(ind + "    let t = Trap(0);")
        else:
            lines.append(ind + "if let Err(t) = %s(s, &mut rf) {" % path)
        for c in sorted(w):
            lines.append(ind + "    rf.r[%d] = sv%d;" % (c, c))
        lines.append(ind + "    rf.pc = %#x;" % pc)
        lines.append(ind + "    rf.pending = %s;" % ("None" if no_pending else "pd0"))
        lines.append(ind + "    s.trap = Some(t);")
        lines.append(ind + "    break 'r crate::EXIT_TRAP;")
        lines.append(ind + "}")
        if tr_logs(path):
            lines.append(ind + "s.un = 0;")
        lines.append(ind + "ic += 1;")
        last = i + 1 == n
        if not last:
            nxt = pc + insn.length_bytes // 2
            lines.append(ind + "if rf.pc != %#x {" % nxt)
            lines.extend(route(body, m_after, by_start, succ, ind + "    ", chain))
            lines.append(ind + "}")
        elif body.stopped is None:
            lines.extend(route(body, m_after, by_start, succ, ind, chain))
        else:
            spc, sinsn, exc = body.stopped
            lines.append(
                ind + "// %#x %s: not specialized (%s)" % (spc, sinsn.type_name, exc)
            )
            lines.append(ind + "break 'r crate::EXIT_NEXT;")
    lines.append("            }")
    lines.append("        }")
    return lines


def region_text(name: str, bodies: list[Body], succ_of: dict | None) -> str:
    """One function for a region's bodies: the registers any of them uses
    live in a local register file (rt.rs Rf) from entry to exit, loaded
    once and written back once; control moves between bodies without the
    dispatcher. An instruction that traps is undone (its written registers
    restored from copies taken at its start, the rest through the state's
    undo log) and the dispatcher runs it through the one-instruction
    interpreter."""
    by_start: dict[int, list[Body]] = {}
    for b in bodies:
        by_start.setdefault(b.block.start, []).append(b)
    loads = sorted(set().union(*(b.read_any | b.writes for b in bodies)))
    writes = sorted(set().union(*(b.writes for b in bodies)))
    import sharc_transpile as tp

    puts = set()
    for b in bodies:
        for step in b.steps:
            puts |= (
                TR.variant_putregs.get(step[2], set(range(128)))
                if TR
                else set(range(128))
            )

    known = sorted(set().union(*(b.read_any for b in bodies)) - tp.PARTIAL_REGS)
    lines = [
        "#[inline(never)]",
        "pub fn %s(s: &mut St, mut at: u32) -> u32 {" % name,
        "    // The caller checked s.cfg.block_ok and s.loops_ok (Engine::step).",
    ]
    if known:
        # Registers read must be known at entry: their masks become
        # constants (a register that would become unknown leaves early).
        lines.append(
            "    if (%s) != u32::MAX {" % " & ".join("s.r[%d].m" % c for c in known)
        )
        lines.append("        return crate::EXIT_BUDGET;")
        lines.append("    }")
    # Bodies start with no delayed transfer pending: so does the region.
    lines.append("    if s.pending.is_some() {")
    lines.append("        return crate::EXIT_BUDGET;")
    lines.append("    }")
    # Registers that are PartialConst in practice: their usual known-bit
    # mask is required (and so a constant), anything else leaves early.
    partial = sorted(set(loads) & set(tp.PARTIAL_MASKS) & REQUIRE_MASKS)
    for c in partial:
        lines.append("    if s.r[%d].m != %#x {" % (c, tp.PARTIAL_MASKS[c]))
        lines.append("        return crate::EXIT_BUDGET;")
        lines.append("    }")
    lines.append("    let mut rf = Rf::default();")
    lines.append("    rf.pc = s.pc_sw;")
    lines.append("    rf.pending = None;")
    for c in loads:
        if c in known:
            lines.append("    rf.r[%d] = V { b: s.r[%d].b, m: u32::MAX };" % (c, c))
        elif c in partial:
            lines.append(
                "    rf.r[%d] = V { b: s.r[%d].b, m: %#x };"
                % (c, c, tp.PARTIAL_MASKS[c])
            )
        else:
            lines.append("    rf.r[%d] = s.r[%d];" % (c, c))
    lines += [
        "    // Instructions completed in this call (s.icount at the exit).",
        "    let mut ic: u64 = 0;",
        "    let exit: u32 = 'r: loop {",
        "        match at {",
    ]
    chain: dict[int, int] | None = {} if CHAINING else None
    for b in bodies:
        succ = None if succ_of is None else succ_of.get(b.block.start, set())
        lines.extend(body_lines(b, by_start, succ, chain))
    lines += [
        "            _ => break 'r crate::EXIT_BUDGET,",
        "        }",
        "    };",
        "    s.icount += ic;",
        "    s.pc_sw = rf.pc;",
        "    s.pending = rf.pending;",
    ]
    for c in writes:
        if c in known and c not in puts:
            # Known at entry and only ever written known: the mask stays.
            lines.append("    s.r[%d].b = rf.r[%d].b;" % (c, c))
        else:
            lines.append("    s.r[%d] = rf.r[%d];" % (c, c))
    lines += [
        "    if exit == crate::EXIT_TRAP {",
        "        s.rollback_log();",
        "    }",
    ]
    if chain:
        # A block of another region follows: call it directly (a tail call;
        # the chain length is bounded in case it is not one).
        lines += [
            "    if exit >= crate::EXIT_CHAIN {",
            "        if s.chain >= crate::CHAIN_MAX {",
            "            return crate::EXIT_NEXT;",
            "        }",
            "        s.chain += 1;",
            "        return match exit - crate::EXIT_CHAIN {",
        ]
        for pc, k in sorted(chain.items(), key=lambda kv: kv[1]):
            lines.append("            %d => @@B_%X@@(s)," % (k, pc))
        lines += [
            "            _ => crate::EXIT_NEXT,",
            "        };",
            "    }",
        ]
    lines += [
        "    exit",
        "}",
    ]
    return "\n".join(lines)


def entry_text(start: int, region: str, bodies: list[Body]) -> str:
    """The dispatcher's function for block START: pick the body for the
    run-time MODE1 (the value each was specialised on) and enter the region."""
    free = [b for b in bodies if b.mode1 is None]
    lines = ["pub fn b_%s(s: &mut St) -> u32 {" % ident(start)]
    if free:
        lines.append("    %s(s, %d)" % (region, free[0].idx))
    else:
        lines += [
            "    let m = s.r[%d];" % MODE1,
            "    if m.m != u32::MAX {",
            "        return crate::EXIT_BUDGET;",
            "    }",
            "    let at = match m.b {",
        ]
        for b in bodies:
            assert b.mode1 is not None
            lines.append("        %#x => %d," % (b.mode1, b.idx))
        lines += [
            "        _ => return crate::EXIT_BUDGET,",
            "    };",
            "    %s(s, at)" % region,
        ]
    lines.append("}")
    return "\n".join(lines)


def build_regions(
    blocks: list[Block],
    tr,
    mode1: dict[int, list[int]] | None,
    func_of: dict[int, int],
    succ_of: dict | None,
    max_insns: int = 1200,
    counts: dict[tuple[int, int], int] | None = None,
    max_regs: int = 28,
) -> list[tuple[str, list[int], str]]:
    """Group the blocks into regions and generate each: [(name, block
    starts, text)].

    With COUNTS (block-to-block transition counts), a block joins the
    region of the block that most often precedes it (when that edge carries
    at least half of its entries), as long as the region stays within
    MAX_INSNS body instructions and MAX_REGS registers: every register a
    region uses is live across it, so a large one spills. Without, blocks
    group by program-db function in address order (MAX_INSNS 0: one block
    each)."""
    planned: dict[int, list[Body]] = {}
    for blk in sorted(blocks, key=lambda x: x.start):
        values: list[int | None] = list((mode1 or {}).get(blk.start) or []) or [None]
        found: list[Body] = []
        for m in values:
            body = plan_body(blk, tr, m, 0)
            if body is not None:
                body.mode1 = m
                found.append(body)
        if found:
            planned[blk.start] = found

    def size_of(starts: set) -> tuple[int, int]:
        regs: set = set()
        n = 0
        for st in starts:
            for body in planned[st]:
                regs |= body.read_any | body.writes
                n += len(body.steps)
        return n, len(regs)

    groups: list[list[int]] = []
    if counts is not None and max_insns > 0:
        group_of: dict[int, set[int]] = {st: {st} for st in planned}
        incoming: dict[int, int] = {}
        for (_a, b), c in counts.items():
            incoming[b] = incoming.get(b, 0) + c
        for (a, b), c in sorted(counts.items(), key=lambda kv: -kv[1]):
            if a not in planned or b not in planned or a == b:
                continue
            if not CROSS_FUNCTIONS and func_of.get(a) != func_of.get(b):
                continue
            if c * 2 < incoming.get(b, 0):
                continue
            ga, gb = group_of[a], group_of[b]
            if ga is gb:
                continue
            n, r = size_of(ga | gb)
            if n > max_insns or r > max_regs:
                continue
            merged = ga | gb
            for st in merged:
                group_of[st] = merged
        seen: set = set()
        for st in sorted(planned):
            members = group_of[st]
            if id(members) in seen:
                continue
            seen.add(id(members))
            groups.append(sorted(members))
    else:
        by_fn: dict[int, list[int]] = {}
        for st in planned:
            by_fn.setdefault(func_of.get(st, st), []).append(st)
        for fn in sorted(by_fn):
            chunk: list[int] = []
            size = 0
            for st in sorted(by_fn[fn]):
                n = sum(len(b.steps) for b in planned[st])
                if chunk and (size + n > max_insns or max_insns == 0):
                    groups.append(chunk)
                    chunk, size = [], 0
                chunk.append(st)
                size += n
            if chunk:
                groups.append(chunk)
    out = []
    for g in groups:
        bodies: list[Body] = []
        for st in g:
            for body in planned[st]:
                body.idx = len(bodies)
                bodies.append(body)
        name = "r_%X" % g[0]
        text = region_text(name, bodies, succ_of)
        ents = [
            entry_text(st, name, [b for b in bodies if b.block.start == st]) for st in g
        ]
        out.append((name, list(g), text + "\n\n" + "\n\n".join(ents)))
    return out


MODE1 = 114
# Regions may span program-db functions (a call and its callee's blocks).
CROSS_FUNCTIONS = True
# Partial registers whose usual mask block code requires at entry (none:
# the masks vary too much between entries to be worth a bail).
REQUIRE_MASKS: set[int] = set()
# Direct calls between regions (--chain; slower on the drive3 frames:
# the dispatcher's indirect call predicts well).
CHAINING = False


def read_coverage(path: str) -> dict[int, dict[int, int]]:
    """sharc-frames --coverage output: pc -> {MODE1 value: count} (known
    MODE1 only; an unknown one counts under -1)."""
    out: dict[int, dict[int, int]] = {}
    with open(path) as fh:
        for line in fh:
            parts = line.split()
            if len(parts) != 4:
                continue
            pc, m, known, count = (
                int(parts[0], 0),
                int(parts[1], 0),
                int(parts[2]),
                int(parts[3]),
            )
            key = m if known else -1
            d = out.setdefault(pc, {})
            d[key] = d.get(key, 0) + count
    return out


BLOCK_PRELUDE = """use super::*;
use crate::generated::core_i as ci;
use crate::generated::syms::*;
use crate::rt::*;

"""


SPEC_PRELUDE = """// Generated by tools/sharc_rsgen.py: core functions specialized on the
// image's instructions (tools/sharc_transpile.py, Translator.variant).
use super::*;
use crate::generated::syms::*;
use crate::generated::tables::*;
use crate::rt::*;
"""


def load_blocks(db: str, image: str, starts: list[int], mem) -> list[Block]:
    from sharc_core.sequencer import decode_at

    con = sqlite3.connect(db)
    out = []
    for start in starts:
        row = con.execute(
            "select end_sw from bblocks where image=? and start_sw=?", (image, start)
        ).fetchone()
        if row is None:
            # An entry inside a program-database block (a return address
            # after a call, an indirect jump target): the rest of that block.
            row = con.execute(
                "select end_sw from bblocks where image=? and start_sw<=? and end_sw>?"
                " order by start_sw desc limit 1",
                (image, start, start),
            ).fetchone()
        if row is None:
            continue  # outside every program-db block: interpreted
        end = row[0]
        insns = []
        pc = start
        while pc < end:
            insn = decode_at(mem, None, pc)
            if insn.length_bytes is None:
                break
            insns.append((pc, insn))
            pc += insn.length_bytes // 2
        if insns:
            out.append(Block(start, end, insns))
    return out


def all_block_starts(db: str, image: str) -> list[int]:
    con = sqlite3.connect(db)
    return [
        r[0]
        for r in con.execute(
            "select start_sw from bblocks where image=? order by start_sw", (image,)
        )
    ]


def loop_ends(decoded: dict) -> list[int]:
    from sharc_core.encoding import _field
    from sharc_core.values import _signed

    ends = []
    for pc, insn in decoded.items():
        if insn.type_name in ("12a_imm", "12a_ureg") and insn.length_bytes is not None:
            reladdr = (_field(insn.fields, "reladdr[22:16]") << 16) | _field(
                insn.fields, "reladdr[15:0]"
            )
            ends.append(pc + _signed(reladdr, 23))
    return sorted(set(ends))


def table_pcs(db: str, image: str) -> list[int]:
    """Every PC the program database decoded (aligned or not): a branch or
    call may reach any of them."""
    con = sqlite3.connect(db)
    return [
        r[0]
        for r in con.execute("select sw from insn where image=? order by sw", (image,))
    ]


def field_tuple(key: str) -> tuple[str, int, int]:
    stem, _, rng = key.partition("[")
    hi = lo = -1
    if rng.endswith("]") and ":" in rng:
        h, _, lo_text = rng[:-1].partition(":")
        if h.isdigit() and lo_text.isdigit():
            hi, lo = int(h), int(lo_text)
    return stem, hi, lo


def insn_blob(syms, decoded: dict) -> bytes:
    """The one-instruction interpreter's table (native canon.rs
    parse_insn_table): magic "SHIX", count, then per instruction pc:u32,
    type:u16, kind:u16, length:i8 (-1 None), nfields:u8, and per field
    key:u16, stem:u16, hi:i8, lo:i8, value:i64."""
    import struct

    out = bytearray(b"SHIX")
    rows = [
        (pc, insn)
        for pc, insn in sorted(decoded.items())
        if insn.length_bytes is not None
    ]
    out += struct.pack("<I", len(rows))
    for pc, insn in rows:
        fields = split_compute(insn.fields)
        out += struct.pack(
            "<IHHbB",
            pc,
            syms.intern(insn.type_name or "unknown"),
            syms.intern(insn.kind),
            insn.length_bytes,
            len(fields),
        )
        for key, value in fields.items():
            stem, hi, lo = field_tuple(key)
            out += struct.pack(
                "<HHbbq", syms.intern(key), syms.intern(stem), hi, lo, value
            )
    return bytes(out)


def generate(
    image: str,
    starts: list[int],
    out_dir: str,
    *,
    per_module: int = 64,
    work_dir: str | None = None,
    mode1: dict[int, list[int]] | None = None,
    successors: dict[int, set[int]] | None = None,
    region_insns: int = 1200,
    transition_counts: dict[tuple[int, int], int] | None = None,
    region_regs: int = 28,
    decode_ranges: list[tuple[int, int]] | None = None,
) -> dict:
    import sharc
    import sharc_run as sr
    import sharc_transpile as tp
    import sharc_transpile_infer as infer
    from sharc_core.sequencer import decode_at

    t0 = time.perf_counter()
    img = sharc.load(image)
    mem = img._mem()
    db = os.path.join(ROOT, "out", "sharcdb", image + ".sqlite")

    core = infer.load(work_dir or os.path.join(ROOT, "out", "native", "transpile-work"))
    out = tp.translate(core)
    tr = out.translator  # type: ignore[attr-defined]
    tr.blk = True
    global TR
    TR = tr
    syms = tr.syms
    t1 = time.perf_counter()

    pcs = set(table_pcs(db, image))
    for start, end in decode_ranges or []:
        pcs.update(range(start, end))
    decoded = {}
    for pc in sorted(pcs):
        insn = decode_at(mem, None, pc)
        decoded[pc] = insn
    blocks = load_blocks(db, image, starts, mem)
    for b in blocks:
        for pc, insn in b.insns:
            decoded.setdefault(pc, insn)
    # Every DO loop's end address (sequencer._start_counted_loop's end_sw):
    # block code knows a loop can only end at one of these.
    tr.loop_ends = frozenset(loop_ends(decoded))
    # Specialized code refers to an instruction by its static.
    named = list(decoded.items()) + [(pc, insn) for b in blocks for pc, insn in b.insns]
    for pc, insn in named:
        tr.static_names[id(insn)] = "(&I_%s)" % ident(pc)
        tr.static_names[id(insn.fields)] = "(&F_%s)" % ident(pc)
    t2 = time.perf_counter()

    os.makedirs(out_dir, exist_ok=True)
    for name in os.listdir(out_dir):
        if name.startswith(("blocks_", "spec_")) and name.endswith(".rs"):
            os.remove(os.path.join(out_dir, name))

    # Statics for the instructions block code runs (their fields fold);
    # every decoded instruction goes in the blob.
    insn_lines = [
        "// Generated by tools/sharc_rsgen.py from %s. Firmware-derived: do not commit."
        % image
    ]
    in_blocks = sorted({pc for b in blocks for pc, _ in b.insns})
    table = [pc for pc in decoded if decoded[pc].length_bytes is not None]

    tr.block_failures = []
    con = sqlite3.connect(os.path.join(ROOT, "out", "sharcdb", image + ".sqlite"))
    func_of = {
        pc: fn
        for pc, fn in con.execute(
            "select sw, function_sw from insn where image=? and function_sw is not null",
            (image,),
        )
    }
    regions = build_regions(
        blocks,
        tr,
        mode1,
        func_of,
        successors,
        region_insns,
        transition_counts,
        region_regs,
    )
    by_start = {b.start: b for b in blocks}
    generated = {st for _n, starts_, _t in regions for st in starts_}
    blocks = [by_start[st] for st in sorted(generated)]
    # Regions per file: about PER_MODULE blocks each.
    modules: list[list[tuple[str, list[int], str]]] = [[]]
    count = 0
    for reg in regions:
        if count >= per_module:
            modules.append([])
            count = 0
        modules[-1].append(reg)
        count += len(reg[1])
    # Statics only for the instructions the code still names (partial
    # evaluation folds the rest into constants).
    still_named: set[int] = set()
    for _n, _s, text in regions:
        still_named |= {int(x, 16) for x in re.findall(r"&[IF]_([0-9A-F]+)\)", text)}
    for _m, text in tr.variant_texts:
        still_named |= {int(x, 16) for x in re.findall(r"&[IF]_([0-9A-F]+)\)", text)}
    for pc in in_blocks:
        if pc in still_named:
            insn_lines.append(insn_static(syms, pc, decoded[pc]))
    files = {"insns.rs": "\n".join(insn_lines) + "\n"}
    blob = insn_blob(syms, decoded)
    where = {
        st: m for m, chunk in enumerate(modules) for _n, sts, _t in chunk for st in sts
    }

    def link(mm: re.Match) -> str:
        pc = int(mm.group(1), 16)
        if pc not in where:
            return "crate::no_block"
        return "crate::generated::image::blocks_%02d::b_%X" % (where[pc], pc)

    for m, chunk in enumerate(modules):
        body = "\n\n".join(text for _n, _s, text in chunk)
        body = re.sub(r"@@B_([0-9A-F]+)@@", link, body)
        files["blocks_%02d.rs" % m] = BLOCK_PRELUDE + "\n" + body + "\n"
    t3 = time.perf_counter()
    spec: dict[int, list[str]] = {}
    for module, text in tr.variant_texts:
        spec.setdefault(module, []).append(text)
    for module in range(tr.variant_modules):
        files["spec_%02d.rs" % module] = (
            SPEC_PRELUDE + "\n" + "\n\n".join(spec.get(module, [])) + "\n"
        )
    # The variants added lookup tables, strings and trap sites.
    out.files["tables.rs"] = tp.render_tables(tr)
    out.files["syms.rs"] = syms.rust()

    gen_dir = 'concat!(env!("SHARC_GEN_DIR"), "/%s")'
    image_lines = [
        "// Generated by tools/sharc_rsgen.py from %s. Firmware-derived: do not commit."
        % image,
        "use crate::rt::*;",
        "use crate::generated::syms::*;",
        "use crate::generated::core_i as ci;",
        "",
        "pub const IMAGE: &str = %s;" % json.dumps(image),
        "pub const IMAGE_SHA256: &str = %s;" % json.dumps(sr._image_sha256(mem)),
        "",
        "include!(%s);" % (gen_dir % "insns.rs"),
        "",
        "/// Every decoded instruction (tools/sharc_rsgen.py insn_blob).",
        "pub static INSN_BLOB: &[u8] = include_bytes!(%s);" % (gen_dir % "insns.bin"),
        "",
        "/// Every DO loop's end address (block code assumes the loop stack",
        "/// holds only these; the runtime checks at import).",
        "pub static LOOP_ENDS: &[i64] = &[%s];"
        % ", ".join("%d" % e for e in sorted(tr.loop_ends)),
        "",
        "/// The decoded instruction at PC (sequencer.decode_at over the image).",
        "pub fn insn_at(pc: Int) -> Option<&'static Insn> {",
        "    crate::canon::insn_table(INSN_BLOB).get(pc)",
        "}",
        "",
    ]
    for m in range(len(modules)):
        image_lines.append(
            "pub mod blocks_%02d {\n    include!(%s);\n}"
            % (m, gen_dir % ("blocks_%02d.rs" % m))
        )
    for m in range(tr.variant_modules):
        image_lines.append(
            "pub mod spec_%02d {\n    include!(%s);\n}"
            % (m, gen_dir % ("spec_%02d.rs" % m))
        )
    entries = []
    for m, chunk in enumerate(modules):
        for _name, starts_, _text in chunk:
            for st in starts_:
                entries.append(
                    "(%#x, blocks_%02d::b_%s as crate::BlockFn)" % (st, m, ident(st))
                )
    image_lines.append(
        "pub static BLOCKS: &[(u32, crate::BlockFn)] = &[\n    %s\n];"
        % ",\n    ".join(entries)
    )
    files["image.rs"] = "\n".join(image_lines) + "\n"

    tp.write(out, out_dir)
    for name, text in files.items():
        with open(os.path.join(out_dir, name), "w") as fh:
            fh.write(text)
    with open(os.path.join(out_dir, "insns.bin"), "wb") as bfh:
        bfh.write(blob)

    report = {
        "image": image,
        "blocks": len(blocks),
        "block_instructions": sum(len(b.insns) for b in blocks),
        "instructions_in_table": len(table),
        "decode_ranges": decode_ranges or [],
        "modules": len(modules),
        "regions": len(regions),
        "per_module": per_module,
        "bytes": {name: len(text) for name, text in files.items()},
        "variants": len(tr.variants),
        "seconds": {
            "transpile": round(t1 - t0, 2),
            "decode": round(t2 - t1, 2),
            "specialize": round(t3 - t2, 2),
            "total": round(time.perf_counter() - t0, 2),
        },
        "block_starts": [b.start for b in blocks],
        "specializer_failures": [["%#x" % pc, why] for pc, why in tr.block_failures],
        "transpile": out.report,
    }
    with open(os.path.join(out_dir, "rsgen-report.json"), "w") as fh:
        json.dump(report, fh, indent=1)
    return report


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("image", help='program database image, e.g. "dt2-1.16"')
    p.add_argument("--out", default=DEFAULT_OUT, help="output directory (under out/)")
    p.add_argument("--blocks", default="", help="comma-separated hex block starts")
    p.add_argument(
        "--decode-range",
        type=decode_range,
        action="append",
        default=[],
        help="add interpreter decode coverage for START:END short-word PCs (repeatable)",
    )
    p.add_argument(
        "--blocks-from", help="a report.json listing blocks (phase 0 or this tool)"
    )
    p.add_argument("--counts", help="JSON with a 'ranked' list of [block_start, count]")
    p.add_argument(
        "--top", type=int, default=0, help="the N most executed blocks from --counts"
    )
    p.add_argument("--all", action="store_true", help="every basic block of the image")
    p.add_argument(
        "--only",
        help="comma-separated hex block starts: generate only these (with the "
        "MODE1 values --coverage gives them)",
    )
    p.add_argument(
        "--chain",
        action="store_true",
        help="direct calls between regions (else every exit returns to the dispatcher)",
    )
    p.add_argument("--no-chain", action="store_true", help=argparse.SUPPRESS)
    p.add_argument(
        "--region-regs",
        type=int,
        default=36,
        help="registers a region may use at most (they are live across it)",
    )
    p.add_argument(
        "--region-insns",
        type=int,
        default=120,
        help="instructions per region function at most (0: one block each)",
    )
    p.add_argument(
        "--transitions",
        help="sharc-frames --transitions output: the block-to-block moves "
        "seen; a region routes each block's exits only to those",
    )
    p.add_argument(
        "--entries",
        help="sharc-frames --interp-coverage FILE's FILE.entries: PCs the "
        "interpreter was entered at; each starts a block (to its program-db "
        "block's end), specialised on the MODE1 values --coverage saw there",
    )
    p.add_argument(
        "--coverage",
        help="sharc-frames --coverage output: the blocks whose start ran, "
        "each specialised on the MODE1 values it was entered with",
    )
    p.add_argument(
        "--per-module", type=int, default=64, help="block functions per Rust file"
    )
    args = p.parse_args(argv)

    db = os.path.join(ROOT, "out", "sharcdb", args.image + ".sqlite")
    starts = [int(x, 16) for x in args.blocks.split(",") if x]
    if args.blocks_from:
        with open(args.blocks_from) as fh:
            rep = json.load(fh)
        blocks_field = rep.get("blocks", [])
        for b in blocks_field if isinstance(blocks_field, list) else []:
            starts.append(b["start"] if isinstance(b, dict) else int(b))
        starts += rep.get("block_starts", [])
    if args.counts and args.top:
        with open(args.counts) as fh:
            ranked = json.load(fh)["ranked"]
        starts += [int(b) for b, _ in ranked[: args.top]]
    if args.all:
        starts = all_block_starts(db, args.image)
    mode1: dict[int, list[int]] | None = None
    if args.coverage:
        cov = read_coverage(args.coverage)
        every = set(all_block_starts(db, args.image))
        mode1 = {}
        for pc, seen in cov.items():
            if pc not in every:
                continue
            starts.append(pc)
            known = sorted((m for m in seen if m >= 0), key=lambda m: -seen[m])
            mode1[pc] = known[:4]
        if args.entries:
            with open(args.entries) as fh:
                for line in fh:
                    parts = line.split()
                    if len(parts) != 2:
                        continue
                    pc = int(parts[0], 0)
                    at = cov.get(pc)
                    if not at:
                        continue
                    starts.append(pc)
                    known = sorted((m for m in at if m >= 0), key=lambda m: -at[m])
                    mode1[pc] = known[:4]
    if args.only:
        keep = {int(x, 16) for x in args.only.split(",") if x}
        starts = [st for st in starts if st in keep]
    starts = sorted(set(starts))
    global CHAINING
    CHAINING = args.chain and not args.no_chain
    successors: dict[int, set[int]] | None = None
    counts: dict[tuple[int, int], int] | None = None
    if args.transitions:
        successors = {}
        counts = {}
        with open(args.transitions) as fh:
            for line in fh:
                parts = line.split()
                if len(parts) == 3:
                    a, b = int(parts[0], 0), int(parts[1], 0)
                    successors.setdefault(a, set()).add(b)
                    counts[(a, b)] = counts.get((a, b), 0) + int(parts[2])
    report = generate(
        args.image,
        starts,
        args.out,
        per_module=args.per_module,
        mode1=mode1,
        successors=successors,
        region_insns=args.region_insns,
        transition_counts=counts,
        region_regs=args.region_regs,
        decode_ranges=args.decode_range,
    )
    print(
        "%d blocks (%d instructions), %d instructions in the table, %d files -> %s (%.1fs)"
        % (
            report["blocks"],
            report["block_instructions"],
            report["instructions_in_table"],
            report["modules"],
            args.out,
            report["seconds"]["total"],
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
