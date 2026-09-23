#!/usr/bin/env python3
# pyright: reportArgumentType=false, reportOptionalSubscript=false, reportReturnType=false
"""Function dossier for SHARC+ VISA code: everything a reader needs before
looking at one instruction, built from tools/sharcldr.py, tools/sharcflow.py,
tools/sharcinv.py and tools/sharc_disasm.py rather than reimplemented.

    uv run python tools/sharcfn.py BLOB.bin ADDR
        [--json OUT] [--listing] [--min-depth N] [--blocks LIST]
    uv run python tools/sharcfn.py BLOB.bin --batch ADDR,ADDR,...
        --out-dir DIR [--min-depth N] [--blocks LIST]

BLOB.bin is the raw SHARC boot stream (out/sections/dt2-1.16/section_7_BLOB.bin).
ADDR is a short-word (VISA PC) address, hex (0x...) or decimal. If it does not
name a function's own entry, the dossier says so and uses the function whose
return-delimited span contains it (tools/sharcinv.py's function_bounds()).
Plain `sharcfn.py BLOB ADDR` prints identification, bounds, call graph and
the summary; `--listing` (on by default for a single address, off for
--batch unless given) also prints the annotated instruction-by-instruction
listing, the tool's main deliverable.

A dossier has five parts:

1. Identification -- block, the block's base_sw convention stated in words,
   file offset of the entry, and the image sha256 (checked against the
   known dt2-1.16 hash when it matches; otherwise just reported).
2. Bounds -- entry, exit, instruction count and how they were found
   (tools/sharcinv.py's two-pass return-delimited-span-then-interior-call-
   target scheme), with both known boundary hazards flagged: an internal
   branch target inside the span that is not itself a function entry, and a
   live exit that jumps backward into a preceding span's shared epilogue.
3. Call graph -- callers/callees/unresolved callees from
   tools/sharcinv.py's whole-inventory cross-reference (so cross-block calls
   resolve too), conditional vs unconditional and delayed vs non-delayed
   Type 8a calls, indirect calls, and a local scan for conditional returns
   (a 9b_abs jump through I4/M6 whose cond is not 31 -- sharcflow.py only
   matches the unconditional return word).
4. The annotated listing -- sw address, raw bytes, form name, a readable
   mnemonic with resolved operands, and inline annotations: literal
   addresses resolved to a named region (RAM or ROM, checked against the
   loaded blocks), float immediates decoded as IEEE-754 and flagged against
   a short list of known constants, loop trip counts with their body range,
   and calls with their resolved target and inventory label.
5. Summary -- the tools/sharcinv.py feature vector, named tables touched,
   whether the parameter frame is referenced, and the distinct literal
   regions used.

Mnemonic rendering decodes compute-field register operands from the public
SHARC+ Core Programming Reference's Figure 18-1/18-2/18-3 and Table
18-11/13/15/16/18/19/21 bit layouts (tools/sharcspec/compute_table.json's
figures_and_bit_layouts), using tools/sharcinv.py's ALU_OPS/SHIFT_OPS/
SHORT_OPS opcode-name tables (built from the same file's aluop/shiftop/
shortcompute rows) rather than re-deriving them, and tools/sharc_trace.py's
UREG_NAMES/UREG_CODES for register-file/status-register names rather than
recreating that 128-entry table. A handful of opcodes are called out in this
tool's task brief as unverified even where a name table already has an
entry for them (ALU 0xe0/0xd9/0xda, shifter 0xb0, multifunction 0x1a/0x1e/
0x1f): these always print with a loud "GAP" marker next to whatever name is
on file, never silently. Type10a_rel (and, as measured here, Type10a_abs
too) is absent from tools/sharc_visa_tables.py's VISA form set entirely, so
any real occurrence surfaces as a disassembler desync -- resynced and
reported like tools/fullscan.py does, not silently skipped. Forms with no
dedicated renderer here fall back to a field dump (name=value, one line)
plus named-region annotation on any address-shaped field -- never skipped,
per the same "loud gap beats silent gap" rule.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import sqlite3
import sys
from collections import Counter

_here = os.path.dirname(os.path.abspath(__file__))
if _here not in sys.path:
    sys.path.insert(0, _here)

import sharcflow  # noqa: E402
import sharcimm  # noqa: E402
import sharcinv  # noqa: E402
import sharcldr  # noqa: E402
from sharc_trace import UREG_CODES, UREG_NAMES  # noqa: E402

EXPECTED_SHA256 = {
    "0f514a12a2255f5c081e292c47f1f29462003177658da4bbae0a22fd737fffa2": "dt2-1.16 section_7_BLOB.bin",
}

# --- named regions not already in sharcinv.NAMED_TABLES --------------------
#
# tools/sharcinv.py's NAMED_TABLES only carries the tables its FFT labeler
# needs (cosine pair, 1024-float pair, exp829, table32, stage5 index table).
# The rest of the task's list of known regions lives here instead of being
# duplicated into that file.
EXTRA_NAMED_REGIONS = {
    0x264138: "ring_head_C",
    0x264170: "ring_head_D",
    0x256388: "pair128_a",
    0x256588: "pair128_b",
    0x252D3C: "shared_context",
    0x25D940: "ram_table_25d940",
    0x2C2CC0: "ram_table128_2c2cc0",
    0x2C2018: "ram_table1024_2c2018",
}
RING_BUFFER_NAMES = {
    0x262138: "ring_C_buf0",
    0x262938: "ring_C_buf1",
    0x263138: "ring_D_buf0",
    0x263938: "ring_D_buf1",
}
PARAM_FRAME_LO, PARAM_FRAME_HI = sharcinv.FRAME_LO, sharcinv.FRAME_HI
PARAM_TRACK_BASE = 0x2559B6
PARAM_TRACK_STRIDE = 0x60
PARAM_SRC_PAGE_LEN = 0x14

# Any DM address in this band, or one matching a named region below, is
# worth reporting RAM/ROM for; a plain compute immediate that happens to
# decode as a plausible float is not.
_ADDR_LIKE_BAND = (0x200000, 0x30000000)


def name_literal_region(value: int):
    """Human label for a 32-bit literal, or None. Prefers
    tools/sharcinv.py's NAMED_TABLES (the canonical list this tool does not
    duplicate); falls through EXTRA_NAMED_REGIONS, the audio ring buffers,
    the per-track parameter frame, then tools/sharcimm.py's peripheral-
    register namer."""
    if value in sharcinv.NAMED_TABLES:
        return sharcinv.NAMED_TABLES[value]
    if value in EXTRA_NAMED_REGIONS:
        return EXTRA_NAMED_REGIONS[value]
    if value in RING_BUFFER_NAMES:
        return RING_BUFFER_NAMES[value]
    if PARAM_FRAME_LO <= value <= PARAM_FRAME_HI:
        if value == PARAM_FRAME_LO:
            return "param_frame_base"
        off = value - PARAM_TRACK_BASE
        if off >= 0:
            track, byte = divmod(off, PARAM_TRACK_STRIDE)
            page = " (SRC page)" if byte < PARAM_SRC_PAGE_LEN else ""
            return "param_frame:track%d+0x%x%s" % (track, byte, page)
        return "param_frame+0x%x" % (value - PARAM_FRAME_LO)
    peripheral = sharcimm.name_address(value)
    if peripheral:
        return "peripheral:" + peripheral
    return None


def _is_addr_like(value: int, region) -> bool:
    lo, hi = _ADDR_LIKE_BAND
    return bool(region) or lo <= value < hi or (value >> 24) == 0x80


# --- float immediates --------------------------------------------------

# name, exact value; matched within a small relative tolerance.
KNOWN_FLOAT_CONSTANTS = (
    ("pi", 3.14159265358979),
    ("pi/8", 0.39269908169872414),
    ("ln(10)", 2.302585092994046),
    ("sqrt(10)", 3.1622776601683795),
    ("2^32", 4294967296.0),
    ("96000.0 (sample rate)", 96000.0),
    ("1/12 (semitone)", 1.0 / 12),
    ("220.0", 220.0),
    ("127.0", 127.0),
    ("128.0", 128.0),
)


def float_constant_name(value: float):
    for name, const in KNOWN_FLOAT_CONSTANTS:
        tol = max(abs(const), 1.0) * 1e-4
        if abs(value - const) <= tol:
            return name
    return None


def describe_literal(value: int, float_capable: bool, mem: sharcldr.LoadedMemory):
    """-> dict describing a 32-bit literal: hex, region (if any), RAM/ROM
    (only when it looks address-shaped), and, if float_capable, the
    IEEE-754 decode and any matched named constant."""
    value &= 0xFFFFFFFF
    out = {"hex": "0x%08x" % value}
    region = name_literal_region(value)
    if region:
        out["region"] = region
    if _is_addr_like(value, region):
        out["memory"] = "ROM" if mem.read(value, 1) is not None else "RAM"
    if float_capable:
        f = sharcinv.float32(value)
        if sharcinv._plausible_float(f) or region is None:
            out["float"] = f
            name = float_constant_name(f)
            if name:
                out["float_constant"] = name
    return out


def _annotate_literal(value: int, float_capable: bool, mem) -> str:
    d = describe_literal(value, float_capable, mem)
    bits = []
    if "float" in d:
        bits.append("%.6g" % d["float"])
        if "float_constant" in d:
            bits.append("== " + d["float_constant"])
    if "region" in d:
        bits.append(d["region"])
    if "memory" in d:
        bits.append(d["memory"])
    return " ; ".join(bits)


# --- register naming ---------------------------------------------------


def reg_name(code: int, is_float: bool) -> str:
    return ("F%d" if is_float else "R%d") % code


def ureg_name(code: int) -> str:
    if 0 <= code < len(UREG_NAMES):
        return UREG_NAMES[code]
    return "UREG%d" % code


# --- compute-field decode -----------------------------------------------
#
# Bit layouts from the SHARC+ Core Programming Reference, geometrically
# extracted into tools/sharcspec/compute_table.json's figures_and_bit_layouts
# (Figure 18-1/18-2/18-3, Table 18-11/13/15/16/18/19/21). Opcode->name tables
# reused from tools/sharcinv.py (built from the same manual's Table
# 18-2/18-5/18-7/18-9 rows) rather than re-derived here.

ALU_OPS = sharcinv.ALU_OPS
SHIFT_OPS = sharcinv.SHIFT_OPS
SHORT_OPS = sharcinv.SHORT_OPS
FLOAT_SHORT_OPS = sharcinv.FLOAT_SHORT_OPS

UNARY_ALU_OPS = {
    "pass",
    "neg",
    "inc",
    "dec",
    "abs",
    "fpass",
    "fneg",
    "frnd",
    "mant",
    "fabs",
    "fix",
    "float",
    "trunc",
    "logb",
    "recips",
    "rsqrts",
    "not",
    "inc_c",
    "dec_c",
}

# Task-flagged decode gaps: opcodes to mark loudly even when a name table
# already carries an entry for them (see module docstring).
GAP_ALU_OPCODES = {0xE0, 0xD9, 0xDA}
GAP_SHIFT_OPCODES = {0xB0}
GAP_MULTIFN_OPCODES = {0x1A, 0x1E, 0x1F}


def _load_multifn_alu_syntax():
    """{opcode_21_16 (6-bit int): syntax string} from
    tools/sharcspec/compute_table.json's multifn_mul_alu (PGR Table 12-12).

    This is a DIFFERENT numbering from tools/sharcinv.py's ALU_OPS
    (PRM Table 18-5, the standalone-ALU opcode space): the fused MUL+ALU
    multifunction op's ALU sub-operation is selected by a 6-bit field at
    bits 21:16 of the compute field (cu[1:0] plus the low 4 bits of what
    Figure 18-1 calls opcode[7:4]), not by the 8-bit ALU_OPS opcode space.
    Read once at import time; never re-derived per instruction."""
    path = os.path.join(_here, "sharcspec", "compute_table.json")
    with open(path) as fh:
        table = json.load(fh)["multifn_mul_alu"]
    return {int(row["opcode_21_16"], 2): row["syntax"] for row in table["rows"]}


MULTIFN_ALU_SYNTAX = _load_multifn_alu_syntax()


def _gap_mark(gap: bool) -> str:
    return "  !!GAP!!" if gap else ""


def decode_alu(opcode: int, field23: int, is_dual: bool, is_float: bool):
    if is_dual:
        # cu=0, opcode[19:16]=0111/1111 identifies dual add/subtract; the
        # rest of `opcode` (its low nibble, bits 15:12) is Rs/Fs, not an
        # ALU_OPS opcode (see tools/sharcinv.py's classify_compute), so no
        # name lookup applies -- structurally fixed, never a gap.
        # Table 18-13: Rs/Fs 15:12, Ra/Fa 11:8, Rx/Fx 7:4, Ry/Fy 3:0.
        rs, ra, rx, ry = ((field23 >> sh) & 0xF for sh in (12, 8, 4, 0))
        Rs, Ra, Rx, Ry = (reg_name(r, is_float) for r in (rs, ra, rx, ry))
        text = "%s = %s + %s; %s = %s - %s" % (Ra, Rx, Ry, Rs, Rx, Ry)
        return text, False
    name = ALU_OPS.get(opcode)
    gap = opcode in GAP_ALU_OPCODES or name is None
    label = name or "alu?0x%02x" % opcode
    # Table 18-11: RN/FN 11:8, Rx/Fx 7:4, Ry/Fy 3:0.
    rn, rx, ry = ((field23 >> sh) & 0xF for sh in (8, 4, 0))
    Rn, Rx, Ry = (reg_name(r, is_float) for r in (rn, rx, ry))
    if name in UNARY_ALU_OPS:
        text = "%s = %s(%s)" % (Rn, label, Rx)
    else:
        text = "%s = %s(%s, %s)" % (Rn, label, Rx, Ry)
    return text + _gap_mark(gap), gap


def decode_mult(
    opcode: int,
    field23: int,
    is_float: bool,
    housekeeping: bool,
    is_mac: bool,
    is_plain_mul: bool,
):
    # Table 18-11 register positions apply here too (single-function compute).
    # cu/opcode classification (housekeeping/plain-mul/MAC, including the
    # opcode==0x30 "FN = FX*FY" special case) comes from
    # tools/sharcinv.py's classify_compute() -- PRM Table 18-7 -- not
    # recomputed here, so this always agrees with the feature-vector counts.
    rn, rx, ry = ((field23 >> sh) & 0xF for sh in (8, 4, 0))
    Rn, Rx, Ry = (reg_name(r, is_float) for r in (rn, rx, ry))
    if housekeeping:
        return "MR housekeeping (opcode 0x%02x)" % opcode, False
    if is_plain_mul:
        return "%s = %s * %s" % (Rn, Rx, Ry), False
    if is_mac:
        top2 = (opcode >> 6) & 3
        op = "MR + %s * %s  (MAC add)" if top2 == 2 else "MR - %s * %s  (MAC sub)"
        return ("%s = " + op) % (Rn, Rx, Ry), False
    return "mult?0x%02x(%s, %s) -> %s  !!GAP!!" % (opcode, Rx, Ry, Rn), True


def decode_shift(opcode: int, field23: int):
    name = SHIFT_OPS.get(opcode)
    gap = opcode in GAP_SHIFT_OPCODES or name is None
    label = name or "shift?0x%02x" % opcode
    rn, rx, ry = ((field23 >> sh) & 0xF for sh in (8, 4, 0))
    text = "R%d = %s(R%d, R%d)" % (rn, label, rx, ry)
    return text + _gap_mark(gap), gap


_MULTIFN_SUBS_ORDER = (
    "R3-0",
    "F3-0",
    "R7-4",
    "F7-4",
    "RXA",
    "FXA",
    "RYA",
    "FYA",
    "RM",
    "FM",
    "RA",
    "FA",
)


def _render_multifn_alu_from_table(field23: int):
    """MUL+ALU multifunction ALU sub-op, decoded from PGR Table 12-12's own
    syntax rows (see MULTIFN_ALU_SYNTAX) rather than tools/sharcinv.py's
    standalone ALU_OPS -- a different numbering (see that table's loader).
    -> rendered text, or None if the 6-bit selector isn't one of its rows
    (reserved/undefined -- a real gap, not a lookup miss)."""
    selector = (field23 >> 16) & 0x3F
    syntax = MULTIFN_ALU_SYNTAX.get(selector)
    if syntax is None:
        return None
    rya = (field23 & 3) + 12
    rxa = ((field23 >> 2) & 3) + 8
    rym = ((field23 >> 4) & 3) + 4
    rxm = ((field23 >> 6) & 3) + 0
    ra = (field23 >> 8) & 0xF
    rm = (field23 >> 12) & 0xF
    values = {
        "R3-0": rxm,
        "F3-0": rxm,
        "R7-4": rym,
        "F7-4": rym,
        "RXA": rxa,
        "FXA": rxa,
        "RYA": rya,
        "FYA": rya,
        "RM": rm,
        "FM": rm,
        "RA": ra,
        "FA": ra,
    }
    text = syntax
    for token in _MULTIFN_SUBS_ORDER:
        text = text.replace(token, "%s%d" % (token[0], values[token]))
    return text


def decode_multifn(top3: int, opcode: int, field23: int):
    # Table 18-18/19 (MUL+dual-add/sub, bits 19:0: Rs/Fs 19:16 then the
    # MUL+ALU 16-bit layout below) -- both its rows (fixed/float) are a
    # single fixed form each (no further opcode sub-table), so this needs
    # no lookup. The four INPUT operands are 2-bit fields restricted to a
    # fixed quad (register_operand_encoding.restricted_2bit_fields): Rxm in
    # R0-3, Rym in R4-7, Rxa in R8-11, Rya in R12-15.
    is_float = bool(top3 & 1)
    is_dual = top3 in (6, 7)
    if is_dual:
        rya = (field23 & 3) + 12
        rxa = ((field23 >> 2) & 3) + 8
        rym = ((field23 >> 4) & 3) + 4
        rxm = ((field23 >> 6) & 3) + 0
        ra = (field23 >> 8) & 0xF
        rm = (field23 >> 12) & 0xF
        rs = (field23 >> 16) & 0xF
        parts = [
            "%s = %s * %s"
            % (
                reg_name(rm, is_float),
                reg_name(rxm, is_float),
                reg_name(rym, is_float),
            ),
            "%s = %s + %s"
            % (
                reg_name(ra, is_float),
                reg_name(rxa, is_float),
                reg_name(rya, is_float),
            ),
            "%s = %s - %s"
            % (
                reg_name(rs, is_float),
                reg_name(rxa, is_float),
                reg_name(rya, is_float),
            ),
        ]
        return "; ".join(parts), False
    text = _render_multifn_alu_from_table(field23)
    if text is not None:
        gap = ((field23 >> 16) & 0x3F) in GAP_MULTIFN_OPCODES
        return text + _gap_mark(gap), gap
    return "multifn_alu?0x%02x(field23=0x%06x)" % (opcode, field23) + _gap_mark(
        True
    ), True


def render_compute(field23: int):
    """-> (mnemonic or None, gap_flag) for a 23-bit compute field, using
    tools/sharcinv.py's classify_compute() to identify cu/opcode and the
    Figure/Table bit layouts above for the register operands."""
    if not field23:
        return None, False
    cu, d = sharcinv.classify_compute(field23)
    opcode = d.get("opcode", 0)
    if cu == "ALU":
        return decode_alu(
            opcode, field23, d.get("is_dual_addsub", False), d.get("is_float", False)
        )
    if cu == "MULT":
        return decode_mult(
            opcode,
            field23,
            d.get("is_float", False),
            d.get("housekeeping", False),
            d.get("is_mac", False),
            d.get("is_plain_mul", False),
        )
    if cu == "SHIFT":
        return decode_shift(opcode, field23)
    if cu == "MULTIFN":
        top3 = (field23 >> 20) & 7
        return decode_multifn(top3, opcode, field23)
    return "cu?%r opcode=0x%02x" % (cu, opcode), False


_BINARY_SHORT_OPS = {0x0, 0x1, 0x3, 0x7, 0x8, 0x9, 0xB, 0xC, 0xD, 0xE, 0xF}
_UNARY_RX_SHORT_OPS = {0x2, 0x4, 0xA}  # pass, not, float
_UNARY_RN_SHORT_OPS = {0x5, 0x6}  # inc, dec (operate on RN itself)


def render_shortcompute(field12: int):
    """Type 2c: Figure 18-2, opcode 11:8, RN 7:4, RX 3:0 (RN is both the
    Y input and the result -- PRM Table 18-21's note)."""
    opcode = (field12 >> 8) & 0xF
    rn = (field12 >> 4) & 0xF
    rx = field12 & 0xF
    is_float = opcode in FLOAT_SHORT_OPS
    name = SHORT_OPS.get(opcode, "short?0x%x" % opcode)
    Rn, Rx = reg_name(rn, is_float), reg_name(rx, is_float)
    if opcode in _UNARY_RN_SHORT_OPS:
        return "%s = %s(%s)" % (Rn, name, Rn)
    if opcode in _UNARY_RX_SHORT_OPS:
        return "%s = %s(%s)" % (Rn, name, Rx)
    return "%s = %s(%s, %s)" % (Rn, name, Rn, Rx)


# --- per-form rendering --------------------------------------------------


def sign_extend(value: int, bits: int) -> int:
    return sharcinv.sign_extend(value, bits)


# PGR Table 10-4 (p.10-33, "Condition and Termination Opcodes"): the 5-bit
# COND field's 32 codes, shared by every IF-conditional compute/branch/
# memory form (Type 2a, 3a/3b/3d, 4a/4b/4d, 5a/5b move+swap, 6a_mem/
# 6a_nomem/6b_shiftimm, 7a/7b, 8a, 9a/9b, 10a_rel, 11a/11c). This is a
# display-only name table -- tools/sharc_trace.py's SIMPLE_COND_BITS and
# _predicate() separately implement the ASTATX-bit semantics for the
# subset of codes it can resolve statically; the two are not meant to be
# merged; this one only has to spell the code the manual gives it. Code
# 0x1F is "TRUE/FOREVER": the PGR footnote says an unspecified COND is
# this value and the compute/branch always executes, so callers never
# print an "IF" for it. 0x0F ("LCE/NOT LCE") is documented only as a
# DO-UNTIL termination test, not a valid IF condition; it is listed here
# only so an occurrence still gets a manual-traceable name instead of a
# bare number.
COND_NAMES = {
    0x00: "EQ", 0x10: "NE",
    0x01: "LT", 0x11: "GE",
    0x02: "LE", 0x12: "GT",
    0x03: "AC", 0x13: "NOT AC",
    0x04: "AV", 0x14: "NOT AV",
    0x05: "MV", 0x15: "NOT MV",
    0x06: "MS", 0x16: "NOT MS",
    0x07: "SV", 0x17: "NOT SV",
    0x08: "SZ", 0x18: "NOT SZ",
    0x09: "FLAG0", 0x19: "NOT FLAG0",
    0x0A: "FLAG1", 0x1A: "NOT FLAG1",
    0x0B: "FLAG2", 0x1B: "NOT FLAG2",
    0x0C: "FLAG3", 0x1C: "NOT FLAG3",
    0x0D: "TF", 0x1D: "NOT TF",
    0x0E: "BM/SF1", 0x1E: "NOT BM/SF1",
    0x0F: "LCE",  # DO-UNTIL termination code only, not a real IF condition
    0x1F: "TRUE",
}
ALWAYS_TRUE_COND = 0x1F


def cond_name(cond: int) -> str:
    return COND_NAMES.get(cond, "cond%d" % cond)


def cond_prefix(f: dict) -> str:
    """'IF <name> ' for a form whose merged fields carry a real, non-
    always-true condition; '' when the form has no "cond" field at all
    (e.g. Type2a_short/2c/2b: PRM/decode_table.json give these no COND
    bits whatsoever -- they are a structurally different, unconditional
    encoding, not just an always-true instance of Type2a) or when the
    condition is 0x1F (already always true, so printing it would only add
    noise to every unconditional listing line)."""
    cond = f.get("cond")
    if cond is None or cond == ALWAYS_TRUE_COND:
        return ""
    return "IF %s " % cond_name(cond)


def _space_dir(f: dict) -> str:
    space = "PM" if f.get("g") else "DM"
    direction = "store" if f.get("d") else "load"
    return space, direction


def render_mem_direct(sw, f, kind):
    """15a/14a/14d: direct DM/PM addressing."""
    addr = f.get("addr", 0)
    ureg = f.get("ureg")
    dreg = f.get("dreg")
    reg = (
        ureg_name(ureg)
        if ureg is not None
        else ("R%d" % dreg if dreg is not None else "?")
    )
    space, direction = _space_dir(f)
    long_ = ", long" if f.get("l") else ""
    if direction == "store":
        return "%s(0x%x) = %s%s" % (space, addr, reg, long_), addr, False
    return "%s = %s(0x%x)%s" % (reg, space, addr, long_), addr, False


def render_mem_indexed(sw, f):
    """3a/3b/3d/6a_mem/1b-style i,m-mod addressing: DM/PM(Ii, Mj), post-
    modified by Mj; u distinguishes the addressing variant (not resolved
    further here -- printed as-is).

    3a/3b/3d name their register with the wide "ureg" field; 6a_mem instead
    has a plain 4-bit "dreg" (R0-R15) -- same fallback tools/sharcfn.py's
    render_mem_immoff() already uses for 4a/4b/4d/15b."""
    i = f.get("i", 0)
    m = f.get("m", 0)
    dreg = f.get("dreg")
    ureg = f.get("ureg")
    reg = (
        "R%d" % dreg
        if dreg is not None
        else (ureg_name(ureg) if ureg is not None else "?")
    )
    space, direction = _space_dir(f)
    u = f.get("u")
    u_note = " u=%d" % u if u is not None else ""
    if direction == "store":
        return "%s(I%d, M%d)%s = %s" % (space, i, m, u_note, reg)
    return "%s = %s(I%d, M%d)%s" % (reg, space, i, m, u_note)


# Bit width of the merged "data" immediate-offset field, by form (PRM Type
# 4a/4b ACCESS opcode tables p.13-29/13-32 and decode_table.json: data[5:5]
# + data[4:0], 6 bits; PGR Figure 15-3/PRM Type15b p.15-11: data[6:0], 7
# bits). merge_fields() only concatenates the split data[hi:lo] pieces raw
# -- it does not sign-extend -- so the renderer has to know each form's
# width itself. tools/sharc_trace.py's "4a"/"4b"/"15b" _execute branches
# already treat this field as a signed twos-complement displacement (an
# index modifier, like the Mj registers, not an unsigned byte count);
# printing it unsigned here disagreed with what actually executes.
_IMMOFF_DATA_BITS = {"4a": 6, "4b": 6, "4d": 6, "15b": 7}


def _fmt_index_offset(i: int, off: int) -> str:
    """'I%d + %d' / 'I%d - %d' for a signed I-register displacement."""
    return "I%d %s %d" % (i, "-" if off < 0 else "+", abs(off))


def render_mem_immoff(sw, f, insn_type):
    """4a/4b/4d/15b: I-register + immediate-offset addressing."""
    i = f.get("i", 0)
    bits = _IMMOFF_DATA_BITS.get(insn_type, 6)
    off = sign_extend(f.get("data", 0), bits)
    dreg = f.get("dreg")
    ureg = f.get("ureg")
    reg = (
        "R%d" % dreg
        if dreg is not None
        else (ureg_name(ureg) if ureg is not None else "?")
    )
    space, direction = _space_dir(f)
    long_ = ", long" if f.get("l") else ""
    addr = _fmt_index_offset(i, off)
    if direction == "store":
        return "%s(%s) = %s%s" % (space, addr, reg, long_)
    return "%s = %s(%s)%s" % (reg, space, addr, long_)


def render_dual_mem(f):
    """1a/1b: simultaneous DM(dmi,dmm) and PM(pmi,pmm) reference. PGR Table
    10-1 ("Opcode Acronyms (ISA/VISA)", p.443-444): DMD "DAG1 access
    direction" and PMD "DAG2 access direction" are each 0 = Read, 1 = Write
    -- the same direction the Type 3/4/6/14/15 "D" field encodes (see
    _space_dir()). The manual's own Type 1a/1b Syntax (p.375) prints
    "DM(Ia,Mb) = dreg" for a write and "dreg = DM(Ia,Mb)" for a read: the
    register moves to whichever side receives the value, same as every
    other memory-form renderer here. A previous version instead always put
    the memory term first and only flipped "=" to "<-" for a read, which
    reads backwards ("DM(...) <- Rd" naturally parses as memory receiving
    from the register, i.e. a write, even when dmd/pmd said read)."""
    dmi, dmm = f.get("dmi", 0), f.get("dmm", 0)
    pmi, pmm = f.get("pmi", 0), f.get("pmm", 0)
    dmdreg, pmdreg = f.get("dmdreg", 0), f.get("pmdreg", 0)
    dm = (
        "DM(I%d, M%d) = R%d" % (dmi, dmm, dmdreg)
        if f.get("dmd")
        else "R%d = DM(I%d, M%d)" % (dmdreg, dmi, dmm)
    )
    pm = (
        "PM(I%d, M%d) = R%d" % (pmi, pmm, pmdreg)
        if f.get("pmd")
        else "R%d = PM(I%d, M%d)" % (pmdreg, pmi, pmm)
    )
    return dm + " ; " + pm


def render_modify(sw, insn_type, f):
    """19a/19a_scaled/19a_bitrev/16a/16b: I-register modify, by raw bytes,
    scaled by normal-word, bit-reversed, or storing a literal while
    modifying (16a/16b)."""
    if insn_type.startswith("19a"):
        # PGR Type 19 encodes the destination as Is XOR Idis, not as a
        # direct register number (see tools/sharc_trace.py's _compute,
        # "19a"/"19a_scaled", citing PGR Table/Figure for Type 19); g
        # selects DAG1 (I0-I7) vs DAG2 (I8-I15) for both registers.
        bank = 8 if f.get("g") else 0
        src_low, dis_low = f.get("is", 0), f.get("idis", 0)
        src, dst = bank + src_low, bank + (src_low ^ dis_low)
        space = "PM" if f.get("g") else "DM"
        val = f.get("data", 0)
        note = {
            "19a": "raw-byte offset",
            "19a_scaled": "scaled by normal-word",
            "19a_bitrev": "bit-reversed addressing",
        }[insn_type]
        return "I%d = modify(I%d, %s)  [%s space=%s]" % (
            dst,
            src,
            hex(val),
            note,
            space,
        )
    i, m = f.get("i", 0), f.get("m", 0)
    space = "PM" if f.get("g") else "DM"
    val = f.get("data", 0)
    return "%s(I%d, M%d) = 0x%x  [also I%d += M%d]" % (
        space,
        i,
        m,
        val & 0xFFFFFFFF,
        i,
        m,
    )


def render_literal_load(f, form):
    ureg = f.get("ureg")
    val = f.get("data", 0)
    reg = ureg_name(ureg) if ureg is not None else "?"
    return "%s = 0x%x" % (reg, val & 0xFFFFFFFF)


def render_18a(f):
    bop = f.get("bop", 0)
    sreg = f.get("sreg", 0)
    code = UREG_CODES.get("USTAT1", 0) + sreg
    reg = ureg_name(code)
    mask = f.get("data", 0)
    names = {
        0: "set",
        1: "clear",
        2: "toggle",
        3: "?bop3",
        4: "bit-test",
        5: "xor-test",
    }
    op = names.get(bop, "bop?%d" % bop)
    if bop in (4, 5):
        return "%s(%s, mask=0x%x) -> BTF" % (op, reg, mask)
    return "%s = %s(%s, 0x%x)" % (reg, op, reg, mask)


# Opcode -> mnemonic for render_shiftimm(), restricted to the ShiftImm
# opcodes tools/sharc_trace.py's _shift_immediate() actually implements and
# cites (PRM Table 17-9 pp.17-10/17-11; PRM p.3-17 for the (SE) note). The
# other rows in tools/sharcspec/compute_table.json's shiftop_shiftimm table
# (rot, fdep, fdep (se), bitext, bffwrp, exp, leftz, lefto, fpack, funpack)
# are left out: that table's own cross_check note says the PRM's OCR'd
# DATA8/BIT6:LEN6/BITLEN12/DATA7 immediate-column split is ambiguous for
# them, and _shift_immediate() itself does not model them (it raises on
# any opcode not listed here), so a generic per-row template would risk
# printing a wrong immediate field width.
_SHIFTIMM_MNEMONICS = {
    0x00: "lshift",
    0x01: "ashift",
    0x08: "or-lshift",
    0x09: "or-ashift",
    0x10: "fext",
    0x12: "fext-se",
    0x30: "bset",
    0x31: "bclr",
    0x32: "btgl",
    0x33: "btst",
}


def render_shiftimm(f):
    """6a_mem/6b_shiftimm's parallel ShiftImm sub-instruction: a 23-bit
    field (PRM Table 18-9 pp.431-433, cross-checked against PGR Table
    12-11; tools/sharcspec/compute_table.json's shiftop_shiftimm) packing a
    6-bit opcode, an 8-bit immediate, and RN/RX register numbers -- the
    same layout tools/sharc_trace.py's _shift_immediate() executes. Opcodes
    outside _SHIFTIMM_MNEMONICS fall back to the raw field dump (see that
    dict's comment for why)."""
    field = f.get("shiftimm", 0) & 0x7FFFFF
    opcode = (field >> 16) & 0x3F
    data8 = (field >> 8) & 0xFF
    rn, rx = (field >> 4) & 0xF, field & 0xF
    dataex = f.get("dataex", 0)
    name = _SHIFTIMM_MNEMONICS.get(opcode)
    if name is None:
        return "shiftimm(dataex=0x%x, shiftimm=0x%x)" % (dataex, field)
    if opcode in (0x00, 0x01, 0x08, 0x09):
        amount = sign_extend(data8, 8)
        if opcode in (0x08, 0x09):
            return "R%d = R%d or %s(R%d, %d)" % (
                rn,
                rn,
                name.split("-")[1],
                rx,
                amount,
            )
        return "R%d = %s(R%d, %d)" % (rn, name, rx, amount)
    if opcode in (0x10, 0x12):
        position = data8 & 0x3F
        length = (dataex << 2) | (data8 >> 6)
        return "R%d = %s(R%d, pos=%d, len=%d)" % (rn, name, rx, position, length)
    if opcode in (0x30, 0x31, 0x32):
        return "R%d = %s(R%d, bit=%d)" % (rn, name, rx, data8)
    return "%s(R%d, bit=%d)  [status only]" % (name, rx, data8)


def render_loop(sw, f, insn_length_bytes, insn_type):
    reladdr = f.get("reladdr")
    start_sw = sw + (insn_length_bytes or 6) // 2
    end_sw = sw + sign_extend(reladdr, 23) if reladdr is not None else None
    if insn_type == "12a_imm":
        count = f.get("data", 0)
        head = "DO 0x%x UNTIL LCE  (trip count %d, literal)" % (end_sw or 0, count)
    else:
        ureg = f.get("ureg")
        head = "DO 0x%x UNTIL LCE  (trip count %s, register)" % (
            end_sw or 0,
            ureg_name(ureg) if ureg is not None else "?",
        )
    body = " body [0x%x, 0x%x)" % (start_sw, end_sw) if end_sw is not None else ""
    return head + body, end_sw


def render_call_or_jump(sw, insn_type, f):
    b = f.get("b")
    cond = f.get("cond")
    j = f.get("j")
    conditional = cond is not None and cond != ALWAYS_TRUE_COND
    delayed_note = "" if j is None else (" delayed" if j else " non-delayed")
    cond_note = "" if not conditional else (" IF %s" % cond_name(cond))
    if insn_type.startswith("25a"):
        return "CALL" + cond_note + " (linked, delayed)"
    kind = "CALL" if b else "JUMP"
    return kind + cond_note + delayed_note


# forms whose merged fields carry a 23-bit (or 12-bit, 2c) compute field
# alongside their own addressing/control fields; kept in sync with
# tools/sharcinv.py's COMPUTE_FORMS list (imported, not duplicated).
COMPUTE_FORMS = sharcinv.COMPUTE_FORMS

DIRECT_MEM_FORMS = {"15a", "14a", "14d"}
INDEXED_MEM_FORMS = {"3a", "3b", "3d", "6a_mem"}
IMMOFF_MEM_FORMS = {"4a", "4b", "4d", "15b"}
DUAL_MEM_FORMS = {"1a", "1b"}
MODIFY_FORMS = {"19a", "19a_scaled", "19a_bitrev", "16a", "16b"}
LITERAL_LOAD_FORMS = {"17a", "17b"}
LOOP_FORMS = {"12a_imm", "12a_ureg"}
CALL_JUMP_FORMS = {
    "25a_direct",
    "25a_pcrel",
    "8a_abs",
    "8a_rel",
    "9a_abs",
    "9a_rel",
    "9b_abs",
    "9b_rel",
}


def render_instruction(
    sw: int,
    insn,
    mem,
    named_tables_touched: set,
    literal_regions: Counter,
    float_immediates: list,
):
    """-> (mnemonic, notes: [str], gap: bool) for one decoded instruction.
    Never returns None: an unhandled form falls back to a field dump."""
    t = insn.type_name
    f = sharcinv.merge_fields(insn.fields)
    notes = []
    gap = False

    if insn.kind == "unknown":
        return (
            "!!UNDECODED!! %s (word0=0x%04x)" % (insn.note, insn.raw or 0),
            notes,
            True,
        )
    if insn.kind == "uncertain":
        notes.append("uncertain form (%s)" % insn.note)

    def literal_note(value, float_capable):
        text = _annotate_literal(value, float_capable, mem)
        region = name_literal_region(value)
        if region:
            named_tables_touched.add(region)
            literal_regions[region] += 1
        if float_capable:
            fl = sharcinv.float32(value)
            if sharcinv._plausible_float(fl):
                float_immediates.append(fl)
        if text:
            notes.append(text)

    compute_text = None
    if t in COMPUTE_FORMS:
        compute_field = f.get("compute")
        if compute_field:
            compute_text, gap = render_compute(compute_field)

    if t == "9b_abs" and insn.raw == sharcflow.RETURN_JUMP:
        return "RETURN", notes, gap

    if t in CALL_JUMP_FORMS:
        head = render_call_or_jump(sw, t, f)
        if t in ("25a_direct", "8a_abs"):
            target = f.get("addr")
        elif t in ("25a_pcrel", "9a_rel", "8a_rel", "9b_rel"):
            target = sharcflow.pcrel_target(sw, f.get("reladdr", 0))
        elif t in ("9a_abs", "9b_abs"):
            # 9a_abs has no addr field: like 9b_abs it jumps through PM(I, M).
            target = None
            head += "  target=indirect PM(I%s, M%s)" % (f.get("pmi"), f.get("pmm"))
        else:
            target = None
        if target is not None:
            head += "  target=0x%x" % target
        if compute_text:
            head = compute_text + "; " + head
        return head, notes, gap

    if t in LOOP_FORMS:
        text, end_sw = render_loop(sw, f, insn.length_bytes, t)
        return text, notes, gap

    if t in DUAL_MEM_FORMS:
        text = render_dual_mem(f)
        if compute_text:
            text = compute_text + "; " + text
        return text, notes, gap

    if t in DIRECT_MEM_FORMS:
        text, addr, _ = render_mem_direct(sw, f, t)
        literal_note(addr, False)
        if compute_text:
            text = compute_text + "; " + text
        return text, notes, gap

    if t in INDEXED_MEM_FORMS:
        text = render_mem_indexed(sw, f)
        if t == "6a_mem":
            # PRM Type 6a performs the ShiftImm sub-op in parallel with the
            # memory transfer (tools/sharc_trace.py's "6a_mem" _execute
            # branch); 6a_mem has no "compute" field, so this is the only
            # place its ShiftImm half gets rendered.
            text += "  [parallel %s]" % render_shiftimm(f)
        if compute_text:
            text = compute_text + "; " + text
        # 3a/3b/3d/6a_mem all carry a "cond" field (decode_table.json) that
        # gates the whole instruction, not just a compute half (PRM p.7924,
        # cited in tools/sharc_trace.py's Type3a handling).
        return cond_prefix(f) + text, notes, gap

    if t in IMMOFF_MEM_FORMS:
        text = render_mem_immoff(sw, f, t)
        if compute_text:
            text = compute_text + "; " + text
        # 4a/4b/4d carry "cond"; 15b does not (cond_prefix(f) is "" then).
        return cond_prefix(f) + text, notes, gap

    if t in MODIFY_FORMS:
        text = render_modify(sw, t, f)
        if "data" in f:
            literal_note(f["data"], t in ("16a",))
        return text, notes, gap

    if t in LITERAL_LOAD_FORMS:
        text = render_literal_load(f, t)
        literal_note(f.get("data", 0), True)
        return text, notes, gap

    if t == "18a":
        text = render_18a(f)
        literal_note(f.get("data", 0), False)
        return text, notes, gap

    if t == "2c":
        text = render_shortcompute(f.get("compute", 0))
        return text, notes, gap

    if t in ("2a", "2a_short", "2b") and compute_text:
        # Only Type2a (48-bit) actually carries a "cond" field; Type2a_short
        # and Type2b are a structurally different 32-bit encoding with no
        # COND bits at all (decode_table.json), not merely an always-true
        # instance of Type2a, so cond_prefix(f) is always "" for them.
        return cond_prefix(f) + compute_text, notes, gap

    if t in ("5a_move", "5b_move"):
        src_hi = f.get("srcureghigh", 0)
        src_lo = (f.get("srcureglow", 0)) & 3
        src = (src_hi << 2) | src_lo
        dst = f.get("dstureg")
        text = "%s = %s" % (ureg_name(dst) if dst is not None else "?", ureg_name(src))
        if compute_text:
            text = compute_text + "; " + text
        return cond_prefix(f) + text, notes, gap

    if t in ("5a_swap", "5b_swap"):
        c, d = f.get("cdreg"), f.get("dreg")
        text = "R%s <-> R%s" % (c, d)
        if compute_text:
            text = compute_text + "; " + text
        return cond_prefix(f) + text, notes, gap

    if t == "3c":
        # decode_table.json's Type3c has its own "d" field (bit 37, PGR
        # p.449's opcode figure), the same Data-direction bit Table 10-1
        # (p.443) defines for every other Type 3/4/6/14/15 memory form (0 =
        # read, 1 = write) and that tools/sharc_trace.py's "3c" _execute
        # branch checks the same way (store when d, else load). A previous
        # version of this branch never read "d" at all and always rendered
        # a store, which disagreed with the tracer for every d=0 (load)
        # encoding -- e.g. DT2 1.16 sw 0x1c653b (dmi=4, dmm=5, d=0, dreg=6)
        # is a load, "R6 = DM(I4, M5)", not "DM(I4, M5) = R6".
        dmi, dmm, dreg = f.get("dmi", 7), f.get("dmm", 7), f.get("dreg", 2)
        _, direction = _space_dir(f)
        if direction == "store":
            text = "DM(I%d, M%d) = R%d" % (dmi, dmm, dreg)
        else:
            text = "R%d = DM(I%d, M%d)" % (dreg, dmi, dmm)
        return text, notes, gap

    if t == "18a":  # pragma: no cover -- handled above; kept for clarity
        return render_18a(f), notes, gap

    if t == "20a":
        ops = [
            name
            for name in ("lpu", "lpo", "spu", "spo", "ppu", "ppo", "fc")
            if f.get(name)
        ]
        text = "status-stack " + (",".join(ops) if ops else "(no-op)")
        return text, notes, gap

    if t in ("21a", "21c"):
        return "NOP/reserved (%s)" % t, notes, gap

    if t in ("22a", "22c"):
        return "EMU %s" % hex(f.get("emu", 0)), notes, gap

    if t in ("25a_rframe", "25c_rframe"):
        return "(return-frame delay slot: %s)" % t, notes, gap

    if t == "26a":
        return "(reserved/26a)", notes, gap

    if t == "7d":
        # docs/findings/06-sharc-engine-and-startup.md: "Type7d is ACONV".
        return (
            "ACONV  breg=%s toby=%s idis=%s"
            % (f.get("breg"), f.get("toby"), f.get("idis")),
            notes,
            gap,
        )

    if t in ("6b_shiftimm",):
        text = render_shiftimm(f)
        return cond_prefix(f) + text, notes, gap

    if compute_text:
        # Reaches here for 7a and 11a (both carry "cond" but have no more
        # specific branch above); 1a/9a_abs/9a_rel/3a/4a/2a/2a_short/2b/
        # 5a_move/5a_swap already returned earlier, each with its own
        # cond_prefix() call above.
        return cond_prefix(f) + compute_text, notes, gap

    # Generic fallback -- never skip an instruction.
    fields_text = " ".join("%s=0x%x" % (k, v) for k, v in sorted(f.items()))
    return "[%s] %s" % (t, fields_text), notes, gap


# --- dossier construction --------------------------------------------------


def sha256_of(path: str) -> str:
    import hashlib

    with open(path, "rb") as fh:
        return hashlib.sha256(fh.read()).hexdigest()


def load_context(blob_path: str, block_idxs, min_depth: int):
    """Build once, reuse for every dossier: the parsed blob, a LoadedMemory
    for RAM/ROM checks, and the full-inventory function list (so cross-block
    call graph resolves) via tools/sharcinv.py's build_inventory()."""
    with open(blob_path, "rb") as fh:
        data = fh.read()
    blocks = sharcldr.parse_blocks(data)
    mem = sharcldr.LoadedMemory.from_stream(data, blocks)
    functions, by_id = sharcinv.build_inventory(blob_path, block_idxs, min_depth)
    blocks_by_idx = {b["index"]: b for b in blocks}
    analyzed = {}
    for idx in block_idxs:
        r = sharcinv.analyze_block(data, blocks_by_idx, idx, min_depth)
        if r is None:
            continue
        r["_insn_sw"] = [r["base_sw"] + off // 2 for off, _ in r["insns"]]
        analyzed[idx] = r
    return {
        "data": data,
        "blocks": blocks,
        "blocks_by_idx": blocks_by_idx,
        "mem": mem,
        "functions": functions,
        "by_id": by_id,
        "analyzed": analyzed,
        "blob_path": blob_path,
        "sha256": sha256_of(blob_path),
    }


def base_sw_note(target_address: int) -> str:
    if target_address >= sharcldr.SW_ALIAS_BASE:
        return (
            "target %#x >= 0x28000000: base_sw = (target_address - 0x28000000) / 2 "
            "(ordinary short-word alias)" % target_address
        )
    if sharcldr.L2_BYTE_BASE <= target_address < sharcldr.L2_BYTE_LIMIT:
        return (
            "target %#x is in the L2 byte window 0x20000000-0x20020000: "
            "base_sw = 0xb80000 + (target_address - 0x20000000) / 2 "
            "(blk69 does NOT use the 0x28000000 alias)" % target_address
        )
    return "target %#x: no known base_sw convention covers this block" % target_address


def find_function(ctx, addr: int):
    """-> fn dict for `addr`, preferring an exact entry match, else the
    function whose [entry, exit) span contains it. None if not found in any
    scanned block."""
    exact = [fn for fn in ctx["functions"] if fn["entry"] == addr]
    if exact:
        return exact[0], True
    containing = [fn for fn in ctx["functions"] if fn["entry"] <= addr < fn["exit"]]
    if containing:
        containing.sort(key=lambda fn: addr - fn["entry"])
        return containing[0], False
    return None, False


def locate_block(ctx, addr: int):
    for idx, block in ctx["analyzed"].items():
        span = block["payload_len"] // 2
        if block["base_sw"] <= addr < block["base_sw"] + span:
            return idx, block
    return None, None


def scan_conditional_returns(block):
    """9b_abs jumps through I4/M6 (b=0, pmm=5, j=1) whose cond isn't 31 --
    sharcflow.py's returns list only matches the unconditional return word
    (raw == RETURN_JUMP), so a real conditional return would be invisible
    there. Cheap to detect from the same aligned instruction stream."""
    out = []
    for off, insn in block["insns"]:
        if insn.type_name != "9b_abs":
            continue
        f = sharcinv.merge_fields(insn.fields)
        if (
            (f.get("b"), f.get("pmm"), f.get("j")) == (0, 5, 1)
            and f.get("cond") not in (31, None)
            and insn.raw != sharcflow.RETURN_JUMP
        ):
            out.append({"sw": block["base_sw"] + off // 2, "cond": f.get("cond")})
    return out


def internal_branch_targets(fn_insns, entry, exit_):
    """JUMP (not CALL) targets inside this function's own span, other than
    its entry -- the hazard flagged in the task brief: a branch target
    inside a return-delimited span is not automatically a function."""
    out = []
    for sw, insn in fn_insns:
        t = insn.type_name
        f = sharcinv.merge_fields(insn.fields)
        target = None
        if t in ("8a_abs", "9a_abs") and f.get("b") == 0:
            target = f.get("addr")
        elif t in ("8a_rel", "9a_rel") and f.get("b") == 0:
            target = sharcflow.pcrel_target(sw, f.get("reladdr", 0))
        if target is not None and entry <= target < exit_ and target != entry:
            out.append((sw, target))
    return out


def backward_exit_note(fn_insns, entry):
    """Flag any unconditional (cond=31) JUMP inside this function's own
    span whose target is below `entry` -- the blk93@0x1c4f81 pattern: the
    function's only live exit jumps backward into a preceding span's shared
    restore+return epilogue instead of returning through its own frame.
    Scans the whole span, not just the last instruction: when the real exit
    sits before the end of the computed span (the tail beyond it absorbed
    by an interior-call-target split, as at 0x1c4f81/0x1c5334), the last
    instruction is not the jump."""
    notes = []
    for sw, insn in fn_insns:
        t = insn.type_name
        if t not in ("8a_abs", "8a_rel", "9a_abs", "9a_rel"):
            continue
        f = sharcinv.merge_fields(insn.fields)
        if f.get("b") != 0 or f.get("cond") not in (31, None):
            continue
        if t in ("8a_abs", "9a_abs"):
            target = f.get("addr")
        else:
            target = sharcflow.pcrel_target(sw, f.get("reladdr", 0))
        if target is not None and target < entry:
            notes.append(
                "unconditional JUMP at 0x%x targets 0x%x, before this function's "
                "own entry -- likely a shared epilogue inside a preceding span, "
                "not a return through this function's own frame" % (sw, target)
            )
    return notes


def build_dossier(ctx, addr: int, want_listing: bool = True):
    idx, block = locate_block(ctx, addr)
    if idx is None:
        return {"addr": addr, "error": "not covered by any scanned code block"}

    fn, exact = find_function(ctx, addr)
    b = ctx["blocks_by_idx"][idx]
    file_offset = b["payload_offset"] + (addr - block["base_sw"]) * 2

    ident = {
        "block": idx,
        "target_address": "0x%x" % b["target_address"],
        "payload_offset": b["payload_offset"],
        "base_sw_convention": base_sw_note(b["target_address"]),
        "base_sw": "0x%x" % block["base_sw"],
        "file_offset_of_entry": "0x%x" % file_offset,
        "image_sha256": ctx["sha256"],
        "image_sha256_known_as": EXPECTED_SHA256.get(
            ctx["sha256"], "(not a recognised image)"
        ),
    }

    result = {"addr": "0x%x" % addr, "identification": ident}

    if fn is None:
        result["error"] = "no return-delimited span in blk%d covers 0x%x" % (idx, addr)
        return result

    if not exact:
        result["address_note"] = (
            "0x%x is not this function's own entry; using the "
            "containing function %s (entry 0x%x)" % (addr, fn["id"], fn["entry"])
        )

    entry, exit_ = fn["entry"], fn["exit"]
    fn_insns = sharcinv.instructions_in(block, entry, exit_)

    bounds = {
        "id": fn["id"],
        "entry": "0x%x" % entry,
        "exit": "0x%x" % exit_,
        "n_insns": fn["n_insns"],
        "entry_kind": fn["entry_kind"],
        "boundary_note": fn.get("boundary_note"),
        "split_from": fn.get("split_from"),
        "tail_split_into": fn.get("tail_split_into"),
        "method": (
            "return-delimited span (tools/sharcflow.py returns), further split at "
            "any direct-call target landing strictly inside it (tools/sharcinv.py "
            "function_bounds())"
        ),
    }
    internal_targets = internal_branch_targets(fn_insns, entry, exit_)
    if internal_targets:
        bounds["internal_branch_targets"] = [
            "0x%x -> 0x%x (branch target inside this span, NOT a separate function)"
            % (sw, t)
            for sw, t in internal_targets
        ]
    backward = backward_exit_note(fn_insns, entry)
    if backward:
        bounds["backward_exit_hazard"] = backward

    cond_returns = [
        r for r in scan_conditional_returns(block) if entry <= r["sw"] < exit_
    ]

    callgraph = {
        "callers": fn["callers"],
        "callees": fn["callees"],
        "unresolved_callees": fn.get("unresolved_callees", []),
        "is_leaf": fn["is_leaf"],
        "has_no_static_caller": fn["has_no_static_caller"],
        "indirect_calls": fn["vector"].get("indirect_calls", 0),
        "conditional_returns": cond_returns,
    }
    if cond_returns:
        callgraph["conditional_return_note"] = (
            "tools/sharcflow.py only matches the unconditional return word "
            "(raw==0x083F343F); these are 9b_abs jumps through I4/M6 with cond != 31 "
            "found by scanning this block's own aligned instructions"
        )

    summary = {
        "label": fn["label"],
        "confidence": fn["confidence"],
        "label_reasons": fn["label_reasons"],
        "vector": fn["vector"],
        "named_tables_touched": fn["vector"].get("named_tables_touched", []),
        "param_frame_referenced": any(
            r.startswith("param_frame") for r in fn["vector"].get("literal_regions", {})
        ),
        "literal_regions": fn["vector"].get("literal_regions", {}),
    }

    result.update({"bounds": bounds, "call_graph": callgraph, "summary": summary})

    if want_listing:
        named_tables_touched = set()
        literal_regions = Counter()
        float_immediates = []
        listing = []
        for sw, insn in fn_insns:
            mnemonic, notes, gap = render_instruction(
                sw,
                insn,
                ctx["mem"],
                named_tables_touched,
                literal_regions,
                float_immediates,
            )
            raw_bytes = (
                insn.raw.to_bytes(insn.length_bytes, "big")
                if insn.raw is not None and insn.length_bytes
                else b""
            )
            listing.append(
                {
                    "sw": "0x%x" % sw,
                    "bytes": raw_bytes.hex(),
                    "form": insn.type_name,
                    "mnemonic": mnemonic,
                    "notes": notes,
                    "gap": gap,
                }
            )
        result["listing"] = listing
        result["listing_extra"] = {
            "named_tables_touched_in_listing": sorted(named_tables_touched),
            "literal_regions_in_listing": dict(literal_regions),
            "float_immediates_in_listing": float_immediates,
            "undecoded_count": sum(1 for r in listing if r["form"] == "unknown"),
            "gap_count": sum(1 for r in listing if r["gap"]),
        }

    return result


# --- printing ---------------------------------------------------------


def print_dossier(d: dict, show_listing: bool):
    addr = d.get("addr")
    print("=== dossier for %s ===" % addr)
    if "error" in d and "identification" not in d:
        print("  ERROR: %s" % d["error"])
        return
    ident = d["identification"]
    print("\n-- identification --")
    for k in (
        "block",
        "target_address",
        "payload_offset",
        "base_sw",
        "file_offset_of_entry",
        "image_sha256",
        "image_sha256_known_as",
    ):
        print("  %-24s %s" % (k, ident[k]))
    print("  base_sw convention:    %s" % ident["base_sw_convention"])

    if "error" in d:
        print("\n  ERROR: %s" % d["error"])
        return
    if d.get("address_note"):
        print("\n  NOTE: %s" % d["address_note"])

    b = d["bounds"]
    print("\n-- bounds --")
    print(
        "  id=%s entry=%s exit=%s n_insns=%s entry_kind=%s"
        % (b["id"], b["entry"], b["exit"], b["n_insns"], b["entry_kind"])
    )
    print("  method: %s" % b["method"])
    if b.get("boundary_note"):
        print("  boundary_note: %s" % b["boundary_note"])
    for t in b.get("internal_branch_targets", []):
        print("  HAZARD: %s" % t)
    for note in b.get("backward_exit_hazard") or []:
        print("  HAZARD: %s" % note)

    cg = d["call_graph"]
    print("\n-- call graph --")
    print("  callers: %s" % (cg["callers"] or "(none found)"))
    print("  callees: %s" % (cg["callees"] or "(none)"))
    if cg["unresolved_callees"]:
        print(
            "  unresolved callees (target outside scanned blocks): %s"
            % ["0x%x" % t for t in cg["unresolved_callees"]]
        )
    print(
        "  is_leaf=%s has_no_static_caller=%s indirect_calls=%d"
        % (cg["is_leaf"], cg["has_no_static_caller"], cg["indirect_calls"])
    )
    if cg["conditional_returns"]:
        print(
            "  conditional returns: %s"
            % [
                ("0x%x cond=%s" % (r["sw"], r["cond"]))
                for r in cg["conditional_returns"]
            ]
        )
        print("  %s" % cg["conditional_return_note"])

    s = d["summary"]
    print("\n-- summary --")
    print("  label: %s (confidence %.2f)" % (s["label"], s["confidence"]))
    for r in s["label_reasons"]:
        print("    - %s" % r)
    print("  named_tables_touched: %s" % s["named_tables_touched"])
    print("  param_frame_referenced: %s" % s["param_frame_referenced"])
    print("  literal_regions: %s" % s["literal_regions"])
    v = s["vector"]
    print(
        "  compute_total=%d mem_load=%d mem_store=%d loop_literal=%d loop_register=%d"
        % (
            v["compute_total"],
            v["mem_load"],
            v["mem_store"],
            v["loop_literal"],
            v["loop_register"],
        )
    )

    if show_listing and "listing" in d:
        print("\n-- listing (%d instructions) --" % len(d["listing"]))
        for row in d["listing"]:
            gap = "  <<< GAP" if row["gap"] else ""
            line = "  %-10s %-16s %-10s %s%s" % (
                row["sw"],
                row["bytes"],
                row["form"],
                row["mnemonic"],
                gap,
            )
            print(line)
            for note in row["notes"]:
                print("             ; %s" % note)
        extra = d["listing_extra"]
        print(
            "\n  named tables touched in listing: %s"
            % extra["named_tables_touched_in_listing"]
        )
        print("  literal regions in listing: %s" % extra["literal_regions_in_listing"])
        print(
            "  undecoded instructions: %d   flagged gap opcodes: %d"
            % (extra["undecoded_count"], extra["gap_count"])
        )


# --- deterministic Phase A engine queue ---------------------------------

ENGINE_QUEUE_SCHEMA = "sharc-engine-queue/v1"
ENGINE_MIN_INSNS = 60
ENGINE_MIN_FLOAT_MUL_MAC = 10
_NOTE_NAME_RE = re.compile(r"^blk(?P<block>\d+)-(?P<entry>[0-9a-fA-F]+)\.md$")
_NOTE_ENTRY_RE = re.compile(r"\*\*Bounds\*\*:[^\n]*?entry `0x([0-9a-fA-F]+)`")


def canonical_json_bytes(value) -> bytes:
    """Canonical queue serialization: sorted keys, UTF-8, final newline."""
    return (
        json.dumps(value, sort_keys=True, indent=2, ensure_ascii=False) + "\n"
    ).encode("utf-8")


def canonical_digest(value) -> str:
    return hashlib.sha256(canonical_json_bytes(value)).hexdigest()


def engine_candidates(functions):
    """Phase A predicate over sharcinv's finalized vector keys only."""
    selected = []
    for fn in functions:
        vector = fn["vector"]
        float_mul_mac = vector["float_mul"] + vector["mac"]
        if (
            fn["n_insns"] >= ENGINE_MIN_INSNS
            and float_mul_mac >= ENGINE_MIN_FLOAT_MUL_MAC
        ):
            selected.append(fn)
    return sorted(selected, key=lambda fn: (fn["block"], fn["entry"]))


def documented_function_entries(notes_dir):
    """Return independently bounded note entries and rejected note reasons.

    A filename alone is not enough: the note must state the same entry in its
    Bounds line.  Thus a continuation note cannot create a completed entry.
    """
    entries, rejected = set(), []
    if not os.path.isdir(notes_dir):
        return entries, [{"reason": "notes directory absent", "path": notes_dir}]
    for name in sorted(os.listdir(notes_dir)):
        match = _NOTE_NAME_RE.match(name)
        if not match:
            continue
        path = os.path.join(notes_dir, name)
        try:
            with open(path, encoding="utf-8") as fh:
                text = fh.read()
        except OSError as exc:
            rejected.append({"path": name, "reason": str(exc)})
            continue
        bounds = _NOTE_ENTRY_RE.search(text)
        expected = int(match.group("entry"), 16)
        if bounds is None:
            rejected.append({"path": name, "reason": "no bounded entry declaration"})
        elif int(bounds.group(1), 16) != expected:
            rejected.append({"path": name, "reason": "filename/bounds entry mismatch"})
        else:
            entries.add((int(match.group("block")), expected))
    return entries, rejected


def source_hashes():
    return {
        name: sha256_of(os.path.join(_here, name))
        for name in ("sharcfn.py", "sharcinv.py", "sharcflow.py", "sharcldr.py")
    }


def sqlite_evidence(sqlite_path, candidates, source_image_sha256=None):
    """Read optional SQLite evidence without modifying it.

    Facts remain advisory unless the database records the exact SHA-256 of
    the source image and it matches ``source_image_sha256``.  The join uses
    SHARC short-word addresses throughout.
    """
    result = {
        "path": sqlite_path,
        "sha256": sha256_of(sqlite_path),
        "mode": "read-only",
        "meta": [],
        "status": "usable",
        "freshness": "not_verified: no source image SHA-256 supplied",
        "evidence_status": "advisory",
        "schema_issues": [],
        "facts": {},
    }
    try:
        con = sqlite3.connect(
            "file:%s?mode=ro" % os.path.abspath(sqlite_path), uri=True
        )
        tables = {
            row[0]
            for row in con.execute("select name from sqlite_master where type='table'")
        }
        if "meta" not in tables:
            result["schema_issues"].append("missing meta table")
        else:
            result["meta"] = [
                {"key": row[0], "value": row[1]}
                for row in con.execute("select key, value from meta order by key")
            ]
            meta_values = {row["key"]: row["value"] for row in result["meta"]}
            image_sha256 = meta_values.get("image_sha256")
            if image_sha256 is None:
                result["freshness"] = (
                    "not_verified: meta lacks full source image SHA-256"
                )
            elif source_image_sha256 is None:
                result["freshness"] = "not_verified: source image SHA-256 not supplied"
            elif image_sha256 == source_image_sha256:
                result["freshness"] = "verified"
            else:
                result["freshness"] = (
                    "stale: SQLite image_sha256 does not match source image SHA-256"
                )
        needed = {"functions", "insn", "refs", "warnings", "decompiled"}
        missing = sorted(needed - tables)
        if missing:
            result["schema_issues"].append("missing tables: " + ", ".join(missing))
        if not result["schema_issues"]:
            for fn in candidates:
                entry = fn["entry"]
                function = con.execute(
                    "select name, instructions, in_main from functions where sw=?",
                    (entry,),
                ).fetchone()
                insn_count = con.execute(
                    "select count(*) from insn where function_sw=?", (entry,)
                ).fetchone()[0]
                refs = con.execute(
                    "select count(*) from refs where from_sw=? or to_sw=?",
                    (entry, entry),
                ).fetchone()[0]
                warnings = con.execute(
                    "select count(*) from warnings where function_sw=?", (entry,)
                ).fetchone()[0]
                decomp = con.execute(
                    "select completed, error from decompiled where function_sw=?",
                    (entry,),
                ).fetchone()
                result["facts"]["0x%x" % entry] = {
                    "function_present": function is not None,
                    "instruction_count": function[1] if function else insn_count,
                    "reference_count": refs,
                    "warning_count": warnings,
                    "decompiler": None
                    if decomp is None
                    else {
                        "available": True,
                        "completed": bool(decomp[0]),
                        "error": decomp[1],
                    },
                }
        else:
            result["status"] = "incompatible"
        con.close()
    except (OSError, sqlite3.Error) as exc:
        result["status"] = "unavailable"
        result["schema_issues"].append(str(exc))

    if result["status"] == "usable" and result["freshness"] == "verified":
        result["evidence_status"] = "verified"
    for fact in result["facts"].values():
        fact["evidence_status"] = result["evidence_status"]
    return result


# Stable output schema for Phase A coverage.  These are only existing
# decoder/classifier observations; a zero means the form/opcode was absent.
TARGET_OPCODE_COVERAGE_KEYS = (
    "Type10a_rel",
    "Type10a_abs",
    "Type2b",
    "ALU_0xe0",
    "ALU_0x05",
    "ALU_0x06",
    "multifunction_0x1a",
    "multifunction_0x1e",
    "multifunction_0x1f",
    "convert_by_RY_0xd9",
    "convert_by_RY_0xda",
    "convert_by_RY_0xdd",
)


def target_opcode_coverage(ctx, candidates):
    """Count only forms/opcodes identified by existing decoder/classifier fields."""
    counts = {key: 0 for key in TARGET_OPCODE_COVERAGE_KEYS}
    unsupported = []
    candidate_ranges = {(fn["block"], fn["entry"], fn["exit"]) for fn in candidates}
    for block_id, entry, exit_ in candidate_ranges:
        block = ctx["analyzed"].get(block_id)
        if block is None:
            unsupported.append("candidate block %d was not analyzed" % block_id)
            continue
        for _sw, insn in sharcinv.instructions_in(block, entry, exit_):
            form = insn.type_name
            if form == "10a_rel":
                counts["Type10a_rel"] += 1
            elif form == "10a_abs":
                counts["Type10a_abs"] += 1
            if form == "2b":
                counts["Type2b"] += 1
            field = (
                sharcinv.merge_fields(insn.fields).get("compute")
                if form in sharcinv.COMPUTE_FORMS
                else None
            )
            if not field:
                continue
            cu, detail = sharcinv.classify_compute(field)
            opcode = detail.get("opcode")
            if cu == "ALU" and opcode in (0xE0, 0x05, 0x06):
                counts["ALU_0x%02x" % opcode] += 1
            if cu == "MULTIFN":
                selector = (field >> 16) & 0x3F
                if selector in (0x1A, 0x1E, 0x1F):
                    counts["multifunction_0x%02x" % selector] += 1
            if cu == "ALU" and opcode in (0xD9, 0xDA, 0xDD):
                counts["convert_by_RY_0x%02x" % opcode] += 1
    return {
        "derived_counts": dict(sorted(counts.items())),
        "unsupported_or_ambiguous": unsupported
        + [
            "Type10a_rel/abs are counted only if the existing decoder emits those form names",
            "convert-by-RY is limited to existing ALU classifier opcodes 0xd9, 0xda, 0xdd",
        ],
        "word_address_units": "all joins and counts use SHARC short-word addresses",
    }


def build_engine_queue(ctx, notes_dir, sqlite_path=None, target_coverage=None):
    inventory = sorted(ctx["functions"], key=lambda fn: (fn["block"], fn["entry"]))
    candidates = engine_candidates(inventory)
    documented, rejected_notes = documented_function_entries(notes_dir)
    rows = []
    for fn in candidates:
        row = {
            "block": fn["block"],
            "entry": "0x%x" % fn["entry"],
            "id": fn["id"],
            "n_insns": fn["n_insns"],
            "float_mul": fn["vector"]["float_mul"],
            "mac": fn["vector"]["mac"],
            "documented": (fn["block"], fn["entry"]) in documented,
        }
        rows.append(row)
    evidence = None
    if sqlite_path:
        if not os.path.isfile(sqlite_path):
            evidence = {
                "path": sqlite_path,
                "status": "unavailable",
                "schema_issues": ["SQLite file absent"],
            }
        else:
            evidence = sqlite_evidence(sqlite_path, candidates, ctx["sha256"])
            for row in rows:
                fact = evidence.get("facts", {}).get(row["entry"])
                if fact is not None:
                    row["sqlite"] = fact
    return {
        "schema_version": ENGINE_QUEUE_SCHEMA,
        "provenance": {
            "source_image_sha256": ctx["sha256"],
            "tool_source_sha256": source_hashes(),
            "predicate": {
                "n_insns_gte": ENGINE_MIN_INSNS,
                "float_multiply_or_mac_gte": ENGINE_MIN_FLOAT_MUL_MAC,
                "vector_keys": ["float_mul", "mac"],
            },
            "canonical_inventory_sha256": canonical_digest(inventory),
            "sqlite": evidence,
        },
        "historical_claim": {
            "read": 21,
            "remaining": 30,
            "membership": "unknown/unverified; no exact remaining set is emitted",
        },
        "documented_notes": {
            "bounded_entries": len(documented),
            "rejected": rejected_notes,
        },
        "counts": {
            "candidates": len(rows),
            "documented": sum(r["documented"] for r in rows),
            "undocumented": sum(not r["documented"] for r in rows),
        },
        "target_opcode_coverage": (
            target_coverage
            if target_coverage is not None
            else target_opcode_coverage(ctx, candidates)
        ),
        "candidates": rows,
    }


def write_batch_dossiers(ctx, addresses, out_dir, want_listing):
    os.makedirs(out_dir, exist_ok=True)
    for addr in addresses:
        d = build_dossier(ctx, addr, want_listing=want_listing)
        fn_id = d.get("bounds", {}).get("id") if "bounds" in d else None
        name = (fn_id or ("blk?@0x%x" % addr)).replace("@", "-").replace("0x", "")
        out_path = os.path.join(out_dir, "%s.json" % name)
        with open(out_path, "wb") as fh:
            fh.write(canonical_json_bytes(d))
        txt_path = os.path.join(out_dir, "%s.txt" % name)
        import io

        buf = io.StringIO()
        old = sys.stdout
        sys.stdout = buf
        try:
            print_dossier(d, show_listing=want_listing)
        finally:
            sys.stdout = old
        with open(txt_path, "w") as fh:
            fh.write(buf.getvalue())
        print(
            "0x%x -> %s (%s)" % (addr, out_path, d.get("bounds", {}).get("id", "ERROR"))
        )


# --- CLI ----------------------------------------------------------------


def _int(text: str) -> int:
    return int(text, 0)


def _int_list(text: str):
    return tuple(int(x, 0) for x in text.split(","))


def main(argv=None):
    ap = argparse.ArgumentParser(description=(__doc__ or "").splitlines()[0])
    ap.add_argument("blob")
    ap.add_argument("addr", nargs="?", type=_int, help="short-word (VISA PC) address")
    ap.add_argument(
        "--batch",
        type=_int_list,
        help="comma-separated addresses; writes one dossier per address",
    )
    ap.add_argument(
        "--out-dir", help="directory for --batch output (required with --batch)"
    )
    ap.add_argument(
        "--engine-queue",
        action="store_true",
        help="build deterministic Phase A engine candidate queue",
    )
    ap.add_argument(
        "--sqlite", help="optional read-only SQLite/Ghidra evidence for --engine-queue"
    )
    ap.add_argument(
        "--dossier-dir", help="write dossiers for undocumented queue candidates"
    )
    ap.add_argument(
        "--notes-dir",
        default=os.path.join(os.path.dirname(_here), "docs", "findings", "functions"),
        help="per-function notes directory for --engine-queue",
    )
    ap.add_argument("--blocks", type=_int_list, default=sharcinv.CODE_BLOCKS)
    ap.add_argument("--min-depth", type=int, default=8)
    ap.add_argument("--json", help="write the single-address dossier as JSON")
    ap.add_argument(
        "--listing", action="store_true", help="print the annotated instruction listing"
    )
    ap.add_argument(
        "--no-listing",
        action="store_true",
        help="suppress the listing in --batch JSON output",
    )
    args = ap.parse_args(argv)

    modes = sum((args.batch is not None, args.addr is not None, args.engine_queue))
    if modes != 1:
        ap.error("give exactly one of ADDR, --batch, or --engine-queue")
    if args.batch and not args.out_dir:
        ap.error("--batch requires --out-dir")
    if args.dossier_dir and not args.engine_queue:
        ap.error("--dossier-dir requires --engine-queue")
    if not os.path.isfile(args.blob):
        ap.error("firmware blob is absent or not a file: %s" % args.blob)

    ctx = load_context(args.blob, args.blocks, args.min_depth)
    print(
        "loaded %s: sha256=%s (%s), %d functions across blocks %s"
        % (
            args.blob,
            ctx["sha256"],
            EXPECTED_SHA256.get(ctx["sha256"], "unrecognised image"),
            len(ctx["functions"]),
            list(args.blocks),
        )
    )

    if args.engine_queue:
        queue = build_engine_queue(ctx, args.notes_dir, args.sqlite)
        output = canonical_json_bytes(queue)
        if args.json:
            parent = os.path.dirname(args.json)
            if parent:
                os.makedirs(parent, exist_ok=True)
            with open(args.json, "wb") as fh:
                fh.write(output)
            print("wrote %s" % args.json)
        else:
            sys.stdout.buffer.write(output)
        if args.dossier_dir:
            undocumented = [
                int(row["entry"], 16)
                for row in queue["candidates"]
                if not row["documented"]
            ]
            write_batch_dossiers(
                ctx, undocumented, args.dossier_dir, want_listing=not args.no_listing
            )
        return 0

    if args.addr is not None:
        want_listing = not args.no_listing
        d = build_dossier(ctx, args.addr, want_listing=want_listing)
        print_dossier(d, show_listing=args.listing or want_listing)
        if args.json:
            with open(args.json, "w") as fh:
                json.dump(d, fh, indent=1)
            print("\nwrote %s" % args.json)
        return 0

    write_batch_dossiers(
        ctx, args.batch, args.out_dir, want_listing=not args.no_listing
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
