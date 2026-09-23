#!/usr/bin/env python3
"""One queryable SQLite database per SHARC+ image: instructions, functions,
control-flow edges, literals, memory-access facts and a relocation-tolerant
function hash for cross-image matching.

    uv run python tools/sharcdb.py build BLOB [--out out/sharcdb/<name>.sqlite]
        [--name NAME] [--min-depth N] [--blocks 1,56,69,...] [--force]
    uv run python tools/sharcdb.py build BLOB1 BLOB2 ... [--out-dir out/sharcdb]
        [--jobs N] [--force]

This exists so a question like "who jumps into 0x1c71ec" or "what writes
0x262138" is a `sqlite3` query in milliseconds, built once, instead of a
fresh ~5s tools/sharcfn.py load-and-decode plus a throwaway script every
time. It reuses every decoder already in this repo -- tools/sharcldr.py
(loader blocks, LoadedMemory, sw<->byte mapping), tools/sharcinv.py (code
block classification, function boundaries, the merged-field helper),
tools/sharcflow.py (call/return site finding, pcrel_target), tools/sharcfn.py
(render_instruction for the mnemonic text, register/condition naming) and
tools/sharc_disasm.py/sharcimm.py (the linear disassembler and its sweep) --
and adds no new instruction decoding of its own.

A CALL-only caller graph misses JUMP edges: a `JUMP IF SV` at sw 0x1c7053
into FUN_1c71ec went unrecorded for weeks because nothing but a bare
tools/sharcflow.py call/return scan was ever run over that address. The
`edges` table below records calls, plain and conditional jumps (with the
not-taken path as a `fallthrough` edge), returns and indirect sites
uniformly, so "callers of X" is a `to_sw = X` query across every kind at
once, not a CALL-only search.

Code-block selection. tools/sharcinv.py's CODE_BLOCKS (1, 56, 69, 76, 78, 80,
88, 91, 93) is a hand-verified list for one specific image (DT2 1.16) --
docs/findings/06-sharc-engine-and-startup.md, "The SHARC code is not one
block". It is NOT a general "decodes densely" rule: several DATA blocks in
that same image alias-decode at 70-97% density (blk19, blk35, blk37, blk48),
as dense as some of the small code fragments, so a decode-density threshold
alone silently mislabels data as code. This file therefore keeps a small,
explicit KNOWN_CODE_BLOCKS table keyed by image sha256 rather than guessing:
DT2 1.15C's block layout was checked by inspection to be identical to 1.16's
(same block indices, same target addresses -- reuses sharcinv.CODE_BLOCKS
directly), and DN2 1.11/1.10E's code blocks were identified by target-address
correspondence to DT2's known regions (the shared 0x282403f0 loader, the
three small 0x2838xxxx blocks, startup at 0x28380548 and the main code
region), the same method docs/findings/11-sharc-cross-image-comparison.md
describes ("Selection uses the same loader-target code regions as the
existing DT2 inventory. The block indices differ where the loader stream's
preceding records differ."). DN2 1.11 also has an extra L2-window code
region past L2_BYTE_LIMIT (blk59, target 0x200779f8): sharcldr.LoadedMemory's
read_sw() fallback only covers L2_BYTE_BASE..L2_BYTE_LIMIT
(0x20000000..0x20020000), so that block's base_sw cannot be resolved with
the reused tooling and it is left out of KNOWN_CODE_BLOCKS -- reported as a
known gap, not silently treated as data. An image whose sha256 is not in
KNOWN_CODE_BLOCKS refuses to build without an explicit --blocks override,
rather than guessing.
"""

from __future__ import annotations

import argparse
import bisect
import collections
import hashlib
import json
import os
import sqlite3
import subprocess
import sys
import time

_here = os.path.dirname(os.path.abspath(__file__))
if _here not in sys.path:
    sys.path.insert(0, _here)

import sharc_visa_tables as visa_tables  # noqa: E402
import sharcflow  # noqa: E402
import sharcfn  # noqa: E402
import sharcimm  # noqa: E402
import sharcinv  # noqa: E402
import sharcldr  # noqa: E402

DB_VERSION = 1

# Bump DB_VERSION whenever the schema or the semantics of an existing column
# change, so build_database()'s skip-rebuild check (sha256 + DB_VERSION) does
# the right thing on the next run.

