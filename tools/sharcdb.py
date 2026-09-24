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
import re
import shutil
import sqlite3
import subprocess
import sys
import time

_here = os.path.dirname(os.path.abspath(__file__))
if _here not in sys.path:
    sys.path.insert(0, _here)

import networkx as nx  # noqa: E402
import sharc_visa_tables as visa_tables  # noqa: E402
import sharcflow  # noqa: E402
import sharcfn  # noqa: E402
import sharcimm  # noqa: E402
import sharcinv  # noqa: E402
import sharcldr  # noqa: E402
from sharc_trace import ACCESS_WIDTHS  # noqa: E402

DB_VERSION = 10

# Bump DB_VERSION whenever the schema or the semantics of an existing column
# change, so build_database()'s skip-rebuild check (sha256 + DB_VERSION) does
# the right thing on the next run.
#
# --- v10: MULT regdef/reguse naming, Type4b/4d mem_access width ------------
#
# _compute_regdef_reguse()'s MULT branch used classify_compute()'s is_float,
# which is wrong for every MULT opcode except 0x30 ("FN = FX*FY"): bit 3 of
# the mulop opcode is MOD1's Integer/Fraction result-format bit, not a
# float/fixed switch (PRM Table 18-7). regdef/reguse rows for other MULT
# opcodes (e.g. 0x48, "UUF"-style fixed-point multiplies) previously named
# F-registers where the manual calls for R-registers.
#
# extract_mem_access() also mislabeled Type4b/4d's l/x/w-encoded byte (bw)
# and short-word (sw) accesses as "word"/"long" (that fallback is only
# correct for forms whose lone "l" bit really means long-word); see
# _immoff_width() and sharc_trace.ACCESS_WIDTHS.
#
# --- v7: interrupt-vector roots ---------------------------------------------
#
# CMMR_SYSCTL.IIVT is set at boot (confirmed by the byte-exact slot targets
# below), so the IVT lives in L1 block 0's 48-bit (PM) space at the fixed
# short-word address IVT_SW -- sc58x-2158x-prm pp.226-231 ("L1 Memory
# Interface": "Block 0 has 256 addresses reserved for internal interrupt
# vector table"; the exact numeric address isn't in either public manual,
# "see the memory-map in the product specific data sheet") and
# sharc-plus-prm Table 4-47 (IIVT=1 -> L1) / Table 4-46 (the 32 interrupt
# names IVT_VECTOR_NAMES below reuses verbatim). _detect_interrupt_vector_roots()
# reads the 32x24-byte slot table there and adds a `roots` row per slot whose
# 4 instructions aren't the all-empty pattern, for every image (DT2 and DN2
# alike -- the location and slot layout were checked byte-for-byte identical
# across all four .sqlite images this repo builds). Two further ISR dispatch
# tables (IVT_DISPATCH_TABLES, an ASTATX-indexed and a SEC-id-indexed one)
# were checked the same way and found to sit entirely inside a boot-stream
# FILL block on DT2 1.16/1.15C (argument=0, i.e. the ROM ships them zeroed) --
# _detect_dispatch_table_roots() detects that case and adds no root rather
# than reporting an all-zero table as "no code pointers" by coincidence.
#
# --- v6: `ptr` -- constant-pointer propagation ------------------------------
#
# mem_access.abs_address (and dataref's resolved_offset) only ever resolved a
# direct absolute form or a same-block literal-then-immediate-offset pair;
# every other computed address -- a literal loaded into an I register three
# blocks earlier, a post-modify walk by a hardware-reset M constant, a
# spilled/reloaded call argument, a DM(I6+k) frame slot -- stayed invisible
# to "who writes address X". analyze_image() now also runs a small,
# deterministic forward constant-propagation per function (see
# _build_ptr_rows()'s own module note, just above it) and records every
# memory access it can pin to a known base register value or a fully known
# address in the new `ptr(image, sw, base_reg, base_value, address,
# direction, width)` table; tools/sharc.py's Image.writers()/readers() query
# it. analyze()-only (no re-decode) is enough to backfill this on an
# existing database -- see analyze_image()'s own CREATE TABLE IF NOT EXISTS.
#
# --- v5: reach now seeds from every root, not just function entries --------
#
# _detect_reach() used to drop a root whose address wasn't inside any
# function's [entry_sw, end_sw) span. The only root kind that can happen to
# (loader_entry -- see _detect_roots) was therefore silently missing from
# `reach` on every image: DT2 1.16 and DN2 1.11 both put their loader entry
# inside a `data` block immediately followed by `fill` padding, with no
# bblock/succ path bridging that padding into the first application
# function (checked directly against both images' bblocks/succ rows). See
# _detect_reach()'s own docstring for the fallback this version adds.
#
# --- ColdFire images (import_ghidra) -----------------------------------------
#
# build_ghidra_database() fills this SAME schema from a tools/ghidradump.py
# dump directory (manifest.json/xrefs.sqlite/disasm/decomp) instead of
# decoding a SHARC+ blob, so every image -- SHARC or ColdFire -- is one
# `sharcdb.load()`/`sharc.py` API. meta.cpu is 'sharc' or 'coldfire';
# sharc.Image.trace() checks it and raises for a ColdFire image (no SHARC
# symbolic tracer for m68k). Column mapping, one column at a time:
#   - insn.sw / functions.entry_sw,end_sw / edges.from_sw,to_sw / dataref.sw:
#     the ColdFire BYTE address (there is no "short word" unit on this CPU;
#     the column is reused as-is rather than adding a parallel `addr` column
#     everywhere it's already keyed on `sw`).
#   - insn.raw: hex bytes exactly as tools/ghidradump.py's disasm_line() wrote
#     them (concatenated 16-bit words, no spaces); insn.mnemonic: its
#     rendered text (Ghidra's CodeUnitFormat string, not ours); insn.block,
#     .form, .fields, .cond, .confidence: NULL (no per-instruction decode of
#     our own to fill them). insn.aligned is always 1 (Ghidra has already
#     disassembled every byte; there is no sweep/depth ambiguity to record).
#   - functions.block/.label/.entry_kind: NULL (no loader block, no SHARC
#     label pass); functions.has_static_caller/.is_leaf: from the `edges`
#     this same import wrote (any incoming/outgoing call edge), same
#     definition as the SHARC build.
#   - edges: one row per tools/ghidradump.py `calls` row (already function-
#     level: a plain intra-function jump is dropped by that tool's own
#     collect(), never a reference at all). kind is the Ghidra reference type
#     with its UNCONDITIONAL_/COMPUTED_ prefix stripped down to this schema's
#     call/jump/cond_jump vocabulary (COMPUTED_* rounds down to the same kind
#     as its unconditional counterpart -- Ghidra had already resolved the
#     target, so there is no "unknown target" left the way SHARC's own
#     'indirect' kind means); the ORIGINAL Ghidra kind string (e.g.
#     "COMPUTED_CALL") is kept in `note` so that distinction isn't lost.
#     `fallthrough`/`return`/`indirect` never appear: a Ghidra dump has no
#     basic-block CFG to derive them from (see `bblocks`/`succ` below).
#   - dataref: one row per tools/ghidradump.py `data_refs` row; value=to_addr,
#     sw=site, form='ghidra', role=the Ghidra kind lowercased
#     (read/write/read_write/data) -- not a SHARC form label (14a/17a/...),
#     since nothing here decoded the instruction that owns it.
#   - decomp(image, function_sw, c): decompiled C read verbatim from
#     tools/ghidradump.py's own decomp/*.c (empty for a SHARC image, which
#     has no decompiler in this pipeline: Ghidra-only table, PK (image,
#     function_sw)).
#   - blocks, literals, mem_access, regdef, reguse, func_hash, bblocks, succ,
#     idom, loops: left EMPTY for a ColdFire image -- known gaps, not
#     silently-wrong data:
#       * blocks: Ghidra's own memory blocks (ram/MCF5441X_* MMIO regions)
#         don't fit this table's loader-block shape (idx/target_address/
#         byte_count); query xrefs.sqlite's own `blocks` table instead.
#       * literals/mem_access/regdef/reguse: instruction-decode facts this
#         import never computes (no re-disassembly of the ColdFire bytes;
#         Ghidra's own P-code has the same information but importing it is
#         future work, not done here).
#       * func_hash: relocation-tolerant hashing needs a decoder's own field
#         layout (SHARC's mask_relocatable); no ColdFire equivalent here, so
#         no cross-image function matching for ColdFire images yet.
#       * bblocks/succ/idom/loops: NO per-instruction CFG survives a Ghidra
#         dump at all -- tools/ghidradump.py's collect() only records a
#         reference that is a CALL or a JUMP to a DIFFERENT function's entry
#         (see its build_rows()); an ordinary intra-function branch is never
#         turned into a reference, so this import cannot reconstruct basic
#         blocks, successors, dominators or loops. `functions`/`edges`
#         (function-level call graph) are still complete; analyze_image()'s
#         idom/loop pass simply sees no bblocks rows for this image and
#         produces no rows for it either (not an error).
#   - roots: the SHARC-only detectors (loader_entry, code_pointer_array,
#     rtos_task) are already gated on `mem`/`blocks`/the `literals` table,
#     which build_ghidra_database() never supplies, so they emit nothing for
#     a ColdFire image without a separate code path. dataref_code_pointer and
#     no_static_entry are already CPU-generic (pure SQL over
#     dataref/edges/functions) and fire the same way for both CPUs. The one
#     ColdFire-specific ADDITION is `vector_entry`: any function whose Ghidra
#     name matches `vector_<N>_handler` (an exception/interrupt vector table
#     entry Ghidra already named) -- a SHARC function is always named
#     FUN_%06x, so this pattern never matches there either, and needed no
#     cpu flag threaded through _detect_roots.
#   - reach/callgraph: fully reused (they only read edges/functions, both
#     CPU-generic tables).

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

