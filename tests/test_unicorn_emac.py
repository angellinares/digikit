# pyright: reportMissingImports=false
"""Unicorn's ColdFire V4e core against the manual's EMAC semantics.

The interrupt handler that sends the SHARC control frame (Digitakt II
0x4002d652, Digitone II 0x40025e36) saves the EMAC state with `movclr` and
moves from MACSR, MASK and ACCext. Before emulating that handler we need to
know Unicorn executes these instructions. Unicorn exposes no EMAC registers,
so each case sets them with instructions and copies them back into D and A
registers. Expected values come from the ColdFire Programmer's Reference
Manual (docs/refs/CFPRM.pdf, chapter 6), not from running Unicorn.

MAC and MSAC with load run as the manual says only with
patches/unicorn-2.1.4-m68k-emac-mac-load.patch; stock Unicorn 2.1.4 fails
every LOAD_CASES entry. Fractional mode runs as the manual says only with
patches/unicorn-2.1.4-m68k-emac-fractional.patch; without it every
FRACTIONAL_CASES entry fails.

Two differences from the manual are pinned as known gaps, so a Unicorn
change that fixes or alters them fails here and gets noticed.
"""

import struct
import unittest

from unicorn import UC_ARCH_M68K, UC_MODE_BIG_ENDIAN, UC_PROT_ALL, Uc, UcError
from unicorn import m68k_const

CODE = 0x10000
DATA = 0x20000

# (name, words, initial registers, instruction count, expected registers)
CASES = (
    (
        # move.l D7,MACSR; move.l D3,ACC0; movclr.l ACC0,D4; move.l ACC0,D5
        # p.6-6: ACC -> Rx, 0 -> ACC while MACSR[OMC] = 0.
        "movclr moves and clears the accumulator",
        ["a907", "a103", "a1c4", "a185"],
        {"D3": 0xDEADBEEF, "D7": 0},
        4,
        {"D4": 0xDEADBEEF, "D5": 0},
    ),
    (
        # move.l D2,MASK; move.l MASK,D6
        # p.6-20: only the low word is written; p.6-13: Rx[31:16] = 0xFFFF.
        "MASK round trip keeps the low word",
        ["ad02", "ad86"],
        {"D2": 0x1234ABCD},
        2,
        {"D6": 0xFFFFABCD},
    ),
    (
        # move.l D6,MACSR; move.l D7,MASK; move.l D6,ACC0;
        # mac.l D1,D2,ACC0 (two words); move.l ACC0,D0
        # p.6-2: ACCx + Ry * Rx -> ACCx in signed integer mode.
        "mac.l without load accumulates the product",
        ["a906", "ad07", "a106", "a401", "0800", "a180"],
        {"D1": 5, "D2": 7, "D6": 0, "D7": 0xFFFF},
        5,
        {"D0": 35},
    ),
    (
        # Setup: move.l D0..D3,ACC0..ACC3; move.l D4,ACCext01;
        # move.l D5,ACCext23; move.l D6,MASK; move.l D7,MACSR.
        # ACCx are loaded before ACCext because a move to ACCx rewrites that
        # accumulator's extension (p.6-15). Then the handler prologue:
        # move.l MACSR,A0; move.l #0,MACSR; move.l ACCext01,D4;
        # move.l ACCext23,D5; movclr.l ACC0..ACC3,D0..D3; move.l MASK,D6.
        # Then move.l ACC0..ACC3,A1..A4 to show the accumulators are 0.
        # D7 = 0xC21 has no bits above 11, so the MACSR gap below does not
        # affect A0.
        "handler prologue saves and clears the EMAC state",
        [
            "a100", "a301", "a502", "a703", "ab04", "af05", "ad06", "a907",
            "a988", "a93c", "0000", "0000", "ab84", "af85",
            "a1c0", "a3c1", "a5c2", "a7c3", "ad86",
            "a189", "a38a", "a58b", "a78c",
        ],
        {
            "D0": 0x12345678, "D1": 0x00000001, "D2": 0xFFFFFFFF,
            "D3": 0x7FFFFFFF, "D4": 0x11223344, "D5": 0x55667788,
            "D6": 0x000000FF, "D7": 0x00000C21,
        },
        21,
        {
            "A0": 0x00000C21, "D0": 0x12345678, "D1": 0x00000001,
            "D2": 0xFFFFFFFF, "D3": 0x7FFFFFFF, "D4": 0x11223344,
            "D5": 0x55667788, "D6": 0xFFFF00FF,
            "A1": 0, "A2": 0, "A3": 0, "A4": 0,
        },
    ),
)