# --- known per-image code-block selections ----------------------------------
#
# See the module docstring for how each entry was derived. dt2-1.15C's block
# layout was verified identical to dt2-1.16's (same indices, same target
# addresses) by direct inspection of both blobs' parsed block lists.
KNOWN_CODE_BLOCKS = {
    # dt2-1.16
    "0f514a12a2255f5c081e292c47f1f29462003177658da4bbae0a22fd737fffa2": sharcinv.CODE_BLOCKS,
    # dt2-1.15C (identical block layout to 1.16 -- verified by inspection)
    "6d4316cddd41edef7a136c10d270313882028a59b96cc8fe97a716b949a7d551": sharcinv.CODE_BLOCKS,
    # dn2-1.11: blk1 loader(0x282403f0), blk56 L2 main(0x20000000),
    # blk57 L2 extension(0x2001e888, within L2_BYTE_LIMIT), blk66/68/70 the
    # three small 0x2838004c/0x283801a4/0x28380328 blocks, blk78 startup
    # (0x28380548), blk81 the small 0x2838261c block, blk83 the main code
    # region (0x28382720). blk59 (0x200779f8, past L2_BYTE_LIMIT) is left out
    # -- see the module docstring.
    "336e340aa0cdcd34e314cfa44849f709a3134f6bd4cd57dfc7e15702c83115e2": (1, 56, 57, 66, 68, 70, 78, 81, 83),
    # dn2-1.10E: same target-address correspondence, shifted indices.
    "174b391822bbe33a99e5f42bd02ef3ea75c0e79351caae6ba90e4cd2c4de5350": (1, 56, 57, 67, 69, 71, 79, 82, 84),
}

SCHEMA = """
CREATE TABLE meta (image TEXT, key TEXT, value TEXT, PRIMARY KEY (image, key));

CREATE TABLE blocks (
    image TEXT, idx INTEGER, target_address INTEGER, byte_count INTEGER,
    kind TEXT,              -- 'code' | 'data' | 'fill'
    base_sw INTEGER,        -- NULL when no known addressing convention covers it
    PRIMARY KEY (image, idx));

-- Every offset tools/sharcimm.py's linear sweep decodes in a scanned code
-- block (tools/sharcpcode.py's 'decoder' table does the same for one main
-- program; this does it per code block, for every scanned block). width is
-- bytes; mnemonic is only rendered for aligned rows (see module docstring).
CREATE TABLE insn (
    image TEXT, sw INTEGER, block INTEGER, width INTEGER, raw TEXT, form TEXT,
    fields TEXT, cond INTEGER, mnemonic TEXT, aligned INTEGER, confidence TEXT,
    function_sw INTEGER,
    PRIMARY KEY (image, sw));

CREATE TABLE functions (
    image TEXT, entry_sw INTEGER, end_sw INTEGER, n_insns INTEGER, block INTEGER,
    name TEXT, has_static_caller INTEGER, is_leaf INTEGER, label TEXT, entry_kind TEXT,
    PRIMARY KEY (image, entry_sw));

-- kind: call, jump, cond_jump, fallthrough (the not-taken path of a
-- cond_jump), return, indirect (to_sw NULL; note carries the register).
CREATE TABLE edges (
    image TEXT, from_sw INTEGER, to_sw INTEGER, kind TEXT, delayed INTEGER, cond INTEGER,
    from_function INTEGER, to_function INTEGER, note TEXT);

CREATE TABLE literals (
    image TEXT, sw INTEGER, value INTEGER, form TEXT, dest_reg TEXT,
    in_code_range INTEGER, in_data_range INTEGER);

CREATE TABLE mem_access (
    image TEXT, sw INTEGER, space TEXT, direction TEXT, base_reg TEXT, modifier TEXT,
    u INTEGER, form TEXT, width TEXT, abs_address INTEGER);

CREATE TABLE func_hash (
    image TEXT, entry_sw INTEGER, exact_hash TEXT, reloc_hash TEXT, n_insns INTEGER, byte_len INTEGER,
    PRIMARY KEY (image, entry_sw));

CREATE INDEX insn_function ON insn(image, function_sw);
CREATE INDEX insn_form ON insn(image, form);
CREATE INDEX edges_to ON edges(image, to_sw);
CREATE INDEX edges_from ON edges(image, from_sw);
CREATE INDEX edges_to_function ON edges(image, to_function);
CREATE INDEX edges_from_function ON edges(image, from_function);
CREATE INDEX literals_value ON literals(image, value);
CREATE INDEX mem_access_sw ON mem_access(image, sw);
CREATE INDEX mem_access_abs ON mem_access(image, abs_address);
CREATE INDEX func_hash_exact ON func_hash(exact_hash);
CREATE INDEX func_hash_reloc ON func_hash(reloc_hash);
CREATE INDEX functions_block ON functions(image, block);
"""

