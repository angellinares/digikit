"""The `test/dsp-facts` probe sequence: what the SHARC+ core of a device does, measured by a module that runs on it.

One source for both sides. `write_inc` writes the include of the DSP module (`facts_seq.inc`: the frame routines, one block of code per frame of
the stream); `tools/device/dkprobe.py` rebuilds the same list of items to name each captured record and `derive` turns the records of all runs in
a capture into a table of facts. The module sends the checksum of the list in its run header, so a module built from another list is detected.

The stream has the framing of `dkprobe_seq` (header, 12 payload words as 4 records of 3 words, checksum; one word a block). A run is one pass over
the items, about 0.4 s on a device; a capture holds dozens of runs, so every item has many samples. An item has one record of 3 words:

  kind  words                                   what it measures
  cyc   min, first, max cycles                  the cycle counter (EMUCLK) read before and after a snippet, 8 repeats (4 for the slow ones): the smallest
                                                difference, the first one (cold caches and predictors) and the largest (interrupts). `empty` is the
                                                cost of the pair of reads; subtract it.
  val   result bits, ASTAT after, STKY after    one instruction on given operands, ASTAT bits 0-13 and STKY cleared before it (as the SHARC probe does)
  reg   first read, second read, cycles         a system or memory-mapped register read twice, and the cycles the first read took (with the pair of reads)
  time  EMUCLK now, delta, delta before         the cycle counter at three consecutive module calls: the real cycles per block and their jitter

Registers a snippet may use: R0-R13, I0-I3, I5, M1-M4 (the module contract allows R0-R15, I0-I5 and M1-M4). MODE1 is read at the start of each frame and written
only by the saturation items (ALUSAT set, interrupts off for 8 instructions), which write the saved value back. ASTAT and STKY are saved at the start of
a frame and written back at its end.
"""

from __future__ import annotations

import functools
import re
import statistics
import zlib
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from tools.device.dkprobe_seq import (
    FRAME_WORDS,
    HDR_MAGIC,
    PAD,
    PAYLOAD_WORDS,
    RECORDS_PER_FRAME,
    WORDS_PER_RECORD,
    fletcher,
    header_word,
)

__all__ = [
    "FRAME_WORDS",
    "HDR_MAGIC",
    "PAD",
    "PAYLOAD_WORDS",
    "RECORDS_PER_FRAME",
    "WORDS_PER_RECORD",
    "fletcher",
    "header_word",
]

KIND = "dsp-facts"
_LABEL = re.compile(r"^_fc\w*:")
CYC, VAL, REG, TIME = "cyc", "val", "reg", "time"
REPS = 8
SLOW_REPS = 4
TIME_RECORDS = 8
# the state of the effect holds the cache test buffer: 6 lines 16 KiB apart (4096 words) and one more word
L2_STRIDE = 4096
L2_LINES = 6
STATE_BYTES = 4 * (L2_STRIDE * (L2_LINES - 1) + 16)
# normal-word addresses: free L1 in block 1 (finding 100: both devices, 0x2dd52c to 0x2ec000), DDR at byte 0x80700000 (HWR Table 6: normal word
# 0x10000000 is byte 0x80000000; stock code and sample data lie below 0x80a00000 on a DT2, the DN2 uses less)
L1_ADDR = 0x2E0000
DDR_ADDR = 0x10000000 + (0x80700000 - 0x80000000) // 4
ASTAT_MASK = 0x3FFF
MODE1_ALUSAT = 13
MODE1_IRPTEN = 12


@dataclass(frozen=True)
class Fact:
    """One item. `op` names the family and variant, `param` is the number the family varies (the count of a loop, an address, an input)."""

    op: str
    kind: str
    param: int = 0
    args: tuple[int, ...] = ()
    reps: int = REPS
    pre: tuple[
        str, ...
    ] = ()  # each repeat, before the first read of the cycle counter (cyc)
    body: tuple[str, ...] = ()  # between the reads (cyc), or the instruction (val)
    alusat: bool = False  # val: run with MODE1.ALUSAT set
    what: str = ""  # one line for the human: the question this item answers
    n: int = -1  # units measured in the body (instructions, passes, divides) for the per-unit figure; -1: the param


# ---- the catalogue -----------------------------------------------------------------------------------------------------------


def _nops(n: int) -> tuple[str, ...]:
    return ("NOP;",) * n


def _wide(lines: tuple[str, ...]) -> tuple[str, ...]:
    return (".NOCOMPRESS;", *lines, ".COMPRESS;")


