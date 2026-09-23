# pyright: reportMissingImports=false
"""tools/sharcdb.py: pure helper functions on synthetic instructions, a full
build over a hand-built boot stream (tests/test_sharcfn.py's
DossierIntegrationTest style), and the acceptance facts from the build task
against the real firmware databases (skipped when the firmware is absent;
marked slow since a full-image build takes real time).

No real firmware is committed; synthetic instructions are built directly
from tools/sharc_visa_tables.py the same way tests/test_sharcflow.py,
tests/test_sharcinv.py and tests/test_sharcfn.py do.
"""

import collections
import os
import pathlib
import sqlite3
import sys
import unittest

import pytest

sys.path.insert(
    0,
    os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "tools"),
)

import sharc_disasm  # noqa: E402
import sharcdb  # noqa: E402
import sharcfn  # noqa: E402
import sharcinv  # noqa: E402
import sharcldr  # noqa: E402
from test_sharc_disasm import encode  # noqa: E402
from test_sharcflow import call8a_rel, cjump, load, push3c, store, words  # noqa: E402
from test_sharcinv import field_insn, ret, rframe  # noqa: E402
from test_sharcldr import block as boot_block  # noqa: E402

DT2_116_BLOB = pathlib.Path("out/sections/dt2-1.16/section_7_BLOB.bin")
DT2_116_SHA256 = "0f514a12a2255f5c081e292c47f1f29462003177658da4bbae0a22fd737fffa2"
DN2_111_BLOB = pathlib.Path("out/sections/dn2-1.11/section_7_BLOB.bin")
DN2_111_SHA256 = "336e340aa0cdcd34e314cfa44849f709a3134f6bd4cd57dfc7e15702c83115e2"


def insn_at(data, offset=0):
    """The single decoded Instruction at `offset` of `data`."""
    return next(sharc_disasm.disassemble(data, offset))


# --- pure helper functions --------------------------------------------------


class MaskRelocatableTest(unittest.TestCase):
    def test_masks_addr_field_but_leaves_the_rest_alone(self):
        a = insn_at(field_insn("25a_direct", addr=0x1000))
        b = insn_at(field_insn("25a_direct", addr=0x2000))
        self.assertNotEqual(a.raw, b.raw)
        self.assertEqual(sharcdb.mask_relocatable(a), sharcdb.mask_relocatable(b))

    def test_masks_reladdr_field(self):
        a = insn_at(field_insn("8a_rel", b=1, cond=31, j=1, ci=0, reladdr=0x10))
        b = insn_at(field_insn("8a_rel", b=1, cond=31, j=1, ci=0, reladdr=0x20))
        self.assertNotEqual(a.raw, b.raw)
        self.assertEqual(sharcdb.mask_relocatable(a), sharcdb.mask_relocatable(b))

    def test_different_conditions_still_hash_differently(self):
        # cond isn't a masked stem, so two calls that differ only in addr
        # collide, but two that differ in cond must not.
        a = insn_at(field_insn("8a_rel", b=1, cond=1, j=1, ci=0, reladdr=0x10))
        b = insn_at(field_insn("8a_rel", b=1, cond=2, j=1, ci=0, reladdr=0x20))
        self.assertNotEqual(sharcdb.mask_relocatable(a), sharcdb.mask_relocatable(b))

    def test_unknown_instruction_masks_to_zero(self):
        unknown = sharc_disasm.Instruction(0, None, "unknown", kind="unknown")
        self.assertEqual(sharcdb.mask_relocatable(unknown), 0)


