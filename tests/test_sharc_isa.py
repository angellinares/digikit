"""Typed SHARC ISA model over the public-manual-derived decode table."""

import os
import struct
import sys
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(__file__)), "tools"))

import sharc_isa  # noqa: E402  # pyright: ignore[reportMissingImports]
import sharc_visa_tables as legacy  # noqa: E402  # pyright: ignore[reportMissingImports]


class InstructionSetTest(unittest.TestCase):
    def setUp(self):
        self.isa = sharc_isa.load_instruction_set()

    def test_forms_have_stable_ids_typed_operands_and_evidence(self):
        form = self.isa.form("6b_shiftimm")
        self.assertEqual(form.id, "6b_shiftimm")
        self.assertEqual(form.table_name, "Type6b_shiftimm")
        self.assertEqual(form.extent_bits, 48)
        self.assertEqual(form.operand("shiftimm").kind, sharc_isa.OperandKind.COMPUTE)
        self.assertEqual(
            tuple(fragment.label for fragment in form.operand("shiftimm").fragments),
            ("shiftimm[22:16]", "shiftimm[15:0]"),
        )
        self.assertEqual(form.evidence[0].claim_id, "isa.form.6b_shiftimm.encoding")
        self.assertEqual(form.evidence[0].status, sharc_isa.EvidenceStatus.DOCUMENTED)

        self.assertEqual(
            self.isa.form("3d").evidence[0].status,
            sharc_isa.EvidenceStatus.UNCONFIRMED,
        )
        self.assertEqual(
            self.isa.form("21p_undoc16").evidence[0].status,
            sharc_isa.EvidenceStatus.PROVISIONAL,
        )

    def test_decode_returns_typed_instruction_and_reassembled_operands(self):
        words = (0x023E, 0x3810, 0x8022)
        result = self.isa.decode_words(words)
        self.assertFalse(result.truncated)
        self.assertIsNotNone(result.instruction)
        insn = result.instruction
        assert insn is not None
        self.assertEqual(insn.form.id, "6b_shiftimm")
        self.assertEqual(insn.extent_bytes, 6)
        self.assertEqual(insn.raw, 0x023E38108022)
        self.assertEqual(insn.field_dict()["cond[4:0]"], 0x1F)
        self.assertEqual(insn.operand_dict()["shiftimm"], 0x108022)

    def test_decode_rejects_a_truncated_selected_form(self):
        result = self.isa.decode_words((0x023E, 0x3810))
        self.assertTrue(result.truncated)
        self.assertIsNone(result.instruction)
        self.assertEqual(tuple(form.id for form in result.candidates), ("6b_shiftimm",))

    def test_legacy_table_is_a_compatibility_view_of_the_model(self):
        for form in self.isa.forms:
            old = legacy.get_type(form.id)
            self.assertIsNotNone(old)
            assert old is not None
            self.assertEqual(old, form.legacy_dict())

            words = [
                (form.frame_value >> shift) & 0xFFFF
                for shift in (32, 16, 0)
            ]
            selected, candidates = legacy.decode(words)
            selection = self.isa.select_frame(sharc_isa.frame_of(words))
            self.assertEqual(
                None if selected is None else selected["name"],
                None if selection.form is None else selection.form.id,
            )
            self.assertEqual(candidates, [candidate.id for candidate in selection.candidates])

    def test_raw_bytes_use_little_endian_parcels(self):
        result = self.isa.decode_bytes(struct.pack("<HHH", 0x023E, 0x3810, 0x8022))
        self.assertEqual(result.instruction.form.id, "6b_shiftimm")  # type: ignore[union-attr]


if __name__ == "__main__":
    unittest.main()
