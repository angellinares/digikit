"""Register codes, reset values, flag bits, access widths and instruction-field access.

Moved verbatim from tools/sharc_trace.py.
"""

from __future__ import annotations

from collections.abc import Mapping

UREG_NAMES = tuple(
    [f"R{i}" for i in range(16)]
    + [f"I{i}" for i in range(16)]
    + [f"M{i}" for i in range(16)]
    + [f"L{i}" for i in range(16)]
    + [f"B{i}" for i in range(16)]
    + [f"S{i}" for i in range(16)]
    + [
        "FADDR",
        "DADDR",
        "UREG_RESERVED_62",
        "PC",
        "PCSTK",
        "PCSTKP",
        "LADDR",
        "CURLCNTR",
        "LCNTR",
        "EMUCLK",
        "EMUCLK2",
        "PX",
        "PX1",
        "PX2",
        "TPERIOD",
        "TCOUNT",
        "USTAT1",
        "USTAT2",
        "MODE1",
        "MMASK",
        "MODE2",
        "FLAGS",
        "ASTATX",
        "ASTATY",
        "STKYX",
        "STKYY",
        "IRPTL",
        "IMASK",
        "IMASKP",
        "MODE1STK",
        "USTAT3",
        "USTAT4",
    ]
)
UREG_CODES = {name: code for code, name in enumerate(UREG_NAMES)}

# Public SHARC+ register tables document these reset values.  Keep this list
# deliberately bounded to core state used by startup rather than treating
# every absent UREG as zero.
CORE_UREG_RESET_VALUES = {
    name: 0
    for name in (
        "MODE1",
        "MMASK",
        "MODE1STK",
        "MODE2",
        "PCSTK",
        "PCSTKP",
        "LADDR",
        "LCNTR",
        "CURLCNTR",
        "ASTATX",
        "ASTATY",
        "STKYX",
        "STKYY",
        "IRPTL",
        "IMASK",
        "IMASKP",
    )
}
CORE_MMR_RESET_VALUES = {
    0x30024: 0,  # CMMR_SYSCTL
    0x31400: 0,  # SHBTB_CFG
    0x31401: 0,  # SHBTB_LOCK_START
    0x31402: 0,  # SHBTB_LOCK_END
    0x3E000: 0,  # SHL1C_CFG
    0x3E002: 0,  # SHL1C_CFG2
}

# ADSP-2156x L1 block 3 aliases.  The normal-word window is the one used by
# the reset path's PM(...)=PX table read; the loader records the same physical
# storage through the short-word/system-byte view.
L1_BLOCK3_NW_BASE = 0x000E0000
L1_BLOCK3_NW_LIMIT = 0x000E8000
L1_BLOCK3_SW_BASE = 0x001C0000

# The harness's opt-in explicit-MMR-model (State.explicit_memory_model, see
# sharc_core/memory.py) carves these two address envelopes out of the
# general "unwritten internal RAM reads as 0" policy.  Core MMRs
# (tools/sharcimm.py's CORE_MMR_REGS: CMMR_SYSCTL 0x30024, SHBTB_* 0x31400-
# 0x31402, SHL1C_CFG* 0x3E000/0x3E002) are named with small, compact
# addresses distinct in scale from the peripheral/system MMR bus below;
# 0x30000-0x32000 is the compact envelope most of them sit in (SHL1C_CFG*
# at 0x3E00x is outside it but is still caught by the CORE_MMR_RESET_VALUES/
# name_address lookup memory.py already does, independent of this range).
CORE_MMR_RANGE = (0x30000, 0x32000)
# tools/sharcimm.py's PERIPHERAL_SPACE is the full ADSP-2156x peripheral bus
# (0x30000000-0x31ffffff); every named block tools/sharcimm.py's BLOCKS
# table actually knows about sits inside 0x31000000-0x310fffff.
SYSTEM_MMR_RANGE = (0x31000000, 0x310FFFFF)

# ASTATX/ASTATY bit positions (SHARC+ PRM ch.4 REGF_ASTATX/REGF_ASTATY).
AZ_BIT, AV_BIT, AN_BIT, AC_BIT, AS_BIT, AI_BIT = 0, 1, 2, 3, 4, 5
MN_BIT, MV_BIT, MU_BIT, MI_BIT = 6, 7, 8, 9
AF_BIT = 10
SV_BIT, SZ_BIT, SS_BIT = 11, 12, 13
SF_BIT = 14  # Shifter Bit FIFO (PRM Table 28-2, REGF_ASTATX bit 14, all.txt:29659)
BTF_BIT = 18
ALUSAT_BIT = 13  # MODE1.ALUSAT
TRUNCATE_BIT = 15  # MODE1.TRUNCATE (PRM Table 28-19, p.28-63): rounding mode
# select for FIX -- 0 rounds to nearest (ties to even), 1 truncates toward
# zero. TRUNC always truncates and does not consult this bit (PRM p.24-.. /
# PGR p.11-37: "The trunc operation always truncates toward 0. The TRUNCATE
# bit does not influence operation of the trunc instruction.").

