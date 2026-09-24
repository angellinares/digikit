# pyright: reportMissingImports=false
"""tools/sharcfn.py: dossier construction and mnemonic rendering on synthetic
code and a hand-built boot stream.

No real firmware is used (it is Elektron's copyright and is not committed);
instructions are built directly from tools/sharc_visa_tables.py the same way
tests/test_sharcflow.py and tests/test_sharcinv.py do, and reuses their
encoding helpers rather than duplicating them.
"""

import json
import os
import sqlite3
import struct
import sys
import tempfile
import unittest
from collections import Counter

sys.path.insert(
    0,
    os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "tools"),
)

import sharc_disasm  # noqa: E402
import sharcfn  # noqa: E402
import sharcinv  # noqa: E402
import sharcldr  # noqa: E402
from test_sharc_disasm import encode  # noqa: E402
from test_sharcflow import call8a_rel, cjump, load, push3c, store, words  # noqa: E402
from test_sharcinv import field_insn, loop_insn, ret, rframe  # noqa: E402
from test_sharcldr import block as boot_block  # noqa: E402


def insn_at(data, offset=0):
    """The single decoded Instruction at `offset` of `data`."""
    return next(sharc_disasm.disassemble(data, offset))


def empty_ctx():
    return set(), Counter(), []


class NamedRegionsTest(unittest.TestCase):
    def test_prefers_sharcinv_named_tables(self):
        addr, name = next(iter(sharcinv.NAMED_TABLES.items()))
        self.assertEqual(sharcfn.name_literal_region(addr), name)

    def test_extra_regions_not_in_sharcinv(self):
        self.assertEqual(sharcfn.name_literal_region(0x264138), "ring_head_C")
        self.assertEqual(sharcfn.name_literal_region(0x264170), "ring_head_D")
        self.assertEqual(sharcfn.name_literal_region(0x256388), "pair128_a")
        self.assertEqual(sharcfn.name_literal_region(0x256588), "pair128_b")
        self.assertEqual(sharcfn.name_literal_region(0x252D3C), "shared_context")
        self.assertEqual(sharcfn.name_literal_region(0x25D940), "ram_table_25d940")

    def test_audio_ring_buffers(self):
        self.assertEqual(sharcfn.name_literal_region(0x262138), "ring_C_buf0")
        self.assertEqual(sharcfn.name_literal_region(0x263938), "ring_D_buf1")

    def test_per_track_parameter_block(self):
        # docs/findings/functions/README.md: 0x2559b6 + track*0x60, SRC page
        # in the first 0x14 bytes of each per-track block.
        track2_base = 0x2559B6 + 2 * 0x60
        region = sharcfn.name_literal_region(track2_base)
        self.assertIn("track2", region)
        self.assertIn("+0x0", region)
        self.assertIn("SRC page", region)
        region2 = sharcfn.name_literal_region(track2_base + 0x20)
        self.assertIn("track2", region2)
        self.assertNotIn("SRC page", region2)

    def test_peripheral_fallback(self):
        region = sharcfn.name_literal_region(0x30024)  # CMMR_SYSCTL
        self.assertIn("CMMR_SYSCTL", region)

    def test_unnamed_address_returns_none(self):
        self.assertIsNone(sharcfn.name_literal_region(0x12345678))


class FloatConstantsTest(unittest.TestCase):
    def test_known_constants_match(self):
        self.assertEqual(sharcfn.float_constant_name(3.14159265), "pi")
        self.assertEqual(sharcfn.float_constant_name(4294967296.0), "2^32")
        self.assertEqual(sharcfn.float_constant_name(96000.0), "96000.0 (sample rate)")
        self.assertIsNone(sharcfn.float_constant_name(1.2345))

    def test_describe_literal_decodes_float_and_flags_constant(self):
        mem = sharcldr.LoadedMemory.from_stream(b"")
        bits = struct.unpack("<I", struct.pack("<f", 96000.0))[0]
        # sharcinv.float32 reinterprets the merged field value big-endian;
        # build the bit pattern the same way compute_table.json's floats
        # round-trip through it (see sharcinv.float32's own docstring use).
        bits = struct.unpack(">I", struct.pack(">f", 96000.0))[0]
        d = sharcfn.describe_literal(bits, float_capable=True, mem=mem)
        self.assertAlmostEqual(d["float"], 96000.0, places=2)
        self.assertEqual(d["float_constant"], "96000.0 (sample rate)")

    def test_describe_literal_ram_vs_rom(self):
        # 0x200000-0x30000000 is the DM address band describe_literal()
        # treats as address-shaped; a plain small test value like 0x1000
        # (not in that band, no named region) correctly gets no RAM/ROM
        # verdict at all -- only an address-shaped literal does.
        data = boot_block(0, 0x200000, 4, payload=b"\x01\x02\x03\x04")
        mem = sharcldr.LoadedMemory.from_stream(data)
        self.assertEqual(
            sharcfn.describe_literal(0x200000, False, mem)["memory"], "ROM"
        )
        self.assertEqual(
            sharcfn.describe_literal(0x25D940, False, mem)["memory"], "RAM"
        )
        self.assertNotIn("memory", sharcfn.describe_literal(0x1000, False, mem))


