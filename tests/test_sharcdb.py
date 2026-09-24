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

import sharc  # noqa: E402
import sharc_disasm  # noqa: E402
import sharcdb  # noqa: E402
import sharcfn  # noqa: E402
import sharcinv  # noqa: E402
import sharcldr  # noqa: E402
from test_sharc_disasm import encode  # noqa: E402
from test_sharcflow import call8a_rel, cjump, load, push3c, store, words  # noqa: E402
from test_sharcinv import field_insn, ret, rframe  # noqa: E402
from test_sharcldr import block as boot_block  # noqa: E402
from sharc_trace import UREG_CODES  # noqa: E402

DT2_116_BLOB = pathlib.Path("out/sections/dt2-1.16/section_7_BLOB.bin")
DT2_116_SHA256 = "0f514a12a2255f5c081e292c47f1f29462003177658da4bbae0a22fd737fffa2"
DN2_111_BLOB = pathlib.Path("out/sections/dn2-1.11/section_7_BLOB.bin")
DN2_111_SHA256 = "336e340aa0cdcd34e314cfa44849f709a3134f6bd4cd57dfc7e15702c83115e2"
DT2_116_GHIDRA_DUMP = pathlib.Path("out/ghidra/dt2-1.16-emac")


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

    def test_immoff_4b_byte_access_is_labeled_byte_not_word(self):
        # Type4b l=0,x=0,w=0 is a byte (bw) access (PRM pp.13-31/13-32's BH
        # Encode Table), not the "long if l else word" fallback's "word".
        insn = insn_at(field_insn("4b", i=6, data=0, dreg=2, g=0, d=1, l=0, w=0, x=0))
        f = sharcinv.merge_fields(insn.fields)
        rows = sharcdb.extract_mem_access("4b", f)
        self.assertEqual(rows[0][6], "byte")

    def test_immoff_4d_short_word_access_is_labeled_short_not_long(self):
        # Type4d l=1,x=0,w=0 is a short-word (sw) access, not "long".
        insn = insn_at(field_insn("4d", i=6, data=0, dreg=2, g=0, d=1, l=1, w=0, x=0))
        f = sharcinv.merge_fields(insn.fields)
        rows = sharcdb.extract_mem_access("4d", f)
        self.assertEqual(rows[0][6], "short-word")

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