# Field-label stems masked to build a relocation-tolerant function hash: a
# PC-relative branch offset ('reladdr'), an absolute call/jump target or
# I-register-modify amount ('addr'/'data'), so two copies of the same
# function at different addresses (or pointing at different-but-matching-
# shaped data) still hash the same.
_MASK_STEMS = ("addr", "reladdr", "data")


def mask_relocatable(insn) -> int:
    """insn.raw with every 'addr'/'reladdr'/'data' field's bits zeroed, using
    tools/sharc_visa_tables.py's own (hi, lo) field layout for insn.type_name
    -- no re-derivation of field positions."""
    if insn.raw is None:
        return 0
    entry = visa_tables.get_type(insn.type_name)
    if entry is None:
        return insn.raw
    raw = insn.raw
    for label, (hi, lo) in entry["fields"].items():
        if label.split("[")[0] in _MASK_STEMS:
            width = hi - lo + 1
            raw &= ~(((1 << width) - 1) << lo)
    return raw


def extract_literal(insn_type: str, f: dict):
    """-> (value, dest_reg) for a literal-carrying form (tools/sharcinv.py's
    LITERAL_FORMS), sign-extended per that table's bit width; None for any
    other form. dest_reg is the register the literal loads into for
    17a/17b (a ureg, e.g. an M-register) and 19a/19a_scaled/19a_bitrev (the
    modified I-register); None for the direct-address forms 15a/14a (the
    literal there is a memory address, not a register's contents) and for
    16a (which stores the literal to memory rather than a register)."""
    spec = sharcinv.LITERAL_FORMS.get(insn_type)
    if spec is None:
        return None
    field, bits, _is_float = spec
    raw_val = f.get(field)
    if raw_val is None:
        return None
    value = sharcfn.sign_extend(raw_val, bits)
    dest = None
    if insn_type in ("17a", "17b"):
        ureg = f.get("ureg")
        if ureg is not None:
            dest = sharcfn.ureg_name(ureg)
    elif insn_type in ("19a", "19a_scaled", "19a_bitrev"):
        # Same Is XOR Idis destination convention as sharcfn.render_modify.
        bank = 8 if f.get("g") else 0
        src_low, dis_low = f.get("is", 0), f.get("idis", 0)
        dest = "I%d" % (bank + (src_low ^ dis_low))
    elif insn_type == "18a":
        sreg = f.get("sreg", 0)
        code = sharcfn.UREG_CODES.get("USTAT1", 0) + sreg
        dest = sharcfn.ureg_name(code)
    return value, dest


def classify_literal_range(value: int, code_spans_sw, code_spans_byte, mem):
    """(in_code_range, in_data_range) for a literal value: whether it falls
    in a scanned code block's own short-word range or its loader byte-alias
    range, or in some other loader-backed (data) range. A value in both is
    reported as code only (a literal is either a code pointer or a data
    pointer, not both)."""
    in_code = any(lo <= value < hi for lo, hi in code_spans_sw)
    if not in_code:
        byte_val = sharcldr.sw_to_byte(value)
        in_code = any(lo <= value < hi or lo <= byte_val < hi for lo, hi in code_spans_byte)
    in_data = False
    if not in_code:
        for lo, hi in mem.ranges():
            if lo <= value < hi:
                in_data = True
                break
    return in_code, in_data


