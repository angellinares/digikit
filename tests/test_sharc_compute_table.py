"""Cross-checks tools/sharc_core/compute.py's dispatch tables (split out of
the old monolithic _compute if-chain into compute_alu.py, compute_mult.py,
compute_shift.py and compute_multi.py) against two independent sources:

1. tools/sharcspec/compute_table.json, the project's single public-manual
   (PRM/PGR) transcription of these encodings: every dispatch key must
   either match a row there, or be in this file's UNDOCUMENTED set (with a
   reason -- each one also carries a citation in its own handler's
   docstring explaining why the manuals do not cover it).
2. A frozen snapshot of every selector-bit combination the pre-refactor
   if-chain (tools/sharc_core/compute.py at commit 7f03aa5) accepted
   without raising, built by probing that old _compute over every
   (cu, opcode)/category/short-opcode/mrdatamove combination -- see
   OLD_ACCEPTED below. Every one of those must still dispatch (not raise)
   through the new tables.
"""

from __future__ import annotations

import json
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TOOLS = os.path.join(ROOT, "tools")
if TOOLS not in sys.path:
    sys.path.insert(0, TOOLS)

from sharc_core.compute import CU3_OPS, _compute  # noqa: E402
from sharc_core.compute_alu import ALU_OPS  # noqa: E402
from sharc_core.compute_mult import (  # noqa: E402
    MR_DATAMOVE_REGISTERS,
    MULT_FIXED_OPS,
    MULT_OPS,
)
from sharc_core.compute_multi import MULTIFN_MUL_ALU_OPS, SHORT_OPS  # noqa: E402
from sharc_core.compute_shift import SHIFT_OPS  # noqa: E402
from sharc_core.values import Const  # noqa: E402

# The cu=1 opcode space: handler functions plus the fixed-point opcodes
# held as MultSpec data (compute_mult.py).
assert not (set(MULT_OPS) & set(MULT_FIXED_OPS))
MULT_ALL = {**MULT_OPS, **MULT_FIXED_OPS}

with open(os.path.join(TOOLS, "sharcspec", "compute_table.json")) as _fh:
    COMPUTE_TABLE = json.load(_fh)


def _pattern_matches(pattern: str, value: int, width: int) -> bool:
    """A compute_table.json opcode pattern (e.g. "0000 F00x", "10yx F00r",
    "__11 0__0") matches VALUE (a WIDTH-bit int) when every '0'/'1' char
    agrees with the corresponding bit; any other char (a MOD-option letter
    like x/y/f/r, or the mod1/mod2/mod3 tables' '_') is a don't-care."""
    bits = pattern.replace(" ", "")
    assert len(bits) == width, (pattern, width)
    for i, ch in enumerate(bits):
        if ch not in "01":
            continue
        bit = (value >> (width - 1 - i)) & 1
        if int(ch) != bit:
            return False
    return True


def _documented(rows, field_name, value, width) -> bool:
    return any(
        row.get(field_name) is not None
        and _pattern_matches(row[field_name], value, width)
        for row in rows
    )


# Encodings absent from both public manuals this project cites (PRM and
# PGR, as transcribed into tools/sharcspec/compute_table.json) for the
# compute unit named. Each is still decoded, with its result and flags
# left Unknown, so the instruction walk does not desync -- see the
# handler's own docstring (cited here) for the page-by-page absence.
UNDOCUMENTED = {
    ("mult", 0x10): "compute_mult.mult_undocumented_10",
    ("shift", 0x14): "compute_shift.shift_undocumented_14",
    ("shift", 0xB0): "compute_shift.shift_undocumented_b0",
    ("cu3", 0xD6): "compute._make_reserved_cu3(0xd6)",
    ("cu3", 0xE0): "compute._make_reserved_cu3(0xe0)",
}


def test_alu_ops_documented_in_json():
    rows = (
        COMPUTE_TABLE["aluop_32_40bit"]["rows"] + COMPUTE_TABLE["aluop_64bit"]["rows"]
    )
    for opcode in ALU_OPS:
        assert _documented(rows, "opcode", opcode, 8), hex(opcode)


def test_dual_add_subtract_documented_in_json():
    rows = COMPUTE_TABLE["dual_add_subtract"]["rows"]
    for top_nibble in (0x7, 0xF):
        assert _documented(rows, "opcode_19_16", top_nibble, 4), hex(top_nibble)