class SyntheticDatarefResolvedOffsetTest(unittest.TestCase):
    """dataref's 'resolved_offset' role for an IMMOFF form (4a/4b/4d) with
    u=1 (SHARC+ Core Programming Reference pp.13-26/13-30/13-34): u=0
    pre-modifies I for the address (I keeps its old value afterwards); u=1
    accesses the CURRENT (unmodified) I and only writes I+offset back
    afterwards (post-modify). So a same-block "I2 = <lit>" literal load
    followed by a u=1 Type4a access must resolve to the literal itself, not
    literal+offset -- the bug this test's build found (tools/sharcdb.py's
    IMMOFF resolved_offset arm ignored `u` and always added the offset,
    unlike tools/sharc_trace.py's own "4a"/"4b" handlers and the `ptr`
    table's _ptr_mem_form(), which already drew this distinction)."""

    def _stream(self, base_sw, u, i2_literal=0x3000, off=8):
        target = sharcldr.sw_to_byte(base_sw)
        i2_ureg = UREG_CODES["I2"]
        access = field_insn(
            "4a", i=2, g=0, d=0, u=u, cond=0x1F, data=off, dreg=5, compute=0)
        code = (
            cjump(base_sw + 0x10)
            + push3c()
            + store(base_sw + 3 + 2)
            + ret()
            + load(0, 0)
            + rframe()
            + load(i2_ureg, i2_literal)
            + access
        )
        return boot_block(0, target, len(code), payload=code)

    def _resolved_offset(self, tmp_path, u):
        stream_path = os.path.join(tmp_path, "stream.bin")
        with open(stream_path, "wb") as fh:
            fh.write(self._stream(0x1C1338, u))
        out_path = os.path.join(tmp_path, "out.sqlite")
        sharcdb.build_database(
            stream_path, out_path, name="synthetic", min_depth=1, blocks=(0,), force=True)
        db = sqlite3.connect(out_path)
        rows = db.execute(
            "SELECT value FROM dataref WHERE role = 'resolved_offset'").fetchall()
        db.close()
        return [r[0] for r in rows]

    def test_postmodify_resolves_to_the_unmodified_literal(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            self.assertEqual(self._resolved_offset(tmp, u=1), [0x3000])

    def test_premodify_still_resolves_to_literal_plus_offset(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            self.assertEqual(self._resolved_offset(tmp, u=0), [0x3008])


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


class SyntheticDelayedTerminatorSuccTest(unittest.TestCase):
    """A delayed conditional Type8a JUMP (j=1): the block must include its
    two delay slots and the succ row for the taken/not-taken edges must key
    off the JUMP's own sw, not the block's last member (its second delay
    slot) -- the bug this file's build found: keying off the last member
    silently relabelled every delayed terminator's edge as a plain
    'fallthrough' to the same (coincidentally correct) address."""

    def _stream(self, base_sw):
        target = sharcldr.sw_to_byte(base_sw)
        jump = call8a_rel(0x10, b=0, cond=1, j=1)  # delayed JUMP IF LT, rel=0x10
        code = (jump + load(0, 0xAAA) + load(1, 0xBBB)  # jump + 2 delay slots
                + load(2, 0) + ret() + load(3, 0) + rframe())
        return boot_block(0, target, len(code), payload=code)

    def _build(self, tmp, base_sw=0x1C1338):
        stream_path = os.path.join(tmp, "stream.bin")
        with open(stream_path, "wb") as fh:
            fh.write(self._stream(base_sw))
        out_path = os.path.join(tmp, "out.sqlite")
        sharcdb.build_database(stream_path, out_path, name="synthetic", min_depth=1, blocks=(0,))
        return out_path

    def test_call_return_and_cond_taken_key_off_the_branch_not_the_last_slot(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            base_sw = 0x1C1338
            out_path = self._build(tmp, base_sw)
            db = sqlite3.connect(out_path)
            block = db.execute(
                "SELECT start_sw, end_sw FROM bblocks WHERE start_sw <= ? AND end_sw > ?",
                (base_sw, base_sw),
            ).fetchone()
            # The delayed jump's own two delay slots (4 sw) stay in this same
            # block: it must not end until after them.
            self.assertEqual(block, (base_sw, base_sw + 7))
            rows = sorted(db.execute(
                "SELECT kind, to_block FROM succ WHERE from_block = ?", (block[0],)
            ).fetchall())
            db.close()
            self.assertEqual(
                rows,
                sorted([("cond_taken", base_sw + 0x10), ("cond_not_taken", base_sw + 7)]),
            )


class SyntheticDoLoopSuccTest(unittest.TestCase):
    """A one-instruction hardware DO..UNTIL loop body: loop_back to the body
    start and loop_exit to the instruction after the loop's last body
    instruction, from the block ending at that last instruction -- even
    though it is an ordinary load, not a branch."""

    def _stream(self, base_sw):
        target = sharcldr.sw_to_byte(base_sw)
        code = (
            field_insn("12a_imm", data=4, mode=0, reladdr=3)  # DO body=[sw+3,sw+3], trip 4
            + field_insn("3c", dmi=0, dmm=0, d=0, dreg=6)  # sw+3: R6 = DM(I0, M0) (load)
            + load(0, 0)  # sw+4: loop-exit target
            + ret() + load(1, 0) + rframe()
        )
        return boot_block(0, target, len(code), payload=code)

    def test_loop_back_and_loop_exit(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            base_sw = 0x1C1338
            stream_path = os.path.join(tmp, "stream.bin")
            with open(stream_path, "wb") as fh:
                fh.write(self._stream(base_sw))
            out_path = os.path.join(tmp, "out.sqlite")
            sharcdb.build_database(stream_path, out_path, name="synthetic", min_depth=1, blocks=(0,))
            db = sqlite3.connect(out_path)
            body_sw = base_sw + 3
            rows = sorted(db.execute(
                "SELECT kind, to_block FROM succ WHERE from_block = "
                "(SELECT start_sw FROM bblocks WHERE start_sw <= ? AND end_sw > ?)",
                (body_sw, body_sw),
            ).fetchall())
            db.close()
            self.assertEqual(
                rows,
                sorted([("loop_back", body_sw), ("loop_exit", base_sw + 4)]),
            )


class RegisterEffectsTest(unittest.TestCase):
    """register_effects(): def/use extraction per typed decode field, one
    case per form family the build task called out. Field dicts are hand-
    built (tools/sharc_trace.py's State.uregs style), not decoded from
    bytes -- these are pure-function checks like ExtractLiteralTest above."""

    def test_alu_compute_binary_defines_rn_uses_rx_and_ry(self):
        # cu=0 (ALU), opcode=0x01 (add), rn=3, rx=4, ry=5 (PRM Table 18-11).
        field23 = (0x01 << 12) | (3 << 8) | (4 << 4) | 5
        defs, uses, unknown = sharcdb.register_effects("2a", {"compute": field23})
        self.assertEqual(defs, [("R3", "compute")])
        self.assertEqual(uses, ["R4", "R5"])
        self.assertEqual(unknown, [])

    def test_alu_compute_unary_uses_only_rx(self):
        # opcode=0x21 (pass) is in UNARY_ALU_OPS.
        field23 = (0x21 << 12) | (3 << 8) | (4 << 4) | 5
        defs, uses, unknown = sharcdb.register_effects("2a", {"compute": field23})
        self.assertEqual(defs, [("R3", "compute")])
        self.assertEqual(uses, ["R4"])

    def test_alu_dual_add_subtract_defines_both_results(self):
        field23 = (0x7 << 16) | (0xA << 12) | (1 << 8) | (2 << 4) | 3  # Rs=A,Ra=1,Rx=2,Ry=3
        defs, uses, unknown = sharcdb.register_effects("2a", {"compute": field23})
        self.assertEqual(sorted(defs), sorted([("R10", "compute"), ("R1", "compute")]))
        self.assertEqual(sorted(uses), ["R2", "R3"])

    def test_mult_mac_uses_mr(self):
        # cu=1 (MULT), top2=(opcode>>6)&3 == 2 (MAC add), is_float=0.
        opcode = 0x80  # top2=2, bit3(float)=0
        field23 = (1 << 20) | (opcode << 12) | (3 << 8) | (4 << 4) | 5
        defs, uses, unknown = sharcdb.register_effects("2a", {"compute": field23})
        self.assertEqual(defs, [("R3", "compute")])
        self.assertEqual(sorted(uses), sorted(["R4", "R5", "MR"]))
        self.assertEqual(unknown, [])

    def test_mult_plain_mod1_opcode_uses_r_registers(self):
        # opcode 0x48 ("01yx f00r" with f=1: MOD1 UUF) is a fixed-point
        # multiply, not float; classify_compute()'s bit-3 is_float used to
        # leak into regdef/reguse as F-registers here.
        opcode = 0x48
        field23 = (1 << 20) | (opcode << 12) | (3 << 8) | (4 << 4) | 5
        defs, uses, unknown = sharcdb.register_effects("2a", {"compute": field23})
        self.assertEqual(defs, [("R3", "compute")])
        self.assertEqual(sorted(uses), ["R4", "R5"])
        self.assertEqual(unknown, [])

    def test_mult_housekeeping_is_unknown(self):
        field23 = (1 << 20) | (0 << 12)  # top2=0 -> housekeeping
        defs, uses, unknown = sharcdb.register_effects("2a", {"compute": field23})
        self.assertEqual(defs, [])
        self.assertEqual(unknown, ["mult_housekeeping"])

    def test_shift_defines_rn_uses_rx_and_ry(self):
        field23 = (2 << 20) | (0x00 << 12) | (3 << 8) | (4 << 4) | 5
        defs, uses, unknown = sharcdb.register_effects("2a", {"compute": field23})
        self.assertEqual(defs, [("R3", "compute")])
        self.assertEqual(sorted(uses), ["R4", "R5"])

    def test_multifn_dual_addsub_defines_three_uses_four(self):
        # top3=6 (MULTIFN, dual, fixed int): rm=12,ra=8,rs=4,rxm=0,rym=4? --
        # built from the exact bit math in _compute_regdef_reguse's dual
        # branch, mirroring tools/sharcfn.py's decode_multifn.
        field23 = (
            (6 << 20) | (0xB << 16) | (0xC << 12) | (0xA << 8)
            | (0x2 << 6) | (0x1 << 4) | (0x3 << 2) | 0x0
        )
        defs, uses, unknown = sharcdb.register_effects("2a", {"compute": field23})
        self.assertEqual(len(defs), 3)
        self.assertEqual(len(uses), 4)
        self.assertEqual(unknown, [])

    def test_multifn_non_dual_is_unknown(self):
        field23 = (4 << 20) | (0x1234 & 0xFFFF)
        defs, uses, unknown = sharcdb.register_effects("2a", {"compute": field23})
        self.assertEqual(defs, [])
        self.assertEqual(unknown, ["multifn_alu_compute"])

    def test_shortcompute_binary_uses_rn_and_rx(self):
        # opcode=0x0 (add, binary): RN=3, RX=4.
        field12 = (0x0 << 8) | (3 << 4) | 4
        defs, uses = sharcdb._shortcompute_regdef_reguse(field12)
        self.assertEqual(defs, [("R3", "compute")])
        self.assertEqual(uses, ["R3", "R4"])

    def test_shortcompute_inc_uses_only_rx_not_rn(self):
        # PRM Table 17-2 (p.17-3): opcode 0101 ('inc') is "RN = RX + 1" --
        # RX is the only register read; RN (the destination) is not.
        field12 = (0x5 << 8) | (3 << 4) | 4  # inc, rn=3, rx=4
        defs, uses = sharcdb._shortcompute_regdef_reguse(field12)
        self.assertEqual(defs, [("R3", "compute")])
        self.assertEqual(uses, ["R4"])

    def test_3c_load_defines_dreg_from_d_field_not_mnemonic(self):
        # The exact shape of the task's Type3c bug: d=0 is a LOAD (R6
        # defined), even though tools/sharcfn.py's render_instruction prints
        # every Type3c as a store.
        defs, uses, _unknown = sharcdb.register_effects("3c", {"dmi": 4, "dmm": 5, "d": 0, "dreg": 6})
        self.assertEqual(defs, [("R6", "mem_load")])
        self.assertEqual(sorted(uses), ["I4", "M5"])

    def test_3c_store_uses_dreg(self):
        defs, uses, _unknown = sharcdb.register_effects("3c", {"dmi": 4, "dmm": 5, "d": 1, "dreg": 6})
        self.assertEqual(defs, [])
        self.assertEqual(sorted(uses), ["I4", "M5", "R6"])

    def test_indexed_load_u1_also_defines_i_via_dag_modify(self):
        f = {"u": 1, "i": 7, "m": 7, "d": 0, "g": 0, "dreg": 2}
        defs, uses, _unknown = sharcdb.register_effects("3a", f)
        self.assertEqual(sorted(defs), sorted([("R2", "mem_load"), ("I7", "dag_modify")]))
        self.assertEqual(sorted(uses), ["I7", "M7"])

    def test_immoff_store_u1(self):
        f = {"u": 1, "i": 6, "d": 1, "g": 0, "dreg": 3}
        defs, uses, _unknown = sharcdb.register_effects("4a", f)
        self.assertEqual(defs, [("I6", "dag_modify")])
        self.assertEqual(sorted(uses), ["I6", "R3"])

    def test_dual_mem_defines_and_uses_per_dmd_pmd(self):
        f = {"dmi": 4, "dmm": 5, "dmdreg": 0, "dmd": 1,  # store
             "pmi": 6, "pmm": 7, "pmdreg": 1, "pmd": 0}  # load
        defs, uses, _unknown = sharcdb.register_effects("1a", f)
        self.assertEqual(defs, [("R1", "mem_load")])
        self.assertEqual(sorted(uses), sorted(["I4", "M5", "I6", "M7", "R0"]))

    def test_19a_modify_dest_is_xor_idis(self):
        f = {"g": 0, "idis": 6, "is": 4, "data": 0x44}
        defs, uses, _unknown = sharcdb.register_effects("19a", f)
        self.assertEqual(defs, [("I2", "dag_modify")])
        self.assertEqual(uses, ["I4"])

    def test_7a_modify_dest_is_xor_idis_and_uses_m(self):
        # PRM Type7a MODIFY (pp.13-46/13-48): "Ia = MODIFY(Ia,Mb)" -- the
        # same Is-XOR-Idis destination trick as Type19a above, but the
        # modify delta is register M3 (a use), not a literal. Previously
        # register_effects() had no branch for "7a" at all -- its own
        # I-register write, unlike its compute half, was invisible.
        f = {"g": 0, "idis": 6, "is": 4, "m": 3, "compute": 0}
        defs, uses, _unknown = sharcdb.register_effects("7a", f)
        self.assertEqual(defs, [("I2", "dag_modify")])
        self.assertEqual(sorted(uses), ["I4", "M3"])

    def test_16a_always_modifies_i_by_m(self):
        f = {"g": 0, "i": 4, "m": 5, "data": 0x1234}
        defs, uses, _unknown = sharcdb.register_effects("16a", f)
        self.assertEqual(defs, [("I4", "dag_modify")])
        self.assertEqual(sorted(uses), ["I4", "M5"])

    def test_move_defines_dst_uses_src(self):
        dst, src = UREG_CODES["MODE1"], UREG_CODES["ASTATX"]
        f = {"dstureg": dst, "srcureghigh": src >> 2, "srcureglow": src & 3}
        defs, uses, _unknown = sharcdb.register_effects("5a_move", f)
        self.assertEqual(defs, [("MODE1", "move")])
        self.assertEqual(uses, ["ASTATX"])

    def test_swap_defines_and_uses_both_registers(self):
        defs, uses, _unknown = sharcdb.register_effects("5a_swap", {"cdreg": 3, "dreg": 5})
        self.assertEqual(sorted(defs), sorted([("R3", "swap"), ("R5", "swap")]))
        self.assertEqual(sorted(uses), ["R3", "R5"])

    def test_literal_load_defines_the_ureg(self):
        defs, uses, _unknown = sharcdb.register_effects("17a", {"ureg": UREG_CODES["LCNTR"]})
        self.assertEqual(defs, [("LCNTR", "literal")])

    def test_18a_set_clear_toggle_is_read_modify_write(self):
        reg = sharcfn.ureg_name(UREG_CODES["USTAT1"])
        defs, uses, _unknown = sharcdb.register_effects("18a", {"sreg": 0, "bop": 0, "data": 3})
        self.assertEqual(defs, [(reg, "literal")])
        self.assertEqual(uses, [reg])

    def test_18a_bit_test_only_reads(self):
        reg = sharcfn.ureg_name(UREG_CODES["USTAT1"])
        defs, uses, _unknown = sharcdb.register_effects("18a", {"sreg": 0, "bop": 4, "data": 3})
        self.assertEqual(defs, [])
        self.assertEqual(uses, [reg])

    def test_loop_literal_defines_lcntr(self):
        defs, uses, _unknown = sharcdb.register_effects("12a_imm", {})
        self.assertEqual(defs, [("LCNTR", "literal")])

    def test_loop_register_defines_lcntr_uses_source(self):
        defs, uses, _unknown = sharcdb.register_effects("12a_ureg", {"ureg": UREG_CODES["ASTATX"]})
        self.assertEqual(defs, [("LCNTR", "move")])
        self.assertEqual(uses, ["ASTATX"])

    def test_shiftimm_unknown_opcode_is_unknown(self):
        # opcode 0x3F is not in _SHIFTIMM_MNEMONICS.
        field = (0x3F << 16) | (3 << 4) | 4
        defs, uses, unknown = sharcdb._shiftimm_regdef_reguse({"shiftimm": field})
        self.assertEqual(defs, [])
        self.assertEqual(unknown, ["shiftimm_unknown_opcode"])

    def test_branch_form_has_no_register_effects(self):
        defs, uses, unknown = sharcdb.register_effects("8a_rel", {"b": 0, "cond": 1, "j": 1})
        self.assertEqual((defs, uses, unknown), ([], [], []))


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

    # --- bblocks/succ/dataref/regdef acceptance facts (a-d) -----------------

    _REACH_SQL = """
        WITH RECURSIVE reach(sw) AS (
          SELECT start_sw FROM bblocks WHERE start_sw <= ? AND end_sw > ?
          UNION
          SELECT s.to_block FROM reach r JOIN succ s ON s.from_block = r.sw
          WHERE s.to_block IS NOT NULL
        )
        SELECT EXISTS(SELECT 1 FROM reach r JOIN bblocks b ON b.start_sw = r.sw
                      WHERE b.start_sw <= ? AND b.end_sw > ?)"""

    def _reaches(self, from_sw, to_sw):
        row = self.db.execute(self._REACH_SQL, (from_sw, from_sw, to_sw, to_sw)).fetchone()
        return bool(row[0])

    def test_a_1c642a_reaches_1c7053_and_its_switch_cases(self):
        self.assertTrue(self._reaches(0x1C642A, 0x1C7053))
        for case_sw in (0x1C65BD, 0x1C6715, 0x1C6782, 0x1C686E):
            self.assertTrue(
                self._reaches(case_sw, 0x1C7053), "case 0x%x does not reach 0x1c7053" % case_sw
            )

    def test_b_delayed_back_edge_is_cond_taken_not_loop_back(self):
        # 0x1c6acc: JUMP IF SZ delayed -> 0x1c6530 -- a back edge formed by
        # an ordinary conditional jump, not a hardware DO..UNTIL loop, so its
        # kind must be cond_taken.
        rows = self.db.execute(
            "SELECT kind, to_block FROM succ WHERE from_block = "
            "(SELECT start_sw FROM bblocks WHERE start_sw <= 0x1c6acc AND end_sw > 0x1c6acc)"
        ).fetchall()
        self.assertIn(("cond_taken", 0x1C6530), rows)
        self.assertNotIn(("loop_back", 0x1C6530), rows)

    def test_b_do_loop_at_1c7040_has_loop_back(self):
        mnemonic = self.db.execute("SELECT mnemonic FROM insn WHERE sw = 0x1c7040").fetchone()[0]
        self.assertIn("0x1c717f", mnemonic)
        rows = self.db.execute(
            "SELECT kind, to_block FROM succ WHERE from_block = "
            "(SELECT start_sw FROM bblocks WHERE start_sw <= 0x1c717f AND end_sw > 0x1c717f)"
        ).fetchall()
        self.assertTrue(any(kind == "loop_back" for kind, _to in rows), rows)

    def test_c_last_writer_of_r6_before_1c6553_is_1c653b(self):
        query = """
            WITH RECURSIVE walk(block_sw, upper_sw) AS (
              SELECT b0.start_sw, ?
              FROM bblocks b0 WHERE b0.start_sw <= ? AND b0.end_sw > ?
              UNION
              SELECT s.from_block, b.end_sw
              FROM walk w
              JOIN bblocks b ON b.start_sw = w.block_sw
              JOIN succ s ON s.to_block = w.block_sw
              WHERE NOT EXISTS (
                SELECT 1 FROM regdef d
                WHERE d.reg = ? AND d.sw >= b.start_sw AND d.sw < w.upper_sw
              )
            )
            SELECT DISTINCT writer_sw FROM (
              SELECT MAX(d.sw) AS writer_sw
              FROM walk w
              JOIN bblocks b ON b.start_sw = w.block_sw
              JOIN regdef d ON d.reg = ? AND d.sw >= b.start_sw AND d.sw < w.upper_sw
              GROUP BY w.block_sw, w.upper_sw
            )"""
        target = 0x1C6553
        rows = self.db.execute(query, (target, target, target, "R6", "R6")).fetchall()
        self.assertEqual([r[0] for r in rows], [0x1C653B])
        # It's a load, not the mnemonic's mis-rendered store (Type3c bug).
        kind = self.db.execute(
            "SELECT kind FROM regdef WHERE sw = 0x1c653b AND reg = 'R6'"
        ).fetchone()[0]
        self.assertEqual(kind, "mem_load")

    def test_d_dataref_i4_table_base_and_2506ec_literal(self):
        rows = self.db.execute(
            "SELECT sw, role FROM dataref WHERE value = 0x8055c840"
        ).fetchall()
        self.assertIn((0x1C6569, "i_reg_base"), rows)
        rows = self.db.execute(
            "SELECT sw FROM dataref WHERE value = 0x2506ec"
        ).fetchall()
        self.assertIn((0x1C307D,), rows)


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


@pytest.mark.slow
@unittest.skipUnless(DT2_116_BLOB.exists() and DN2_111_BLOB.exists(),
                      "DT2 1.16 and DN2 1.11 SHARC blobs are not both available")
class SharcApiGoldenFactsTest(unittest.TestCase):
    """The tools/sharc.py analyze-pass golden facts, through the API rather
    than raw SQL: reachability from an RTOS root, a natural loop, a
    generically-detected code-pointer array, last_def and a cross-image
    func_hash match."""

    @classmethod
    def setUpClass(cls):
        cls.dt2 = sharc.load("dt2-1.16", db_dir="out/sharcdb")
        cls.dn2 = sharc.load("dn2-1.11", db_dir="out/sharcdb")

    @classmethod
    def tearDownClass(cls):
        cls.dt2.close()
        cls.dn2.close()

    def test_per_frame_chain_reachable_from_rtos_root(self):
        reached = {r[0] for r in self.dt2.sql(
            "SELECT function_sw FROM reach WHERE image='dt2-1.16' AND root_sw=0x1c7749"
        )}
        for fn in (0x1C2B24, 0x1C642A, 0x1C71EC, 0x1CBF07):
            self.assertIn(fn, reached, "0x1c7749 does not reach 0x%x" % fn)

    def test_loader_entry_reaches_boot_chain(self):
        # 0x1c1338 sits in a data/fill gap no function's span covers, so
        # reach must fall back to the next function by address (see
        # _detect_reach's docstring) to find this chain at all.
        reached = {r[0] for r in self.dt2.sql(
            "SELECT function_sw FROM reach WHERE image='dt2-1.16' AND root_sw=0x1c1338"
        )}
        for fn in (0x1C13E6, 0x1C7FF9, 0x1C15E3):
            self.assertIn(fn, reached, "loader entry 0x1c1338 does not reach 0x%x" % fn)

    def test_1c642a_loop_at_1c6530(self):
        rows = self.dt2.sql(
            "SELECT header_block FROM loops WHERE image='dt2-1.16' AND function_sw=0x1c642a AND header_block=0x1c6530"
        )
        self.assertTrue(rows)

    def test_8055c840_has_fifteen_code_pointer_array_roots(self):
        rows = self.dt2.sql(
            "SELECT count(*) FROM roots WHERE image='dt2-1.16' AND kind='code_pointer_array' "
            "AND note LIKE 'table=0x8055c840%'"
        )
        self.assertEqual(rows[0][0], 15)

    def test_ivt_slot15_is_seci_at_1c0b7b(self):
        # tools/sharcdb.py's DB_VERSION v7: the L1 hardware IVT at IVT_SW,
        # decoded generically from the 32x24-byte slot table -- slot 15
        # (SECI, sharc-plus-prm Table 4-46) targets 0x1c0b7b on DT2 1.16,
        # byte-exact against the raw image (docs/findings).
        rows = self.dt2.sql(
            "SELECT note FROM roots WHERE image='dt2-1.16' AND kind='interrupt_vector' AND sw=0x1c0b7b"
        )
        self.assertEqual(rows, [("slot 15 SECI",)])

    def test_card_1cbf07_names_the_function_and_its_roots(self):
        card = self.dt2.card(0x1CBF07)
        self.assertIn("FUN_1cbf07", card)
        self.assertIn("roots:", card)
        self.assertIn("dataref_code_pointer", card)

    def test_last_def_r6_before_1c6553(self):
        self.assertEqual(self.dt2.last_def("R6", 0x1C6553), 0x1C653B)

    def test_dn2_stage6_matches_dt2(self):
        matches = self.dt2.match(self.dn2, 0x1CBF07)
        self.assertIn(0xB806F5, {int(m["entry_sw"], 16) for m in matches})

    def test_ptr_resolves_1c15e3_loop_store_base(self):
        # FUN_1c15e3's workspace-copy loop stores through I4 (itself copied
        # from I10, literal-loaded at 0x1c1668 -- docs/findings/06's "Boot
        # fills the pointer table the mix reads"): the constant-pointer pass
        # should resolve the loop's own store site, 0x1c16cc, to that base.
        hits = {h["sw"]: h for h in self.dt2.writers(0x252D78)}
        self.assertIn("0x1c16cc", hits)
        self.assertEqual(hits["0x1c16cc"]["kind"], "resolved")
        self.assertEqual(hits["0x1c16cc"]["base_reg"], "I4")

    def test_ptr_resolves_1c642a_stage_c_selector_read(self):
        # docs/findings/06: FUN_1c642a spills its R4 workspace argument
        # (0x2412c8, from the single call at 0x1c3083) to DM(I6-4), reloads
        # it, and forms I1 = I4 + 0xdc64 - 0x80 before reading the stage C
        # selector at 0x1c6c0f -- DM(0x2412c8 + 0xdbe4) = 0x24eeac.
        hits = {h["sw"]: h for h in self.dt2.readers(0x24EEAC)}
        self.assertIn("0x1c6c0f", hits)
        self.assertEqual(hits["0x1c6c0f"]["kind"], "resolved")
        self.assertEqual(hits["0x1c6c0f"]["address"], "0x24eeac")


@pytest.mark.slow
@unittest.skipUnless(DT2_116_GHIDRA_DUMP.exists(), "DT2 1.16 ColdFire Ghidra dump is not available")
class ColdfireImportTest(unittest.TestCase):
    """The task's ColdFire golden facts, through tools/sharc.py's own API
    against a real import-ghidra build: vector-191 calls the DSPI2 frame
    builder, that builder has exactly two callers, the machine-type-copy
    callers are found, the machine dispatch is inside its real (small)
    function and that function is reachable from a root, and trace() refuses
    a ColdFire image (see sharcdb.py's DB_VERSION comment for the schema
    mapping this exercises)."""

    @classmethod
    def setUpClass(cls):
        cls.out_path = "out/sharcdb/dt2-1.16-cf.sqlite"
        sharcdb.build_ghidra_database(str(DT2_116_GHIDRA_DUMP), cls.out_path, name="dt2-1.16-cf")
        cls.img = sharc.load("dt2-1.16-cf", db_dir="out/sharcdb")

    @classmethod
    def tearDownClass(cls):
        cls.img.close()

    def test_1_vector_191_handler_calls_the_dspi2_frame_builder(self):
        fn = self.img.func(0x4002DD74)
        self.assertEqual(fn["entry_sw"], "0x4002dd0c")
        self.assertIn("0x400cd2bc", self.img.callees(0x4002DD0C))

    def test_2_dspi2_frame_builder_has_exactly_two_callers(self):
        callers = {c["from_sw"] for c in self.img.callers(0x400CD2BC)}
        self.assertEqual(callers, {"0x4002dd74", "0x400ceccc"})

    def test_3_machine_type_copy_has_callers(self):
        self.assertTrue(self.img.callers(0x4002D438))

    def test_4_machine_dispatch_is_inside_its_real_function_and_reachable(self):
        fn = self.img.func(0x400CAF48)
        self.assertEqual(fn["entry_sw"], "0x400cae8c")
        reached = self.img.sql(
            "SELECT COUNT(*) FROM reach WHERE image='dt2-1.16-cf' AND function_sw=0x400cae8c"
        )
        self.assertGreater(reached[0][0], 0)

    def test_5_trace_refuses_a_coldfire_image(self):
        with self.assertRaises(NotImplementedError):
            self.img.trace(0x4002DD0C)


if __name__ == "__main__":
    unittest.main()