class ComputeRenderingTest(unittest.TestCase):
    def _field23(self, cu, opcode, rn=0, rx=0, ry=0, mf=0):
        return (mf << 22) | (cu << 20) | (opcode << 12) | (rn << 8) | (rx << 4) | ry

    def test_alu_binary(self):
        f = self._field23(0, 0x01, rn=3, rx=4, ry=5)  # 'add'
        text, gap = sharcfn.render_compute(f)
        self.assertEqual(text, "R3 = add(R4, R5)")
        self.assertFalse(gap)

    def test_alu_unary(self):
        f = self._field23(0, 0x22, rn=1, rx=2)  # 'neg'
        text, gap = sharcfn.render_compute(f)
        self.assertEqual(text, "R1 = neg(R2)")
        self.assertFalse(gap)

    def test_alu_float_op(self):
        f = self._field23(0, 0x81, rn=1, rx=2, ry=3)  # 'fadd', float
        text, gap = sharcfn.render_compute(f)
        self.assertEqual(text, "F1 = fadd(F2, F3)")

    def test_alu_task_flagged_gap_opcode_still_marked(self):
        # 0xe0 has a name in sharcinv.ALU_OPS ('copysign') but the task
        # brief calls it out as unverified; it must still be flagged.
        self.assertIn(0xE0, sharcinv.ALU_OPS)
        f = self._field23(0, 0xE0, rn=1, rx=2, ry=3)
        text, gap = sharcfn.render_compute(f)
        self.assertTrue(gap)
        self.assertIn("GAP", text)
        self.assertIn("copysign", text)

    def test_alu_unmapped_opcode_is_a_gap(self):
        # 0x99: top nibble 9 (not 7/F, so not dual-add-sub) and absent from
        # sharcinv.ALU_OPS.
        self.assertNotIn(0x99, sharcinv.ALU_OPS)
        f = self._field23(0, 0x99, rn=1, rx=2, ry=3)
        text, gap = sharcfn.render_compute(f)
        self.assertTrue(gap)
        self.assertIn("alu?0x99", text)

    def test_dual_add_subtract(self):
        # cu=0, opcode[19:16]=0111 fixed (PRM Table 18-10/18-13): Rs 15:12,
        # Ra 11:8, Rx 7:4, Ry 3:0.
        f = (0 << 20) | (0x7 << 16) | (0xA << 12) | (1 << 8) | (2 << 4) | 3
        text, gap = sharcfn.render_compute(f)
        self.assertEqual(text, "R1 = R2 + R3; R10 = R2 - R3")

    def test_mult_plain_opcode_0x30_is_not_a_gap(self):
        # PRM Table 18-7's special case: opcode 0011 0000 = 'FN = FX * FY'.
        f = self._field23(1, 0x30, rn=5, rx=6, ry=7)
        text, gap = sharcfn.render_compute(f)
        self.assertEqual(text, "F5 = F6 * F7")
        self.assertFalse(gap)

    def test_mult_mac_add(self):
        # top2=10 (MAC add), fixed-point: opcode top bits '10'.
        f = self._field23(1, 0b10000000, rn=1, rx=2, ry=3)
        text, gap = sharcfn.render_compute(f)
        self.assertIn("MAC add", text)
        self.assertFalse(gap)

    def test_mult_unmapped_is_a_gap(self):
        f = self._field23(
            1, 0b11000000 | 0b0100, rn=1, rx=2, ry=3
        )  # top2=11, MAC sub -> named
        text, gap = sharcfn.render_compute(f)
        self.assertFalse(gap)  # MAC sub is a resolved category, not a gap

    def test_shift_known_op(self):
        f = self._field23(2, 0x00, rn=1, rx=2, ry=3)  # 'lshift'
        text, gap = sharcfn.render_compute(f)
        self.assertEqual(text, "R1 = lshift(R2, R3)")
        self.assertFalse(gap)

    def test_shift_task_flagged_gap_opcode(self):
        self.assertNotIn(0xB0, sharcinv.SHIFT_OPS)
        f = self._field23(2, 0xB0, rn=1, rx=2, ry=3)
        text, gap = sharcfn.render_compute(f)
        self.assertTrue(gap)
        self.assertIn("GAP", text)

    def test_multifn_mul_alu(self):
        # mf=1, selector (bits 21:16) = 0b000100 -- PGR Table 12-12's first
        # row, 'RM = R3-0 * R7-4 (SSFR), RA = RXA + RYA'. Rxm/Rym/Rxa/Rya are
        # each a 2-bit CODE (0-3) restricted to their own quad (Rxm in
        # R0-3, Rym in R4-7, Rxa in R8-11, Rya in R12-15) -- the rendered
        # text must show the quad-offset final register, not the raw code.
        rya_code, rxa_code, rym_code, rxm_code, ra, rm = 3, 2, 1, 0, 5, 6
        selector = 0b000100
        field23 = (
            (1 << 22)
            | (selector << 16)
            | (rm << 12)
            | (ra << 8)
            | (rxm_code << 6)
            | (rym_code << 4)
            | (rxa_code << 2)
            | rya_code
        )
        text, gap = sharcfn.render_compute(field23)
        self.assertEqual(text, "R6 = R0 * R5 (SSFR), R5 = R10 + R15")
        self.assertFalse(gap)

    def test_multifn_mul_alu_reserved_selector_is_a_gap(self):
        # 0b000111 is not one of PGR Table 12-12's rows.
        selector = 0b000111
        field23 = (1 << 22) | (selector << 16)
        text, gap = sharcfn.render_compute(field23)
        self.assertTrue(gap)

    def test_multifn_mul_dual_addsub(self):
        # top3=110 (MUL + dual add/sub, fixed). Same quad-restricted 2-bit
        # input codes as the MUL+ALU case.
        rs, rm, ra = 7, 6, 5
        rxm_code, rym_code, rxa_code, rya_code = 0, 1, 2, 3
        field23 = (
            (1 << 22)
            | (0b10 << 20)
            | (rs << 16)
            | (rm << 12)
            | (ra << 8)
            | (rxm_code << 6)
            | (rym_code << 4)
            | (rxa_code << 2)
            | rya_code
        )
        text, gap = sharcfn.render_compute(field23)
        self.assertIn("R6 = R0 * R5", text)
        self.assertIn("R5 = R10 + R15", text)
        self.assertIn("R7 = R10 - R15", text)
        self.assertFalse(gap)


class ShortComputeTest(unittest.TestCase):
    def test_binary_op(self):
        field12 = (0x0 << 8) | (3 << 4) | 4  # 'add', rn=3, rx=4
        self.assertEqual(sharcfn.render_shortcompute(field12), "R3 = add(R3, R4)")

    def test_unary_rx_op(self):
        field12 = (0x2 << 8) | (3 << 4) | 4  # 'pass'
        self.assertEqual(sharcfn.render_shortcompute(field12), "R3 = pass(R4)")

    def test_inc_dec_read_rx_not_rn(self):
        # PRM Table 17-2 (p.17-3): opcode 0101/0110's "Instruction" column
        # is "RN = RX + 1" / "RN = RX - 1" -- RX is the only source read;
        # RN is write-only, unlike the earlier ("operate on RN itself")
        # belief this test used to encode.
        field12 = (0x6 << 8) | (3 << 4) | 4  # 'dec', rn=3, rx=4
        self.assertEqual(sharcfn.render_shortcompute(field12), "R3 = dec(R4)")

    def test_float_op_uses_f_registers(self):
        field12 = (0x8 << 8) | (3 << 4) | 4  # 'fadd'
        self.assertEqual(sharcfn.render_shortcompute(field12), "F3 = fadd(F3, F4)")