def test_mult_ops_documented_in_json_or_undocumented():
    rows = (
        COMPUTE_TABLE["mulop_32_40bit"]["rows"] + COMPUTE_TABLE["mulop_64bit"]["rows"]
    )
    undocumented = {opcode for (unit, opcode) in UNDOCUMENTED if unit == "mult"}
    for opcode in MULT_ALL:
        assert opcode in undocumented or _documented(rows, "opcode", opcode, 8), hex(
            opcode
        )
    # The two multiply-accumulate-into-MRF pre-checks compute.py runs ahead
    # of the mf/cu dispatch (0xb4, 0xb0) share this same table.
    for opcode in (0xB4, 0xB0):
        assert _documented(rows, "opcode", opcode, 8), hex(opcode)


def test_shift_ops_documented_in_json_or_undocumented():
    rows = COMPUTE_TABLE["shiftop_shiftimm"]["rows"]
    undocumented = {opcode for (unit, opcode) in UNDOCUMENTED if unit == "shift"}
    for opcode in SHIFT_OPS:
        assert opcode in undocumented or _documented(rows, "shiftop_8bit", opcode, 8), (
            hex(opcode)
        )


def test_cu3_ops_are_all_undocumented():
    # No JSON table names a compute unit for cu=3 at all (PRM's top-level
    # compute selector: "not used by SINGLEFN") -- every cu=3 opcode this
    # decoder accepts is expected to be in UNDOCUMENTED, not matched
    # against any table.
    for opcode in CU3_OPS:
        assert ("cu3", opcode) in UNDOCUMENTED, hex(opcode)


def test_short_ops_documented_in_json():
    rows = COMPUTE_TABLE["shortcompute"]["rows"]
    for opcode in SHORT_OPS:
        assert _documented(rows, "opcode_11_8", opcode, 4), hex(opcode)


def test_mrdatamove_documented_in_json():
    rows = COMPUTE_TABLE["mrdatamove"]["rows"]
    for opcode in MR_DATAMOVE_REGISTERS:
        assert _documented(rows, "opcode_15_12", opcode, 4), hex(opcode)


def test_multifn_mul_alu_documented_in_json():
    rows = COMPUTE_TABLE["multifn_mul_alu"]["rows"]
    for category in MULTIFN_MUL_ALU_OPS:
        assert _documented(rows, "opcode_21_16", category, 6), hex(category)


def test_multifn_dual_range_documented_by_json_note():
    # multifn_mul_dual_addsub's two rows carry no bit pattern at all
    # ("opcode_21_16": null -- "opcode value itself is not printed in
    # either source"); the float row's own note says PGR assigns the
    # whole opcode[21:16]=11xxxx range to it, which is exactly the
    # (category >> 4) == 0b11 range compute.py routes to
    # multifn_dual_mul_add_subtract. Checked by string match on that note
    # rather than the generic bit-pattern matcher, since the JSON has no
    # pattern here to match against.
    rows = COMPUTE_TABLE["multifn_mul_dual_addsub"]["rows"]
    assert any("11xxxx" in (row.get("note") or "") for row in rows)


def test_dispatch_tables_have_no_duplicate_or_shadowed_keys():
    # ALU_OPS must not claim any opcode the dual-add-subtract range match
    # (top nibble 0x7/0xx) already owns: compute.py checks that range
    # before consulting ALU_OPS, so a colliding entry would silently never
    # be reached.
    dual_range = set(range(0x70, 0x80)) | set(range(0xF0, 0x100))
    assert not (set(ALU_OPS) & dual_range)
    # MULT_OPS must not claim 0xb4/0xb0: compute.py's pre-mf/cu checks
    # intercept those field patterns before MULT_OPS is even consulted.
    assert 0xB4 not in MULT_ALL
    assert 0xB0 not in MULT_ALL
    # Sizes double-check nothing was silently dropped or duplicated when
    # the tables were assembled by hand across compute_alu.py/
    # compute_mult.py/compute_shift.py/compute_multi.py (a Python dict
    # literal cannot contain a duplicate key, but confirms the count
    # actually intended, not just "it didn't raise").
    # 43 (32/40-bit) + 15 (SC58x/2158x PRM Table 18-6, 64-bit float ALU,
    # opcodes 0x11-0x1f, lane F1) = 58, + 0x63 Rn = clip Rx by Ry = 59.
    assert len(ALU_OPS) == 59  # +1: 0x89 Fn = (Fx + Fy) / 2 (PRM p.19-4)
    # MULT_OPS is generated from PRM Table 17-7's MOD1/MOD2/MOD3 bit layout
    # (compute_mult.py's _build_mult_ops), not hand-typed per opcode: 16 sat
    # + 2 clear + 8 rnd + 36 plain-multiply + 96 accumulate/subtract, minus
    # 0xB4/0xB0 (compute.py intercepts those before MULT_OPS), + float (1) +
    # undocumented 0x10 (1) + 64-bit float (Table 18-8, opcodes 0x31-0x33,
    # lane F1) (3) = 161.
    assert len(MULT_ALL) == 161
    assert len(SHIFT_OPS) == 13
    assert len(CU3_OPS) == 2  # lane F1: 0xd6 (pre-existing) and 0xe0
    assert len(SHORT_OPS) == 16
    assert len(MULTIFN_MUL_ALU_OPS) == 7