# MAC and MSAC with load (p.6-3 to 6-5, p.6-22 to 6-23). D2 holds a decoy:
# stock Unicorn reads a data-register Rx from D2 whatever the extension
# word says.
# (name, words, initial registers, memory, instruction count, expected registers)
LOAD_CASES = (
    (
        # move.l D7,MACSR; move.l D5,ACC0; mac.w D6u,D0u,(A1),D4,ACC0;
        # move.l ACC0,D1. This is Digitakt II 0x400db9e0. ACC0 + 3 * 5 ->
        # ACC0 and (A1) -> D4; ext bit 5 is 0, so MASK (0 after reset) is
        # not used.
        "mac.w with load, Ry = D6",
        ["a907", "a105", "a891", "00c6", "a181"],
        {
            "D7": 0, "D5": 0, "D6": 0x00030002, "D0": 0x00050004,
            "D2": 0x00090008, "D4": 0xAAAAAAAA, "A1": DATA,
        },
        {DATA: 0x0000002A},
        4,
        {"D1": 15, "D4": 0x0000002A, "A1": DATA},
    ),
    (
        # move.l D7,MACSR; move.l D5,ACC0; mac.l D4,D0,(A1),D5,ACC0;
        # move.l ACC0,D1. ACC0 + 7 * 5 -> ACC0 and (A1) -> D5.
        "mac.l with load reads Rx from the extension word",
        ["a907", "a105", "aa91", "0804", "a181"],
        {"D7": 0, "D5": 0, "D0": 5, "D2": 9, "D4": 7, "A1": DATA},
        {DATA: 0x0000002A},
        4,
        {"D1": 35, "D5": 0x0000002A},
    ),
    (
        # move.l D7,MACSR; move.l D5,ACC0; move.l D1,MASK;
        # msac.w D4u,D0u,(A2)+&,D5,ACC0; move.l ACC0,D1.
        # ACC0 - 7 * 5 -> ACC0. Ext bit 5 applies MASK: 0x21010 & 0xFFFF0FFF
        # is 0x20010, and 0x21010 itself is not mapped.
        "msac.w with load subtracts and applies MASK",
        ["a907", "a105", "ad01", "aa9a", "01e4", "a181"],
        {
            "D7": 0, "D5": 100, "D1": 0x00000FFF, "D0": 0x00050004,
            "D2": 0x00090008, "D4": 0x00070006, "A2": 0x21010,
        },
        {DATA + 0x10: 0x11223344},
        5,
        {"D1": 65, "D5": 0x11223344},
    ),
    (
        # move.l D7,MACSR; move.l D5,ACC0; mac.w D6l,D0l,-(A2),D5,ACC0;
        # move.l ACC0,D1. ACC0 + 2 * 4 -> ACC0, A2 - 4 -> A2, (A2) -> D5.
        "mac.w with load, predecrement",
        ["a907", "a105", "aaa2", "0006", "a181"],
        {
            "D7": 0, "D5": 0, "D6": 0x00030002, "D0": 0x00050004,
            "D2": 0x00090008, "A2": DATA + 0x20,
        },
        {DATA + 0x1C: 0x11223344},
        4,
        {"D1": 8, "D5": 0x11223344, "A2": DATA + 0x1C},
    ),
)