class PerInstructionRenderingTest(unittest.TestCase):
    def setUp(self):
        self.mem = sharcldr.LoadedMemory.from_stream(b"")

    def test_return_recognized_before_generic_jump_dispatch(self):
        data = words(sharcinv.RETURN_JUMP >> 16, sharcinv.RETURN_JUMP & 0xFFFF)
        insn = insn_at(data)
        self.assertEqual(insn.type_name, "9b_abs")
        mnemonic, notes, gap = sharcfn.render_instruction(
            0x1000, insn, self.mem, *empty_ctx()
        )
        self.assertEqual(mnemonic, "RETURN")
        self.assertFalse(gap)

    def test_plain_jump_is_not_a_return(self):
        insn = insn_at(cjump(0x2000))  # 25a_direct, always a call, not a return
        mnemonic, notes, gap = sharcfn.render_instruction(
            0x1000, insn, self.mem, *empty_ctx()
        )
        self.assertIn("CALL", mnemonic)
        self.assertIn("target=0x2000", mnemonic)

    def test_conditional_delayed_jump(self):
        insn = insn_at(call8a_rel(0x10, b=0, cond=5, j=1))
        mnemonic, notes, gap = sharcfn.render_instruction(
            0x1000, insn, self.mem, *empty_ctx()
        )
        self.assertIn("JUMP", mnemonic)
        # cond=5 is PGR Table 10-4's "MV" -- named, not the bare number a
        # reader could confuse with an address or register.
        self.assertIn("IF MV", mnemonic)
        self.assertNotIn("cond=5", mnemonic)
        self.assertIn("delayed", mnemonic)

    def test_loop_literal_trip_count_and_body(self):
        sw, insn = loop_insn(0x1000, count=64, reladdr=10, form="12a_imm")
        mnemonic, notes, gap = sharcfn.render_instruction(
            sw, insn, self.mem, *empty_ctx()
        )
        self.assertIn("trip count 64, literal", mnemonic)
        self.assertIn("body [0x1003, 0x100a)", mnemonic)

    def test_loop_register_trip_count(self):
        sw, insn = loop_insn(
            0x1000, count=4, reladdr=10, form="12a_ureg"
        )  # ureg=4 -> I4
        mnemonic, notes, gap = sharcfn.render_instruction(
            sw, insn, self.mem, *empty_ctx()
        )
        self.assertIn("register)", mnemonic)

    def test_literal_load_annotates_region_and_float(self):
        bits = struct.unpack(">I", struct.pack(">f", 96000.0))[0]
        insn = insn_at(field_insn("17a", ureg=0, data=bits))
        named, regions, floats = set(), sharcfnCounterLike(), []
        mnemonic, notes, gap = sharcfn.render_instruction(
            0x1000, insn, self.mem, named, regions, floats
        )
        self.assertEqual(mnemonic, "R0 = 0x%08x" % bits)
        self.assertTrue(any("96000.0" in n for n in notes))
        self.assertTrue(any(abs(x - 96000.0) < 1 for x in floats))

    def test_undecoded_instruction_gets_loud_marker(self):
        insn = sharc_disasm.Instruction(
            offset=0,
            length_bytes=None,
            type_name="unknown",
            fields={},
            raw=0xBEEF,
            kind="unknown",
            note="no form matches",
        )
        mnemonic, notes, gap = sharcfn.render_instruction(
            0x1000, insn, self.mem, *empty_ctx()
        )
        self.assertIn("UNDECODED", mnemonic)
        self.assertTrue(gap)

    def test_unhandled_form_falls_back_to_field_dump_not_silence(self):
        insn = insn_at(encode("21a"))
        mnemonic, notes, gap = sharcfn.render_instruction(
            0x1000, insn, self.mem, *empty_ctx()
        )
        self.assertTrue(mnemonic)  # never empty/None


class Type19ModifyRenderingTest(unittest.TestCase):
    """render_modify()'s destination for the Type19 MODIFY family: PGR
    Table/Figure for Type 19 encodes it as Is XOR Idis, not as Is directly
    (tools/sharc_trace.py's "19a"/"19a_scaled" _compute branch already does
    this, citing the same figure)."""

    def setUp(self):
        self.mem = sharcldr.LoadedMemory.from_stream(b"")

    def render(self, form, **fields):
        data = field_insn(form, **fields)
        insn = insn_at(data)
        mnemonic, notes, gap = sharcfn.render_instruction(
            0x1000, insn, self.mem, *empty_ctx()
        )
        return mnemonic

    def test_dag1_destination_is_is_xor_idis_not_is(self):
        # is=4, idis=6, g=0 (DAG1, I0-I7): dest = 4^6 = 2, not 4.
        mnemonic = self.render("19a", **{"g": 0, "idis": 6, "is": 4, "data": 0x44})
        self.assertIn("I2 = modify(I4, 0x44)", mnemonic)
        self.assertNotIn("I4 = modify(I4,", mnemonic)

    def test_dag2_bank_offset_applies_to_both_registers(self):
        # Same is/idis but g=1 (DAG2, I8-I15): both registers shift by +8.
        mnemonic = self.render("19a", **{"g": 1, "idis": 6, "is": 4, "data": 0x44})
        self.assertIn("I10 = modify(I12, 0x44)", mnemonic)

    def test_19a_scaled_destination_also_uses_xor(self):
        mnemonic = self.render(
            "19a_scaled", **{"g": 0, "idis": 3, "is": 5, "data": 0x10}
        )
        self.assertIn("I6 = modify(I5, 0x10)", mnemonic)

    def test_19a_bitrev_destination_also_uses_xor(self):
        mnemonic = self.render(
            "19a_bitrev", **{"g": 0, "idis": 1, "is": 2, "data": 0x8}
        )
        self.assertIn("I3 = modify(I2, 0x8)", mnemonic)