def extract_mem_access(insn_type: str, f: dict):
    """-> [(space, direction, base_reg, modifier, u, form, width, abs_address), ...]
    for a memory-referencing form; [] for any other form. form is
    tools/sharcinv.py's MEM_FORMS label ('direct'/'i,m-mod'/'i+imm'/
    'i,m-mod+shift') or 'dual' for the simultaneous DM+PM forms, so it stays
    in step with that table rather than reinventing addressing-mode names."""
    rows = []
    if insn_type in sharcfn.DIRECT_MEM_FORMS:
        space, direction = sharcfn._space_dir(f)
        rows.append((space, direction, None, None, None,
                     sharcinv.MEM_FORMS.get(insn_type, insn_type),
                     "long" if f.get("l") else "word", f.get("addr")))
    elif insn_type in sharcfn.INDEXED_MEM_FORMS:
        space, direction = sharcfn._space_dir(f)
        rows.append((space, direction, "I%d" % f.get("i", 0), "M%d" % f.get("m", 0),
                     f.get("u"), sharcinv.MEM_FORMS.get(insn_type, insn_type), None, None))
    elif insn_type in sharcfn.IMMOFF_MEM_FORMS:
        space, direction = sharcfn._space_dir(f)
        bits = sharcfn._IMMOFF_DATA_BITS.get(insn_type, 6)
        off = sharcfn.sign_extend(f.get("data", 0), bits)
        rows.append((space, direction, "I%d" % f.get("i", 0), str(off), None,
                     sharcinv.MEM_FORMS.get(insn_type, insn_type),
                     "long" if f.get("l") else "word", None))
    elif insn_type in sharcfn.DUAL_MEM_FORMS:
        rows.append(("DM", "store" if f.get("dmd") else "load", "I%d" % f.get("dmi", 0),
                     "M%d" % f.get("dmm", 0), None, "dual", None, None))
        rows.append(("PM", "store" if f.get("pmd") else "load", "I%d" % f.get("pmi", 0),
                     "M%d" % f.get("pmm", 0), None, "dual", None, None))
    return rows


def _slots(insns, n):
    """The two aligned instructions after insns[n] if they follow it without
    a gap -- the same delay-slot adjacency check as tools/sharcflow.py's
    find_sites() (its slots()/after() are closures, not exported; see this
    file's proposed-spec note in the build report)."""
    out = []
    for k in (1, 2):
        if n + k >= len(insns):
            return None
        prev_off, prev = insns[n + k - 1]
        if prev_off + prev.length_bytes != insns[n + k][0]:
            return None
        out.append(insns[n + k])
    return out


def _after_sw(base_sw, pair):
    off, insn = pair[1]
    return base_sw + (off + insn.length_bytes) // 2


def git_commit():
    try:
        return subprocess.check_output(
            ["git", "rev-parse", "--short", "HEAD"], cwd=_here, stderr=subprocess.DEVNULL,
        ).decode().strip()
    except Exception:
        return None


def open_db(path):
    out_dir = os.path.dirname(os.path.abspath(path))
    if out_dir:
        os.makedirs(out_dir, exist_ok=True)
    if os.path.exists(path):
        os.remove(path)
    db = sqlite3.connect(path)
    db.executescript(SCHEMA)
    return db


def read_meta(path):
    """-> {key: value} from an existing DB's meta table, or {} if the file is
    absent, empty, or not a sharcdb database."""
    if not os.path.exists(path):
        return {}
    try:
        db = sqlite3.connect(path)
        try:
            return dict(db.execute("SELECT key, value FROM meta").fetchall())
        finally:
            db.close()
    except sqlite3.DatabaseError:
        return {}