# Signed fractional mode, MACSR[F/I] = 1 (MCF54418RM section 5.3, p.5-9 to
# 5-17, PDF p.151 to 159; store rules CFPRM p.6-6 to 6-9). The product is
# (operandY * operandX) << 1 of the signed operands, truncated or rounded
# (MACSR[R/T]) to product[63:24] and sign-extended into ACC[47:0] =
# {ACCext[15:8], ACCn, ACCext[7:0]}; -1 * -1 is zero-filled to +1.0.
# Each case clears ACC0 first with movclr.l ACC0,D3.
# (name, words, initial registers, instruction count, expected registers)
FRACTIONAL_CASES = (
    (
        # move.l D7,MACSR; movclr ACC0,D3; mac.w D6u,D0u,ACC0; movclr ACC0,D1
        "mac.w: 0.5 * 0.5 = 0.25",
        ["a907", "a1c3", "a006", "00c0", "a1c1"],
        {"D7": 0x20, "D0": 0x40000000, "D6": 0x40000000},
        4,
        {"D1": 0x20000000},
    ),
    (
        # move.l D7,MACSR; movclr ACC0,D3; mac.l D4,D5,ACC0; movclr ACC0,D2
        "mac.l: -0.5 * 0.5 = -0.25",
        ["a907", "a1c3", "aa04", "0800", "a1c2"],
        {"D7": 0x20, "D4": 0xC0000000, "D5": 0x40000000},
        4,
        {"D2": 0xE0000000},
    ),
    (
        # move.l D7,MACSR; movclr ACC0,D3; msac.l D4,D5,ACC0; movclr ACC0,D2
        "msac.l: 0 - 0.5 * 0.5 = -0.25",
        ["a907", "a1c3", "aa04", "0900", "a1c2"],
        {"D7": 0x20, "D4": 0x40000000, "D5": 0x40000000},
        4,
        {"D2": 0xE0000000},
    ),
    (
        # move.l D7,MACSR; movclr ACC0,D3; mac.l D4,D5,ACC0;
        # move.l MACSR,D2; movclr ACC0,D1.
        # -1 * -1 = +1.0 = ACC 0x0080_0000_0000: ACC[47:39] differ, so EV
        # (p.5-17); without OMC the store is ACC[39:8] (CFPRM p.6-8).
        "-1 * -1 is +1.0, sets EV, stores unsaturated",
        ["a907", "a1c3", "aa04", "0800", "a982", "a1c1"],
        {"D7": 0x20, "D4": 0x80000000, "D5": 0x80000000},
        5,
        {"D2": 0x21, "D1": 0x80000000},
    ),
    (
        # As above with MACSR[OMC]: the store saturates by ACC[47]
        # (CFPRM p.6-8, OMC,S/U,R/T = 100).
        "OMC saturates a fractional store",
        ["a907", "a1c3", "aa04", "0800", "a982", "a1c1"],
        {"D7": 0xA0, "D4": 0x80000000, "D5": 0x80000000},
        5,
        {"D2": 0xA1, "D1": 0x7FFFFFFF},
    ),
    (
        # move.l D7,MACSR; movclr ACC0,D3; mac.l D4,D5,ACC0; movclr ACC0,D2.
        # MACSR[S/U] stores ACC[47:24] rounded, in Rx[15:0] (CFPRM p.6-8).
        "S/U stores a 16-bit fraction",
        ["a907", "a1c3", "aa04", "0800", "a1c2"],
        {"D7": 0x60, "D4": 0xC0000000, "D5": 0x40000000},
        4,
        {"D2": 0x0000E000},
    ),
    (
        # move.l D7,MACSR; movclr ACC0,D3; mac.l D4,D5,ACC0;
        # move.l ACCext01,D1. 1 * 0xC00000 << 1 = 0x1800000: product[23:0]
        # is the halfway 0x800000 and product[24] is 1, so R/T rounds
        # product[63:24] from 1 up to 2 (p.5-17). ACC0's low extension
        # byte is D1[7:0] (CFPRM p.6-10).
        "R/T rounds the product to nearest even, up",
        ["a907", "a1c3", "aa04", "0800", "ab81"],
        {"D7": 0x30, "D4": 1, "D5": 0x00C00000},
        4,
        {"D1": 2},
    ),
    (
        # 1 * 0x400000 << 1 = 0x800000: halfway with product[24] = 0 stays 0.
        "R/T rounds the product to nearest even, down",
        ["a907", "a1c3", "aa04", "0800", "ab81"],
        {"D7": 0x30, "D4": 1, "D5": 0x00400000},
        4,
        {"D1": 0},
    ),
    (
        # Without R/T the product is truncated: 0x1800000 >> 24 = 1.
        "truncation without R/T",
        ["a907", "a1c3", "aa04", "0800", "ab81"],
        {"D7": 0x20, "D4": 1, "D5": 0x00C00000},
        4,
        {"D1": 1},
    ),
    (
        # move.l D7,MACSR; move.l D6,ACC0; move.l D5,MACSR; movclr ACC0,D1.
        # ACC0 is a register: a mode change moves its place in the 48-bit
        # accumulator, not its bits (p.5-9), which the handler's EMAC
        # save and restore (p.5-11) relies on.
        "integer to fractional keeps ACC0",
        ["a907", "a106", "a905", "a1c1"],
        {"D7": 0, "D5": 0x20, "D6": 0x12345678},
        4,
        {"D1": 0x12345678},
    ),
    (
        "fractional to integer keeps ACC0",
        ["a907", "a106", "a905", "a1c1"],
        {"D7": 0x20, "D5": 0, "D6": 0x12345678},
        4,
        {"D1": 0x12345678},
    ),
)