class Type7aModifyRenderingTest(unittest.TestCase):
    """render_instruction() had no branch for Type7a MODIFY at all (SHARC+
    Core Programming Reference pp.13-46/13-48, "Ia = MODIFY(Ia,Mb)"): a
    pure-MODIFY (compute=0) instance fell through to the raw "[7a] ..."
    field dump, and a compute!=0 instance rendered only its compute half,
    silently dropping the MODIFY. Dest is Is XOR Idis, the same trick
    Type19a already uses (see Type19ModifyRenderingTest above) -- idis=0
    leaves dest equal to source, matching the PRM's own worked example
    "I3 = MODIFY(I3,M5); /* Semantically same as MODIFY(I3,M5) */"."""

    def setUp(self):
        self.mem = sharcldr.LoadedMemory.from_stream(b"")

    def render(self, **fields):
        data = field_insn("7a", **fields)
        insn = insn_at(data)
        mnemonic, _notes, _gap = sharcfn.render_instruction(
            0x1000, insn, self.mem, *empty_ctx()
        )
        return mnemonic

    def test_pure_modify_same_register_has_no_dest_assignment(self):
        mnemonic = self.render(
            **{"g": 0, "cond": 0x1F, "is": 7, "idis": 0, "m": 7, "compute": 0}
        )
        self.assertEqual(mnemonic, "modify(I7, M7)")

    def test_compute_and_modify_to_a_different_register_both_render(self):
        # is=4, idis=6, g=0 -> dest = 4^6 = 2 (a different I register); a
        # nonzero compute (cu=0 opcode=0x01 'add', rn=3,rx=4,ry=5 -- see
        # ComputeRenderingTest.test_alu_binary) must still show, not
        # silently replace the MODIFY.
        mnemonic = self.render(
            **{"g": 0, "cond": 0x1F, "is": 4, "idis": 6, "m": 3, "compute": 0x1345}
        )
        self.assertEqual(mnemonic, "R3 = add(R4, R5); I2 = modify(I4, M3)")
        self.assertNotIn("[7a]", mnemonic)


class Type6aRenderingTest(unittest.TestCase):
    """render_mem_indexed()'s destination register for 6a_mem (a plain
    "dreg" field, unlike 3a/3b/3d's wide "ureg"), and render_shiftimm()'s
    readable decode of the parallel ShiftImm sub-instruction PRM Type 6a/6b
    share (PRM Table 18-9 pp.431-433; tools/sharc_trace.py's
    _shift_immediate())."""

    def setUp(self):
        self.mem = sharcldr.LoadedMemory.from_stream(b"")

    def render(self, form, **fields):
        data = field_insn(form, **fields)
        insn = insn_at(data)
        mnemonic, notes, gap = sharcfn.render_instruction(
            0x1000, insn, self.mem, *empty_ctx()
        )
        return mnemonic

    def test_6a_mem_destination_is_dreg_not_placeholder(self):
        # opcode 0x00 (lshift) with rn=rx=0, amount=0 -> a no-op shiftimm,
        # so the mnemonic below is driven entirely by the mem-transfer half.
        mnemonic = self.render(
            "6a_mem",
            **{"i": 4, "m": 5, "cond": 0x1F, "g": 0, "d": 0, "dreg": 0},
        )
        self.assertIn("R0 = DM(I4, M5)", mnemonic)
        self.assertNotIn("? =", mnemonic)

    def test_6a_mem_renders_parallel_shiftimm_field_extract(self):
        # opcode 0x10 (fext), rn=12, rx=10, dataex=3, data8=5 ->
        # position=5, length=(3<<2)|0=12.
        shiftimm = (0x10 << 16) | (5 << 8) | (12 << 4) | 10
        mnemonic = self.render(
            "6a_mem",
            **{
                "i": 4,
                "m": 5,
                "cond": 0x1F,
                "g": 0,
                "d": 0,
                "dreg": 0,
                "dataex": 3,
                "shiftimm": shiftimm,
            },
        )
        self.assertIn("R0 = DM(I4, M5)", mnemonic)
        self.assertIn("R12 = fext(R10, pos=5, len=12)", mnemonic)

    def test_render_shiftimm_shift_immediate(self):
        # opcode 0x01 (ashift), rn=3, rx=4, data8=8 (amount=8).
        shiftimm = (0x01 << 16) | (8 << 8) | (3 << 4) | 4
        mnemonic = self.render(
            "6b_shiftimm", **{"cond": 0x1F, "dataex": 0, "shiftimm": shiftimm}
        )
        self.assertEqual(mnemonic, "R3 = ashift(R4, 8)")

    def test_render_shiftimm_unmodelled_opcode_falls_back_to_raw_dump(self):
        # opcode 0x02 ("rot") is in PRM's table but not implemented by
        # tools/sharc_trace.py's _shift_immediate(); stays a raw dump
        # rather than guessing at its field widths.
        shiftimm = (0x02 << 16) | (8 << 8) | (3 << 4) | 4
        mnemonic = self.render(
            "6b_shiftimm", **{"cond": 0x1F, "dataex": 0, "shiftimm": shiftimm}
        )
        self.assertIn("shiftimm(dataex=0x0, shiftimm=0x%x)" % shiftimm, mnemonic)