# Every selector-bit combination the pre-refactor if-chain (tools/
# sharc_core/compute.py at commit 7f03aa5, before this split) accepted
# without raising a ValueError, built by probing that old _compute over
# every (cu, opcode) pair (0..3 x 0..255), every short-compute opcode
# (0..15), every mrdatamove (opcode nibble, direction) pair and every
# multifunction category (0..63) -- fixed register-field values throughout
# (rn/rx/ry and friends do not affect any accept/reject branch, only which
# Const/Unknown a handler returns) -- and keeping the ones that did not
# raise. mrdatamove keys are "opcode,direction" strings, matching how the
# probe recorded them.
OLD_ACCEPTED = {
    "cu0": [
        1,
        2,
        5,
        6,
        10,
        11,
        33,
        34,
        37,
        38,
        41,
        42,
        48,
        64,
        65,
        66,
        67,
        97,
        98,
        112,
        113,
        114,
        115,
        116,
        117,
        118,
        119,
        120,
        121,
        122,
        123,
        124,
        125,
        126,
        127,
        129,
        130,
        138,
        146,
        161,
        162,
        165,
        173,
        176,
        189,
        193,
        196,
        197,
        201,
        202,
        205,
        217,
        218,
        221,
        224,
        225,
        226,
        227,
        240,
        241,
        242,
        243,
        244,
        245,
        246,
        247,
        248,
        249,
        250,
        251,
        252,
        253,
        254,
        255,
    ],
    "cu1": [0, 9, 16, 48, 64, 72, 112, 116, 124, 176, 180, 188],
    "cu2": [0, 4, 20, 32, 112, 124, 136, 140, 176, 192, 196, 200, 204],
    "cu3": [214],
    "mrdatamove": [
        "0,0",
        "0,1",
        "1,0",
        "1,1",
        "2,0",
        "2,1",
        "4,0",
        "4,1",
        "5,0",
        "5,1",
        "6,0",
        "6,1",
    ],
    "multifn": [
        0,
        1,
        24,
        25,
        26,
        28,
        29,
        30,
        31,
        48,
        49,
        50,
        51,
        52,
        53,
        54,
        55,
        56,
        57,
        58,
        59,
        60,
        61,
        62,
        63,
    ],
    "short": [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
}


def _field_dict(field23: int) -> dict:
    return {
        "compute[22:16]": (field23 >> 16) & 0x7F,
        "compute[15:0]": field23 & 0xFFFF,
    }


def _values():
    one = Const(1)
    return {code: one for code in range(256)}


def test_old_accepted_short_still_dispatches():
    values = _values()
    for opcode in OLD_ACCEPTED["short"]:
        field = (opcode << 8) | (1 << 4) | 2
        _compute({"compute": field}, True, values, {}, approx_recips=True)


def test_old_accepted_mrdatamove_still_dispatches():
    values = _values()
    for key in OLD_ACCEPTED["mrdatamove"]:
        opcode_s, direction_s = key.split(",")
        opcode, direction = int(opcode_s), int(direction_s)
        field = (1 << 22) | (direction << 16) | (opcode << 12) | (1 << 8) | 0x23
        _compute(_field_dict(field), False, values, {}, approx_recips=True)


def test_old_accepted_cu_sweep_still_dispatches():
    values = _values()
    for cu_key in ("cu0", "cu1", "cu2", "cu3"):
        cu = int(cu_key[2:])
        for opcode in OLD_ACCEPTED[cu_key]:
            field = (cu << 20) | (opcode << 12) | (1 << 8) | (2 << 4) | 3
            _compute(_field_dict(field), False, values, {}, approx_recips=True)


def test_old_accepted_multifn_still_dispatches():
    values = _values()
    for category in OLD_ACCEPTED["multifn"]:
        field = (1 << 22) | (category << 16) | (1 << 12) | (2 << 8) | 0
        _compute(_field_dict(field), False, values, {}, approx_recips=True)