def _fill_database(db, name, sha, blob_path, code_blocks, min_depth, ctx):
    data = ctx["data"]
    mem = ctx["mem"]
    functions = ctx["functions"]
    analyzed = ctx["analyzed"]
    blocks_by_idx = ctx["blocks_by_idx"]

    meta_rows = [
        (name, "image", name),
        (name, "image_sha256", sha),
        (name, "db_version", str(DB_VERSION)),
        (name, "tool_commit", git_commit() or ""),
        (name, "build_time", time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())),
        (name, "blob_path", os.path.abspath(blob_path)),
        (name, "min_depth", str(min_depth)),
        (name, "blocks", json.dumps(list(code_blocks))),
    ]
    db.executemany("INSERT INTO meta VALUES (?,?,?)", meta_rows)

    block_rows = []
    for b in ctx["blocks"]:
        idx = b["index"]
        if b["fill"]:
            kind = "fill"
        elif idx in code_blocks:
            kind = "code"
        else:
            kind = "data"
        base_sw = analyzed[idx]["base_sw"] if idx in analyzed else sharcinv.sw_base_for_target(b["target_address"])
        block_rows.append((name, idx, b["target_address"], b["byte_count"], kind, base_sw))
    db.executemany("INSERT INTO blocks VALUES (?,?,?,?,?,?)", block_rows)

    entry_to_fn = {}
    func_rows = []
    for fn in functions:
        entry_to_fn.setdefault(fn["entry"], fn)
        func_rows.append((
            name, fn["entry"], fn["exit"], fn["n_insns"], fn["block"],
            "FUN_%06x" % fn["entry"], int(not fn["has_no_static_caller"]),
            int(fn["is_leaf"]), fn["label"], fn["entry_kind"],
        ))
    db.executemany("INSERT INTO functions VALUES (?,?,?,?,?,?,?,?,?,?)", func_rows)

    block_spans = collections.defaultdict(list)
    for fn in functions:
        block_spans[fn["block"]].append((fn["entry"], fn["exit"]))
    for spans in block_spans.values():
        spans.sort()

    def owner_of(block_idx, sw):
        spans = block_spans.get(block_idx)
        if not spans:
            return None
        entries = [e for e, _ in spans]
        i = bisect.bisect_right(entries, sw) - 1
        if i < 0:
            return None
        entry, exit_ = spans[i]
        return entry if entry <= sw < exit_ else None

    def resolve_target_function(to_sw, from_block):
        """The function owning to_sw: an exact entry match in any block
        (how nearly every call/jump target resolves), else the containing
        function in the SAME block as the jump (covers an intra-block back-
        edge into the middle of a function). A cross-block jump into the
        middle of another block's function is not resolved (None) -- see
        the module docstring."""
        if to_sw is None:
            return None
        fn = entry_to_fn.get(to_sw)
        if fn is not None:
            return fn["entry"]
        return owner_of(from_block, to_sw)

    code_spans_sw = [
        (analyzed[i]["base_sw"], analyzed[i]["base_sw"] + blocks_by_idx[i]["payload_len"] // 2)
        for i in code_blocks if i in analyzed
    ]
    code_spans_byte = [
        (blocks_by_idx[i]["target_address"], blocks_by_idx[i]["target_address"] + blocks_by_idx[i]["byte_count"])
        for i in code_blocks
    ]

    insn_rows, edge_rows, literal_rows, mem_rows, hash_rows = [], [], [], [], []

    for idx in code_blocks:
        block = analyzed.get(idx)
        if block is None:
            continue
        b = blocks_by_idx[idx]
        base_sw = block["base_sw"]
        payload = data[b["payload_offset"]: b["payload_offset"] + b["payload_len"]]

        table = sharcimm.decode_all(payload)
        depth = sharcimm.depths(table, len(payload))
        sweep = sharcimm.sweep_offsets(table, len(payload))
        aligned_set = {off for off in sweep if depth.get(off, 0) >= min_depth}
        for off in sorted(table):
            insn = table[off]
            sw = base_sw + off // 2
            is_aligned = off in aligned_set
            f = sharcinv.merge_fields(insn.fields)
            mnemonic = None
            if is_aligned:
                mnemonic, _notes, _gap = sharcfn.render_instruction(
                    sw, insn, mem, set(), collections.Counter(), [])
            fn_sw = owner_of(idx, sw) if is_aligned else None
            insn_rows.append((
                name, sw, idx, insn.length_bytes,
                None if insn.raw is None else "%x" % insn.raw,
                insn.type_name, json.dumps(insn.fields, sort_keys=True),
                f.get("cond"), mnemonic, int(is_aligned), insn.kind, fn_sw,
            ))

        sites = block["sites"]
        aligned_insns = block["insns"]
        return_sws = {r["sw"] for r in sites["returns"]}
        linked_indirect_sws = {c["sw"] for c in sites["indirect_calls"]}

        for c in sites["calls"]:
            edge_rows.append((
                name, c["sw"], c["target"], "call", int(bool(c["delayed"])), c["cond"],
                owner_of(idx, c["sw"]), resolve_target_function(c["target"], idx),
                "linked" if c.get("linked") else None,
            ))
        for r in sites["returns"]:
            edge_rows.append((
                name, r["sw"], None, "return", 1, None, owner_of(idx, r["sw"]), None,
                ("after=0x%x" % r["after"]) if r["after"] is not None else None,
            ))

        for n, (off, insn) in enumerate(aligned_insns):
            t = insn.type_name
            f = sharcinv.merge_fields(insn.fields)
            sw = base_sw + off // 2

            if t in ("8a_abs", "8a_rel", "9a_rel", "9b_rel") and f.get("b") == 0:
                # Type9a_abs/9b_abs have no 'addr' field at all (only
                # pmi/pmm): they are indirect through PM(I,M), same family
                # as 9b_abs's return jump, not a direct/pcrel target -- see
                # the indirect branch below. 9a_rel/9b_rel and 8a_rel all
                # carry a real 'reladdr' (PRM Type9); only 8a_abs carries a
                # real 'addr'.
                cond = f.get("cond")
                target = f.get("addr") if t == "8a_abs" else \
                    sharcflow.pcrel_target(sw, f.get("reladdr", 0))
                delayed = f.get("j")
                conditional = cond is not None and cond != sharcfn.ALWAYS_TRUE_COND
                kind = "cond_jump" if conditional else "jump"
                edge_rows.append((
                    name, sw, target, kind, None if delayed is None else int(bool(delayed)),
                    cond, owner_of(idx, sw), resolve_target_function(target, idx), None,
                ))
                if conditional:
                    fallthrough = None
                    if delayed:
                        pair = _slots(aligned_insns, n)
                        if pair:
                            fallthrough = _after_sw(base_sw, pair)
                    else:
                        fallthrough = base_sw + (off + insn.length_bytes) // 2
                    if fallthrough is not None:
                        edge_rows.append((
                            name, sw, fallthrough, "fallthrough",
                            int(bool(delayed)) if delayed is not None else None, cond,
                            owner_of(idx, sw), resolve_target_function(fallthrough, idx),
                            "not-taken path of the conditional jump at this sw",
                        ))

            elif t in ("9a_abs", "9b_abs") and sw not in return_sws:
                # Indirect jump/call through PM(Ipmi, Mpmm) -- no address
                # field exists on either form (see the comment above); a
                # 9a_abs is otherwise identical to a return-eligible 9b_abs
                # but also carries a parallel compute field.
                note = "PM(I%s, M%s)" % (f.get("pmi"), f.get("pmm"))
                note += (" -- indirect call site (push+store in delay slots)"
                         if sw in linked_indirect_sws else " -- indirect jump")
                edge_rows.append((
                    name, sw, None, "indirect", 1, f.get("cond"), owner_of(idx, sw), None, note,
                ))

            literal = extract_literal(t, f)
            if literal is not None:
                value, dest = literal
                in_code, in_data = classify_literal_range(value, code_spans_sw, code_spans_byte, mem)
                literal_rows.append((name, sw, value, t, dest, int(in_code), int(in_data)))

            for row in extract_mem_access(t, f):
                mem_rows.append((name, sw) + row)

        for fn in functions:
            if fn["block"] != idx:
                continue
            fn_insns = sharcinv.instructions_in(block, fn["entry"], fn["exit"])
            exact_bytes, reloc_bytes, n_hashed = bytearray(), bytearray(), 0
            for _sw, insn in fn_insns:
                if insn.raw is None or not insn.length_bytes:
                    continue
                exact_bytes += insn.raw.to_bytes(insn.length_bytes, "big")
                reloc_bytes += mask_relocatable(insn).to_bytes(insn.length_bytes, "big")
                n_hashed += 1
            hash_rows.append((
                name, fn["entry"], hashlib.sha256(bytes(exact_bytes)).hexdigest(),
                hashlib.sha256(bytes(reloc_bytes)).hexdigest(), n_hashed, len(exact_bytes),
            ))

    db.executemany("INSERT INTO insn VALUES (?,?,?,?,?,?,?,?,?,?,?,?)", insn_rows)
    db.executemany("INSERT INTO edges VALUES (?,?,?,?,?,?,?,?,?)", edge_rows)
    db.executemany("INSERT INTO literals VALUES (?,?,?,?,?,?,?)", literal_rows)
    db.executemany("INSERT INTO mem_access VALUES (?,?,?,?,?,?,?,?,?,?)", mem_rows)
    db.executemany("INSERT INTO func_hash VALUES (?,?,?,?,?,?)", hash_rows)

    return {
        "n_insn": len(insn_rows), "n_functions": len(func_rows), "n_edges": len(edge_rows),
        "n_literals": len(literal_rows), "n_mem_access": len(mem_rows), "n_func_hash": len(hash_rows),
    }


def build_database(blob_path, out_path, name=None, min_depth=8, blocks=None, force=False):
    """Build (or skip, if the sha256 and DB_VERSION already match) one
    image's database at out_path. Returns a stats dict."""
    t0 = time.time()
    sha = sharcfn.sha256_of(blob_path)
    if name is None:
        name = os.path.basename(os.path.dirname(os.path.abspath(blob_path))) or \
            os.path.splitext(os.path.basename(blob_path))[0]
    code_blocks = tuple(blocks) if blocks is not None else KNOWN_CODE_BLOCKS.get(sha)
    if code_blocks is None:
        raise SystemExit(
            "sharcdb: no known code-block list for image sha256 %s (%s); pass --blocks to override"
            % (sha, blob_path)
        )
    if not force and os.path.exists(out_path):
        existing = read_meta(out_path)
        if existing.get("image_sha256") == sha and existing.get("db_version") == str(DB_VERSION):
            return {
                "name": name, "path": out_path, "skipped": True,
                "seconds": time.time() - t0, "size": os.path.getsize(out_path),
            }

    ctx = sharcfn.load_context(blob_path, code_blocks, min_depth)
    db = open_db(out_path)
    try:
        counts = _fill_database(db, name, sha, blob_path, code_blocks, min_depth, ctx)
        db.commit()
    finally:
        db.close()
    return {
        "name": name, "path": out_path, "skipped": False,
        "seconds": time.time() - t0, "size": os.path.getsize(out_path), **counts,
    }


def _build_one_cli(blob, out, name, min_depth, blocks, force):
    try:
        return build_database(blob, out, name=name, min_depth=min_depth, blocks=blocks, force=force)
    except Exception as exc:
        return {"name": name, "path": out, "skipped": False, "seconds": 0, "size": 0, "error": str(exc)}


def _int_list(text):
    return tuple(int(x, 0) for x in text.split(","))


def cmd_build(args):
    jobs = []
    for blob in args.blobs:
        single = len(args.blobs) == 1
        name = args.name if (args.name and single) else \
            (os.path.basename(os.path.dirname(os.path.abspath(blob))) or
             os.path.splitext(os.path.basename(blob))[0])
        out = args.out if (args.out and single) else os.path.join(args.out_dir, name + ".sqlite")
        jobs.append((blob, out, name))

    results = []
    if len(jobs) == 1:
        blob, out, name = jobs[0]
        results.append(_build_one_cli(blob, out, name, args.min_depth, args.blocks, args.force))
    else:
        import concurrent.futures

        workers = args.jobs or min(len(jobs), os.cpu_count() or 1)
        with concurrent.futures.ProcessPoolExecutor(max_workers=workers) as ex:
            futs = [
                ex.submit(_build_one_cli, blob, out, name, args.min_depth, args.blocks, args.force)
                for blob, out, name in jobs
            ]
            results = [fut.result() for fut in futs]

    ok = True
    for r in results:
        if r.get("error"):
            ok = False
            print("FAILED  %-16s %s" % (r["name"], r["error"]))
        elif r["skipped"]:
            print("%-16s SKIPPED (up to date)  %s  %.1fs" % (r["name"], r["path"], r["seconds"]))
        else:
            print(
                "%-16s built  %s  %.1fs  %.1f KB  insn=%d functions=%d edges=%d literals=%d "
                "mem_access=%d func_hash=%d"
                % (r["name"], r["path"], r["seconds"], r["size"] / 1024, r.get("n_insn", 0),
                   r.get("n_functions", 0), r.get("n_edges", 0), r.get("n_literals", 0),
                   r.get("n_mem_access", 0), r.get("n_func_hash", 0))
            )
    return 0 if ok else 1


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = ap.add_subparsers(dest="cmd", required=True)

    b = sub.add_parser("build", help="build one or more image databases")
    b.add_argument("blobs", nargs="+", help="section_7_*.bin path(s)")
    b.add_argument("--out", help="output .sqlite path (only meaningful with a single blob)")
    b.add_argument("--out-dir", default="out/sharcdb", help="directory for multi-blob builds")
    b.add_argument("--name", help="image name (only meaningful with a single blob); "
                                  "default is the blob's parent directory name")
    b.add_argument("--min-depth", type=int, default=8)
    b.add_argument("--blocks", type=_int_list, help="override the code-block index list (comma-separated)")
    b.add_argument("--force", action="store_true", help="rebuild even if sha256+DB_VERSION already match")
    b.add_argument("--jobs", type=int, default=0,
                   help="parallel worker processes for multiple blobs (default: one per blob)")
    args = ap.parse_args(argv)

    if args.cmd == "build":
        return cmd_build(args)
    return 1


if __name__ == "__main__":
    sys.exit(main())