class ImmediateOffsetSignednessTest(unittest.TestCase):
    """render_mem_immoff()'s "data" displacement: Type15b's decode_table.json
    field is data[6:0] (7 bits), Type4a/4b/4d's is data[5:5]+data[4:0] (6
    bits); merge_fields() only concatenates the split pieces, it never
    sign-extends. tools/sharc_trace.py's "15b"/"4a"/"4b" _execute branches
    already treat the merged value as a signed twos-complement index
    modifier (_signed(..., 7) / _signed(..., 6)) before using it as an
    address delta -- this class checks the listing renderer agrees."""

    def setUp(self):
        self.mem = sharcldr.LoadedMemory.from_stream(b"")

    def render(self, form, **fields):
        data = field_insn(form, **fields)
        insn = insn_at(data)
        mnemonic, notes, gap = sharcfn.render_instruction(
            0x1000, insn, self.mem, *empty_ctx()
        )
        return mnemonic

    def test_15b_seven_bit_offset_above_63_is_negative(self):
        # data=115 (7-bit raw): sign_extend(115, 7) == 115 - 128 == -13.
        mnemonic = self.render("15b", i=6, d=1, l=0, g=0, ureg=2, data=115)
        self.assertIn("DM(I6 - 13) = R2", mnemonic)
        self.assertNotIn("+ 115", mnemonic)

    def test_15b_seven_bit_offset_below_64_stays_positive(self):
        mnemonic = self.render("15b", i=6, d=1, l=0, g=0, ureg=2, data=14)
        self.assertIn("DM(I6 + 14) = R2", mnemonic)

    def test_4a_six_bit_offset_above_31_is_negative(self):
        # data=0x3F (6-bit raw, all ones): sign_extend(0x3F, 6) == -1.
        # compute=0 -- no parallel compute -- isolates the mem-access half.
        mnemonic = self.render(
            "4a", i=6, g=0, d=1, u=0, cond=0x1F, data=0x3F, dreg=2, compute=0
        )
        self.assertIn("DM(I6 - 1) = R2", mnemonic)

    def test_4b_six_bit_offset_above_31_is_negative(self):
        mnemonic = self.render(
            "4b", i=6, g=0, d=0, u=0, cond=0x1F, data=0x3F, dreg=3, l=1, w=1, x=1
        )
        self.assertIn("R3 = DM(I6 - 1)", mnemonic)

    def test_15a_data32_is_an_i_register_relative_offset_not_absolute(self):
        # DT2 1.16 sw 0x1c69f1, raw a309ffffffbf (g=0, i=1, d=1, l=0,
        # ureg=9/R9, addr=0xffffffbf): a previous version of
        # render_mem_direct() grouped Type15a with the pure-absolute
        # 14a/14d forms and rendered this "DM(0xffffffbf) = R9", dropping
        # the I1 register entirely. PRM (out/refs/sharc-plus-prm) p.387's
        # Syntax Summary ("DM(<data32>,Ia) = Ureg") and p.388's Description
        # ("The I register is pre-modified with an immediate value...") say
        # this is I-register-relative: addr=0xffffffbf as a signed 32-bit
        # data32 is -0x41 (-65), so the correct rendering is "DM(I1 -
        # 0x41)", the same signed-offset style _fmt_index_offset() already
        # uses for Type4a/15b's own (narrower) immediate.
        mnemonic = self.render(
            "15a", g=0, i=1, d=1, l=0, ureg=9, addr=0xFFFFFFBF
        )
        self.assertIn("DM(I1 - 0x41) = R9", mnemonic)
        self.assertNotIn("DM(0xffffffbf)", mnemonic)


class CondPrefixRenderingTest(unittest.TestCase):
    """render_instruction()'s IF-condition rendering: PGR Table 10-4
    (p.10-33) names the 5-bit COND field's 32 codes. Type2a, 3a/3b/3d,
    4a/4b/4d, 5a/5b (move+swap) and 6b_shiftimm all carry a real "cond"
    field that gated the whole instruction (PRM p.7924's Type3a note,
    cited in tools/sharc_trace.py) but that render_instruction() dropped
    on the floor entirely -- a predicated instruction looked identical to
    an unconditional one. Type2a_short/2c/2b are checked too, to confirm
    they are correctly left alone: decode_table.json gives them no COND
    bits at all (a structurally different, always-unconditional 32-bit
    encoding), not merely an always-true instance of Type2a."""

    def setUp(self):
        self.mem = sharcldr.LoadedMemory.from_stream(b"")

    def render(self, form, **fields):
        data = field_insn(form, **fields)
        insn = insn_at(data)
        mnemonic, notes, gap = sharcfn.render_instruction(
            0x1000, insn, self.mem, *empty_ctx()
        )
        return mnemonic

    # Table 18-11 ALU register fields (RN 11:8, RX 7:4, RY 3:0) on top of
    # ALU_OPS[0x81] == 'fadd': F8 = fadd(F8, F2), the same encoding as the
    # real DT2 1.16 blob's 0x1cbf47 (independently derived here from the
    # public bit layout, not copied from the firmware).
    FADD_F8_F8_F2 = (0x81 << 12) | (8 << 8) | (8 << 4) | 2

    def test_2a_nontrue_cond_renders_if_prefix(self):
        mnemonic = self.render("2a", cond=1, compute=self.FADD_F8_F8_F2)
        self.assertEqual(mnemonic, "IF LT F8 = fadd(F8, F2)")

    def test_2a_always_true_cond_has_no_prefix(self):
        mnemonic = self.render("2a", cond=0x1F, compute=self.FADD_F8_F8_F2)
        self.assertEqual(mnemonic, "F8 = fadd(F8, F2)")
        self.assertNotIn("IF", mnemonic)

    def test_2a_short_has_no_cond_field_to_render(self):
        # Type2a_short has no COND bits at all -- unlike Type2a, there is
        # no encoding of this instruction that is conditional.
        mnemonic = self.render("2a_short", compute=self.FADD_F8_F8_F2)
        self.assertEqual(mnemonic, "F8 = fadd(F8, F2)")
        self.assertNotIn("IF", mnemonic)

    def test_3a_nontrue_cond_renders_if_prefix(self):
        mnemonic = self.render(
            "3a", u=0, i=1, m=4, cond=3, g=0, d=1, l=0, ureg=12, compute=0
        )
        self.assertIn("IF AC", mnemonic)
        self.assertIn("DM(I1, M4)", mnemonic)

    def test_4a_nontrue_cond_renders_if_prefix(self):
        mnemonic = self.render(
            "4a", i=6, g=0, d=1, u=0, cond=4, data=1, dreg=2, compute=0
        )
        self.assertTrue(mnemonic.startswith("IF AV "))

    def test_5a_move_nontrue_cond_renders_if_prefix(self):
        # dstureg picks a UREG_NAMES entry; srcureghigh/srcureglow=0 -> R0.
        mnemonic = self.render(
            "5a_move", cond=6, srcureghigh=0, srcureglow=0, dstureg=32, compute=0
        )
        self.assertTrue(mnemonic.startswith("IF MS "))

    def test_5a_swap_nontrue_cond_renders_if_prefix(self):
        mnemonic = self.render("5a_swap", cond=8, cdreg=3, dreg=5, compute=0)
        self.assertEqual(mnemonic, "IF SZ R3 <-> R5")

    def test_6b_shiftimm_nontrue_cond_renders_if_prefix(self):
        shiftimm = (0x01 << 16) | (8 << 8) | (3 << 4) | 4  # ashift, amount=8
        mnemonic = self.render(
            "6b_shiftimm", cond=7, dataex=0, shiftimm=shiftimm
        )
        self.assertEqual(mnemonic, "IF SV R3 = ashift(R4, 8)")