class ExtractLiteralTest(unittest.TestCase):
    def test_17b_sign_extends_a_16_bit_negative_value_and_names_the_m_register(self):
        insn = insn_at(field_insn("17b", ureg=37, data=0xFFFF))  # M5, PGR ureg code 37
        f = sharcinv.merge_fields(insn.fields)
        value, dest = sharcdb.extract_literal("17b", f)
        self.assertEqual(value, -1)
        self.assertEqual(dest, "M5")

    def test_17b_small_positive_values_are_unaffected(self):
        for raw, expected in ((1, 1), (0, 0)):
            insn = insn_at(field_insn("17b", ureg=37, data=raw))
            f = sharcinv.merge_fields(insn.fields)
            value, dest = sharcdb.extract_literal("17b", f)
            self.assertEqual(value, expected)
            self.assertEqual(dest, "M5")

    def test_19a_destination_is_is_xor_idis_not_is(self):
        insn = insn_at(field_insn("19a", **{"g": 0, "idis": 6, "is": 4, "data": 0x44}))
        f = sharcinv.merge_fields(insn.fields)
        value, dest = sharcdb.extract_literal("19a", f)
        self.assertEqual(value, 0x44)
        self.assertEqual(dest, "I2")

    def test_18a_names_the_status_register(self):
        insn = insn_at(field_insn("18a", sreg=0, bop=0, data=0x3))
        f = sharcinv.merge_fields(insn.fields)
        value, dest = sharcdb.extract_literal("18a", f)
        self.assertEqual(value, 0x3)
        self.assertEqual(dest, "USTAT1")

    def test_direct_address_forms_have_no_destination_register(self):
        insn = insn_at(field_insn("15a", addr=0x200000, ureg=0, g=0, d=0, l=0))
        f = sharcinv.merge_fields(insn.fields)
        value, dest = sharcdb.extract_literal("15a", f)
        self.assertEqual(value, 0x200000)
        self.assertIsNone(dest)

    def test_non_literal_form_returns_none(self):
        insn = insn_at(ret())
        f = sharcinv.merge_fields(insn.fields)
        self.assertIsNone(sharcdb.extract_literal("9b_abs", f))


class ExtractMemAccessTest(unittest.TestCase):
    def test_indexed_store_records_base_and_modifier(self):
        insn = insn_at(field_insn("3a", u=1, i=7, m=7, cond=31, g=0, d=1, l=0, ureg=15))
        f = sharcinv.merge_fields(insn.fields)
        rows = sharcdb.extract_mem_access("3a", f)
        self.assertEqual(len(rows), 1)
        space, direction, base_reg, modifier, u, form, width, abs_addr = rows[0]
        self.assertEqual((space, direction, base_reg, modifier, u), ("DM", "store", "I7", "M7", 1))
        self.assertEqual(form, sharcinv.MEM_FORMS["3a"])
        self.assertIsNone(abs_addr)

    def test_direct_load_records_the_absolute_address(self):
        insn = insn_at(field_insn("15a", addr=0x252658, ureg=0, g=0, d=0, l=0))
        f = sharcinv.merge_fields(insn.fields)
        rows = sharcdb.extract_mem_access("15a", f)
        self.assertEqual(len(rows), 1)
        space, direction, base_reg, modifier, u, form, width, abs_addr = rows[0]
        self.assertEqual((space, direction, base_reg, modifier), ("DM", "load", None, None))
        self.assertEqual(abs_addr, 0x252658)

    def test_immoff_offset_is_signed(self):
        # 6-bit field, 40 -> -24 (sign_extend(40, 6)).
        insn = insn_at(field_insn("4a", i=6, data=40, dreg=2, g=0, d=1, l=0))
        f = sharcinv.merge_fields(insn.fields)
        rows = sharcdb.extract_mem_access("4a", f)
        self.assertEqual(rows[0][2:4], ("I6", "-24"))

    def test_dual_mem_form_yields_two_rows(self):
        insn = insn_at(field_insn(
            "1a", dmi=4, dmm=5, dmd=1, dmdreg=0, pmi=6, pmm=7, pmd=0, pmdreg=1,
            compute=0,
        ))
        f = sharcinv.merge_fields(insn.fields)
        rows = sharcdb.extract_mem_access("1a", f)
        self.assertEqual(len(rows), 2)
        self.assertEqual((rows[0][0], rows[0][1], rows[0][2], rows[0][3]), ("DM", "store", "I4", "M5"))
        self.assertEqual((rows[1][0], rows[1][1], rows[1][2], rows[1][3]), ("PM", "load", "I6", "M7"))

    def test_non_memory_form_yields_nothing(self):
        self.assertEqual(sharcdb.extract_mem_access("9b_abs", {}), [])