def _cyc() -> list[Fact]:
    f: list[Fact] = []
    add = f.append
    add(
        Fact(
            "empty",
            CYC,
            0,
            n=0,
            what="cost of the pair of cycle counter reads (overhead of every other cyc item)",
        )
    )
    add(
        Fact(
            "emuclk_read",
            CYC,
            16,
            body=("R2 = EMUCLK;",) * 16,
            what="cost of one cycle counter read",
        )
    )
    for n in (16, 64):
        add(
            Fact(
                "nop",
                CYC,
                n,
                body=_nops(n),
                what="cycles per NOP in the assembler's default encoding",
            )
        )
        add(
            Fact(
                "nop48",
                CYC,
                n,
                body=_wide(_nops(n)),
                what="cycles per NOP as a 48-bit instruction (fetch width)",
            )
        )
    for n in (16, 64):
        add(
            Fact(
                "alu_indep",
                CYC,
                n,
                body=("R2 = R0 + R1;",) * n,
                what="cycles per independent ALU instruction (default encoding)",
            )
        )
        add(
            Fact(
                "alu_indep48",
                CYC,
                n,
                body=_wide(("R2 = R0 + R1;",) * n),
                what="the same as 48-bit instructions",
            )
        )
    for n in (16, 64):
        add(
            Fact(
                "alu_dep",
                CYC,
                n,
                body=("R2 = R0 + R1;", "R3 = R2 + R1;") * (n // 2),
                what="cycles per dependent ALU instruction (result latency)",
            )
        )
    add(
        Fact(
            "fmul_indep",
            CYC,
            32,
            body=("F2 = F0 * F1;",) * 32,
            what="cycles per independent float multiply",
        )
    )
    add(
        Fact(
            "fmul_dep",
            CYC,
            32,
            body=("F2 = F0 * F1;", "F3 = F2 * F1;") * 16,
            what="cycles per dependent float multiply (multiplier latency)",
        )
    )
    add(
        Fact(
            "mac_fixed",
            CYC,
            32,
            body=("MRF = MRF + R0 * R1 (SSF);",) * 32,
            what="cycles per fixed-point multiply-accumulate",
        )
    )
    add(
        Fact(
            "mac_float_dual",
            CYC,
            32,
            body=("F8 = F0 * F1, F12 = F4 + F5;",) * 32,
            what="cycles per float multiply with parallel add (multifunction)",
        )
    )
    for body, count in ((1, 64), (1, 256), (4, 64), (16, 16), (16, 64)):
        add(
            Fact(
                f"do_b{body}",
                CYC,
                count,
                args=(body,),
                body=("@LOOP",),
                what=f"hardware loop of {body} instruction(s): cycles per pass and the start cost",
            )
        )
    add(
        Fact(
            "br_not_taken",
            CYC,
            32,
            pre=("R0 = 1;", "R0 = PASS R0;"),
            body=("IF EQ JUMP @END;",) * 32,
            what="cycles per conditional jump that is not taken",
        )
    )
    add(
        Fact(
            "br_taken",
            CYC,
            16,
            pre=("R0 = 1;", "R0 = PASS R0;"),
            body=("@TAKEN IF NE JUMP",) * 16,
            what="cycles per conditional jump that is taken (to the next instruction)",
        )
    )
    add(
        Fact(
            "jump",
            CYC,
            16,
            body=("@TAKEN JUMP",) * 16,
            what="cycles per unconditional jump",
        )
    )
    add(
        Fact(
            "jump_db",
            CYC,
            8,
            body=("@TAKEN JUMP DB",) * 8,
            what="cycles per delayed jump with two NOPs in its slots",
        )
    )
    add(
        Fact(
            "call_rts",
            CYC,
            16,
            body=("CALL _fc_leaf;",) * 16,
            what="cycles per CALL and RTS pair",
        )
    )
    consts = ("R0 = 0x40400000;", "R1 = 0x40e00000;", "R2 = 0x40000000;")
    add(
        Fact(
            "div_newton",
            CYC,
            1,
            pre=consts,
            body=("DK_DIV(F4, F0, F1, F2, F3, F5);",),
            what="cycles of RECIPS with two Newton steps and the multiply (a divide), dependent chain",
        )
    )
    add(
        Fact(
            "div_newton4",
            CYC,
            4,
            pre=consts,
            body=(
                "DK_DIV(F4, F0, F1, F2, F3, F5);",
                "DK_DIV(F6, F0, F1, F2, F3, F7);",
                "DK_DIV(F8, F0, F1, F2, F3, F9);",
                "DK_DIV(F10, F0, F1, F2, F3, F11);",
            ),
            what="four independent divides: how much of a divide overlaps",
        )
    )
    add(
        Fact(
            "recips_seed",
            CYC,
            16,
            pre=("R1 = 0x40e00000;",),
            body=("F2 = RECIPS F1;",) * 16,
            what="cycles per RECIPS seed",
        )
    )
    add(
        Fact(
            "rsqrt_newton",
            CYC,
            1,
            pre=("R0 = 0x40400000;", "R6 = 0x3f000000;", "R7 = 0x3fc00000;"),
            body=("DK_RSQRT(F4, F0, F6, F7, F3, F5);",),
            what="cycles of RSQRTS with three Newton steps",
        )
    )
    # memory: L2 data of the module (the state of the effect), through the L1 data cache; then free L1; the cache test; DDR last
    seq_pre = ("I0 = DM(_fc_state);",)
    add(
        Fact(
            "dm_ld_l2_seq",
            CYC,
            32,
            pre=seq_pre,
            body=("R0 = DM(I0, M6);",) * 32,
            what="32 loads from consecutive L2 words (cold, then warm in the L1 data cache)",
        )
    )
    add(
        Fact(
            "dm_ld_l2_same",
            CYC,
            32,
            pre=seq_pre,
            body=("R0 = DM(I0, M5);",) * 32,
            what="32 loads of one L2 word (L1 data cache hit)",
        )
    )
    add(
        Fact(
            "dm_st_l2_seq",
            CYC,
            32,
            pre=seq_pre,
            body=("DM(I0, M6) = R0;",) * 32,
            what="32 stores to consecutive L2 words",
        )
    )
    add(
        Fact(
            "dm_ldst_l2",
            CYC,
            16,
            n=32,
            pre=seq_pre,
            body=("R0 = DM(I0, M5);", "DM(I0, M6) = R0;") * 16,
            what="16 load and store pairs on L2 words (the data stays as it was)",
        )
    )
    l1_pre = (f"I0 = {L1_ADDR};",)
    add(
        Fact(
            "dm_ld_l1_seq",
            CYC,
            32,
            pre=l1_pre,
            body=("R0 = DM(I0, M6);",) * 32,
            what="32 loads from consecutive words of free L1 SRAM (no cache)",
        )
    )
    add(
        Fact(
            "dm_ldst_l1",
            CYC,
            16,
            n=32,
            pre=l1_pre,
            body=("R0 = DM(I0, M5);", "DM(I0, M6) = R0;") * 16,
            what="16 load and store pairs on L1 SRAM words (the data stays as it was)",
        )
    )
    for name, stride, what in (
        (
            "dm_ld_l2_lines",
            16,
            "48 loads spread over 6 cache lines (one line apart): no conflict",
        ),
        (
            "dm_ld_l2_conflict",
            L2_STRIDE,
            "48 loads over 6 lines 16 KiB apart: all map to one set of a 16 KiB cache. Slower than dm_ld_l2_lines means L2 data is cached",
        ),
    ):
        last = -(L2_LINES - 1) * stride
        add(
            Fact(
                name,
                CYC,
                stride,
                n=8 * L2_LINES,
                args=(last,),
                pre=("I0 = DM(_fc_state);", f"M1 = {stride};", f"M2 = {last};"),
                body=("@PASSES",),
                what=what,
            )
        )
    add(
        Fact(
            "dm_ld_ddr_seq",
            CYC,
            32,
            reps=SLOW_REPS,
            pre=(f"I0 = {DDR_ADDR};",),
            body=("R0 = DM(I0, M6);",) * 32,
            what="32 loads from consecutive DDR words through the normal-word alias (reads only)",
        )
    )
    return f


FIX_INPUTS = (
    0x7FC00000,
    0xFFC00000,
    0x7F800000,
    0xFF800000,
    0x4F800000,
    0xCF800000,
    0x3F000000,
    0xBF000000,
    0x3FC00000,
)
NAN, QNAN, INF, NINF = 0x7F800001, 0x7FC00000, 0x7F800000, 0xFF800000
DENORM, NDENORM, MINNORM = 0x00000001, 0x80000001, 0x00800000


def _val() -> list[Fact]:
    f: list[Fact] = []
    add = f.append
    for sat in (False, True):
        sfx = "_sat" if sat else ""
        for x in FIX_INPUTS:
            add(
                Fact(
                    f"fix{sfx}",
                    VAL,
                    x,
                    args=(x,),
                    body=("R0 = FIX F1;",),
                    alusat=sat,
                    what="FIX of NaN, infinity, overflow and halves: result and AI/AV, with ALUSAT clear and set",
                )
            )
    float2 = (
        (
            "fadd",
            "F0 = F1 + F2;",
            (
                (DENORM, 0),
                (DENORM, DENORM),
                (MINNORM, 0x80800001),
                (NAN, 0x3F800000),
                (INF, NINF),
                (INF, 0x3F800000),
            ),
        ),
        (
            "fmul",
            "F0 = F1 * F2;",
            (
                (MINNORM, 0x3F000000),
                (DENORM, 0x3F800000),
                (INF, 0),
                (QNAN, 0x3F800000),
                (0x00000100, 0x00000100),
            ),
        ),
        (
            "fmax",
            "F0 = MAX(F1, F2);",
            ((QNAN, 0x3F800000), (0x3F800000, QNAN), (DENORM, 0)),
        ),
        (
            "fmin",
            "F0 = MIN(F1, F2);",
            ((QNAN, 0x3F800000), (0x3F800000, QNAN), (DENORM, 0)),
        ),
        (
            "fcomp",
            "COMP(F1, F2);",
            ((QNAN, 0x3F800000), (0x3F800000, QNAN), (INF, INF)),
        ),
    )
    for op, line, cases in float2:
        for a, b in cases:
            add(
                Fact(
                    op,
                    VAL,
                    a,
                    args=(a, b),
                    body=(line,),
                    what="float edge cases: denormal inputs and results, NaN inputs (AI), infinity",
                )
            )
    for a in (QNAN, DENORM, 0x80000000, INF):
        add(
            Fact(
                "fabs",
                VAL,
                a,
                args=(a,),
                body=("F0 = ABS F1;",),
                what="ABS of NaN, denormal, -0, infinity",
            )
        )
    for sat in (False, True):
        sfx = "_sat" if sat else ""
        int2: tuple[tuple[str, str, tuple[tuple[int, int], ...]], ...] = (
            (
                "add",
                "R0 = R1 + R2;",
                ((0x7FFFFFFF, 1), (0x80000000, 0x80000000), (0x80000000, 0xFFFFFFFF)),
            ),
            ("sub", "R0 = R1 - R2;", ((0x80000000, 1), (0x7FFFFFFF, 0xFFFFFFFF))),
        )
        for iop, iline, icases in int2:
            for a, b in icases:
                add(
                    Fact(
                        f"{iop}{sfx}",
                        VAL,
                        a,
                        args=(a, b),
                        body=(iline,),
                        alusat=sat,
                        what="fixed-point overflow with ALUSAT clear and set: wrapped or saturated result, AV, sticky bits",
                    )
                )
        add(
            Fact(
                f"neg{sfx}",
                VAL,
                0x80000000,
                args=(0x80000000,),
                body=("R0 = -R1;",),
                alusat=sat,
                what="negation of the most negative number",
            )
        )
        add(
            Fact(
                f"abs{sfx}",
                VAL,
                0x80000000,
                args=(0x80000000,),
                body=("R0 = ABS R1;",),
                alusat=sat,
                what="ABS of the most negative number",
            )
        )
    # `R0 = R1 * R2 (SSFR);` is not used: Selache takes (SSFR) there for a symbol
    add(
        Fact(
            "mul_ssf",
            VAL,
            0x80000000,
            args=(0x80000000, 0x80000000),
            body=("R0 = R1 * R2 (SSF);",),
            what="fractional multiply of -1.0 by -1.0 (saturation of the product)",
        )
    )
    add(
        Fact(
            "mul_ssfr_mrf",
            VAL,
            0x80000000,
            args=(0x80000000, 0x80000000),
            body=("MRF = R1 * R2 (SSFR);", "R0 = MR1F;"),
            what="the same, rounded, through MRF",
        )
    )
    add(
        Fact(
            "mul_ssi",
            VAL,
            0x10000,
            args=(0x10000, 0x10000),
            body=("R0 = R1 * R2 (SSI);",),
            what="integer multiply that overflows 32 bits",
        )
    )
    return f


SYSREGS = ("IMASK", "IMASKP", "IRPTL", "LIRPTL", "MODE2", "MMASK", "TPERIOD", "TCOUNT")
MMRS = (
    ("SHDBG_REVID", 0x30026),
    ("SHDBG_CORE_ID", 0x300ED),
    ("SHDBG_OSPID", 0x30023),
    ("CMMR_SYSCTL", 0x30024),
    ("SHL1C_CFG", 0x3E000),
    ("SHL1C_CFG2", 0x3E002),
    *(
        (f"SHL1C_RANGE_{'START' if i % 2 == 0 else 'END'}{i // 2}", 0x3E010 + i)
        for i in range(16)
    ),
    ("CMMR_PFB_NOCHRT0_START", 0x3B100),
    ("CMMR_PFB_NOCHRT0_END", 0x3B101),
    ("CMMR_GPERR_STAT", 0x3B000),
    ("CMMR_PWR_L1_SD_CTL", 0x3E030),
    ("CMMR_PWR_GLB_CTL", 0x3E031),
    ("CMMR_PWR_L1_LS_CTL", 0x3E032),
    ("PFM_CFG", 0x30200),
)


def _reg() -> list[Fact]:
    f = [
        Fact(
            f"sys_{n.lower()}",
            REG,
            0,
            body=(f"R0 = {n};",),
            what=f"system register {n}",
        )
        for n in SYSREGS
    ]
    f += [
        Fact(
            f"mmr_{n.lower()}",
            REG,
            a,
            body=(f"R0 = DM({a:#x});",),
            what=f"memory-mapped register {n} at {a:#x}",
        )
        for n, a in MMRS
    ]
    return f


def facts() -> list[Fact]:
    """The items in the order the module runs them. Cycle counts first, then instruction semantics, then the register reads, and the DDR reads last."""
    return list(_facts())


@functools.cache
def _facts() -> tuple[Fact, ...]:
    cyc = _cyc()
    ddr = [x for x in cyc if x.op.startswith("dm_ld_ddr")]
    out: list[Fact] = [
        Fact("block", TIME, i, what="cycle counter at three consecutive module calls")
        for i in range(TIME_RECORDS)
    ]
    out += [x for x in cyc if x not in ddr]
    out += _val()
    out += _reg()
    out += ddr
    return tuple(out)


# ---- the sequence interface (the same as dkprobe_seq) -----------------------------------------------------------------------


def items() -> list[tuple[str, int]]:
    return [(x.op, x.param) for x in facts()]


def table_words() -> list[int]:
    """One word per item (a CRC of its identity): the module sends the checksum of these in its run header."""
    return [
        zlib.crc32(repr((x.op, x.kind, x.param, x.args, x.reps, x.alusat)).encode())
        for x in facts()
    ]


def table_checksum() -> int:
    return fletcher(table_words())


def records_per_run() -> int:
    n = 2 + len(items())
    return -(-n // RECORDS_PER_FRAME) * RECORDS_PER_FRAME


def frames_per_run() -> int:
    return records_per_run() // RECORDS_PER_FRAME


def words_per_run() -> int:
    return frames_per_run() * FRAME_WORDS


# ---- the generated include --------------------------------------------------------------------------------------------------


def _store3(a: str, b: str, c: str) -> list[str]:
    return [f"DM(I5, M6) = {a};", f"DM(I5, M6) = {b};", f"DM(I5, M6) = {c};"]


def _expand(f: Fact, r: int) -> list[str]:
    """Replace the placeholders of a cyc body by code that needs labels of the record `r`."""
    out: list[str] = []
    if f.body == ("@LOOP",):
        (body_len,) = f.args
        label = f"_fc_l{r}"
        return [
            f"LCNTR = {f.param}, DO {label} UNTIL LCE;",
            *["NOP;"] * (body_len - 1),
            f"{label}: NOP;",
        ]
    if f.body == ("@PASSES",):
        lines = ["R0 = DM(I0, M1);"] * (L2_LINES - 1) + ["R0 = DM(I0, M2);"]
        label = f"_fc_l{r}"
        return [
            f"LCNTR = 8, DO {label} UNTIL LCE;",
            *lines[:-1],
            f"{label}: {lines[-1]}",
        ]
    for j, line in enumerate(f.body):
        if line.startswith("@TAKEN "):
            ins = line[len("@TAKEN ") :]
            lbl = f"_fc_t{r}_{j}"
            if ins.endswith(" DB"):
                out += [f"{ins[:-3]} {lbl} (DB);", "NOP;", "NOP;", f"{lbl}:"]
            else:
                out += [f"{ins} {lbl};", f"{lbl}:"]
        elif "@END" in line:
            out.append(line.replace("@END", f"_fc_e{r}"))
        else:
            out.append(line)
    if any("@END" in line for line in f.body):
        out.append(f"_fc_e{r}:")
    return out


def _emit_cyc(f: Fact, r: int) -> list[str]:
    """The repeats keep their statistics in DM words (the snippets use R0-R12): the cycle counter goes through R14 and R15, the difference through R13."""
    top = f"_fc_c{r}"
    body = _expand(f, r)
    lines = [
        "R0 = 0x7fffffff;",
        "DM(_fc_min) = R0;",
        "R0 = 0;",
        "DM(_fc_first) = R0;",
        "DM(_fc_max) = R0;",
        f"R0 = {f.reps};",
        "DM(_fc_reps) = R0;",
        f"{top}:",
        *f.pre,
        "R14 = EMUCLK;",
        *body,
        "R15 = EMUCLK;",
        "R13 = R15 - R14;",
        "R10 = DM(_fc_min);",
        "COMP(R13, R10);",
        "IF LT R10 = PASS R13;",
        "DM(_fc_min) = R10;",
        "R10 = DM(_fc_max);",
        "COMP(R13, R10);",
        "IF GT R10 = PASS R13;",
        "DM(_fc_max) = R10;",
        "R10 = DM(_fc_first);",
        "R10 = PASS R10;",
        "IF EQ R10 = PASS R13;",
        "DM(_fc_first) = R10;",
        "R10 = DM(_fc_reps);",
        "R10 = R10 - 1;",
        "DM(_fc_reps) = R10;",
        f"IF NE JUMP {top};",
        "R0 = DM(_fc_min);",
        "R1 = DM(_fc_first);",
        "R2 = DM(_fc_max);",
        *_store3("R0", "R1", "R2"),
    ]
    return lines


def _emit_val(f: Fact) -> list[str]:
    ops = [f"R1 = {f.args[0]:#x};"] if f.args else []
    if len(f.args) > 1:
        ops.append(f"R2 = {f.args[1]:#x};")
    pre: list[str] = ["R0 = 0;"]
    post: list[str] = []
    if f.alusat:
        pre = [
            "R0 = MODE1;",
            "DM(_fc_mode1) = R0;",
            f"R5 = {0xFFFFFFFF ^ (1 << MODE1_IRPTEN):#x};",
            "R5 = R0 AND R5;",
            f"R5 = BSET R5 BY {MODE1_ALUSAT};",
            "MODE1 = R5;",
            "NOP;",
            "NOP;",
            "R0 = 0;",
        ]
        post = ["R5 = DM(_fc_mode1);", "MODE1 = R5;", "NOP;", "NOP;"]
    return [
        *ops,
        *pre,
        "R6 = DM(_fc_astat0);",
        "R7 = 0xffffc000;",
        "R7 = R6 AND R7;",
        "R12 = 0;",
        "ASTAT = R7;",
        "STKY = R12;",
        "NOP;",
        "NOP;",
        *f.body,
        "NOP;",
        "NOP;",
        "R3 = ASTAT;",
        "R4 = STKY;",
        *post,
        *_store3("R0", "R3", "R4"),
    ]


def _emit_reg(f: Fact) -> list[str]:
    """Two reads of the register. The cache registers (SHL1C, 0x3e000 to 0x3e0ff) get what the manual asks of any access to them: 48-bit instructions, and twelve
    unrelated 48-bit instructions after it (sc58x-2158x-prm p. 8-9)."""
    core = [
        "R14 = EMUCLK;",
        *f.body,
        "R15 = EMUCLK;",
        "R1 = R0;",
        *f.body,
        "R13 = R15 - R14;",
    ]
    if f.op.startswith("mmr_shl1c_"):
        core = [".NOCOMPRESS;", *core, *_nops(12), ".COMPRESS;"]
    return [*core, *_store3("R1", "R0", "R13")]


def _emit_record(r: int, facts_: list[Fact]) -> list[str]:
    n = len(facts_)
    if r == 0:
        return [
            "R0 = DM(_fc_run);",
            "R1 = FC_NITEMS;",
            "R2 = FC_TABCKS;",
            *_store3("R0", "R1", "R2"),
        ]
    if r == 1:
        return [
            "R0 = MODE1;",
            "R1 = DM(_fc_astat0);",
            "R2 = DM(_fc_stky0);",
            *_store3("R0", "R1", "R2"),
        ]
    if r - 2 >= n:
        return ["R0 = -1;", *_store3("R0", "R0", "R0")]
    f = facts_[r - 2]
    if f.kind == TIME:
        return [
            "R0 = DM(_fc_t0);",
            "R1 = DM(_fc_d1);",
            "R2 = DM(_fc_d2);",
            *_store3("R0", "R1", "R2"),
        ]
    if f.kind == CYC:
        return _emit_cyc(f, r)
    if f.kind == VAL:
        return _emit_val(f)
    return _emit_reg(f)


def inc_text() -> str:
    fs = facts()
    lines = [
        "// Generated by tools/device/dkprobe_facts.py (`uv run python -m tools.device.dkprobe_facts --write PATH`). Do not edit.",
        "// The frame routines of the dsp-facts stream: frame k builds records 4k to 4k+3 in the frame buffer through I5 (M6 = 1).",
        '#include "dsp/recip.inc"',
        f"#define FC_NITEMS {len(fs)}",
        f"#define FC_NFRAMES {frames_per_run()}",
        f"#define FC_NRECORDS {records_per_run()}",
        f"#define FC_TABCKS {table_checksum()}",
        f"#define FC_STATE_BYTES {STATE_BYTES}",
        "",
        ".SECTION/PM seg_pmco;",
        ".GLOBAL _fc_leaf;",
        "_fc_leaf:",
        "    RTS;",
        "_fc_ladder:",
        "    R0 = DM(_fc_fr);",
    ]
    for k in range(frames_per_run()):
        lines += [f"    R1 = {k};", "    COMP(R0, R1);", f"    IF EQ JUMP _fc_f{k};"]
    lines.append("    JUMP _fc_f0;")
    for k in range(frames_per_run()):
        lines.append(f"_fc_f{k}:")
        for r in range(RECORDS_PER_FRAME * k, RECORDS_PER_FRAME * (k + 1)):
            if 2 <= r < 2 + len(fs):
                f = fs[r - 2]
                lines.append(f"    // record {r}: {f.op} {f.param:#x} ({f.kind})")
            else:
                lines.append(f"    // record {r}")
            lines += [x if _LABEL.match(x) else "    " + x for x in _emit_record(r, fs)]
        lines.append("    JUMP _fc_fin;")
    return "\n".join(lines) + "\n"


def write_inc(path: Path) -> None:
    path.write_text(inc_text())


# ---- the host side: the table of facts --------------------------------------------------------------------------------------


def _stat(v: list[int]) -> dict[str, int]:
    return {"min": min(v), "median": int(statistics.median(v)), "max": max(v)}


def _slope(lo: dict[str, Any], hi: dict[str, Any], key: str) -> float:
    return (hi[key] - lo[key]) / (hi["param"] - lo["param"])


def derive(samples: dict[int, list[tuple[int, int, int]]]) -> dict[str, Any]:
    """The table of facts from the records of every run in the capture: statistics per item and the numbers they give together."""
    fs = facts()
    rows: list[dict[str, Any]] = []
    by_op: dict[str, list[dict[str, Any]]] = {}
    block: list[int] = []
    for i, f in enumerate(fs):
        s = samples.get(i, [])
        row: dict[str, Any] = {
            "index": i,
            "op": f.op,
            "kind": f.kind,
            "param": f.param,
            "runs": len(s),
            "what": f.what,
        }
        if f.kind == CYC:
            row["units"] = f.n if f.n >= 0 else f.param
        if f.args:
            row["args"] = [f"{a:#x}" for a in f.args]
        if s and f.kind == CYC:
            row["cycles_min"] = min(w[0] for w in s)
            row["cycles_cold"] = int(statistics.median(w[1] for w in s))
            row["cycles_max"] = max(w[2] for w in s)
            row["cycles_median"] = int(statistics.median(w[0] for w in s))
        elif s and f.kind == VAL:
            # the item clears ASTAT bits 0 to 13 only: the bits above are the firmware's own state at that moment, so they are left out
            vals = sorted({(w[0], w[1] & ASTAT_MASK, w[2]) for w in s})
            row["result"] = f"{vals[0][0]:#010x}"
            row["astat"] = f"{vals[0][1]:#010x}"
            row["stky"] = f"{vals[0][2]:#010x}"
            row["stable"] = len(vals) == 1
        elif s and f.kind == REG:
            first = sorted({w[0] for w in s})
            second = sorted({w[1] for w in s})
            row["first_read"] = [f"{v:#010x}" for v in first[:4]]
            row["second_read"] = [f"{v:#010x}" for v in second[:4]]
            row["stable"] = len(first) == 1 and first == second
            row["cycles_min"] = min(w[2] for w in s)
        elif s and f.kind == TIME:
            for w in s:
                block += [w[1], w[2]]
        rows.append(row)
        if f.kind == CYC and "cycles_min" in row:
            by_op.setdefault(f.op, []).append(row)
    derived: dict[str, Any] = {}
    empty = by_op.get("empty", [{}])[0].get("cycles_min")
    if empty is not None:
        derived["overhead_cycles"] = empty
        net: dict[str, Any] = {}
        for op, rs in by_op.items():
            if op == "empty":
                continue
            rs = sorted(rs, key=lambda r: r["param"])
            entry: dict[str, Any] = {
                "net": {str(r["param"]): r["cycles_min"] - empty for r in rs},
                "cold_extra": {
                    str(r["param"]): r["cycles_cold"] - r["cycles_min"] for r in rs
                },
            }
            last = rs[-1]
            if last["units"] > 0:
                entry["per_unit"] = round(
                    (last["cycles_min"] - empty) / last["units"], 3
                )
            if len(rs) > 1:
                entry["slope"] = round(_slope(rs[0], rs[-1], "cycles_min"), 3)
            net[op] = entry
        derived["cyc"] = net
        lines, conflict = net.get("dm_ld_l2_lines"), net.get("dm_ld_l2_conflict")
        if lines and conflict:
            ln = next(iter(lines["net"].values()))
            cf = next(iter(conflict["net"].values()))
            derived["l2_conflict_over_lines"] = round(cf / ln, 2) if ln > 0 else None
    block = [b for b in block if b > 0]
    if block:
        med = statistics.median_low(block)
        # a delta far from the median is a block that was skipped or late; the rest gives the jitter
        near = [b for b in block if abs(b - med) <= med * 0.25]
        derived["block_cycles"] = {
            "samples": len(block),
            "median": int(med),
            "min": min(near),
            "max": max(near),
            "stdev": round(statistics.pstdev(near), 1),
            "outliers": len(block) - len(near),
            "outlier_values": sorted({b for b in block if b not in near})[:8],
        }
    return {"items": rows, "derived": derived}


def compare(a: dict[str, Any], b: dict[str, Any]) -> dict[str, Any]:
    """A device table against another (the emulator): cycle counts as ratios, values and registers as equal or not."""
    ia = {r["index"]: r for r in a["facts"]["items"]}
    ib = {r["index"]: r for r in b["facts"]["items"]}
    cyc: list[dict[str, Any]] = []
    vals: list[dict[str, Any]] = []
    for i in sorted(set(ia) & set(ib)):
        ra, rb = ia[i], ib[i]
        if ra["kind"] == CYC and "cycles_min" in ra and "cycles_min" in rb:
            cyc.append(
                {
                    "op": ra["op"],
                    "param": ra["param"],
                    "a": ra["cycles_min"],
                    "b": rb["cycles_min"],
                    "ratio": round(ra["cycles_min"] / rb["cycles_min"], 2)
                    if rb["cycles_min"]
                    else None,
                }
            )
        elif ra["kind"] == VAL and "result" in ra and "result" in rb:
            same = (ra["result"], ra["astat"], ra["stky"]) == (
                rb["result"],
                rb["astat"],
                rb["stky"],
            )
            if not same:
                vals.append(
                    {
                        "op": ra["op"],
                        "args": ra.get("args"),
                        "a": [ra["result"], ra["astat"], ra["stky"]],
                        "b": [rb["result"], rb["astat"], rb["stky"]],
                    }
                )
        elif (
            ra["kind"] == REG
            and "first_read" in ra
            and "first_read" in rb
            and (
                ra["first_read"] != rb["first_read"]
                or ra["second_read"] != rb["second_read"]
            )
        ):
            vals.append(
                {
                    "op": ra["op"],
                    "a": ra["first_read"] + ra["second_read"],
                    "b": rb["first_read"] + rb["second_read"],
                }
            )
    da, db = a["facts"]["derived"], b["facts"]["derived"]
    return {
        "derived": {"a": da, "b": db},
        "cycles": cyc,
        "different_values": vals,
        "same": not vals,
        "compared": len(set(ia) & set(ib)),
        "only_a": sorted(set(ia) - set(ib))[:20],
        "only_b": sorted(set(ib) - set(ia))[:20],
        "same_sequence": a["sequence"] == b["sequence"],
    }


def compare_lines(res: dict[str, Any]) -> list[str]:
    out = [
        f"compared {res['compared']} items: {len(res['different_values'])} value or register items differ; "
        f"{len(res['cycles'])} cycle items (a is the capture, b the reference)"
    ]
    for v in res["different_values"][:12]:
        out.append(
            f"  differs: {v['op']} {v.get('args') or ''} capture {v['a']} reference {v['b']}"
        )
    big = sorted(
        (c for c in res["cycles"] if c["ratio"]),
        key=lambda c: abs((c["ratio"] or 1) - 1),
        reverse=True,
    )
    for c in big[:8]:
        out.append(
            f"  cycles {c['op']}({c['param']}): {c['a']} against {c['b']} (x{c['ratio']})"
        )
    ba, bb = (
        res["derived"]["a"].get("block_cycles"),
        res["derived"]["b"].get("block_cycles"),
    )
    if ba or bb:
        out.append(
            f"  block cycles: capture {ba and ba['median']} (stdev {ba and ba['stdev']}), reference {bb and bb['median']}"
        )
    return out


def main(argv: list[str] | None = None) -> int:
    import argparse

    ap = argparse.ArgumentParser(
        description="Write the dsp-facts sequence include for the DSP module"
    )
    ap.add_argument("--write", type=Path, required=True, help="the .inc file to write")
    args = ap.parse_args(argv)
    write_inc(args.write)
    print(
        f"{len(items())} items, {records_per_run()} records, {words_per_run()} words per run, table checksum 0x{table_checksum():08x}, state {STATE_BYTES} bytes"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