class Type3cDirectionRenderingTest(unittest.TestCase):
    """Type3c's "d" field (decode_table.json bit 37; PGR Table 10-1 p.443,
    "D  Data direction  0 = Memory read  1 = Memory write") picks a load or
    a store, exactly like tools/sharc_trace.py's "3c" _execute branch
    (store when d, else load). A previous version of render_instruction()'s
    "3c" branch never read "d" at all and always rendered a store."""

    def setUp(self):
        self.mem = sharcldr.LoadedMemory.from_stream(b"")

    def render(self, **fields):
        data = field_insn("3c", **fields)
        insn = insn_at(data)
        self.assertEqual(insn.type_name, "3c")
        mnemonic, notes, gap = sharcfn.render_instruction(
            0x1000, insn, self.mem, *empty_ctx()
        )
        return mnemonic

    def test_d0_is_a_load(self):
        # DT2 1.16 sw 0x1c653b: dmi=4, dmm=5, d=0, dreg=6.
        mnemonic = self.render(dmi=4, dmm=5, d=0, dreg=6)
        self.assertEqual(mnemonic, "R6 = DM(I4, M5)")

    def test_d1_is_a_store(self):
        mnemonic = self.render(dmi=4, dmm=5, d=1, dreg=6)
        self.assertEqual(mnemonic, "DM(I4, M5) = R6")


class DualMemDirectionRenderingTest(unittest.TestCase):
    """Type1a/1b's "dmd"/"pmd" fields (PGR Table 10-1 p.444: DMD "DAG1
    access direction", PMD "DAG2 access direction", each 0 = Read, 1 =
    Write -- the manual's own Type 1a Syntax on p.375 prints "DM(Ia,Mb) =
    dreg" for a write and "dreg = DM(Ia,Mb)" for a read). tools/sharc_trace.py
    has no "1a"/"1b" _execute branch (an unimplemented form there), so
    this form is checked against the manual only, not against the tracer.
    A previous version of render_dual_mem() always put the memory term on
    the left and only swapped "=" for "<-" on a read, which reads backwards
    for the read case."""

    def setUp(self):
        self.mem = sharcldr.LoadedMemory.from_stream(b"")

    def render(self, **fields):
        data = field_insn("1a", **fields)
        insn = insn_at(data)
        self.assertEqual(insn.type_name, "1a")
        mnemonic, notes, gap = sharcfn.render_instruction(
            0x1000, insn, self.mem, *empty_ctx()
        )
        return mnemonic

    def test_dm_read_puts_register_on_the_left(self):
        mnemonic = self.render(
            dmd=0, dmi=7, dmm=0, dmdreg=12, pmd=1, pmi=2, pmm=3, pmdreg=12
        )
        self.assertIn("R12 = DM(I7, M0)", mnemonic)
        self.assertNotIn("DM(I7, M0) <-", mnemonic)

    def test_dm_write_puts_register_on_the_right(self):
        mnemonic = self.render(
            dmd=1, dmi=7, dmm=0, dmdreg=12, pmd=1, pmi=2, pmm=3, pmdreg=12
        )
        self.assertIn("DM(I7, M0) = R12", mnemonic)

    def test_pm_read_puts_register_on_the_left(self):
        mnemonic = self.render(
            dmd=1, dmi=0, dmm=1, dmdreg=2, pmd=0, pmi=4, pmm=7, pmdreg=10
        )
        self.assertIn("R10 = PM(I4, M7)", mnemonic)
        self.assertNotIn("PM(I4, M7) <-", mnemonic)

    def test_pm_write_puts_register_on_the_right(self):
        mnemonic = self.render(
            dmd=1, dmi=0, dmm=1, dmdreg=2, pmd=1, pmi=4, pmm=7, pmdreg=10
        )
        self.assertIn("PM(I4, M7) = R10", mnemonic)


_DT2_116_BLOB = os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
    "out",
    "sections",
    "dt2-1.16",
    "section_7_BLOB.bin",
)


@unittest.skipUnless(
    os.path.exists(_DT2_116_BLOB),
    "out/sections/dt2-1.16/section_7_BLOB.bin is not available",
)
class RealBlobType19AndType6aRenderingTest(unittest.TestCase):
    """Listing fixes checked against the real DT2 1.16 SHARC blob. Skips when
    the firmware is not present (firmware is never committed)."""

    @classmethod
    def setUpClass(cls):
        # Loading and decoding the blob takes seconds, so build the 0x1cbf07
        # dossier once for every check below.
        ctx = sharcfn.load_context(_DT2_116_BLOB, sharcinv.CODE_BLOCKS, min_depth=8)
        cls.dossier = sharcfn.build_dossier(ctx, 0x1CBF07, want_listing=True)
        cls.rows = {
            row["sw"]: row["mnemonic"] for row in cls.dossier.get("listing", [])
        }

    def test_dossier_builds_without_error(self):
        self.assertNotIn("error", self.dossier)

    def test_type19_modify_and_type6a_destinations(self):
        self.assertIn("I2 = modify(I4, 0x44)", self.rows["0x1cbf95"])
        self.assertIn("R0 = DM(I4, M5)", self.rows["0x1cbf84"])

    def test_type15b_immediate_offset_beyond_63_is_signed(self):
        # 0x1cbf0c's raw data[6:0] is 115; tools/sharc_trace.py executes it
        # as _signed(115, 7) == -13, so the listing must agree.
        self.assertIn("DM(I6 - 13) = R2", self.rows["0x1cbf0c"])
        self.assertNotIn("+ 115", self.rows["0x1cbf0c"])

    def test_type2a_conditional_compute_renders_if_cond(self):
        # 0x1cbf47 is a Type2a with cond 1 (LT, PGR Table 10-4).
        self.assertEqual("IF LT F8 = fadd(F8, F2)", self.rows["0x1cbf47"])