def reg(name):
    return getattr(m68k_const, "UC_M68K_REG_" + name)


def run(words, init, count, memory=None):
    code = b"".join(struct.pack(">H", int(word, 16)) for word in words)
    uc = Uc(UC_ARCH_M68K, UC_MODE_BIG_ENDIAN)
    uc.ctl_set_cpu_model(m68k_const.UC_CPU_M68K_CFV4E)
    uc.mem_map(CODE, 0x1000, UC_PROT_ALL)
    uc.mem_write(CODE, code)
    if memory:
        uc.mem_map(DATA, 0x1000, UC_PROT_ALL)
        for address, value in memory.items():
            uc.mem_write(address, struct.pack(">I", value))
    uc.reg_write(reg("SR"), 0x2700)
    for name, value in init.items():
        uc.reg_write(reg(name), value)
    uc.emu_start(CODE, CODE + len(code), count=count)
    return uc, CODE + len(code)


class UnicornEmacTest(unittest.TestCase):
    def check(self, name, uc, end, expect):
        self.assertEqual(uc.reg_read(reg("PC")), end, "stopped early")
        for register, value in expect.items():
            self.assertEqual(
                uc.reg_read(reg(register)) & 0xFFFFFFFF,
                value,
                "%s: %s = %#010x" % (name, register, uc.reg_read(reg(register))),
            )

    def test_cases(self):
        for name, words, init, count, expect in CASES:
            with self.subTest(name):
                uc, end = run(words, init, count)
                self.check(name, uc, end, expect)

    def test_load_cases(self):
        for name, words, init, memory, count, expect in LOAD_CASES:
            with self.subTest(name):
                try:
                    uc, end = run(words, init, count, memory)
                except UcError as exc:
                    self.fail("%s: %s" % (name, exc))
                self.check(name, uc, end, expect)

    def test_fractional_cases(self):
        for name, words, init, count, expect in FRACTIONAL_CASES:
            with self.subTest(name):
                uc, end = run(words, init, count)
                self.check(name, uc, end, expect)

    def test_known_gap_macsr_read_keeps_high_bits(self):
        # move.l #$ffffffff,MACSR; move.l MACSR,D0
        # p.6-12 clears Rx[31:12]; Unicorn returns all 32 bits.
        uc, end = run(["a93c", "ffff", "ffff", "a980"], {}, 2)
        self.assertEqual(uc.reg_read(reg("PC")), end)
        self.assertEqual(uc.reg_read(reg("D0")) & 0xFFFFFFFF, 0xFFFFFFFF)

    def test_known_gap_move_acc_to_acc_not_implemented(self):
        # move.l D7,MACSR; move.l D1,ACC1; move.l D2,ACC2; move.l ACC1,ACC2
        # p.6-14 defines a511; Unicorn takes an exception on it, which shows
        # up as an unmapped read because no vector table is mapped.
        with self.assertRaises(UcError):
            run(
                ["a907", "a301", "a502", "a511"],
                {"D1": 0x11111111, "D2": 0x22222222, "D7": 0},
                4,
            )


if __name__ == "__main__":
    unittest.main()