class ClassifyLiteralRangeTest(unittest.TestCase):
    def setUp(self):
        self.code_spans_sw = [(0x1C1338, 0x1C2000)]
        self.code_spans_byte = [(sharcldr.sw_to_byte(0x1C1338), sharcldr.sw_to_byte(0x1C2000))]
        data = boot_block(0, 0x260000, 4, payload=b"abcd")
        self.mem = sharcldr.LoadedMemory.from_stream(data)

    def test_value_inside_code_sw_range(self):
        in_code, in_data = sharcdb.classify_literal_range(
            0x1C1400, self.code_spans_sw, self.code_spans_byte, self.mem)
        self.assertTrue(in_code)
        self.assertFalse(in_data)

    def test_value_inside_code_byte_alias(self):
        byte_addr = sharcldr.sw_to_byte(0x1C1400)
        in_code, in_data = sharcdb.classify_literal_range(
            byte_addr, self.code_spans_sw, self.code_spans_byte, self.mem)
        self.assertTrue(in_code)

    def test_value_inside_a_loaded_data_block(self):
        in_code, in_data = sharcdb.classify_literal_range(
            0x260000, self.code_spans_sw, self.code_spans_byte, self.mem)
        self.assertFalse(in_code)
        self.assertTrue(in_data)

    def test_value_outside_everything(self):
        in_code, in_data = sharcdb.classify_literal_range(
            -1, self.code_spans_sw, self.code_spans_byte, self.mem)
        self.assertFalse(in_code)
        self.assertFalse(in_data)


# --- synthetic full-build tests ---------------------------------------------