@unittest.skipUnless(
    os.path.exists(_DT2_116_BLOB),
    "out/sections/dt2-1.16/section_7_BLOB.bin is not available",
)
class RealBlobType3cRenderingTest(unittest.TestCase):
    """DT2 1.16 sw 0x1c653b (dmi=4, dmm=5, d=0, dreg=6): a Type3c load that
    a previous version of render_instruction()'s "3c" branch rendered as a
    store, "DM(I4, M5) = R6", disagreeing with tools/sharc_trace.py's "3c"
    _execute branch, which executes d=0 as a LOAD R6 = DM(0x250738)."""

    @classmethod
    def setUpClass(cls):
        ctx = sharcfn.load_context(_DT2_116_BLOB, sharcinv.CODE_BLOCKS, min_depth=8)
        cls.dossier = sharcfn.build_dossier(ctx, 0x1C642A, want_listing=True)
        cls.rows = {
            row["sw"]: row["mnemonic"] for row in cls.dossier.get("listing", [])
        }

    def test_dossier_builds_without_error(self):
        self.assertNotIn("error", self.dossier)

    def test_type3c_load_not_rendered_as_a_store(self):
        self.assertEqual("R6 = DM(I4, M5)", self.rows["0x1c653b"])


@unittest.skipUnless(
    os.path.exists(_DT2_116_BLOB),
    "out/sections/dt2-1.16/section_7_BLOB.bin is not available",
)
class RealBlobType14dRenderingTest(unittest.TestCase):
    """DT2 1.16 sw 0x1c257d (d=0, l=1, w=0, ex=0, x=0, dreg=11): a Type14d
    load that a previous version of render_mem_direct() rendered as
    "R11 = DM(0x255906), long" by reusing Type14a/15a's "l" == "(LW) 32-bit
    register-pair" rule. Per the PRM (out/refs/sharc-plus-prm pp.384-387,
    Figure 15-2's BHSE Encode Table), Type14d's "l" instead picks a
    sub-word width (byte/short), so l=1,x=0 is a short-word, zero-extended
    load -- "(sw)" -- the same width tools/sharc_trace.py's "14d" _execute
    branch actually reads, not a 32-bit long."""

    @classmethod
    def setUpClass(cls):
        ctx = sharcfn.load_context(_DT2_116_BLOB, sharcinv.CODE_BLOCKS, min_depth=8)
        cls.dossier = sharcfn.build_dossier(ctx, 0x1C24E9, want_listing=True)
        cls.rows = {
            row["sw"]: row["mnemonic"] for row in cls.dossier.get("listing", [])
        }

    def test_dossier_builds_without_error(self):
        self.assertNotIn("error", self.dossier)

    def test_type14d_load_renders_as_short_word_not_long(self):
        self.assertEqual("R11 = DM(0x255906) (sw)", self.rows["0x1c257d"])


def sharcfnCounterLike():
    from collections import Counter

    return Counter()


class DossierIntegrationTest(unittest.TestCase):
    """Build a tiny synthetic single-block boot stream and run the whole
    identify -> bounds -> call graph -> listing pipeline over it, the way
    tests/test_sharcinv.py exercises function_bounds()/compute_vector()."""

    def _stream(self, base_sw):
        target = sharcldr.sw_to_byte(base_sw)
        code = (
            cjump(base_sw + 0x10)
            + push3c()
            + store(base_sw + 3 + 2)
            + ret()
            + load(0, 0)
            + rframe()
        )
        return boot_block(0, target, len(code), payload=code), len(code)

    def test_identification_and_bounds_on_a_synthetic_block(self):
        base_sw = 0x1C1338
        stream, code_len = self._stream(base_sw)
        path = "/tmp/sharcfn_test_stream.bin"
        with open(path, "wb") as fh:
            fh.write(stream)
        try:
            ctx = sharcfn.load_context(path, (0,), min_depth=1)
            self.assertEqual(ctx["sha256"], sharcfn.sha256_of(path))
            d = sharcfn.build_dossier(ctx, base_sw, want_listing=True)
            self.assertNotIn("error", d)
            self.assertEqual(d["identification"]["block"], 0)
            self.assertIn("0x28000000", d["identification"]["base_sw_convention"])
            self.assertEqual(d["bounds"]["entry"], "0x%x" % base_sw)
            self.assertGreater(len(d["listing"]), 0)
            self.assertEqual(d["listing"][0]["sw"], "0x%x" % base_sw)
        finally:
            os.remove(path)

    def test_unknown_address_reports_an_error_not_a_crash(self):
        base_sw = 0x1C1338
        stream, _ = self._stream(base_sw)
        path = "/tmp/sharcfn_test_stream2.bin"
        with open(path, "wb") as fh:
            fh.write(stream)
        try:
            ctx = sharcfn.load_context(path, (0,), min_depth=1)
            d = sharcfn.build_dossier(ctx, 0x999999, want_listing=False)
            self.assertIn("error", d)
        finally:
            os.remove(path)