-- Decompiled C per function, verbatim from tools/ghidradump.py's decomp/*.c
-- (see the DB_VERSION comment's ColdFire-images section above). Empty for a
-- SHARC image -- no decompiler in this pipeline.
CREATE TABLE decomp (image TEXT, function_sw INTEGER, c TEXT, PRIMARY KEY (image, function_sw));

-- Basic blocks: a maximal straight-line run of aligned instructions, ended
-- by any branch/call/return/indirect site (after its delay slots, when
-- delayed -- see _gather_terminators) or by a hardware DO..UNTIL loop's
-- last body instruction (a terminator despite not being a branch itself),
-- and started by a function entry, any edge's target, or the instruction
-- after such a terminator. end_sw is exclusive (one past the last member
-- instruction), the same convention functions.end_sw already uses. block is
-- the loader/vendor-memory block index (blocks.idx) it was found scanning
-- -- unrelated to "basic block".
CREATE TABLE bblocks (
    image TEXT, start_sw INTEGER, end_sw INTEGER, function_sw INTEGER, n_insns INTEGER,
    block INTEGER,
    PRIMARY KEY (image, start_sw));

-- Basic-block control-flow successors. kind: fallthrough (plain, non-branch
-- block-to-block continuation -- present so a recursive CTE can walk
-- straight-line code, not just branch sites), jump, cond_taken,
-- cond_not_taken, call_return (the block after a call's delay slots --
-- the call edge itself, into the callee, stays in `edges`), loop_back (a
-- hardware DO..UNTIL's body-end -> body-start back edge), loop_exit,
-- return, indirect. to_block is NULL for return/indirect (no static
-- target) and for any edge whose target this pass never turned into a
-- bblocks row (a cross-image or unscanned-region jump); such a row is
-- still recorded so a "does every block exit account for its kind" audit
-- doesn't have to fall back to `edges`.
CREATE TABLE succ (
    image TEXT, from_block INTEGER, to_block INTEGER, kind TEXT);

-- Every constant an instruction materialises as an address: a 17a/17b/16a/
-- 16b/18a/19a*/15a/14a literal (tools/sharcdb.py's own `literals` table,
-- given a role here) plus any 15a/14a/14d absolute mem_access address (kept
-- separate from `literals` since 14d isn't a LITERAL_FORMS entry). role:
-- i_reg_base (a literal loaded directly into an I register -- a PM/DM table
-- base), abs_load/abs_store (a direct absolute address, from mem_access's
-- own direction so this never inherits a mnemonic-only bug -- see
-- tools/sharcfn.py's Type3c note), literal (any other literal-carrying
-- form: 16a/16b, 18a, 17a/17b into a non-I register, 19a* -- a modify
-- delta, not an address), resolved_offset (an IMMOFF form's I+immediate
-- address, resolved when the I register was just given a literal by a
-- 17a/17b in the SAME basic block -- see bblocks/succ above).
CREATE TABLE dataref (
    image TEXT, value INTEGER, sw INTEGER, form TEXT, role TEXT);

-- Register definitions and uses, from the typed decode fields (not from
-- mnemonic text -- see tools/sharcfn.py's Type3c direction bug in the task
-- report). kind: compute (an ALU/MULT/SHIFT/MULTIFN/short-compute result),
-- mem_load (a data register loaded from memory), literal (a ureg set
-- directly from an immediate, or a hardware-loop LCNTR literal), move (a
-- 5a/5b_move ureg-to-ureg transfer, or a 12a_ureg loop count source), swap
-- (5a/5b_swap), dag_modify (an I register written by address-modify
-- semantics: 19a*'s Is XOR Idis dest, 7a's Is XOR Idis dest with an
-- M-register delta, 16a/16b's mandatory I+=M, or a 3a/3b/3d/4a/4b/4d
-- access with u=1), unknown (a register-touching form
-- family this pass could not fully resolve -- see the build report's
-- per-form unknown counts; never silently omitted).
CREATE TABLE regdef (
    image TEXT, sw INTEGER, reg TEXT, kind TEXT);

CREATE TABLE reguse (
    image TEXT, sw INTEGER, reg TEXT);

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
CREATE INDEX bblocks_function ON bblocks(image, function_sw);
CREATE INDEX bblocks_end ON bblocks(image, end_sw);
CREATE INDEX succ_from ON succ(image, from_block);
CREATE INDEX succ_to ON succ(image, to_block);
CREATE INDEX dataref_value ON dataref(image, value);
CREATE INDEX dataref_sw ON dataref(image, sw);
CREATE INDEX regdef_reg ON regdef(image, reg);
CREATE INDEX regdef_sw ON regdef(image, sw);
CREATE INDEX reguse_reg ON reguse(image, reg);
CREATE INDEX reguse_sw ON reguse(image, sw);

-- --- analysis tables (tools/sharcdb.py's analyze_image(), DB_VERSION 3) -----
--
-- Built once per image from edges/succ/dataref/literals with networkx,
-- reusing nothing decoded here: see analyze_image()'s docstring for the
-- detection heuristics (a root's kind explains how it was found).

-- kind: loader_entry (the scanned code region's own BFLAG_FIRST target),
-- rtos_task (an R4 value passed to a call site shaped like the R12/R8/R4
-- task-create idiom -- note carries the helper function and call site),
-- dataref_code_pointer (a literal/table value from `dataref` that lands
-- inside a scanned function), code_pointer_array (one resolved entry of a
-- consecutive in-code-pointer run found at an i_reg_base table address --
-- note carries the table base and index), no_static_entry (a function with
-- no edges of any kind pointing at its entry -- the same set `unentered`
-- clusters), interrupt_vector (SHARC only: one populated slot of the L1
-- hardware IVT at IVT_SW -- note carries 'slot N NAME', NAME from
-- IVT_VECTOR_NAMES/sharc-plus-prm Table 4-46; see the DB_VERSION v7 comment
-- above), vector_entry (ColdFire only: a function Ghidra itself named
-- vector_<N>_handler -- an exception/interrupt vector table entry; note
-- carries that name).
CREATE TABLE roots (image TEXT, sw INTEGER, kind TEXT, note TEXT);
CREATE INDEX roots_image ON roots(image);

-- Functions reachable from each root through call/jump/cond_jump edges
-- (function-level, from the `edges` table's own from_function/to_function).
-- depth counts CALL edges only -- a tail/cross-function JUMP costs nothing,
-- so this is shortest CALL depth, not instruction distance.
CREATE TABLE reach (
    image TEXT, root_sw INTEGER, function_sw INTEGER, depth INTEGER,
    PRIMARY KEY (image, root_sw, function_sw));
CREATE INDEX reach_function ON reach(image, function_sw);

-- Call-graph closure per function (CALL edges only). is_recursive is a
-- self-loop or membership in a call-graph SCC of size > 1.
CREATE TABLE callgraph (
    image TEXT, function_sw INTEGER, n_callers INTEGER, n_callees INTEGER,
    n_transitive_callees INTEGER, max_depth INTEGER, is_recursive INTEGER,
    PRIMARY KEY (image, function_sw));

-- Immediate dominators of a function's own basic-block CFG (bblocks/succ
-- restricted to that function), from networkx.immediate_dominators. The
-- entry block itself has no row (its idom is conventionally itself).
CREATE TABLE idom (
    image TEXT, function_sw INTEGER, block INTEGER, idom_block INTEGER,
    PRIMARY KEY (image, function_sw, block));

-- Natural loops from back edges (succ edge whose target dominates its
-- source) plus hardware DO..UNTIL loops (succ's own loop_back kind), merged
-- per header when several back edges share one. depth is loop nesting by
-- body containment (1 = outermost).
CREATE TABLE loops (
    image TEXT, function_sw INTEGER, header_block INTEGER, n_blocks INTEGER,
    kind TEXT, depth INTEGER,
    PRIMARY KEY (image, function_sw, header_block));

-- Functions with no static entry (the roots.kind='no_static_entry' set),
-- grouped by the likely dispatcher: the code-pointer array/literal that
-- names them as a target, else the nearest preceding function with an
-- indirect site, else 'none'.
CREATE TABLE unentered (
    image TEXT, function_sw INTEGER, cluster TEXT,
    PRIMARY KEY (image, function_sw));

-- Constant-pointer propagation results (_build_ptr_rows(), DB_VERSION 6):
-- every memory access whose effective address this pass's forward constant
-- propagation resolves to a known base I register value or a fully known
-- address. base_reg/base_value are NULL/NULL for a pure absolute 14a/14d
-- access (no base register at all); address is NULL when only the base is
-- known (e.g. an indexed I,M form whose M modifier isn't a tracked
-- constant) -- tools/sharc.py's Image.writers()/readers() report that case
-- separately ('base_only') rather than silently dropping it.
CREATE TABLE ptr (
    image TEXT, sw INTEGER, base_reg TEXT, base_value INTEGER, address INTEGER,
    direction TEXT, width TEXT);
CREATE INDEX ptr_address ON ptr(image, address);
CREATE INDEX ptr_base_value ON ptr(image, base_value);
CREATE INDEX ptr_sw ON ptr(image, sw);
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


# Type4b/4d (decode_table.json: l/w/x at bits 18/17/16, no "ex" bit) share
# Type3b's ACCESS/BH/BHSE l/x/w encode table verbatim (PRM pp.13-31/13-32/
# 13-34): the "long if l else word" fallback used for every other
# DIRECT_MEM_FORMS/IMMOFF_MEM_FORMS member (whose lone "l" bit really does
# mean a plain long-word/Ureg-pair access, e.g. 4a/15a/14a/15b) mislabels a
# byte (bw, l=0) access as "word" and a short-word (sw, l=1) access as
# "long" for these two forms.
_LXW_WIDTH_FORMS = {"4b", "4d"}


def _immoff_width(insn_type: str, f: dict):
    if insn_type in _LXW_WIDTH_FORMS:
        fields = (f.get("l", 0), f.get("x", 0), f.get("w", 0))
        return ACCESS_WIDTHS.get(fields, "unknown(l=%d,x=%d,w=%d)" % fields)
    return "long" if f.get("l") else "word"


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
                     _immoff_width(insn_type, f), None))
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


# --- basic blocks and their control-flow successors -------------------------
#
# Two-phase build across every scanned loader code block: _gather_terminators
# finds, per block, every site that ends a basic block (a branch/call/return/
# indirect, after its delay slots when delayed, or a hardware DO..UNTIL
# loop's last body instruction) and the successor edges it implies; then
# _build_blocks_and_succ collects every terminator's target and "after"
# address into one global leader set (so a target in a DIFFERENT loader code
# block still gets a leader there) before splitting each block's aligned
# instructions into basic blocks against that set. See the module's SCHEMA
# comment on `bblocks`/`succ` for the kind vocabulary.

def _gather_terminators(base_sw, aligned_insns, sites):
    """(seq, sw_index, terminators, extra_leaders) for one loader code
    block's aligned instructions.

    seq: [(sw, off, insn), ...] in address order.
    sw_index: {sw: position in seq}.
    terminators: {sw: {'end_after': Optional[int], 'succs': [(kind, to_sw), ...]}}
    for every site that ends a basic block there.
    extra_leaders: a DO/DO-UNTIL loop's own body-start sw, added even when
    the loop's end address does not resolve to an aligned instruction here
    (e.g. the loop body is scanned but its last instruction fell just short
    of --min-depth)."""
    seq = [(base_sw + off // 2, off, insn) for off, insn in aligned_insns]
    sw_index = {sw: i for i, (sw, _off, _insn) in enumerate(seq)}
    terminators = {}
    extra_leaders = set()

    def add(sw, end_after, succs):
        entry = terminators.setdefault(sw, {"end_after": None, "succs": []})
        entry["succs"].extend(succs)
        if end_after is not None:
            entry["end_after"] = end_after

    return_sws = {r["sw"] for r in sites["returns"]}
    linked_indirect_sws = {c["sw"] for c in sites["indirect_calls"]}

    # Calls and returns already carry their post-delay-slot address
    # (tools/sharcflow.py's find_sites 'returns_to'/'after'); the call
    # target itself is a `edges` table concern (kind='call'), not `succ` --
    # here a call only contributes the call_return edge back into its own
    # function.
    for c in sites["calls"]:
        add(c["sw"], c.get("returns_to"), [("call_return", c.get("returns_to"))])
    for ic in sites["indirect_calls"]:
        add(ic["sw"], ic.get("returns_to"), [("call_return", ic.get("returns_to"))])
    for r in sites["returns"]:
        add(r["sw"], r.get("after"), [("return", None)])

    for n, (off, insn) in enumerate(aligned_insns):
        t = insn.type_name
        f = sharcinv.merge_fields(insn.fields)
        sw = base_sw + off // 2

        if t in ("8a_abs", "8a_rel", "9a_rel", "9b_rel") and f.get("b") == 0:
            cond = f.get("cond")
            target = f.get("addr") if t == "8a_abs" else \
                sharcflow.pcrel_target(sw, f.get("reladdr", 0))
            delayed = f.get("j")
            conditional = cond is not None and cond != sharcfn.ALWAYS_TRUE_COND
            if delayed:
                pair = _slots(aligned_insns, n)
                end_after = _after_sw(base_sw, pair) if pair else None
            else:
                end_after = base_sw + (off + insn.length_bytes) // 2
            succs = [(("cond_taken" if conditional else "jump"), target)]
            if conditional and end_after is not None:
                succs.append(("cond_not_taken", end_after))
            add(sw, end_after, succs)

        elif t in ("9a_abs", "9b_abs") and sw not in return_sws and sw not in linked_indirect_sws:
            # A generic indirect jump (not the fixed return word, not an
            # indirect call's push+store idiom): both forms carry their own
            # 'j' bit (decode_table.json), so -- unlike the fixed-pattern
            # return word and the indirect-call idiom, which are only ever
            # matched delayed -- a plain indirect JUMP can be non-delayed
            # too (confirmed on dt2-1.16 at sw 0x1c6579, rendered "JUMP
            # non-delayed target=indirect PM(I4, M5)"); trusting a blanket
            # "always 2 slots" here silently swallowed the following
            # instructions into the wrong basic block.
            delayed = f.get("j")
            if delayed:
                pair = _slots(aligned_insns, n)
                end_after = _after_sw(base_sw, pair) if pair else None
            else:
                end_after = base_sw + (off + insn.length_bytes) // 2
            add(sw, end_after, [("indirect", None)])

        elif t in ("12a_imm", "12a_ureg"):
            # DO addr UNTIL LCE: no delay slots of its own (the hardware
            # loop mechanism, not an instruction-level delayed branch) --
            # the terminator is the loop's LAST body instruction, at
            # end_sw, not this DO instruction. Same end_sw formula as
            # tools/sharcfn.py's render_loop / tools/sharc_trace.py's
            # _start_counted_loop.
            reladdr = f.get("reladdr")
            if reladdr is not None:
                start_sw = sw + (insn.length_bytes or 6) // 2
                extra_leaders.add(start_sw)
                end_sw = sw + sharcfn.sign_extend(reladdr, 23)
                pos = sw_index.get(end_sw)
                if pos is not None:
                    end_insn = seq[pos][2]
                    end_after = end_sw + (end_insn.length_bytes or 0) // 2
                    add(end_sw, end_after, [("loop_back", start_sw), ("loop_exit", end_after)])

    return seq, sw_index, terminators, extra_leaders


def _build_blocks_and_succ(name, code_blocks, analyzed, functions, owner_of):
    """(bblock_rows, succ_rows, leaders_by_idx) for every scanned loader code
    block. leaders_by_idx (idx -> set of basic-block-start sw's in that
    block) is returned for reuse by the dataref pass below, which resets its
    "last literal loaded into I register" tracking at each basic-block
    boundary.

    Only intra-loader-code-block successors are resolved into a to_block: a
    branch whose target this build never disassembled (a different image,
    an unscanned region, or -- rare -- a different loader code block this
    pass has not looked at yet) gets a succ row with a to_block that matches
    no bblocks row, the same "unresolved" shape the existing edges/
    to_function pair already uses; the interprocedural call graph itself
    stays in `edges`, as call_return already only crosses back into the
    caller."""
    per_idx = {}
    global_leaders = set()

    fn_entries_by_idx = collections.defaultdict(set)
    for fn in functions:
        fn_entries_by_idx[fn["block"]].add(fn["entry"])

    for idx in code_blocks:
        block = analyzed.get(idx)
        if block is None:
            continue
        base_sw = block["base_sw"]
        seq, sw_index, terminators, extra_leaders = _gather_terminators(
            base_sw, block["insns"], block["sites"])
        per_idx[idx] = (seq, sw_index, terminators)
        if seq:
            global_leaders.add(seq[0][0])
        global_leaders |= extra_leaders
        global_leaders |= fn_entries_by_idx.get(idx, set())
        for term in terminators.values():
            if term["end_after"] is not None:
                global_leaders.add(term["end_after"])
            for _kind, to_sw in term["succs"]:
                if to_sw is not None:
                    global_leaders.add(to_sw)

    bblock_rows, succ_rows, leaders_by_idx = [], [], {}

    for idx, (seq, sw_index, terminators) in per_idx.items():
        local_leaders = {sw for sw in global_leaders if sw in sw_index}
        if seq:
            local_leaders.add(seq[0][0])
        for i in range(1, len(seq)):
            prev_sw, _prev_off, prev_insn = seq[i - 1]
            cur_sw, _cur_off, _cur_insn = seq[i]
            if prev_sw + (prev_insn.length_bytes or 0) // 2 != cur_sw:
                # A decode gap: force a split so the CFG never silently
                # bridges it.
                local_leaders.add(cur_sw)
        leaders_by_idx[idx] = local_leaders

        blocks, cur_members = [], []
        for item in seq:
            sw = item[0]
            if cur_members and sw in local_leaders:
                blocks.append(cur_members)
                cur_members = []
            cur_members.append(item)
        if cur_members:
            blocks.append(cur_members)

        for members in blocks:
            start_sw = members[0][0]
            last_sw, _last_off, last_insn = members[-1]
            end_sw_excl = last_sw + (last_insn.length_bytes or 0) // 2
            fn_sw = owner_of(idx, start_sw)
            bblock_rows.append((name, start_sw, end_sw_excl, fn_sw, len(members), idx))

            # The block's terminator may not be its LAST member: a delayed
            # branch/call/indirect's own sw is followed by its own delay
            # slots, which stay in this same block (see the module
            # docstring). Search every member, not just the last one --
            # keying off last_sw alone silently relabelled every delayed
            # terminator's real edge (cond_taken/call_return/loop_back/...)
            # as a plain 'fallthrough' to the same (coincidentally correct)
            # address, and dropped it entirely on the rare block whose
            # delay slots pushed end_sw_excl to an address this pass also
            # split on for an unrelated reason.
            term = None
            for member_sw, _off, _insn in members:
                candidate = terminators.get(member_sw)
                if candidate is not None:
                    term = candidate
                    break
            if term is None:
                if end_sw_excl in local_leaders:
                    succ_rows.append((name, start_sw, end_sw_excl, "fallthrough"))
            else:
                for kind, to_sw in term["succs"]:
                    succ_rows.append((name, start_sw, to_sw, kind))

    return bblock_rows, succ_rows, leaders_by_idx


# --- register def/use -------------------------------------------------------
#
# Mirrors tools/sharcfn.py's register-operand bit layouts (Figure 18-1/18-2/
# 18-3, Table 18-11/13/18/19/21) directly from the typed decode fields --
# never from mnemonic text, per the Type3c direction bug in the build
# report -- so the register identity always agrees with what that file's
# renderer would print for the same instruction. unmodelled sub-cases return
# an 'unknown' tag instead of a guessed def/use, per instruction, so the
# build report can count them per form.

def _compute_regdef_reguse(field23):
    """Register defs/uses for a 23-bit parallel compute field (1a/2a/
    2a_short/2b/3a/4a/5a_move/5a_swap/7a/9a_abs/9a_rel/11a). -> (defs:
    [(reg, 'compute')], uses: [reg], unknown: [tag])."""
    defs, uses, unknown = [], [], []
    cu, d = sharcinv.classify_compute(field23)
    if cu is None:
        return defs, uses, unknown
    opcode = d.get("opcode", 0)
    if cu == "ALU":
        is_float = d.get("is_float", False)
        if d.get("is_dual_addsub"):
            rs, ra, rx, ry = ((field23 >> sh) & 0xF for sh in (12, 8, 4, 0))
            Rs, Ra, Rx, Ry = (sharcfn.reg_name(r, is_float) for r in (rs, ra, rx, ry))
            defs += [(Rs, "compute"), (Ra, "compute")]
            uses += [Rx, Ry]
        else:
            name = sharcinv.ALU_OPS.get(opcode)
            rn, rx, ry = ((field23 >> sh) & 0xF for sh in (8, 4, 0))
            Rn, Rx, Ry = (sharcfn.reg_name(r, is_float) for r in (rn, rx, ry))
            defs.append((Rn, "compute"))
            if name in sharcfn.UNARY_ALU_OPS:
                uses.append(Rx)
            else:
                # Binary, or an opcode ALU_OPS doesn't name: Table 18-11's
                # register positions are fixed regardless of which named op
                # this is, so Rn/Rx/Ry are certain even when the opcode
                # isn't; default to binary (both operands used) rather than
                # guess unary.
                uses += [Rx, Ry]
    elif cu == "MULT":
        # classify_compute()'s is_float is only correct for opcode 0x30
        # ("FN = FX*FY"); every other MULT opcode's bit 3 is MOD1's
        # Integer/Fraction result-format bit, not a float/fixed switch --
        # see tools/sharcfn.py's decode_mult for the same fix and citation.
        is_float = opcode == 0x30
        rn, rx, ry = ((field23 >> sh) & 0xF for sh in (8, 4, 0))
        Rn, Rx, Ry = (sharcfn.reg_name(r, is_float) for r in (rn, rx, ry))
        if d.get("housekeeping"):
            unknown.append("mult_housekeeping")
        elif d.get("is_plain_mul"):
            defs.append((Rn, "compute"))
            uses += [Rx, Ry]
        elif d.get("is_mac"):
            defs.append((Rn, "compute"))
            uses += [Rx, Ry, "MR"]
        else:
            unknown.append("mult_other")
    elif cu == "SHIFT":
        rn, rx, ry = ((field23 >> sh) & 0xF for sh in (8, 4, 0))
        defs.append(("R%d" % rn, "compute"))
        uses += ["R%d" % rx, "R%d" % ry]
    elif cu == "MULTIFN":
        top3 = (field23 >> 20) & 7
        is_float = bool(top3 & 1)
        if d.get("is_dual_addsub"):
            # Table 18-18/19: fixed layout, no opcode sub-table -- same
            # register math as tools/sharcfn.py's decode_multifn dual branch.
            rya = (field23 & 3) + 12
            rxa = ((field23 >> 2) & 3) + 8
            rym = ((field23 >> 4) & 3) + 4
            rxm = ((field23 >> 6) & 3) + 0
            ra = (field23 >> 8) & 0xF
            rm = (field23 >> 12) & 0xF
            rs = (field23 >> 16) & 0xF
            Rm, Ra, Rs, Rxm, Rym, Rxa, Rya = (
                sharcfn.reg_name(v, is_float) for v in (rm, ra, rs, rxm, rym, rxa, rya)
            )
            defs += [(Rm, "compute"), (Ra, "compute"), (Rs, "compute")]
            uses += [Rxm, Rym, Rxa, Rya]
        else:
            # The regular MUL+ALU multifunction op's ALU sub-operation is a
            # table lookup (tools/sharcfn.py's MULTIFN_ALU_SYNTAX, PGR Table
            # 12-12) whose operand count varies by row; left unmodelled here
            # rather than guessed.
            unknown.append("multifn_alu_compute")
    else:
        unknown.append("compute_cu_%r" % cu)
    return defs, uses, unknown


def _shortcompute_regdef_reguse(field12):
    """Type 2c (PRM Table 17-2/18-21): RN 7:4 is always the result; the
    unary-Rx opcodes (pass/not/float, and inc/dec -- PRM's "RN = RX +/- 1"
    reads only RX, never RN; see tools/sharcfn.py's render_shortcompute)
    read only RX; every other opcode reads both RN and RX (PRM/
    tools/sharcfn.py's render_shortcompute: "RN = op(RN, RX)")."""
    opcode = (field12 >> 8) & 0xF
    rn, rx = (field12 >> 4) & 0xF, field12 & 0xF
    is_float = opcode in sharcfn.FLOAT_SHORT_OPS
    Rn, Rx = sharcfn.reg_name(rn, is_float), sharcfn.reg_name(rx, is_float)
    defs = [(Rn, "compute")]
    if opcode in sharcfn._UNARY_RX_SHORT_OPS:
        uses = [Rx]
    else:
        uses = [Rn, Rx]
    return defs, uses


def _shiftimm_regdef_reguse(f):
    """6a_mem's parallel ShiftImm sub-instruction (tools/sharcfn.py's
    render_shiftimm): RN 7:4 is the result for every opcode this repo
    models, RX 3:0 is always read; the "or-" variants also read RN as the
    other OR operand; an opcode outside _SHIFTIMM_MNEMONICS (including
    btst, which that table names but whose own branch there is status-only)
    is left unmodelled."""
    defs, uses, unknown = [], [], []
    field = f.get("shiftimm", 0) & 0x7FFFFF
    opcode = (field >> 16) & 0x3F
    rn, rx = (field >> 4) & 0xF, field & 0xF
    Rn, Rx = "R%d" % rn, "R%d" % rx
    if opcode not in sharcfn._SHIFTIMM_MNEMONICS:
        unknown.append("shiftimm_unknown_opcode")
        return defs, uses, unknown
    if opcode in (0x08, 0x09):
        defs.append((Rn, "compute"))
        uses += [Rn, Rx]
    elif opcode in (0x00, 0x01, 0x10, 0x12, 0x30, 0x31, 0x32):
        defs.append((Rn, "compute"))
        uses.append(Rx)
    else:
        # btst (0x33): "[status only]" in render_shiftimm -- no Rn write.
        uses.append(Rx)
    return defs, uses, unknown


def _mem_regdef_reguse(t, f):
    """Direct (15a/14a/14d), indexed (3a/3b/3d/6a_mem), immediate-offset
    (4a/4b/4d/15b) and Type3c memory forms. Direction always comes from the
    form's own `d` field (sharcfn._space_dir, or Type3c's own `d`), never
    from mnemonic text -- see the module docstring's Type3c note."""
    defs, uses = [], []
    if t == "3c":
        # Not in tools/sharcfn.py's DIRECT/INDEXED_MEM_FORMS (that module's
        # render_instruction special-cases Type3c as the R2 push idiom
        # unconditionally instead); decoded directly here from its own
        # dmi/dmm/d/dreg fields (see the module docstring's Type3c note).
        i, m, d, dreg = f.get("dmi", 0), f.get("dmm", 0), f.get("d"), f.get("dreg")
        uses += ["I%d" % i, "M%d" % m]
        if dreg is not None:
            reg = "R%d" % dreg
            if d:
                uses.append(reg)
            else:
                defs.append((reg, "mem_load"))
        return defs, uses

    space, direction = sharcfn._space_dir(f)
    dreg, ureg = f.get("dreg"), f.get("ureg")
    reg = "R%d" % dreg if dreg is not None else \
        (sharcfn.ureg_name(ureg) if ureg is not None else None)
    if t in sharcfn.INDEXED_MEM_FORMS:
        uses += ["I%d" % f.get("i", 0), "M%d" % f.get("m", 0)]
    elif t in sharcfn.IMMOFF_MEM_FORMS:
        uses.append("I%d" % f.get("i", 0))
    if reg is not None:
        if direction == "store":
            uses.append(reg)
        else:
            defs.append((reg, "mem_load"))
    return defs, uses


def _dual_mem_regdef_reguse(f):
    """1a/1b: simultaneous DM(dmi,dmm)/PM(pmi,pmm) reference; dmd/pmd follow
    the same 1=store/0=load convention as sharcfn._space_dir's `d`."""
    defs, uses = [], []
    dmi, dmm, dmdreg = f.get("dmi", 0), f.get("dmm", 0), f.get("dmdreg")
    pmi, pmm, pmdreg = f.get("pmi", 0), f.get("pmm", 0), f.get("pmdreg")
    uses += ["I%d" % dmi, "M%d" % dmm, "I%d" % pmi, "M%d" % pmm]
    if dmdreg is not None:
        reg = "R%d" % dmdreg
        if f.get("dmd"):
            uses.append(reg)
        else:
            defs.append((reg, "mem_load"))
    if pmdreg is not None:
        reg = "R%d" % pmdreg
        if f.get("pmd"):
            uses.append(reg)
        else:
            defs.append((reg, "mem_load"))
    return defs, uses


def register_effects(insn_type: str, f: dict):
    """-> (defs: [(reg, kind)], uses: [reg], unknown: [tag]) for every
    register a decoded instruction defines or uses, deduplicated, from its
    typed fields (see the per-helper docstrings above and the module's
    SCHEMA comment on `regdef`/`reguse` for the kind vocabulary). Returns
    ([], [], []) for a form with no modelled register effect at all (a
    branch/call/return/NOP/EMU/etc. -- these are not "unknown", they are
    simply out of this table's scope: they define/use no general register
    tools/sharc_trace.py's State.uregs models the same way this file's other
    tables do)."""
    defs, uses, unknown = [], [], []
    t = insn_type

    if t in sharcinv.COMPUTE_FORMS:
        field23 = f.get("compute")
        if field23:
            d2, u2, unk2 = _compute_regdef_reguse(field23)
            defs += d2
            uses += u2
            unknown += unk2
    elif t == "2c":
        field12 = f.get("compute")
        if field12 is not None:
            d2, u2 = _shortcompute_regdef_reguse(field12)
            defs += d2
            uses += u2

    if t in sharcfn.DIRECT_MEM_FORMS or t in sharcfn.INDEXED_MEM_FORMS or \
            t in sharcfn.IMMOFF_MEM_FORMS or t == "3c":
        d2, u2 = _mem_regdef_reguse(t, f)
        defs += d2
        uses += u2
    elif t in sharcfn.DUAL_MEM_FORMS:
        d2, u2 = _dual_mem_regdef_reguse(f)
        defs += d2
        uses += u2

    if t == "6a_mem":
        d3, u3, unk3 = _shiftimm_regdef_reguse(f)
        defs += d3
        uses += u3
        unknown += unk3

    if f.get("u") and f.get("i") is not None:
        # DAG post-modify: the memory forms above already record I%d as
        # used (the pre-modify base for the access); u=1 also writes it.
        i_reg = "I%d" % f["i"]
        defs.append((i_reg, "dag_modify"))
        uses.append(i_reg)

    if t in ("5a_move", "5b_move"):
        dst = f.get("dstureg")
        src_hi, src_lo = f.get("srcureghigh", 0), f.get("srcureglow", 0) & 3
        src = (src_hi << 2) | src_lo
        if dst is not None:
            defs.append((sharcfn.ureg_name(dst), "move"))
        uses.append(sharcfn.ureg_name(src))
    elif t in ("5a_swap", "5b_swap"):
        c, d = f.get("cdreg"), f.get("dreg")
        if c is not None and d is not None:
            rc, rd = "R%d" % c, "R%d" % d
            defs += [(rc, "swap"), (rd, "swap")]
            uses += [rc, rd]
    elif t in ("17a", "17b"):
        ureg = f.get("ureg")
        if ureg is not None:
            defs.append((sharcfn.ureg_name(ureg), "literal"))
    elif t == "18a":
        sreg = f.get("sreg", 0)
        code = sharcfn.UREG_CODES.get("USTAT1", 0) + sreg
        reg = sharcfn.ureg_name(code)
        bop = f.get("bop", 0)
        uses.append(reg)
        if bop not in (4, 5):
            # set/clear/toggle (and the unnamed bop=3) read-modify-write;
            # bit-test/xor-test (4/5) only read the register into BTF.
            defs.append((reg, "literal"))
    elif t in ("19a", "19a_scaled", "19a_bitrev"):
        bank = 8 if f.get("g") else 0
        src_low, dis_low = f.get("is", 0), f.get("idis", 0)
        src, dst = bank + src_low, bank + (src_low ^ dis_low)
        defs.append(("I%d" % dst, "dag_modify"))
        uses.append("I%d" % src)
    elif t == "7a":
        # PRM Type7a MODIFY (pp.13-46/13-48): "Ia = MODIFY(Ia,Mb)" -- the
        # same source-XOR-idis destination trick as Type19a above, but the
        # modify delta is an M register (not a literal), so it is also a
        # use, like 16a/16b's mandatory I+=M below. Previously unmodelled
        # entirely: Type7a's compute half (COMPUTE_FORMS, above) recorded
        # register effects, but this instruction's own I-register write
        # never did, at any decode_table.json field, so the DAG modify a
        # Type7a always performs was invisible to `regdef`/`reguse`.
        bank = 8 if f.get("g") else 0
        src_low, dis_low = f.get("is", 0), f.get("idis", 0)
        src, dst = bank + src_low, bank + (src_low ^ dis_low)
        defs.append(("I%d" % dst, "dag_modify"))
        uses += ["I%d" % src, "M%d" % (bank + f.get("m", 0))]
    elif t in ("16a", "16b"):
        i, m = f.get("i", 0), f.get("m", 0)
        defs.append(("I%d" % i, "dag_modify"))
        uses += ["I%d" % i, "M%d" % m]
    elif t == "12a_imm":
        defs.append(("LCNTR", "literal"))
    elif t == "12a_ureg":
        ureg = f.get("ureg")
        defs.append(("LCNTR", "move"))
        if ureg is not None:
            uses.append(sharcfn.ureg_name(ureg))

    seen_def, uniq_defs = set(), []
    for reg, kind in defs:
        if (reg, kind) not in seen_def:
            seen_def.add((reg, kind))
            uniq_defs.append((reg, kind))
    uniq_uses = list(dict.fromkeys(uses))
    return uniq_defs, uniq_uses, unknown


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
        (name, "cpu", "sharc"),
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

    bblock_rows, succ_rows, leaders_by_idx = _build_blocks_and_succ(
        name, code_blocks, analyzed, functions, owner_of)

    insn_rows, edge_rows, literal_rows, mem_rows, hash_rows = [], [], [], [], []
    dataref_rows, regdef_rows, reguse_rows = [], [], []
    unknown_form_counts = collections.Counter()

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
        local_leaders = leaders_by_idx.get(idx, set())
        last_i_literal = {}

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

            if sw in local_leaders:
                # A new basic block starts here: dataref's 'resolved_offset'
                # only fires for an I-register literal load in the SAME
                # basic block as its use (see the module's SCHEMA comment).
                last_i_literal = {}

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

                if t in ("15a", "14a"):
                    # The literal IS the memory address here; use the same
                    # d-field direction extract_mem_access does, not a
                    # separate guess, so this never disagrees with
                    # mem_access's own abs_load/abs_store rows for the
                    # same instruction.
                    _space, direction = sharcfn._space_dir(f)
                    role = "abs_store" if direction == "store" else "abs_load"
                elif t in ("17a", "17b") and dest is not None and dest.startswith("I") \
                        and dest[1:].isdigit():
                    # A direct register load (ureg = literal); a 19a* dest is
                    # also an "I%d" string but is a modify DELTA, not the
                    # register's new value, so it stays 'literal' below.
                    role = "i_reg_base"
                    last_i_literal[int(dest[1:])] = value & 0xFFFFFFFF
                else:
                    role = "literal"
                # dataref is address-shaped values: store the unsigned 32-bit
                # pattern (0x8055c840), not extract_literal's sign-extended
                # int (literals.value is signed on purpose, for an ordinary
                # arithmetic immediate like -1; an address materialised from
                # the same bit pattern should read as the address).
                dataref_rows.append((name, value & 0xFFFFFFFF, sw, t, role))

            mem_access_rows = extract_mem_access(t, f)
            for row in mem_access_rows:
                mem_rows.append((name, sw) + row)
                abs_address = row[7]
                if abs_address is not None and t not in ("15a", "14a"):
                    # 14d: a direct absolute address that extract_literal
                    # doesn't cover (not a LITERAL_FORMS entry).
                    direction = row[1]
                    role = "abs_store" if direction == "store" else "abs_load"
                    dataref_rows.append((name, abs_address, sw, t, role))

            if t in sharcfn.IMMOFF_MEM_FORMS:
                i_reg = f.get("i", 0)
                base = last_i_literal.get(i_reg)
                if base is not None:
                    bits = sharcfn._IMMOFF_DATA_BITS.get(t, 6)
                    off_val = sharcfn.sign_extend(f.get("data", 0), bits)
                    modified = (base + off_val) & 0xFFFFFFFF
                    # PRM Type4a/4b/4d pp.13-26/13-30/13-34 ("u"): u=0
                    # pre-modifies I for the address (I keeps its old value
                    # afterwards); u=1 accesses the CURRENT (unmodified) I
                    # and only writes I+offset back afterwards
                    # (post-modify) -- so THIS access is at `base`, not
                    # `base + off_val`, when u=1. Type15b has no u bit and
                    # is always pre-modify (PRM p.15-11). This matches
                    # tools/sharcdb.py's own `ptr` table (_ptr_mem_form)
                    # and tools/sharc_trace.py's "4a"/"4b" handlers, which
                    # already draw this same distinction -- this
                    # `resolved_offset` role had not been updated to match.
                    post_modify = t != "15b" and bool(f.get("u"))
                    address = base if post_modify else modified
                    dataref_rows.append((name, address, sw, t, "resolved_offset"))
                    if post_modify:
                        # I is permanently updated to the modified value
                        # now, so a later same-block IMMOFF access to the
                        # same I register must resolve against the new
                        # value, not the stale literal.
                        last_i_literal[i_reg] = modified

            defs, uses, unknown = register_effects(t, f)
            for reg, kind in defs:
                regdef_rows.append((name, sw, reg, kind))
            for reg in uses:
                reguse_rows.append((name, sw, reg))
            for tag in unknown:
                unknown_form_counts[tag] += 1

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
    db.executemany("INSERT INTO bblocks VALUES (?,?,?,?,?,?)", bblock_rows)
    db.executemany("INSERT INTO succ VALUES (?,?,?,?)", succ_rows)
    db.executemany("INSERT INTO dataref VALUES (?,?,?,?,?)", dataref_rows)
    db.executemany("INSERT INTO regdef VALUES (?,?,?,?)", regdef_rows)
    db.executemany("INSERT INTO reguse VALUES (?,?,?)", reguse_rows)
    for tag, count in unknown_form_counts.items():
        meta_rows_extra = [(name, "unknown_regdef_" + tag, str(count))]
        db.executemany("INSERT INTO meta VALUES (?,?,?)", meta_rows_extra)

    return {
        "n_insn": len(insn_rows), "n_functions": len(func_rows), "n_edges": len(edge_rows),
        "n_literals": len(literal_rows), "n_mem_access": len(mem_rows), "n_func_hash": len(hash_rows),
        "n_bblocks": len(bblock_rows), "n_succ": len(succ_rows), "n_dataref": len(dataref_rows),
        "n_regdef": len(regdef_rows), "n_reguse": len(reguse_rows),
        "unknown_form_counts": dict(unknown_form_counts),
    }


# --- analysis pass (roots/reach/callgraph/idom/loops/unentered) ------------
#
# Runs once per build (and standalone via `sharcdb analyze`) over the tables
# _fill_database already wrote: edges, succ, dataref, literals and
# functions. Nothing here decodes another instruction; it is pure graph work
# on what is already in the database, using networkx instead of another
# hand-rolled CFG walk.

# --- interrupt vector table (see DB_VERSION v7 comment above) ---------------

IVT_SW = 0x120000
IVT_N_SLOTS = 32
IVT_SLOT_BYTES = 24
# An unpopulated slot's four 48-bit words, verified identical across all four
# images this repo builds.
IVT_EMPTY_INSN = bytes((0, 0, 0, 0, 0x3E, 0x0B))
# The trailing two bytes of a direct-jump 8a_abs instruction (see
# _gather_terminators's own t in ("8a_abs", ...) check) -- distinguishes a
# real jump word from the LCNTR-literal/other filler words some slots also
# carry (e.g. slot 5's second word, '020000 05760f').
_IVT_JUMP_SUFFIX = b"\x3e\x06"

# sharc-plus-prm Table 4-46 "Interrupt Priority and Vectors".
IVT_VECTOR_NAMES = {
    0: "EMUI", 1: "RSTI", 2: "reserved", 3: "PARI", 4: "ILOPI", 5: "CB7I",
    6: "IICDI", 7: "SOVFI", 8: "ILADI", 9: "reserved", 10: "reserved",
    11: "TMZHI", 12: "BKPI", 13: "FIR", 14: "IIR", 15: "SECI",
    16: "reserved", 17: "reserved", 18: "reserved", 19: "reserved",
    20: "RINSEQI", 21: "CB15I", 22: "TMZLI", 23: "FIXI", 24: "FLTOI",
    25: "FLTUI", 26: "FLTII", 27: "EMULI", 28: "SFT0I", 29: "SFT1I",
    30: "SFT2I", 31: "SFT3I",
}

# Two further ISR dispatch tables outside the IVT proper (addresses in this
# repo's bare DM convention -- literals.value/dataref.value/ptr.address all
# already use it; add sharcldr.SW_ALIAS_BASE to reach a LoadedMemory byte
# address). Checked against DT2 1.16/1.15C: both sit entirely inside a
# zero-argument FILL block (see DB_VERSION v7 comment), so this table only
# ever fires the "runtime-filled, no root added" branch there today; it is
# still CPU/image-generic so a future image that ships these tables
# initialized gets code_pointer_array roots for free.
IVT_DISPATCH_TABLES = (
    (0x240ad8, 8, "generic interrupt dispatch (by ASTATX number)"),
    (0x240948, 2, "SECI dispatch (by SEC id)"),
)


def _detect_interrupt_vector_roots(mem):
    """[(target_sw, 'interrupt_vector', 'slot N NAME'), ...] -- one per
    populated IVT slot (see the module-level IVT_* constants). mem is
    optional: without it (no LoadedMemory on hand), this detector is
    skipped, like loader_entry/code_pointer_array above."""
    if mem is None:
        return []
    roots = []
    byte_base = sharcldr.sw_to_byte(IVT_SW)
    for slot in range(IVT_N_SLOTS):
        chunk = mem.read(byte_base + slot * IVT_SLOT_BYTES, IVT_SLOT_BYTES)
        if chunk is None:
            break
        if chunk == IVT_EMPTY_INSN * 4:
            continue
        target = None
        for k in range(4):
            insn = chunk[k * 6:(k + 1) * 6]
            if insn[4:6] == _IVT_JUMP_SUFFIX:
                target = int.from_bytes(insn[0:3], "little")
                break
        if target is None:
            # A populated slot with no recognisable direct-jump word --
            # left unmodelled rather than guessed (none seen on any of the
            # four images checked).
            continue
        name = IVT_VECTOR_NAMES.get(slot, "reserved")
        roots.append((target, "interrupt_vector", "slot %d %s" % (slot, name)))
    return roots


def _detect_dispatch_table_roots(mem, blocks, owner):
    """[(value, 'code_pointer_array', note), ...] for IVT_DISPATCH_TABLES,
    same consecutive-in-code-pointer-run logic as _detect_roots's
    i_reg_base table scan. A table whose base sits inside a FILL block is
    reported as runtime-filled and skipped -- see the module note above."""
    if mem is None or blocks is None:
        return []
    roots = []
    for base, stride, desc in IVT_DISPATCH_TABLES:
        byte_base = base + sharcldr.SW_ALIAS_BASE
        if sharcldr.fill_block_for_address(blocks, byte_base, space="byte") is not None:
            continue
        values, first_owner = [], None
        for i in range(64):
            raw = mem.read(byte_base + i * stride, min(stride, 4))
            if raw is None:
                break
            value = int.from_bytes(raw, "little")
            fn = owner(value)
            if fn is None or (first_owner is not None and fn != first_owner):
                break
            first_owner = fn
            values.append(value)
        if len(values) >= 2:
            for i, value in enumerate(values):
                roots.append((value, "code_pointer_array", "%s table=0x%x idx=%d" % (desc, base, i)))
    return roots


def _owner_factory(function_spans):
    """A closure mapping any sw to the entry_sw of the function whose
    [entry_sw, end_sw) span contains it (not necessarily an aligned
    instruction there -- see analyze_image()'s note on RTOS task entries),
    or None."""
    spans = sorted(function_spans)
    starts = [e for e, _ in spans]

    def owner(sw):
        i = bisect.bisect_right(starts, sw) - 1
        if i < 0:
            return None
        entry, end = spans[i]
        return entry if entry <= sw < end else None

    return owner


def _detect_roots(db, name, mem, blocks):
    """[(sw, kind, note), ...] -- see the `roots` table's SCHEMA comment for
    the kind vocabulary. `mem`/`blocks` (a LoadedMemory and its parsed block
    list) are optional: without them, loader_entry and code_pointer_array
    roots are skipped (their note explains why) rather than guessed."""
    function_spans = db.execute(
        "SELECT entry_sw, end_sw FROM functions WHERE image=?", (name,)
    ).fetchall()
    owner = _owner_factory(function_spans)
    roots = []

    if blocks is not None:
        firsts = sharcldr.entry_points(blocks)
        if firsts:
            # The main program's own entry: the LAST BFLAG_FIRST target, the
            # same one tools/sharcldr.py's main_program() takes as the final
            # application's code (a boot stream may carry earlier FIRST
            # blocks for a bootstrap stage this file never scans).
            roots.append((firsts[-1], "loader_entry", "last of %d BFLAG_FIRST entries" % len(firsts)))

    roots += _detect_interrupt_vector_roots(mem)
    roots += _detect_dispatch_table_roots(mem, blocks, owner)

    # RTOS task entries: a CALL whose immediately preceding instructions (in
    # the SAME function, within a small window -- this idiom is always a
    # tight run of literal loads right before the call) load R4/R8/R12 by
    # literal, where R4's value itself resolves into a scanned function's
    # span. That last check is what tells this idiom apart from the many
    # other calls that happen to load exactly these three registers for an
    # unrelated reason (checked against both DT2 1.16 and DN2 1.11: without
    # it, a dozen-plus unrelated call sites match; with it, only the real
    # task-create helper's call sites survive, in both images, without
    # hardcoding either helper's address).
    reg_rows = db.execute(
        "SELECT sw, value, dest_reg FROM literals WHERE image=? AND dest_reg IN ('R4','R8','R12') ORDER BY sw",
        (name,),
    ).fetchall()
    by_reg = collections.defaultdict(list)
    for sw, value, reg in reg_rows:
        by_reg[reg].append((sw, value))
    by_reg_sw = {reg: [s for s, _v in lst] for reg, lst in by_reg.items()}

    def last_within(reg, sw, window=20):
        sws = by_reg_sw.get(reg)
        if not sws:
            return None
        i = bisect.bisect_left(sws, sw)
        if i == 0:
            return None
        s, v = by_reg[reg][i - 1]
        return v if sw - s <= window else None

    call_rows = db.execute(
        "SELECT from_sw, to_function FROM edges WHERE image=? AND kind='call' AND to_function IS NOT NULL",
        (name,),
    ).fetchall()
    for from_sw, to_fn in call_rows:
        r4 = last_within("R4", from_sw)
        r8 = last_within("R8", from_sw)
        r12 = last_within("R12", from_sw)
        if r4 is None or r8 is None or r12 is None:
            continue
        r4u = r4 & 0xFFFFFFFF
        if owner(r4u) is None:
            continue
        roots.append((r4u, "rtos_task", "helper=0x%x call_site=0x%x" % (to_fn, from_sw)))

    # Every dataref value that lands inside a scanned function's span: a
    # callback or dispatch pointer materialised as an ordinary literal/table
    # address rather than a CALL/JUMP target.
    for value, sw in db.execute(
        "SELECT value, MIN(sw) FROM dataref WHERE image=? GROUP BY value", (name,)
    ).fetchall():
        if owner(value) is not None:
            roots.append((value, "dataref_code_pointer", "first ref at 0x%x" % sw))

    # Loader-backed code-pointer arrays: consecutive 32-bit little-endian
    # words at an i_reg_base table address (a literal loaded directly into
    # an I register -- the generic "table base" pattern) that decode to
    # addresses inside ONE function. The array ends at the first word that
    # resolves to a different function (or to none) -- e.g. 0x8055c840's
    # entries 0-14 all land in FUN_1c642a; entry 15 lands elsewhere and ends
    # the run. Capped at 64 words so a false-positive base can't spin.
    if mem is not None:
        for (base,) in db.execute(
            "SELECT DISTINCT value FROM dataref WHERE image=? AND role='i_reg_base'", (name,)
        ).fetchall():
            values, first_owner = [], None
            for i in range(64):
                raw = mem.read(base + i * 4, 4)
                if raw is None:
                    break
                value = int.from_bytes(raw, "little")
                fn = owner(value)
                if fn is None or (first_owner is not None and fn != first_owner):
                    break
                first_owner = fn
                values.append(value)
            if len(values) >= 2:
                for i, value in enumerate(values):
                    roots.append((value, "code_pointer_array", "table=0x%x idx=%d" % (base, i)))

    # Functions no edge of any kind targets -- the same generalised "no
    # static caller" check as sharcdb.sql's canned query.
    for (entry,) in db.execute(
        "SELECT f.entry_sw FROM functions f WHERE f.image=? AND NOT EXISTS "
        "(SELECT 1 FROM edges e WHERE e.image=f.image AND e.to_function=f.entry_sw)",
        (name,),
    ).fetchall():
        roots.append((entry, "no_static_entry", None))

    # ColdFire vector table entries: Ghidra itself named these
    # vector_<N>_handler (see build_ghidra_database); a SHARC function is
    # always named FUN_%06x, so this never matches a SHARC image either --
    # no cpu flag needed here.
    for entry, fn_name in db.execute(
        r"SELECT entry_sw, name FROM functions WHERE image=? AND name LIKE 'vector\_%\_handler' ESCAPE '\'",
        (name,),
    ).fetchall():
        roots.append((entry, "vector_entry", fn_name))

    return roots


def _function_call_jump_graph(db, name, function_entries):
    """A networkx DiGraph over function entries: a CALL edge costs 1 (a call
    depth increment), a JUMP/COND_JUMP edge costs 0 (a tail/cross-function
    jump reaches its target at no extra call depth). Where both a call and a
    jump connect the same pair, the jump's 0 wins (min cost)."""
    G = nx.DiGraph()
    G.add_nodes_from(function_entries)
    for a, b in db.execute(
        "SELECT DISTINCT from_function, to_function FROM edges WHERE image=? AND kind='call' "
        "AND from_function IS NOT NULL AND to_function IS NOT NULL AND from_function != to_function",
        (name,),
    ).fetchall():
        G.add_edge(a, b, weight=1)
    for a, b in db.execute(
        "SELECT DISTINCT from_function, to_function FROM edges WHERE image=? AND kind IN ('jump','cond_jump') "
        "AND from_function IS NOT NULL AND to_function IS NOT NULL AND from_function != to_function",
        (name,),
    ).fetchall():
        G.add_edge(a, b, weight=0)
    return G


def _detect_reach(db, name, roots, function_entries):
    """[(root_sw, function_sw, depth), ...]: every function reachable from
    each distinct root address, by shortest CALL depth (0-weighted
    jump/cond_jump edges included in the walk, per
    _function_call_jump_graph).

    Every root normally maps to the function whose [entry_sw, end_sw) span
    contains it (owner(), below). loader_entry is the exception: it's the
    scanned region's raw BFLAG_FIRST target (see _detect_roots), which can
    land outside every function's span. Checked directly against both DT2
    1.16 and DN2 1.11: in both, the loader entry sits at the start of a
    `data` block immediately followed by `fill` padding, and the last
    bblock the scanner finds before that padding has no succ row at all --
    there is no live bblock/succ path from the root into the next
    function, so walking that CFG would find nothing to seed with. The
    fallback below is the next function by address instead: the gap is
    either unscanned or alignment padding, i.e. a straight-line
    continuation rather than a call, so it is seeded at depth 0 -- the
    same effect as giving the loader entry a synthetic function span up to
    that next entry, without adding a fake row to `functions` and
    skewing its count. This fallback is general (any owner-less root
    would take it), but today only loader_entry roots ever do -- every
    other root kind is only added when it already resolves to a function
    (see _detect_roots)."""
    owner = _owner_factory(db.execute(
        "SELECT entry_sw, end_sw FROM functions WHERE image=?", (name,)
    ).fetchall())
    starts = sorted(function_entries)
    G = _function_call_jump_graph(db, name, function_entries)
    rows = []
    for sw in sorted({r[0] for r in roots}):
        fn = owner(sw)
        if fn is None:
            i = bisect.bisect_right(starts, sw)
            if i >= len(starts):
                continue
            fn = starts[i]
        for target_fn, dist in nx.single_source_dijkstra_path_length(G, fn, weight="weight").items():
            rows.append((sw, target_fn, int(round(dist))))
    return rows


def _detect_callgraph(db, name, function_entries):
    """[(function_sw, n_callers, n_callees, n_transitive_callees, max_depth,
    is_recursive), ...], CALL edges only. Uses networkx's SCC condensation
    so the whole image's transitive-callee counts and longest call chains
    are one O(V+E) pass, not one traversal per function."""
    G = nx.DiGraph()
    G.add_nodes_from(function_entries)
    for a, b in db.execute(
        "SELECT from_function, to_function FROM edges WHERE image=? AND kind='call' "
        "AND from_function IS NOT NULL AND to_function IS NOT NULL",
        (name,),
    ).fetchall():
        G.add_edge(a, b)

    C = nx.condensation(G)
    order = list(nx.topological_sort(C))
    desc_sccs, longest = {}, {}
    for c in reversed(order):
        s, best = set(), 0
        for succ in C.successors(c):
            s.add(succ)
            s |= desc_sccs[succ]
            best = max(best, 1 + longest[succ])
        desc_sccs[c] = s
        longest[c] = best

    mapping = C.graph["mapping"]
    rows = []
    for fn in function_entries:
        c = mapping[fn]
        members = C.nodes[c]["members"]
        transitive = set()
        for sc in desc_sccs[c]:
            transitive |= C.nodes[sc]["members"]
        if len(members) > 1:
            transitive |= members - {fn}
        is_recursive = len(members) > 1 or G.has_edge(fn, fn)
        rows.append((fn, G.in_degree(fn), G.out_degree(fn), len(transitive), longest[c], int(is_recursive)))
    return rows


def _detect_dominators_and_loops(db, name):
    """(idom_rows, loop_rows): per function, networkx.immediate_dominators
    on that function's own bblocks/succ subgraph, then natural loops from
    back edges (succ target dominates succ source) merged per header, kind
    'hw_do' when any merged back edge is succ's own loop_back, else
    'branch_back'."""
    by_func = collections.defaultdict(list)
    for start, fn in db.execute(
        "SELECT start_sw, function_sw FROM bblocks WHERE image=?", (name,)
    ).fetchall():
        if fn is not None:
            by_func[fn].append(start)
    succ_by_from = collections.defaultdict(list)
    for f, t, k in db.execute("SELECT from_block, to_block, kind FROM succ WHERE image=?", (name,)).fetchall():
        succ_by_from[f].append((t, k))

    idom_rows, loop_rows = [], []
    for fn, starts in by_func.items():
        block_set = set(starts)
        H = nx.DiGraph()
        H.add_nodes_from(starts)
        for b in starts:
            for t, k in succ_by_from.get(b, ()):
                if t in block_set:
                    H.add_edge(b, t, kind=k)
        if fn not in H:
            continue
        idom = nx.immediate_dominators(H, fn)
        for node, idom_node in idom.items():
            if node != fn:
                idom_rows.append((fn, node, idom_node))

        def dominates(target, node, _idom=idom, _root=fn):
            n = node
            while True:
                if n == target:
                    return True
                if n == _root:
                    return False
                n = _idom.get(n, _root)

        preds = collections.defaultdict(list)
        for u, v in H.edges():
            preds[v].append(u)

        by_header = collections.defaultdict(lambda: {"body": set(), "hw_do": False})
        for u, v, data in H.edges(data=True):
            if v not in idom or not dominates(v, u):
                continue
            body, stack = {v, u}, ([] if u == v else [u])
            while stack:
                n = stack.pop()
                for p in preds[n]:
                    if p not in body:
                        body.add(p)
                        stack.append(p)
            rec = by_header[v]
            rec["body"] |= body
            if data.get("kind") == "loop_back":
                rec["hw_do"] = True

        headers = list(by_header.items())
        for i, (header, rec) in enumerate(headers):
            depth = 1 + sum(
                1 for j, (h2, r2) in enumerate(headers)
                if j != i and rec["body"] < r2["body"]
            )
            loop_rows.append((
                fn, header, len(rec["body"]), "hw_do" if rec["hw_do"] else "branch_back", depth,
            ))

    return idom_rows, loop_rows


def _detect_unentered(db, name, roots):
    """[(function_sw, cluster), ...] for every roots.kind='no_static_entry'
    function: the code-pointer array/literal that names it, else the
    nearest preceding function with an indirect site, else 'none'."""
    no_entry = sorted(sw for sw, kind, _note in roots if kind == "no_static_entry")
    array_note = {sw: note for sw, kind, note in roots if kind == "code_pointer_array"}
    literal_note = {sw: note for sw, kind, note in roots if kind == "dataref_code_pointer"}

    indirect_owners = sorted({
        r[0] for r in db.execute(
            "SELECT DISTINCT from_function FROM edges WHERE image=? AND kind='indirect' AND from_function IS NOT NULL",
            (name,),
        ).fetchall()
    })

    rows = []
    for fn in no_entry:
        if fn in array_note:
            cluster = "array:" + array_note[fn]
        elif fn in literal_note:
            cluster = "literal:" + literal_note[fn]
        else:
            i = bisect.bisect_left(indirect_owners, fn) - 1
            cluster = "indirect:0x%x" % indirect_owners[i] if i >= 0 else "none"
        rows.append((fn, cluster))
    return rows


# --- constant-pointer propagation (`ptr` table, DB_VERSION 6) --------------
#
# A simple, deterministic forward dataflow per function, over the same
# bblocks/succ this file already built, tracking known 32-bit constants for
# every register (I0-I15, R0-R15, M/L/B, ...) plus DM(I6+k) frame slots keyed
# by their raw offset (I6 -- docs/findings/06 -- is this ABI's frame
# pointer; its own runtime value is never tracked, only slot identity, so a
# spill/reload pair resolves regardless of what I6 actually holds at
# runtime). Rounds re-seed a function's R4/R8/R12 from its call sites'
# previous-round values when every caller agrees, approximating the RTOS-
# task/RPC-argument convention _detect_roots's rtos_task detector already
# relies on for a single call site; a handful of rounds is enough for a
# short call chain to converge without chasing a true fixed point.
#
# Modelled, mirroring tools/sharc_trace.py's own handling of each form (the
# chosen source of truth for scaling/addressing, since this pass is a static
# approximation of exactly what that tracer executes dynamically):
#   - 17a/17b literal loads (extract_literal already generalises the
#     destination to any ureg, R included).
#   - 19a/19a_scaled MODIFY: dest is Is XOR Idis (bank-adjusted by `g`);
#     19a_scaled's immediate is scaled x4/x2 for a normal-/short-word
#     modifier, exactly as the tracer's assume_nw32 branch does (this repo
#     always assumes 32-bit internal normal words -- tools/sharc.py's
#     trace()). 19a_bitrev's destination is always invalidated: bit-reversed
#     addressing is not a plain add.
#   - 5a/5b_move ureg-to-ureg copies.
#   - A conservative add/sub/inc/dec/pass/neg subset of the 23-bit parallel
#     compute field (COMPUTE_FORMS) and the 2c short-compute field -- every
#     other opcode (multiply, shift, logic, float, dual add/subtract, ...)
#     invalidates its destination rather than guessing.
#   - Every direct (14a/14d), indexed (3a/3b/3d/6a_mem) and immediate-offset
#     (4a/4b/4d/15b) memory form's own addressing math, INCLUDING Type15a's
#     I+immediate address: tools/sharcfn.py's DIRECT_MEM_FORMS groups 15a
#     with the pure-absolute 14a/14d, but PRM p.388 and tools/sharc_trace.py's
#     own "15a" handler both give it an I-register-relative address (its
#     DM(I6-4)/DM(0xfffffffc,I6) example is the exact frame-slot pairing
#     this pass exists to resolve) -- treating it as pure-absolute here would
#     silently mis-locate every Type15a access whose I register isn't zero.
#     I-register post-modify (u=1) commits the new value; pre-modify/no-
#     writeback (u=0) leaves the register untouched, exactly as the tracer's
#     own post_modify branch does. Bank selection (g=1 -> physical register
#     +8, DAG2/PM) is applied uniformly here -- unlike this file's EXISTING
#     extract_mem_access, whose INDEXED_MEM_FORMS branch never adds that +8
#     (a pre-existing, out-of-scope gap confirmed directly against
#     tools/sharcspec/decode_table.json's field widths and sharc_trace.py's
#     own bank math; left alone rather than risk-edited under this task).
#   - Type3c (the plain-R-register "DM(Ii,Mm) = Rd" encoding tools/sharcfn.py
#     also renders as the I7/M7 stack-push idiom, but dmi/dmm are ordinary
#     register fields -- FUN_1c15e3's workspace-copy loop uses it with
#     dmi=I4/dmm=M6): always DM, always mandatory post-modify (no g/u
#     fields at all), exactly like an INDEXED_MEM_FORMS u=1 access.
#
# NOT modelled -- the affected register/slot is always invalidated, never
# guessed: Type16a/16b (immediate-to-memory store), Type1a/1b dual DM+PM
# forms (tools/sharc_trace.py does not execute either), any ALU/MULT/SHIFT/
# MULTIFN opcode outside the small integer subset above, and any memory LOAD
# from a non-frame-slot address (the loaded value is runtime data, not a
# static fact this pass can know from decoding alone).
#
# Merge at a block join keeps a key only when every already-visited
# predecessor's out-state agrees on it. Blocks are visited in a per-function
# topological order that drops loop back edges, reusing analyze_image()'s
# own already-computed `idom` (an edge u->v is a back edge exactly when v
# dominates u -- the same test _detect_dominators_and_loops uses); a loop
# header's in-state is therefore its value on FIRST arrival only, never
# updated by a later iteration's own store/modify -- which is exactly what
# makes a loop that walks a pointer across iterations resolve to one
# loop-invariant base address rather than "unknown" (FUN_1c15e3's per-
# iteration store, tested below). A not-yet-visited or back-edge predecessor
# simply contributes nothing to a merge, the same safe "drop rather than
# guess" default this pass uses throughout. A block with NO confident
# predecessor at all -- the function's own entry, or one only an unresolved
# indirect jump reaches (a dispatch-table case; see _build_ptr_rows) --
# falls back to the entry baseline for registers and an empty frame, and is
# itself marked unconfident so that guess can never zero out a real,
# still-tracked value at a later genuine reconvergence (see
# _build_ptr_rows's own note on FUN_1c642a, where this mattered directly).

_PTR_ROUNDS = 4

# Hardware DAG-modify reset values (docs/findings/06), the same ones
# tools/sharc.py's trace() seeds by default -- held in every function's
# entry state regardless of caller, not something call-site seeding could
# ever discover on its own.
_PTR_M_RESET = {"M5": 0, "M6": 1, "M7": 0xFFFFFFFF, "M13": 0, "M14": 1, "M15": 0xFFFFFFFF}

_PTR_CALL_ARG_REGS = ("R4", "R8", "R12")


def _ptr_bank_reg(prefix, num, f):
    """"I%d"/"M%d" with PGR's DAG1/DAG2 bank offset (g=1 -> +8) applied --
    see the module note above on the existing extract_mem_access gap this
    intentionally does NOT share."""
    return "%s%d" % (prefix, num + (8 if f.get("g") else 0))


def _ptr_alu_updates(t, f, state):
    """New constant register values from an instruction's own parallel ALU
    compute field (its 'compute' key) -- {} for any opcode outside the
    add/sub/inc/dec/pass/neg subset this pass models, a float op, a dual
    add/subtract, or no compute field at all. The caller invalidates the
    defined register(s) when this returns nothing for them (register_effects
    already knows a compute field always redefines its Rn)."""
    if t in sharcinv.COMPUTE_FORMS:
        field23 = f.get("compute")
        if not field23:
            return {}
        cu, d = sharcinv.classify_compute(field23)
        if cu != "ALU" or d.get("is_dual_addsub") or d.get("is_float"):
            return {}
        name = sharcinv.ALU_OPS.get(d.get("opcode", 0))
        rn, rx, ry = ((field23 >> sh) & 0xF for sh in (8, 4, 0))
        Rn, Rx, Ry = ("R%d" % rn, "R%d" % rx, "R%d" % ry)
        if name in ("add", "sub"):
            vx, vy = state.get(Rx), state.get(Ry)
            if vx is not None and vy is not None:
                return {Rn: ((vx + vy) if name == "add" else (vx - vy)) & 0xFFFFFFFF}
        elif name in ("pass", "neg", "inc", "dec"):
            vx = state.get(Rx)
            if vx is not None:
                return {Rn: {"pass": vx, "neg": -vx, "inc": vx + 1, "dec": vx - 1}[name] & 0xFFFFFFFF}
        return {}
    if t == "2c":
        field12 = f.get("compute")
        if field12 is None:
            return {}
        opcode = (field12 >> 8) & 0xF
        if opcode in sharcfn.FLOAT_SHORT_OPS:
            return {}
        rn, rx = (field12 >> 4) & 0xF, field12 & 0xF
        Rn, Rx = "R%d" % rn, "R%d" % rx
        if opcode in (0x0, 0x1):  # add, sub: RN = RN op RX
            vn, vx = state.get(Rn), state.get(Rx)
            if vn is not None and vx is not None:
                return {Rn: ((vn + vx) if opcode == 0x0 else (vn - vx)) & 0xFFFFFFFF}
        elif opcode == 0x2:  # pass: RN = RX
            vx = state.get(Rx)
            if vx is not None:
                return {Rn: vx}
        elif opcode in (0x5, 0x6):  # inc, dec: RN = RN +- 1
            vn = state.get(Rn)
            if vn is not None:
                return {Rn: ((vn + 1) if opcode == 0x5 else (vn - 1)) & 0xFFFFFFFF}
        return {}
    return {}


def _ptr_reg_updates(t, f, state):
    """New constant values from a pure register-to-register form: literal
    load (17a/17b), MODIFY (19a/19a_scaled/19a_bitrev) or move (5a/5b_move).
    {} for anything else (memory forms are _ptr_mem_form's job)."""
    if t in ("17a", "17b"):
        literal = extract_literal(t, f)
        if literal is None:
            return {}
        value, dest = literal
        return {dest: value & 0xFFFFFFFF} if dest is not None else {}

    if t in ("19a", "19a_scaled", "19a_bitrev"):
        if t == "19a_bitrev":
            return {}  # never a plain add -- see the module note.
        bank = 8 if f.get("g") else 0
        src_low, dis_low = f.get("is", 0), f.get("idis", 0)
        src, dst = "I%d" % (bank + src_low), "I%d" % (bank + (src_low ^ dis_low))
        base = state.get(src)
        if base is None:
            return {}
        delta = sharcfn.sign_extend(f.get("data", 0), 32)
        if t == "19a_scaled":
            delta *= 4 if f.get("w") else 2
        return {dst: (base + delta) & 0xFFFFFFFF}

    if t in ("5a_move", "5b_move"):
        dst = f.get("dstureg")
        if dst is None:
            return {}
        src_hi, src_lo = f.get("srcureghigh", 0), f.get("srcureglow", 0) & 3
        value = state.get(sharcfn.ureg_name((src_hi << 2) | src_lo))
        return {sharcfn.ureg_name(dst): value} if value is not None else {}

    return {}


def _ptr_access_reg(f):
    """The register on the OTHER side of a memory access (the one loaded
    from / stored to memory) -- same dreg-else-ureg fallback as
    _mem_regdef_reguse, reused here for the DM(I6+k) frame-slot case only."""
    dreg, ureg = f.get("dreg"), f.get("ureg")
    if dreg is not None:
        return "R%d" % dreg
    if ureg is not None:
        return sharcfn.ureg_name(ureg)
    return None


def _ptr_frame_access(frame, off, direction, reg, state):
    """DM(I6+off)'s own store/load of a frame slot keyed by offset alone --
    I6's own value is never consulted (see the module note). `frame` is
    mutated in place; -> the register's new value for a load, or None (a
    store, an unknown reg, or a not-yet-known slot). The caller folds a
    non-None result into its own new_values rather than writing `state`
    directly here: _ptr_exec's blanket "pop every register_effects() def"
    step runs AFTER this function returns and would otherwise immediately
    discard a direct write (confirmed against FUN_1c642a's own I6-4
    spill/reload -- see _build_ptr_rows's module note)."""
    if reg is None:
        return None
    if direction == "store":
        value = state.get(reg)
        if value is not None:
            frame[off] = value
        else:
            frame.pop(off, None)
        return None
    return frame.get(off)


def _ptr_mem_form(t, f, state, frame):
    """(new_values, ptr_row) for one memory-referencing instruction.
    new_values only ever names the base I register, for a post-modify commit
    (INDEXED/IMMOFF, u=1); ptr_row is a (base_reg, base_value, address,
    direction, width) tuple for the `ptr` table, or None when this access's
    base is I6 (frame-slot bookkeeping only -- see _ptr_frame_access) or
    nothing about it is constant at all."""
    if t in ("14a", "14d"):
        _space, direction = sharcfn._space_dir(f)
        width = "long" if f.get("l") else "word"
        address = f.get("addr", 0) & 0xFFFFFFFF
        return {}, (None, None, address, direction, width)

    if t == "15a":
        base_reg = _ptr_bank_reg("I", f.get("i", 0), f)
        _space, direction = sharcfn._space_dir(f)
        width = "long" if f.get("l") else "word"
        off = (f.get("addr", 0) & 0xFFFFFFFF) * 4 & 0xFFFFFFFF
        if base_reg == "I6":
            reg = _ptr_access_reg(f)
            value = _ptr_frame_access(frame, off, direction, reg, state)
            return ({reg: value} if value is not None else {}), None
        base_value = state.get(base_reg)
        if base_value is None:
            return {}, None
        return {}, (base_reg, base_value, (base_value + off) & 0xFFFFFFFF, direction, width)

    if t == "3c":
        # The same indexed-post-modify addressing as INDEXED_MEM_FORMS
        # below, under a narrower encoding (always DM, always post-modify,
        # a plain R-register dreg -- no g/u/ureg fields at all; see
        # tools/sharc_trace.py's own "3c" handler, which this mirrors
        # exactly, and _mem_regdef_reguse's docstring on why it isn't a
        # member of sharcfn.INDEXED_MEM_FORMS).
        base_reg, mod_reg = "I%d" % f.get("dmi", 0), "M%d" % f.get("dmm", 0)
        direction = "store" if f.get("d") else "load"
        base_value, mod_value = state.get(base_reg), state.get(mod_reg)
        if base_value is None:
            return {}, None
        new_values = {}
        if mod_value is not None:
            new_values[base_reg] = (base_value + mod_value * 4) & 0xFFFFFFFF
        return new_values, (base_reg, base_value, base_value, direction, "word")

    if t in sharcfn.INDEXED_MEM_FORMS:
        base_reg = _ptr_bank_reg("I", f.get("i", 0), f)
        mod_reg = _ptr_bank_reg("M", f.get("m", 0), f)
        u = bool(f.get("u"))
        _space, direction = sharcfn._space_dir(f)
        base_value, mod_value = state.get(base_reg), state.get(mod_reg)
        if base_value is None:
            return {}, None
        if u:
            new_values = {}
            if mod_value is not None:
                new_values[base_reg] = (base_value + mod_value * 4) & 0xFFFFFFFF
            return new_values, (base_reg, base_value, base_value, direction, "word")
        address = (base_value + mod_value * 4) & 0xFFFFFFFF if mod_value is not None else None
        return {}, (base_reg, base_value, address, direction, "word")

    if t in sharcfn.IMMOFF_MEM_FORMS:
        base_reg = _ptr_bank_reg("I", f.get("i", 0), f)
        bits = sharcfn._IMMOFF_DATA_BITS.get(t, 6)
        scale = 8 if (t == "15b" and f.get("l")) else 4
        off = (sharcfn.sign_extend(f.get("data", 0), bits) * scale) & 0xFFFFFFFF
        u = bool(f.get("u")) if t != "15b" else False
        _space, direction = sharcfn._space_dir(f)
        width = "long" if f.get("l") else "word"
        if base_reg == "I6":
            reg = _ptr_access_reg(f)
            value = _ptr_frame_access(frame, off, direction, reg, state)
            return ({reg: value} if value is not None else {}), None
        base_value = state.get(base_reg)
        if base_value is None:
            return {}, None
        if u:
            return {base_reg: (base_value + off) & 0xFFFFFFFF}, (base_reg, base_value, base_value, direction, width)
        return {}, (base_reg, base_value, (base_value + off) & 0xFFFFFFFF, direction, width)

    return {}, None


def _ptr_exec(t, f, state, frame):
    """Advance one instruction's effect on `state` (register name -> known
    32-bit value) and `frame` (I6+offset -> known 32-bit value), both
    mutated in place; -> a `ptr` row tuple or None. Every register
    register_effects() says this instruction DEFINES is either given a
    modelled new value or invalidated -- never left holding a stale value
    from before this instruction, even for a form/opcode this pass does not
    model itself. (register_effects' own I-register naming is g-bank-
    unadjusted for a post-modify def -- see the module note; popping that
    name too, alongside applying this function's own correctly-banked one,
    is at worst an extra, harmless invalidation of an unrelated register,
    never a wrong value.)"""
    defs, _uses, _unknown = register_effects(t, f)
    new_values = dict(_ptr_alu_updates(t, f, state))
    new_values.update(_ptr_reg_updates(t, f, state))
    mem_updates, ptr_row = _ptr_mem_form(t, f, state, frame)
    new_values.update(mem_updates)
    for reg, _kind in defs:
        state.pop(reg, None)
    state.update(new_values)
    return ptr_row


def _ptr_merge(states):
    """A block's in-state (register state or frame-slot state alike): a key
    survives only when every already-visited predecessor's out-state has it
    with the SAME value."""
    if not states:
        return {}
    common = set(states[0])
    for s in states[1:]:
        common &= set(s)
    return {k: states[0][k] for k in common if all(s[k] == states[0][k] for s in states)}


def _ptr_topo_order(entry, starts, succ_by_from, idom):
    """`starts` (one function's bblock start_sw's) in a forward order: a
    plain topological sort of that function's own block graph with loop
    back edges dropped, reusing analyze_image()'s already-computed `idom`
    for the back-edge test (v dominates u -- _detect_dominators_and_loops's
    own test) instead of re-deriving dominance here. Any block the result
    doesn't order (unreachable from `entry`, or on a residual cycle an
    irreducible CFG's single-entry idom doesn't fully break) is appended in
    address order -- _build_ptr_rows's merge only ever consumes an already-
    visited predecessor, so this is a safe (if occasionally lossy)
    approximation, never a wrong one."""
    def dominates(target, node):
        n = node
        while True:
            if n == target:
                return True
            if n == entry:
                return False
            n = idom.get(n, entry)

    block_set = set(starts)
    G = nx.DiGraph()
    G.add_nodes_from(starts)
    for b in starts:
        for to_block, _kind in succ_by_from.get(b, ()):
            if to_block in block_set and not dominates(to_block, b):
                G.add_edge(b, to_block)
    try:
        order = list(nx.topological_sort(G))
    except nx.NetworkXUnfeasible:
        return sorted(starts)
    seen = set(order)
    order += sorted(x for x in starts if x not in seen)
    return order


def _build_ptr_rows(db, name, idom_rows):
    """[(sw, base_reg, base_value, address, direction, width), ...] -- the
    `ptr` table's own rows for one image. See the long module note above
    this section for what is and is not modelled."""
    insns = db.execute(
        "SELECT sw, form, fields FROM insn WHERE image=? AND aligned=1 ORDER BY sw", (name,)
    ).fetchall()
    decoded = {
        sw: (form, sharcinv.merge_fields(json.loads(fields_json)))
        for sw, form, fields_json in insns if form is not None and fields_json is not None
    }
    if not decoded:
        return []

    bblock_rows = db.execute(
        "SELECT start_sw, end_sw, function_sw FROM bblocks WHERE image=?", (name,)
    ).fetchall()
    if not bblock_rows:
        return []  # a ColdFire import: no per-instruction CFG survives it at all.

    by_func = collections.defaultdict(list)
    block_end = {}
    for start, end, fn in bblock_rows:
        block_end[start] = end
        if fn is not None:
            by_func[fn].append(start)
    for starts in by_func.values():
        starts.sort()

    succ_by_from = collections.defaultdict(list)
    preds_by_to = collections.defaultdict(list)
    for from_block, to_block, kind in db.execute(
        "SELECT from_block, to_block, kind FROM succ WHERE image=?", (name,)
    ).fetchall():
        if to_block is not None:
            succ_by_from[from_block].append((to_block, kind))
            preds_by_to[to_block].append(from_block)

    starts_sorted = sorted(block_end)
    block_members = collections.defaultdict(list)
    for sw in sorted(decoded):
        i = bisect.bisect_right(starts_sorted, sw) - 1
        if i < 0:
            continue
        start = starts_sorted[i]
        if start <= sw < block_end[start]:
            block_members[start].append(sw)

    calls_out, callers_in = collections.defaultdict(set), collections.defaultdict(list)
    for from_sw, from_fn, to_fn in db.execute(
        "SELECT from_sw, from_function, to_function FROM edges WHERE image=? AND kind='call' "
        "AND from_function IS NOT NULL AND to_function IS NOT NULL", (name,),
    ).fetchall():
        calls_out[from_fn].add(from_sw)
        callers_in[to_fn].append((from_sw, from_fn))

    idom_by_func = collections.defaultdict(dict)
    for fn, block, idom_block in idom_rows:
        idom_by_func[fn][block] = idom_block
    topo_by_func = {
        fn: _ptr_topo_order(fn, starts, succ_by_from, idom_by_func.get(fn, {}))
        for fn, starts in by_func.items()
    }

    seed, ptr_rows = {}, []
    for _round in range(_PTR_ROUNDS):
        call_state = {}
        block_out, block_out_frame, block_confident = {}, {}, {}
        ptr_rows = []
        for fn, starts in by_func.items():
            block_set = set(starts)
            call_sws = calls_out.get(fn, ())
            for start in topo_by_func[fn]:
                # A block with no CONFIDENT predecessor -- the true entry,
                # or one only an unresolved indirect jump reaches (a common
                # dispatch-table-case shape; succ/bblocks has no edge into
                # it at all, since a JUMP through a register carries no
                # static target -- see FUN_1c642a's own table-driven
                # dispatch) -- falls back to the SAME entry baseline
                # (M-reset + this round's call-site seed) for its register
                # state, but an EMPTY frame (a case's own stack use, if any,
                # is not this pass's business) and is itself marked NOT
                # confident: unlike a real merge input, its guessed baseline
                # must never be allowed to zero out a genuinely-tracked
                # value at some later, real reconvergence point downstream
                # (confirmed against FUN_1c642a directly: without this
                # distinction, one indirect-dispatch case's contentless
                # state wiped the whole function's frame tracking from that
                # point on).
                preds = [
                    p for p in preds_by_to.get(start, ())
                    if p in block_set and p in block_out and block_confident.get(p)
                ]
                if start == fn or not preds:
                    state = dict(_PTR_M_RESET)
                    state.update(seed.get(fn, {}))
                    frame = {}
                    confident = start == fn
                else:
                    state = _ptr_merge([block_out[p] for p in preds])
                    frame = _ptr_merge([block_out_frame[p] for p in preds])
                    confident = True
                for sw in block_members.get(start, ()):
                    if sw in call_sws:
                        call_state[(fn, sw)] = {r: state[r] for r in _PTR_CALL_ARG_REGS if r in state}
                    form, f = decoded[sw]
                    row = _ptr_exec(form, f, state, frame)
                    if row is not None:
                        ptr_rows.append((sw,) + row)
                block_out[start], block_out_frame[start], block_confident[start] = state, frame, confident

        seed = {}
        for fn, calls in callers_in.items():
            snaps = [call_state.get((from_fn, call_sw), {}) for call_sw, from_fn in calls]
            agree = {}
            for reg in _PTR_CALL_ARG_REGS:
                values = [s.get(reg) for s in snaps]
                if values and all(v is not None and v == values[0] for v in values):
                    agree[reg] = values[0]
            if agree:
                seed[fn] = agree

    return ptr_rows


def analyze_image(db, name, mem=None, blocks=None):
    """Fill roots/reach/callgraph/idom/loops/unentered for one already-built
    image (its edges/succ/dataref/literals/functions tables must already
    exist). Idempotent: clears this image's rows from each table first, so
    re-running (e.g. `sharcdb analyze` on a DB built by an older tool
    version) is safe. `mem`/`blocks` are an optional LoadedMemory and its
    parsed block list, for the two root kinds that need to read loaded
    memory directly (see _detect_roots); build_database() always has them
    on hand, `cmd_analyze` reloads them from meta's blob_path when the blob
    is still present locally."""
    t0 = time.time()
    # `ptr` (DB_VERSION 6) may not exist yet on a database `cmd_analyze` is
    # re-running in place from an older tool version -- unlike every other
    # table here, SCHEMA's own CREATE TABLE never ran for it in that case.
    db.execute("""CREATE TABLE IF NOT EXISTS ptr (
        image TEXT, sw INTEGER, base_reg TEXT, base_value INTEGER, address INTEGER,
        direction TEXT, width TEXT)""")
    db.execute("CREATE INDEX IF NOT EXISTS ptr_address ON ptr(image, address)")
    db.execute("CREATE INDEX IF NOT EXISTS ptr_base_value ON ptr(image, base_value)")
    db.execute("CREATE INDEX IF NOT EXISTS ptr_sw ON ptr(image, sw)")

    for table in ("roots", "reach", "callgraph", "idom", "loops", "unentered", "ptr"):
        db.execute("DELETE FROM %s WHERE image=?" % table, (name,))

    function_entries = [r[0] for r in db.execute(
        "SELECT entry_sw FROM functions WHERE image=?", (name,)
    ).fetchall()]

    roots = _detect_roots(db, name, mem, blocks)
    reach_rows = _detect_reach(db, name, roots, function_entries)
    callgraph_rows = _detect_callgraph(db, name, function_entries)
    idom_rows, loop_rows = _detect_dominators_and_loops(db, name)
    unentered_rows = _detect_unentered(db, name, roots)
    ptr_rows = _build_ptr_rows(db, name, idom_rows)

    db.executemany("INSERT INTO roots VALUES (?,?,?,?)", [(name,) + r for r in roots])
    db.executemany("INSERT INTO reach VALUES (?,?,?,?)", [(name,) + r for r in reach_rows])
    db.executemany("INSERT INTO callgraph VALUES (?,?,?,?,?,?,?)", [(name,) + r for r in callgraph_rows])
    db.executemany("INSERT INTO idom VALUES (?,?,?,?)", [(name,) + r for r in idom_rows])
    db.executemany("INSERT INTO loops VALUES (?,?,?,?,?,?)", [(name,) + r for r in loop_rows])
    db.executemany("INSERT INTO unentered VALUES (?,?,?)", [(name,) + r for r in unentered_rows])
    db.executemany("INSERT INTO ptr VALUES (?,?,?,?,?,?,?)", [(name,) + r for r in ptr_rows])

    return {
        "seconds": time.time() - t0, "n_roots": len(roots), "n_reach": len(reach_rows),
        "n_callgraph": len(callgraph_rows), "n_idom": len(idom_rows), "n_loops": len(loop_rows),
        "n_unentered": len(unentered_rows), "n_ptr": len(ptr_rows),
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

    # Build into a temp file in the same directory and os.replace() it into
    # place at the very end, so a concurrent reader of out_path (e.g. an
    # agent running sharcdb.sql queries against out/sharcdb/*.sqlite while
    # this build is in flight) never sees a half-written database -- either
    # the old file (still fully valid) or the new one, never a torn one.
    out_dir = os.path.dirname(os.path.abspath(out_path)) or "."
    os.makedirs(out_dir, exist_ok=True)
    tmp_path = os.path.join(
        out_dir, ".%s.tmp-%d-%d" % (os.path.basename(out_path), os.getpid(), int(time.time() * 1e6))
    )
    try:
        db = open_db(tmp_path)
        try:
            counts = _fill_database(db, name, sha, blob_path, code_blocks, min_depth, ctx)
            counts["analyze"] = analyze_image(db, name, mem=ctx["mem"], blocks=ctx["blocks"])
            db.commit()
        finally:
            db.close()
        os.replace(tmp_path, out_path)
    except BaseException:
        if os.path.exists(tmp_path):
            os.remove(tmp_path)
        raise
    return {
        "name": name, "path": out_path, "skipped": False,
        "seconds": time.time() - t0, "size": os.path.getsize(out_path), **counts,
    }


# --- ColdFire import from a tools/ghidradump.py dump -------------------------
#
# See the DB_VERSION comment above for the full column-by-column schema
# mapping and the tables this deliberately leaves empty.

_GHIDRA_CALL_KIND = {
    "UNCONDITIONAL_CALL": "call",
    "COMPUTED_CALL": "call",
    "UNCONDITIONAL_JUMP": "jump",
    "COMPUTED_JUMP": "jump",
    "CONDITIONAL_JUMP": "cond_jump",
}

_GHIDRA_DATAREF_ROLE = {
    "READ": "read", "WRITE": "write", "READ_WRITE": "read_write", "DATA": "data",
}

_DISASM_LINE = re.compile(r"^[0-9a-f]{8}  ")


def _parse_disasm_insns(path):
    """[(sw, raw_hex, mnemonic), ...] from one tools/ghidradump.py
    disasm/*.s file, in file order -- see that file's disasm_line()'s own
    '%08x  %-24s  %s' format (address, space-joined 16-bit words, rendered
    text). A comment (';...') or label ('name:') line doesn't start with 8
    hex digits and two spaces and is skipped."""
    rows = []
    with open(path) as f:
        for line in f:
            if not _DISASM_LINE.match(line):
                continue
            parts = re.split(r" {2,}", line.rstrip("\n"), maxsplit=2)
            if len(parts) != 3:
                continue
            addr_hex, words, text = parts
            rows.append((int(addr_hex, 16), words.replace(" ", ""), text))
    return rows


def _fill_ghidra_database(db, name, sha, dump_dir, manifest, functions, ranges, calls, data_refs):
    meta_rows = [
        (name, "image", name),
        (name, "image_sha256", sha),
        (name, "db_version", str(DB_VERSION)),
        (name, "tool_commit", git_commit() or ""),
        (name, "build_time", time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())),
        (name, "cpu", "coldfire"),
        (name, "ghidra_dump_dir", os.path.abspath(dump_dir)),
        (name, "ghidra_project", manifest.get("project") or ""),
        (name, "ghidra_program", manifest.get("program") or ""),
        (name, "language", manifest.get("language") or ""),
    ]
    db.executemany("INSERT INTO meta VALUES (?,?,?)", meta_rows)

    callers = collections.defaultdict(set)
    callees = collections.defaultdict(set)
    edge_rows = []
    for from_fn, to_fn, to_addr, site, kind in calls:
        if to_fn is not None:
            callees[from_fn].add(to_fn)
            callers[to_fn].add(from_fn)
        edge_rows.append((name, site, to_addr, _GHIDRA_CALL_KIND.get(kind, "jump"), None, None,
                          from_fn, to_fn, kind))

    dataref_rows = [
        (name, to_addr, site, "ghidra", _GHIDRA_DATAREF_ROLE.get(kind, kind.lower()))
        for site, _func, to_addr, kind in data_refs
    ]

    func_rows, insn_rows, decomp_rows = [], [], []
    for entry, fname, size, decomp_path, disasm_path in functions:
        # A Ghidra function's body can be several disjoint ranges (a switch
        # whose case bodies live far from the entry, seen on ~130 functions
        # in dt2-1.16-emac -- e.g. ParameterSet::vfunc_23 spans entry..entry
        # +141 plus a dozen scattered chunks up near 0x400e2258). functions.
        # end_sw only covers the ENTRY's own contiguous range (the one whose
        # lo == entry), not every scattered chunk: using max(hi) across all
        # ranges instead made func()'s [entry_sw, end_sw) span swallow
        # unrelated code in between -- confirmed by hand on 0x400caf48, which
        # sits inside its own small FUN_400cae8c, not inside vfunc_23's
        # bogus multi-megabyte span. A scattered function's other chunks are
        # simply not covered by any functions row here (func() on an address
        # in one of them returns None or, if another function's own entry
        # range happens to include it, that other function -- never this
        # one); their instructions are still in `insn` (grouped by disasm
        # file, i.e. by owning function, not by this span).
        spans = ranges.get(entry) or [(entry, entry + max(size, 1) - 1)]
        entry_span = next((hi for lo, hi in spans if lo == entry), max(hi for _lo, hi in spans))
        end_sw = entry_span + 1
        insns = _parse_disasm_insns(os.path.join(dump_dir, disasm_path)) if disasm_path else []
        for sw, raw_hex, mnemonic in insns:
            insn_rows.append((name, sw, None, len(raw_hex) // 2, raw_hex, None, None, None,
                              mnemonic, 1, None, entry))
        func_rows.append((
            name, entry, end_sw, len(insns), None, fname,
            int(bool(callers.get(entry))), int(not callees.get(entry)), None, None,
        ))
        if decomp_path:
            full = os.path.join(dump_dir, decomp_path)
            if os.path.exists(full):
                with open(full) as f:
                    decomp_rows.append((name, entry, f.read()))

    db.executemany("INSERT INTO functions VALUES (?,?,?,?,?,?,?,?,?,?)", func_rows)
    db.executemany("INSERT INTO insn VALUES (?,?,?,?,?,?,?,?,?,?,?,?)", insn_rows)
    db.executemany("INSERT INTO edges VALUES (?,?,?,?,?,?,?,?,?)", edge_rows)
    db.executemany("INSERT INTO dataref VALUES (?,?,?,?,?)", dataref_rows)
    db.executemany("INSERT INTO decomp VALUES (?,?,?)", decomp_rows)

    return {
        "n_functions": len(func_rows), "n_insn": len(insn_rows), "n_edges": len(edge_rows),
        "n_dataref": len(dataref_rows), "n_decomp": len(decomp_rows),
    }


def build_ghidra_database(dump_dir, out_path, name=None, force=False):
    """Import a tools/ghidradump.py dump directory (manifest.json, xrefs.sqlite,
    disasm/, decomp/) into the same sharcdb schema as build_database(), for
    a ColdFire image. Returns a stats dict; see the DB_VERSION comment for
    the exact column mapping and the tables this leaves empty."""
    t0 = time.time()
    manifest_path = os.path.join(dump_dir, "manifest.json")
    if not os.path.exists(manifest_path):
        raise SystemExit("sharcdb import-ghidra: no manifest.json in %s" % dump_dir)
    with open(manifest_path) as f:
        manifest = json.load(f)
    if not manifest.get("complete"):
        raise SystemExit("sharcdb import-ghidra: %s is not a complete dump (manifest 'complete' is false)" % dump_dir)
    sha = manifest.get("image_sha256")
    if not sha:
        raise SystemExit("sharcdb import-ghidra: %s has no image_sha256 in its manifest" % dump_dir)
    if name is None:
        name = os.path.basename(os.path.normpath(dump_dir))

    if not force and os.path.exists(out_path):
        existing = read_meta(out_path)
        if existing.get("image_sha256") == sha and existing.get("db_version") == str(DB_VERSION):
            return {
                "name": name, "path": out_path, "skipped": True,
                "seconds": time.time() - t0, "size": os.path.getsize(out_path),
            }

    xrefs_path = os.path.join(dump_dir, "xrefs.sqlite")
    src = sqlite3.connect(xrefs_path)
    try:
        functions = src.execute(
            "SELECT entry, name, size, decomp, disasm FROM functions ORDER BY entry"
        ).fetchall()
        ranges = collections.defaultdict(list)
        for func, lo, hi in src.execute("SELECT func, lo, hi FROM function_ranges"):
            ranges[func].append((lo, hi))
        calls = src.execute("SELECT from_func, to_func, to_addr, site, kind FROM calls").fetchall()
        data_refs = src.execute("SELECT site, func, to_addr, kind FROM data_refs").fetchall()
    finally:
        src.close()

    out_dir = os.path.dirname(os.path.abspath(out_path)) or "."
    os.makedirs(out_dir, exist_ok=True)
    tmp_path = os.path.join(
        out_dir, ".%s.tmp-%d-%d" % (os.path.basename(out_path), os.getpid(), int(time.time() * 1e6))
    )
    try:
        db = open_db(tmp_path)
        try:
            counts = _fill_ghidra_database(db, name, sha, dump_dir, manifest, functions, ranges, calls, data_refs)
            counts["analyze"] = analyze_image(db, name, mem=None, blocks=None)
            db.commit()
        finally:
            db.close()
        os.replace(tmp_path, out_path)
    except BaseException:
        if os.path.exists(tmp_path):
            os.remove(tmp_path)
        raise
    return {
        "name": name, "path": out_path, "skipped": False,
        "seconds": time.time() - t0, "size": os.path.getsize(out_path), **counts,
    }


def cmd_import_ghidra(args):
    name = args.name or os.path.basename(os.path.normpath(args.dump_dir))
    out = args.out or os.path.join(args.out_dir, name + ".sqlite")
    try:
        r = build_ghidra_database(args.dump_dir, out, name=name, force=args.force)
    except SystemExit as exc:
        print("FAILED  %s  %s" % (name, exc))
        return 1
    if r["skipped"]:
        print("%-16s SKIPPED (up to date)  %s  %.1fs" % (r["name"], r["path"], r["seconds"]))
        return 0
    print(
        "%-16s built  %s  %.1fs  %.1f KB  functions=%d insn=%d edges=%d dataref=%d decomp=%d"
        % (r["name"], r["path"], r["seconds"], r["size"] / 1024, r["n_functions"], r["n_insn"],
           r["n_edges"], r["n_dataref"], r["n_decomp"])
    )
    a = r.get("analyze") or {}
    if a:
        print(
            "    analyze  %.1fs  roots=%d reach=%d callgraph=%d idom=%d loops=%d unentered=%d ptr=%d"
            % (a["seconds"], a["n_roots"], a["n_reach"], a["n_callgraph"], a["n_idom"],
               a["n_loops"], a["n_unentered"], a.get("n_ptr", 0))
        )
    return 0


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
                "mem_access=%d func_hash=%d bblocks=%d succ=%d dataref=%d regdef=%d reguse=%d"
                % (r["name"], r["path"], r["seconds"], r["size"] / 1024, r.get("n_insn", 0),
                   r.get("n_functions", 0), r.get("n_edges", 0), r.get("n_literals", 0),
                   r.get("n_mem_access", 0), r.get("n_func_hash", 0), r.get("n_bblocks", 0),
                   r.get("n_succ", 0), r.get("n_dataref", 0), r.get("n_regdef", 0), r.get("n_reguse", 0))
            )
            a = r.get("analyze") or {}
            if a:
                print(
                    "    analyze  %.1fs  roots=%d reach=%d callgraph=%d idom=%d loops=%d unentered=%d ptr=%d"
                    % (a["seconds"], a["n_roots"], a["n_reach"], a["n_callgraph"], a["n_idom"],
                       a["n_loops"], a["n_unentered"], a.get("n_ptr", 0))
                )
            if r.get("unknown_form_counts"):
                print("    unknown regdef/reguse forms: " +
                      ", ".join("%s=%d" % kv for kv in sorted(r["unknown_form_counts"].items())))
    return 0 if ok else 1


def cmd_analyze(args):
    """Re-run analyze_image() over one or more already-built databases,
    in place (temp-copy + os.replace, same atomicity as a build)."""
    ok = True
    for path in args.dbs:
        if not os.path.exists(path):
            print("FAILED  %s  no such file" % path)
            ok = False
            continue
        meta = read_meta(path)
        name = meta.get("image")
        if not name:
            print("FAILED  %s  not a sharcdb database (no meta.image)" % path)
            ok = False
            continue

        mem, blocks = None, None
        blob_path = meta.get("blob_path")
        if blob_path and os.path.exists(blob_path):
            with open(blob_path, "rb") as fh:
                data = fh.read()
            blocks = sharcldr.parse_blocks(data)
            mem = sharcldr.LoadedMemory.from_stream(data, blocks)
        else:
            print("    %-16s blob not found (%s); loader_entry/code_pointer_array/"
                  "interrupt_vector roots skipped" % (name, blob_path))

        out_dir = os.path.dirname(os.path.abspath(path)) or "."
        tmp_path = os.path.join(
            out_dir, ".%s.tmp-%d-%d" % (os.path.basename(path), os.getpid(), int(time.time() * 1e6))
        )
        try:
            shutil.copy2(path, tmp_path)
            db = sqlite3.connect(tmp_path)
            try:
                stats = analyze_image(db, name, mem=mem, blocks=blocks)
                # Keep meta.db_version current so a later `build` (without
                # --force) doesn't see a stale version and redo the whole
                # decode just to re-stamp it.
                db.execute("INSERT OR REPLACE INTO meta VALUES (?,?,?)", (name, "db_version", str(DB_VERSION)))
                db.commit()
            finally:
                db.close()
            os.replace(tmp_path, path)
        except BaseException:
            if os.path.exists(tmp_path):
                os.remove(tmp_path)
            raise
        print(
            "%-16s analyzed  %s  %.1fs  roots=%d reach=%d callgraph=%d idom=%d loops=%d unentered=%d ptr=%d"
            % (name, path, stats["seconds"], stats["n_roots"], stats["n_reach"], stats["n_callgraph"],
               stats["n_idom"], stats["n_loops"], stats["n_unentered"], stats.get("n_ptr", 0))
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

    a = sub.add_parser("analyze", help="re-run the roots/reach/callgraph/idom/loops/unentered pass")
    a.add_argument("dbs", nargs="+", help="out/sharcdb/<image>.sqlite path(s)")

    g = sub.add_parser("import-ghidra", help="import a tools/ghidradump.py dump as a ColdFire image database")
    g.add_argument("dump_dir", help="e.g. out/ghidra/dt2-1.16-emac")
    g.add_argument("--name", help="image name; default is the dump directory's own name")
    g.add_argument("--out", help="output .sqlite path (default: --out-dir/<name>.sqlite)")
    g.add_argument("--out-dir", default="out/sharcdb")
    g.add_argument("--force", action="store_true", help="rebuild even if sha256+DB_VERSION already match")
    args = ap.parse_args(argv)

    if args.cmd == "build":
        return cmd_build(args)
    if args.cmd == "import-ghidra":
        return cmd_import_ghidra(args)
    if args.cmd == "analyze":
        return cmd_analyze(args)
    return 1


if __name__ == "__main__":
    sys.exit(main())
