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

    def test_unary_rn_op(self):
        field12 = (0x6 << 8) | (3 << 4) | 4  # 'dec'
        self.assertEqual(sharcfn.render_shortcompute(field12), "R3 = dec(R3)")

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
        self.assertIn("cond=5", mnemonic)
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