# Bits every fixed-point ALU op (add/sub/inc/dec/pass/not/and/or/xor/compare)
# defines: AZ/AV/AN/AC/AS/AI, plus AF which ch.19's intro says every
# fixed-point ALU op clears (PRM p.439).
ALU_FLAGS_MASK = (
    (1 << AZ_BIT)
    | (1 << AV_BIT)
    | (1 << AN_BIT)
    | (1 << AC_BIT)
    | (1 << AS_BIT)
    | (1 << AI_BIT)
    | (1 << AF_BIT)
)
# Multiplier-result flags (MN/MV/MU/MI); the tracer does not model the
# multiplier result format, so these are always left unknown except for the
# MR data-move, which the PRM (p.493) documents as clearing all four.
MULT_FLAGS_MASK = (1 << MN_BIT) | (1 << MV_BIT) | (1 << MU_BIT) | (1 << MI_BIT)
# Shifter-result flags (SV/SZ/SS); forgotten (not guessed) for an
# undocumented shifter opcode or an unrecognized compute unit.
SHIFT_FLAGS_MASK = (1 << SV_BIT) | (1 << SZ_BIT) | (1 << SS_BIT)

# Type3b's (l, x, w) ACCESS/BH/BHSE encode table (SHARC+ Core Programming
# Reference rev. 1.4, pp. 13-16--13-19), used by this module's own "3b"
# _execute branch below. Type4d's BH/BHSE tables (pp.13-34/13-35) are this
# table's w=0 slice. Type4b is NOT this table: see TYPE4B_ACCESS_WIDTHS.
ACCESS_WIDTHS = {
    (0, 1, 1): "normal-word",
    (0, 0, 0): "byte",
    (0, 1, 0): "byte-sign-extended",
    (1, 0, 0): "short-word",
    (1, 1, 0): "short-word-sign-extended",
    (1, 1, 1): "long-word",
}

# Type4b's (l, x, w) BH/BHSE encode tables (PRM p.13-32). They differ from
# Type3b's in two rows: (1, 1, 1) is the row with no suffix, the plain
# normal-word access, and (0, 1, 1) is absent. Type4b has no (lw) option:
# Table 13-12 (p.13-29) lists none, while Table 13-8 (Type3b, p.13-16) and
# Type3b's ACCESS table (p.13-18) spell (lw) out as l=w=x=1. The DT2 1.16
# image agrees: its 156 aligned (1, 1, 1) accesses include post-modify
# strides of 7 words (misaligned for a 64-bit access every other step) and
# loads whose neighbour register holds a live value, e.g. sw 0x1cccf3
# "R1 = DM(I0 - 5)" then sw 0x1cccf5 "(1, 1, 1) R0 = DM(I0)" then sw
# 0x1ccd03 "DM(I4, M6) = R1".
TYPE4B_ACCESS_WIDTHS = {
    (1, 1, 1): "normal-word",
    (0, 0, 0): "byte",
    (0, 1, 0): "byte-sign-extended",
    (1, 0, 0): "short-word",
    (1, 1, 0): "short-word-sign-extended",
}

# IF-condition codes (PGR Table 10-4) that read a single ASTATX bit,
# optionally complemented.
SIMPLE_COND_BITS = {
    0x03: (AC_BIT, False),
    0x13: (AC_BIT, True),
    0x04: (AV_BIT, False),
    0x14: (AV_BIT, True),
    0x05: (MV_BIT, False),
    0x15: (MV_BIT, True),
    0x06: (MN_BIT, False),
    0x16: (MN_BIT, True),
    0x07: (SV_BIT, False),
    0x17: (SV_BIT, True),
    0x08: (SZ_BIT, False),
    0x18: (SZ_BIT, True),
    0x0D: (BTF_BIT, False),
    0x1D: (BTF_BIT, True),
}


def _field(f: Mapping[str, int], stem: str) -> int:
    # Most lookups name a key exactly; no decoded form has both "x" and
    # "x[...]" keys, so the exact hit is the value the scan would return.
    value = f.get(stem)
    if value is not None:
        return value
    for key, value in f.items():
        if key == stem or key.startswith(stem + "["):
            return value
    raise KeyError(stem)


def _wide(f: Mapping[str, int], stem: str) -> int:
    return (_field(f, stem + "[31:16]") << 16) | _field(f, stem + "[15:0]")


def _split_compute_fields(f: Mapping[str, int]) -> dict[str, int]:
    """F plus its 23-bit "compute" field split into the "compute[22:16]" and
    "compute[15:0]" fields ``_compute`` reads (Type 3a names the field
    whole). It depends on the decoded fields alone, so a generator
    evaluates it once per instruction, at generation time."""
    fields = dict(f)
    compute = _field(f, "compute")
    fields["compute[22:16]"] = compute >> 16
    fields["compute[15:0]"] = compute & 0xFFFF
    return fields