class SyntheticBuildTest(unittest.TestCase):
    """Build a tiny single-block boot stream and run the whole build_database()
    pipeline over it, the way tests/test_sharcfn.py's DossierIntegrationTest
    exercises tools/sharcfn.py's dossier pipeline."""

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
        return boot_block(0, target, len(code), payload=code)

    def _build(self, tmp_path, base_sw=0x1C1338, force=False):
        stream_path = os.path.join(tmp_path, "stream.bin")
        with open(stream_path, "wb") as fh:
            fh.write(self._stream(base_sw))
        out_path = os.path.join(tmp_path, "out.sqlite")
        stats = sharcdb.build_database(
            stream_path, out_path, name="synthetic", min_depth=1, blocks=(0,), force=force)
        return stream_path, out_path, stats

    def test_meta_and_blocks(self, tmp_path=None):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            stream_path, out_path, stats = self._build(tmp)
            self.assertFalse(stats["skipped"])
            db = sqlite3.connect(out_path)
            meta = dict(db.execute("SELECT key, value FROM meta").fetchall())
            self.assertEqual(meta["image_sha256"], sharcfn.sha256_of(stream_path))
            self.assertEqual(meta["db_version"], str(sharcdb.DB_VERSION))
            blocks = db.execute("SELECT idx, kind FROM blocks").fetchall()
            self.assertEqual(blocks, [(0, "code")])
            db.close()

    def test_function_and_instructions_recorded(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            base_sw = 0x1C1338
            _stream_path, out_path, _stats = self._build(tmp, base_sw=base_sw)
            db = sqlite3.connect(out_path)
            funcs = db.execute("SELECT entry_sw, name FROM functions").fetchall()
            self.assertEqual(funcs, [(base_sw, "FUN_1c1338")])
            n_insn = db.execute("SELECT count(*) FROM insn").fetchone()[0]
            self.assertGreater(n_insn, 0)
            aligned_mnemonics = db.execute(
                "SELECT mnemonic FROM insn WHERE aligned = 1 AND sw = ?", (base_sw,)
            ).fetchone()
            self.assertIsNotNone(aligned_mnemonics[0])
            db.close()

    def test_call_and_return_edges(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            base_sw = 0x1C1338
            _stream_path, out_path, _stats = self._build(tmp, base_sw=base_sw)
            db = sqlite3.connect(out_path)
            calls = db.execute(
                "SELECT from_sw, to_sw, kind, delayed FROM edges WHERE kind = 'call'"
            ).fetchall()
            self.assertEqual(calls, [(base_sw, base_sw + 0x10, "call", 1)])
            returns = db.execute(
                "SELECT from_sw, kind FROM edges WHERE kind = 'return'"
            ).fetchall()
            self.assertEqual(len(returns), 1)
            db.close()

    def test_skip_rebuild_when_sha256_and_version_match(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            self._build(tmp)
            _stream_path, out_path, stats2 = self._build(tmp)
            self.assertTrue(stats2["skipped"])
            _stream_path, out_path, stats3 = self._build(tmp, force=True)
            self.assertFalse(stats3["skipped"])

    def test_unknown_image_without_known_code_blocks_requires_override(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            stream_path = os.path.join(tmp, "stream.bin")
            with open(stream_path, "wb") as fh:
                fh.write(self._stream(0x1C1338))
            out_path = os.path.join(tmp, "out.sqlite")
            with self.assertRaises(SystemExit):
                sharcdb.build_database(stream_path, out_path, name="synthetic", min_depth=1)


class SyntheticJumpEdgeTest(unittest.TestCase):
    """A single conditional Type8a JUMP (b=0): the edges table must record
    both the taken cond_jump and the not-taken fallthrough path."""

    def _stream(self, base_sw):
        target = sharcldr.sw_to_byte(base_sw)
        # cond=1 (LT), b=0 (JUMP not CALL), j=0 (non-delayed) -> plain
        # conditional jump with an ordinary (non-delay-slot) fallthrough.
        jump = call8a_rel(0x20, b=0, cond=1, j=0)
        code = jump + load(0, 0) + ret() + load(1, 0) + rframe()
        return boot_block(0, target, len(code), payload=code)

    def test_cond_jump_and_fallthrough_edges(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            base_sw = 0x1C1338
            stream_path = os.path.join(tmp, "stream.bin")
            with open(stream_path, "wb") as fh:
                fh.write(self._stream(base_sw))
            out_path = os.path.join(tmp, "out.sqlite")
            sharcdb.build_database(stream_path, out_path, name="synthetic", min_depth=1, blocks=(0,))
            db = sqlite3.connect(out_path)
            rows = db.execute(
                "SELECT to_sw, kind, cond FROM edges WHERE from_sw = ? ORDER BY kind", (base_sw,)
            ).fetchall()
            db.close()
            kinds = {kind for _to, kind, _cond in rows}
            self.assertIn("cond_jump", kinds)
            self.assertIn("fallthrough", kinds)
            cond_jump = next(r for r in rows if r[1] == "cond_jump")
            self.assertEqual(cond_jump[0], base_sw + 0x20)
            self.assertEqual(cond_jump[2], 1)


# --- real-firmware acceptance tests ------------------------------------------


def _connect(path):
    return sqlite3.connect(path)


@pytest.mark.slow
@unittest.skipUnless(DT2_116_BLOB.exists(), "DT2 1.16 SHARC blob is not available")
class Dt2116AcceptanceTest(unittest.TestCase):
    """The nine DT2 1.16 acceptance facts from the sharcdb build task,
    against a full real-image build."""

    @classmethod
    def setUpClass(cls):
        cls.out_path = "out/sharcdb/dt2-1.16.sqlite"
        cls.stats = sharcdb.build_database(str(DT2_116_BLOB), cls.out_path, name="dt2-1.16")
        cls.db = _connect(cls.out_path)

    @classmethod
    def tearDownClass(cls):
        cls.db.close()

    def test_image_sha256(self):
        self.assertEqual(sharcfn.sha256_of(str(DT2_116_BLOB)), DT2_116_SHA256)

    def test_1_cond_jump_sv_into_fun_1c71ec(self):
        rows = self.db.execute(
            "SELECT to_sw, cond FROM edges WHERE from_sw = 0x1c7053 AND kind = 'cond_jump'"
        ).fetchall()
        self.assertEqual(rows, [(0x1C71EC, 7)])

    def test_2_callers_of_1c2b24_and_its_call_to_1c642a(self):
        callers = {r[0] for r in self.db.execute(
            "SELECT from_function FROM edges WHERE to_sw = 0x1c2b24"
        ).fetchall()}
        self.assertIn(0x1C75D8, callers)
        call = self.db.execute(
            "SELECT to_sw FROM edges WHERE from_sw = 0x1c3083 AND kind = 'call'"
        ).fetchall()
        self.assertEqual(call, [(0x1C642A,)])

    def test_3_stage6_callers_and_stage4_5_exclusive_to_1c71ec(self):
        stage6_callers = sorted(r[0] for r in self.db.execute(
            "SELECT from_sw FROM edges WHERE to_sw = 0x1cbf07 AND kind = 'call'"
        ).fetchall())
        self.assertEqual(stage6_callers, [0x1C73D5, 0x1C7434])
        for stage_sw in (0x1CD286, 0x1CC79E):
            callers = {r[0] for r in self.db.execute(
                "SELECT from_function FROM edges WHERE to_sw = ? AND kind = 'call'", (stage_sw,)
            ).fetchall()}
            self.assertEqual(callers, {0x1C71EC})

    def test_4_eight_pushes_via_i7_m7_in_fun_1c71ec(self):
        expected = [0x1C735A, 0x1C736F, 0x1C7384, 0x1C739F, 0x1C73BA, 0x1C73D2, 0x1C73EA, 0x1C7431]
        rows = sorted(r[0] for r in self.db.execute(
            """SELECT sw FROM mem_access
               WHERE direction = 'store' AND base_reg = 'I7' AND modifier LIKE 'M7%'
                 AND sw BETWEEN 0x1c71ec AND 0x1c75d8"""
        ).fetchall())
        for sw in expected:
            self.assertIn(sw, rows)

    def test_5_m_register_immediates_at_1c0f3a(self):
        rows = self.db.execute(
            """SELECT sw, value, dest_reg FROM literals
               WHERE sw BETWEEN 0x1c0f3a AND 0x1c0f44 AND dest_reg IN ('M5','M6','M7','M13','M14','M15')
               ORDER BY sw"""
        ).fetchall()
        self.assertTrue(rows)
        for _sw, value, _dest in rows:
            self.assertIn(value, (-1, 0, 1))

    def test_6_indirect_edges(self):
        for sw in (0x1C6579, 0x1C66EC, 0x1C6C25):
            rows = self.db.execute(
                "SELECT kind, to_sw FROM edges WHERE from_sw = ?", (sw,)
            ).fetchall()
            self.assertTrue(any(kind == "indirect" and to_sw is None for kind, to_sw in rows),
                             "no indirect edge at 0x%x: %r" % (sw, rows))

    def test_7_mnemonics(self):
        row = self.db.execute("SELECT mnemonic FROM insn WHERE sw = 0x1cbf95").fetchone()
        self.assertIn("I2 = modify(I4, 0x44)", row[0])
        row = self.db.execute("SELECT mnemonic FROM insn WHERE sw = 0x1cbf47").fetchone()
        self.assertEqual(row[0], "IF LT F8 = fadd(F8, F2)")

    def test_build_reports_a_size_and_a_time(self):
        self.assertGreater(self.stats["size"], 0)
        self.assertGreaterEqual(self.stats["seconds"], 0)


@pytest.mark.slow
@unittest.skipUnless(DN2_111_BLOB.exists(), "DN2 1.11 SHARC blob is not available")
class Dn2111AcceptanceTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.out_path = "out/sharcdb/dn2-1.11.sqlite"
        sharcdb.build_database(str(DN2_111_BLOB), cls.out_path, name="dn2-1.11")
        cls.db = _connect(cls.out_path)

    @classmethod
    def tearDownClass(cls):
        cls.db.close()

    def test_image_sha256(self):
        self.assertEqual(sharcfn.sha256_of(str(DN2_111_BLOB)), DN2_111_SHA256)

    def test_8_cond_jump_sz_into_1c9b73(self):
        rows = self.db.execute(
            "SELECT to_sw, cond FROM edges WHERE from_sw = 0x1c99a8 AND kind = 'cond_jump'"
        ).fetchall()
        self.assertEqual(rows, [(0x1C9B73, 8)])


@pytest.mark.slow
@unittest.skipUnless(DT2_116_BLOB.exists() and DN2_111_BLOB.exists(),
                      "DT2 1.16 and DN2 1.11 SHARC blobs are not both available")
class CrossImageMatchTest(unittest.TestCase):
    """Acceptance fact 9: stage 6 matches DT2 <-> DN2 by relocation-tolerant
    hash, at the addresses docs/findings/11 already records by exact-byte
    comparison."""

    @classmethod
    def setUpClass(cls):
        cls.dt2_path = "out/sharcdb/dt2-1.16.sqlite"
        cls.dn2_path = "out/sharcdb/dn2-1.11.sqlite"
        sharcdb.build_database(str(DT2_116_BLOB), cls.dt2_path, name="dt2-1.16")
        sharcdb.build_database(str(DN2_111_BLOB), cls.dn2_path, name="dn2-1.11")
        cls.db = _connect(cls.dt2_path)
        cls.db.execute("ATTACH ? AS dn2", (cls.dn2_path,))

    @classmethod
    def tearDownClass(cls):
        cls.db.close()

    def test_stage6_matches_by_relocation_tolerant_hash(self):
        row = self.db.execute(
            """SELECT a.reloc_hash = b.reloc_hash FROM func_hash a, dn2.func_hash b
               WHERE a.entry_sw = 0x1cbf07 AND b.entry_sw = 0xb806f5"""
        ).fetchone()
        self.assertIsNotNone(row, "one or both stage-6 entries are missing from func_hash")
        self.assertEqual(row[0], 1)


if __name__ == "__main__":
    unittest.main()
