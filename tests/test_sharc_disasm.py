"""tools/sharc_visa_tables.py, tools/sharc_disasm.py and tools/sharccompare.py on words built from the table."""

import os
import struct
import sys
import unittest

sys.path.insert(
    0,
    os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "tools"),
)

from test_sharcldr import block  # noqa: E402

import sharc_disasm  # noqa: E402
import sharc_visa_tables as T  # noqa: E402
import sharccompare  # noqa: E402
import sharcldr  # noqa: E402


def encode(name, extra=0):
    """-> little-endian bytes of a form-`name` instruction: its fixed bits, plus `extra` elsewhere."""
    t = T.get_type(name)
    insn = t["opcode_value"] | (extra & ~t["opcode_mask"] & ((1 << t["bits"]) - 1))
    words = [
        (insn >> (t["bits"] - 16 * (i + 1))) & 0xFFFF for i in range(t["bits"] // 16)
    ]
    return struct.pack("<%dH" % len(words), *words)


def undecodable_word():
    for w in range(0x10000):
        if T.decode([w, 0, 0]) == (None, []):
            return w
    raise AssertionError("every word decodes")


class TableTest(unittest.TestCase):
    def test_names(self):
        names = [t["name"] for t in T.TYPES]
        self.assertEqual(len(names), len(set(names)))
        self.assertEqual(T.form_name("Type5b (move)"), "5b_move")
        self.assertEqual(T.form_name("Type8a_abs"), "8a_abs")

    def test_masks_and_fields_fit_the_width(self):
        for t in T.TYPES:
            low = (1 << (48 - t["bits"])) - 1
            self.assertEqual(t["frame_mask"] & low, 0, t["name"])
            self.assertLess(t["opcode_mask"], 1 << t["bits"], t["name"])
            for label, (hi, lo) in t["fields"].items():
                self.assertTrue(0 <= lo <= hi < t["bits"], (t["name"], label))

    def test_15b_fixes_seven_bits(self):
        t = T.get_type("15b")
        self.assertEqual(t["fixed_bits"], 7)
        self.assertEqual(bin(t["opcode_mask"]).count("1"), 7)


class DisasmTest(unittest.TestCase):
    def test_02_prefix_is_a_48_bit_shift_immediate(self):
        data = struct.pack("<HHH", 0x023E, 0x3810, 0x8022)
        rec = next(sharc_disasm.disassemble(data))
        self.assertEqual(
            (rec.type_name, rec.length_bytes, rec.kind), ("6b_shiftimm", 6, "confident")
        )
        self.assertEqual(rec.raw, 0x023E38108022)
        self.assertEqual(
            rec.fields,
            {
                "cond[4:0]": 0x1F,
                "dataex[3:0]": 7,
                "shiftimm[22:16]": 0x10,
                "shiftimm[15:0]": 0x8022,
            },
        )

    def test_01_prefix_bit39_selects_32bit_compute(self):
        data = bytes.fromhex("a80180820e0700001d00")
        recs = list(sharc_disasm.disassemble(data))
        self.assertEqual(
            (recs[0].type_name, recs[0].length_bytes, recs[0].kind),
            ("2a_short", 4, "confident"),
        )
        self.assertEqual(
            recs[0].fields, {"compute[22:16]": 0x28, "compute[15:0]": 0x8280}
        )
        self.assertEqual(recs[1].type_name, "8a_rel")

    def test_01_prefix_bit39_clear_stays_48bit_type2a(self):
        rec = next(sharc_disasm.disassemble(bytes.fromhex("280180820e07")))
        self.assertEqual(
            (rec.type_name, rec.length_bytes, rec.kind), ("2a", 6, "confident")
        )
        self.assertEqual(rec.fields["cond[4:0]"], 20)

    def test_15_prefix_is_documented_scaled_type19_modify(self):
        data = bytes.fromhex("8715fffffeff")
        rec = next(sharc_disasm.disassemble(data))
        self.assertEqual(
            (rec.type_name, rec.length_bytes, rec.kind),
            ("19a_scaled", 6, "confident"),
        )
        self.assertEqual(rec.raw, 0x1587FFFFFFFE)
        self.assertEqual(
            rec.fields,
            {
                "w": 1,
                "g": 0,
                "idis[2:0]": 0,
                "is[2:0]": 7,
                "data[31:16]": 0xFFFF,
                "data[15:0]": 0xFFFE,
            },
        )

    def test_walk(self):
        t = T.get_type("17b")
        hi, lo = t["fields"]["ureg[6:0]"]
        data = encode("17b", 0x55 << lo) + encode("15b")
        recs = list(sharc_disasm.disassemble(data))
        self.assertEqual(
            [(r.offset, r.type_name, r.length_bytes) for r in recs],
            [(0, "17b", 4), (4, "15b", 4)],
        )
        self.assertEqual(recs[0].fields["ureg[6:0]"], 0x55)

    def test_unknown_stops(self):
        data = (
            encode("17b")
            + struct.pack("<HHH", undecodable_word(), 0, 0)
            + encode("17b")
        )
        recs = list(sharc_disasm.disassemble(data))
        self.assertEqual([r.kind for r in recs], ["confident", "unknown"])
        self.assertEqual(recs[-1].offset, 4)
        report = sharc_disasm.walk_and_report(data)
        self.assertEqual((report.end_offset, report.instructions), (4, 1))

    def test_buffer_ends_inside_an_instruction(self):
        wide = next(
            t["name"]
            for t in T.TYPES
            if t["bits"] == 48
            and not t["uncertain"]
            and T.decode(struct.unpack("<3H", encode(t["name"])))[0] is not None
            and T.decode(struct.unpack("<3H", encode(t["name"])))[0]["name"]
            == t["name"]
        )
        recs = list(sharc_disasm.disassemble(encode(wide)[:4]))
        self.assertEqual(recs[-1].kind, "unknown")

    def test_raise_and_empty(self):
        with self.assertRaises(sharc_disasm.Desync):
            list(sharc_disasm.disassemble(b"\x00", on_unknown="raise"))
        self.assertEqual(list(sharc_disasm.disassemble(b"")), [])


class CompareTest(unittest.TestCase):
    def test_names_agree(self):
        self.assertTrue(sharccompare.names_agree("8a", "8a_abs"))
        self.assertTrue(sharccompare.names_agree("15b", "15b"))
        self.assertFalse(sharccompare.names_agree("5a_move", "5a_swap"))
        self.assertFalse(sharccompare.names_agree("17b", "15b"))
        self.assertFalse(sharccompare.names_agree(None, "15b"))

    def test_sweeps_agree_on_built_words(self):
        data = encode("17b") + struct.pack("<H", undecodable_word()) + encode("15b")
        ours = sharccompare.sweep_ours(sharc_disasm, data, 0)
        spec = sharccompare.sweep_spec(data, 0)
        self.assertEqual(ours[0], (4, "17b"))
        self.assertEqual(ours[4], (None, None))
        self.assertEqual(
            sharccompare.compare(ours, spec, 5)["same_form"],
            sharccompare.compare(ours, spec, 5)["common_instructions"],
        )


def _pack48(value):
    """Little-endian bytes of a raw 48-bit word (test_sharcimm.py's helper,
    duplicated rather than cross-imported to avoid a test-module cycle)."""
    words = [(value >> (48 - 16 * (i + 1))) & 0xFFFF for i in range(3)]
    return struct.pack("<3H", *words)


def _insn17a(ureg, value):
    return encode(
        "17a",
        put(
            "17a",
            **{
                "ureg[6:0]": ureg,
                "data[31:16]": value >> 16,
                "data[15:0]": value & 0xFFFF,
            },
        ),
    )


def put(name, **values):
    fields = T.get_type(name)["fields"]
    extra = 0
    for label, value in values.items():
        hi, lo = fields[label]
        extra |= value << lo
    return extra


class ConfidentDecodeTest(unittest.TestCase):
    """tools/sharc_disasm.resolve_confident_width()'s two entry points --
    decode_confident() (flat buffer) and decode_confident_loaded() (loader-
    backed memory) -- both used by tools/sharc_core.sequencer.decode_at(), so
    a single-PC decode makes the identical width/form choice a whole-image
    walk (tools/sharcimm.py's decode_all(), tests/test_sharcimm.py's
    WidthPreferenceTest) makes. See docs/findings/05-sharc-isa-and-decoding.md,
    "One decode path"."""

    def test_flat_prefers_narrower_form_over_naive_wide_pick(self):
        # Same fixture as tests/test_sharcimm.py's WidthPreferenceTest: word0
        # is Type2c (16 bits) but also satisfies Type2b's (32-bit) mask, and
        # select_frame() alone picks the wider form on the leading-fixed-bits
        # tie-break -- until real code follows and its own run breaks down.
        word0 = encode("2c", 0)
        naive = next(sharc_disasm.disassemble(word0 + b"\x00" * 24, 0, count=1))
        self.assertEqual(naive.type_name, "2b")  # sanity: naive pick is wrong

        filler = b"".join(_insn17a(4 + (i % 8), 0x1000 + i) for i in range(12))
        data = word0 + filler
        resolved = sharc_disasm.decode_confident(data, 0)
        self.assertEqual((resolved.type_name, resolved.length_bytes), ("2c", 2))

    def test_flat_lands_on_never_aligned_form_directly_and_still_decodes(self):
        # decode_confident() only excludes NEVER_ALIGNED_FORMS from a
        # *successor* chain (see _raw_at_flat below); landing on one
        # directly still decodes it -- decode_all()'s table simply has no
        # verdict on a PC only reachable this way (never "aligned" real
        # code), not a "this PC is definitely not code" one. See
        # tests/test_sharc_trace.py's PhaseAOpcodeRegressionTest, which
        # relies on this for Type10a_rel's own field layout.
        data = _pack48(0xE00000000000)  # a pure Type10a_rel decode trap
        resolved = sharc_disasm.decode_confident(data, 0)
        self.assertEqual((resolved.type_name, resolved.kind), ("10a_rel", "confident"))

    def test_raw_at_flat_excludes_never_aligned_forms(self):
        # The successor-chain exclusion decode_confident() does keep, so a
        # wide form's own chain "breaking" on a decode-trap word is treated
        # exactly as decode_all()'s table.get() treats it: as no successor
        # at all (DT2 1.16 sw 0x1c4b99, tests/test_sharcimm.py's
        # WidthPreferenceTest).
        data = _pack48(0xE00000000000)
        self.assertIsNone(sharc_disasm._raw_at_flat(data, 0))

    def test_loaded_matches_flat_on_the_same_bytes(self):
        word0 = encode("2c", 0)
        filler = b"".join(_insn17a(4 + (i % 8), 0x1000 + i) for i in range(12))
        data = word0 + filler
        flat = sharc_disasm.decode_confident(data, 0)

        sw = 0x100000
        addr = sharcldr.sw_to_byte(sw)
        mem = sharcldr.LoadedMemory.from_stream(block(0, addr, len(data), payload=data))
        loaded = sharc_disasm.decode_confident_loaded(mem, sw)
        self.assertEqual(
            (loaded.type_name, loaded.length_bytes), (flat.type_name, flat.length_bytes)
        )
        self.assertEqual(loaded.fields, flat.fields)

    def test_short_compute_rejoins_beyond_eight_instructions_with_uncertain_tail(self):
        # Synthetic ADD followed by ten immediate loads and a NOP. The
        # alternative wide stream reads the constants as BIT instructions;
        # both streams remain confident until they rejoin after the NOP.
        words = [0xC020]
        for i in range(10):
            words.extend([0x0F00 + i, 0x1400, 0x0100])
        words.extend([0x0001, 0x8000, 0, 0])
        data = struct.pack("<%dH" % len(words), *words)
        naive = next(sharc_disasm.disassemble(data, count=1))
        self.assertEqual(naive.type_name, "2b")
        flat = sharc_disasm.decode_confident(data, 0)
        self.assertEqual((flat.type_name, flat.length_bytes), ("2c", 2))
        sw = 0x100000
        addr = sharcldr.sw_to_byte(sw)
        mem = sharcldr.LoadedMemory.from_stream(block(0, addr, len(data), payload=data))
        loaded = sharc_disasm.decode_confident_loaded(mem, sw)
        self.assertEqual((loaded.type_name, loaded.length_bytes), ("2c", 2))
        self.assertEqual(loaded.fields, flat.fields)

    def test_short_compute_resolves_when_next_short_compute_is_also_ambiguous(self):
        # Synthetic words: a Type3b load, a short compute (0xC029), two shifts,
        # a second short compute (0xC020) and an indexed load, then ten
        # immediate loads. The raw chain reads each short compute with its
        # successor word as a 2b, so the first one only looks clean once the
        # second has itself resolved to 2c.
        words = [0x473E, 0x403D, 0xC029, 0x023E, 0x0000, 0x0801]
        words += [0x023E, 0x0000, 0x0823, 0xC020, 0x9A08, 0x0220]
        for i in range(10):
            words.extend([0x0F00 + i, 0x1400, 0x0100])
        words.extend([0x0001, 0x8000, 0, 0])
        data = struct.pack("<%dH" % len(words), *words)
        naive = next(sharc_disasm.disassemble(data, 4, count=1))
        self.assertEqual(naive.type_name, "2b")
        flat = sharc_disasm.decode_confident(data, 4)
        self.assertEqual((flat.type_name, flat.length_bytes), ("2c", 2))
        self.assertEqual(sharc_disasm.decode_confident(data, 0).type_name, "3b")
        sw = 0x100000
        addr = sharcldr.sw_to_byte(sw)
        mem = sharcldr.LoadedMemory.from_stream(block(0, addr, len(data), payload=data))
        loaded = sharc_disasm.decode_confident_loaded(mem, sw + 2)
        self.assertEqual((loaded.type_name, loaded.length_bytes), ("2c", 2))
        self.assertEqual(loaded.fields, flat.fields)
        # With nothing valid after it, 2b stays.
        broken = struct.pack("<6H", 0x473E, 0x403D, 0xC029, 0x023E, 0, 0)
        self.assertEqual(sharc_disasm.decode_confident(broken, 4).type_name, "2b")

    def test_loaded_lands_on_never_aligned_form_directly_and_still_decodes(self):
        sw = 0x100000
        addr = sharcldr.sw_to_byte(sw)
        data = _pack48(0xE00000000000)
        mem = sharcldr.LoadedMemory.from_stream(block(0, addr, len(data), payload=data))
        resolved = sharc_disasm.decode_confident_loaded(mem, sw)
        self.assertEqual((resolved.type_name, resolved.kind), ("10a_rel", "confident"))

    def test_raw_at_loaded_excludes_never_aligned_forms(self):
        sw = 0x100000
        addr = sharcldr.sw_to_byte(sw)
        data = _pack48(0xE00000000000)
        mem = sharcldr.LoadedMemory.from_stream(block(0, addr, len(data), payload=data))
        self.assertIsNone(sharc_disasm._raw_at_loaded(mem, 2 * sw))

    def test_loaded_unmapped_pc_fails_closed(self):
        mem = sharcldr.LoadedMemory.from_stream(b"")
        resolved = sharc_disasm.decode_confident_loaded(mem, 0x100000)
        self.assertEqual(resolved.kind, "unknown")


if __name__ == "__main__":
    unittest.main()