class EngineQueueTest(unittest.TestCase):
    def fn(self, block, entry, n_insns, float_mul, mac):
        return {
            "id": "blk%d@0x%x" % (block, entry),
            "block": block,
            "entry": entry,
            "exit": entry + n_insns,
            "n_insns": n_insns,
            "vector": {"float_mul": float_mul, "mac": mac},
        }

    def test_predicate_and_stable_order(self):
        functions = [
            self.fn(93, 0x30, 60, 10, 0),
            self.fn(69, 0x20, 80, 4, 6),
            self.fn(1, 0x10, 59, 99, 0),
            self.fn(1, 0x11, 60, 9, 0),
        ]
        selected = sharcfn.engine_candidates(functions)
        self.assertEqual(
            [(f["block"], f["entry"]) for f in selected], [(69, 0x20), (93, 0x30)]
        )
        self.assertEqual(
            sharcfn.canonical_digest(selected),
            sharcfn.canonical_digest(
                sharcfn.engine_candidates(list(reversed(selected)))
            ),
        )

    def test_documented_bounds_excludes_continuations_and_mismatches(self):
        with tempfile.TemporaryDirectory() as notes:
            with open(os.path.join(notes, "blk93-1c20.md"), "w") as fh:
                fh.write("- **Bounds**: entry `0x1c20`, exit `0x1c30`")
            with open(os.path.join(notes, "blk93-1c21.md"), "w") as fh:
                fh.write("continuation of `0x1c20`")
            with open(os.path.join(notes, "blk93-1c22.md"), "w") as fh:
                fh.write("- **Bounds**: entry `0x1c20`")
            entries, rejected = sharcfn.documented_function_entries(notes)
        self.assertEqual(entries, {(93, 0x1C20)})
        self.assertEqual(len(rejected), 2)

    def test_queue_historical_uncertainty_and_repeatable_bytes(self):
        ctx = {
            "sha256": "image",
            "analyzed": {},
            "functions": [self.fn(93, 0x30, 60, 8, 2), self.fn(69, 0x20, 70, 10, 0)],
        }
        with tempfile.TemporaryDirectory() as notes:
            queue = sharcfn.build_engine_queue(ctx, notes)
            self.assertEqual(
                queue["historical_claim"]["membership"],
                "unknown/unverified; no exact remaining set is emitted",
            )
            self.assertEqual(
                queue["counts"], {"candidates": 2, "documented": 0, "undocumented": 2}
            )
            queue_again = sharcfn.build_engine_queue(ctx, notes)
            self.assertEqual(
                sharcfn.canonical_json_bytes(queue),
                sharcfn.canonical_json_bytes(queue_again),
            )
            self.assertEqual(json.loads(sharcfn.canonical_json_bytes(queue)), queue)

    def test_queue_accepts_indexed_target_opcode_coverage(self):
        ctx = {
            "sha256": "image",
            "functions": [self.fn(93, 0x30, 60, 8, 2)],
        }
        coverage = {
            "derived_counts": {"fixture": 7},
            "unsupported_or_ambiguous": [],
            "word_address_units": "fixture",
        }
        with tempfile.TemporaryDirectory() as notes:
            queue = sharcfn.build_engine_queue(
                ctx, notes, target_coverage=coverage
            )
        self.assertEqual(queue["target_opcode_coverage"], coverage)

    def test_target_opcode_coverage_has_stable_keys_when_absent_or_present(self):
        fn = self.fn(93, 0x1000, 60, 10, 0)
        empty_block = {"base_sw": 0x1000, "insns": [], "_insn_sw": []}
        absent = sharcfn.target_opcode_coverage({"analyzed": {93: empty_block}}, [fn])[
            "derived_counts"
        ]
        type10a_abs = sharc_disasm.Instruction(
            offset=0,
            length_bytes=4,
            type_name="10a_abs",
            fields={},
            raw=0,
            kind="confident",
            note="synthetic",
        )
        present_block = {
            "base_sw": 0x1000,
            "insns": [(0, type10a_abs)],
            "_insn_sw": [0x1000],
        }
        present = sharcfn.target_opcode_coverage(
            {"analyzed": {93: present_block}}, [fn]
        )["derived_counts"]
        self.assertEqual(set(absent), set(sharcfn.TARGET_OPCODE_COVERAGE_KEYS))
        self.assertEqual(list(absent), list(present))
        self.assertEqual(absent["Type10a_abs"], 0)
        self.assertEqual(present["Type10a_abs"], 1)

    def test_sqlite_provenance_and_word_address_join(self):
        fn = self.fn(93, 0x1C20, 60, 10, 0)
        with tempfile.NamedTemporaryFile(suffix=".sqlite") as fh:
            con = sqlite3.connect(fh.name)
            con.executescript("""
                create table meta(key text primary key, value text);
                create table functions(sw integer primary key, name text, instructions integer, in_main integer);
                create table insn(sw integer primary key, length integer, mnemonic text, raw text, flow text, fallthrough_sw integer, pcode text, function_sw integer, in_main integer);
                create table refs(from_sw integer, to_sw integer, type text, op_index integer);
                create table warnings(function_sw integer, message text, normalised text, addr1 integer, addr2 integer);
                create table decompiled(function_sw integer primary key, seconds real, completed integer, error text, c text);
                insert into meta values('image', 'synthetic');
                insert into functions values(7200, 'f', 61, 1);
                insert into insn values(7200, 2, '', '', '', null, '', 7200, 1);
                insert into refs values(7200, 7201, 'call', 0);
                insert into warnings values(7200, 'w', 'w', null, null);
                insert into decompiled values(7200, 1, 1, null, 'c');
            """)
            con.commit()
            con.close()
            evidence = sharcfn.sqlite_evidence(fh.name, [dict(fn, entry=7200)])
            con = sqlite3.connect(fh.name)
            con.execute("insert into meta values('image_sha256', 'other-image')")
            con.commit()
            con.close()
            stale = sharcfn.sqlite_evidence(
                fh.name, [dict(fn, entry=7200)], source_image_sha256="source-image"
            )
        self.assertEqual(evidence["status"], "usable")
        self.assertEqual(evidence["meta"], [{"key": "image", "value": "synthetic"}])
        self.assertEqual(evidence["evidence_status"], "advisory")
        self.assertTrue(evidence["facts"]["0x1c20"]["function_present"])
        self.assertEqual(evidence["facts"]["0x1c20"]["instruction_count"], 61)
        self.assertEqual(evidence["facts"]["0x1c20"]["evidence_status"], "advisory")
        self.assertTrue(stale["freshness"].startswith("stale:"))
        self.assertEqual(stale["evidence_status"], "advisory")
        self.assertEqual(stale["facts"]["0x1c20"]["evidence_status"], "advisory")


if __name__ == "__main__":
    unittest.main()
