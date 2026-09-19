"""tools/sharc_visa_tables.py, tools/sharc_disasm.py and tools/sharccompare.py on words built from the table."""

import os
import struct
import sys
import unittest

sys.path.insert(0, os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))), 'tools'))

import sharc_disasm  # noqa: E402
import sharc_visa_tables as T  # noqa: E402
import sharccompare  # noqa: E402


def encode(name, extra=0):
    """-> little-endian bytes of a form-`name` instruction: its fixed bits, plus `extra` elsewhere."""
    t = T.get_type(name)
    insn = t['opcode_value'] | (extra & ~t['opcode_mask'] & ((1 << t['bits']) - 1))
    words = [(insn >> (t['bits'] - 16 * (i + 1))) & 0xFFFF for i in range(t['bits'] // 16)]
    return struct.pack('<%dH' % len(words), *words)


def undecodable_word():
    for w in range(0x10000):
        if T.decode([w, 0, 0]) == (None, []):
            return w
    raise AssertionError('every word decodes')


class TableTest(unittest.TestCase):
    def test_names(self):
        names = [t['name'] for t in T.TYPES]
        self.assertEqual(len(names), len(set(names)))
        self.assertEqual(T.form_name('Type5b (move)'), '5b_move')
        self.assertEqual(T.form_name('Type8a_abs'), '8a_abs')

    def test_masks_and_fields_fit_the_width(self):
        for t in T.TYPES:
            low = (1 << (48 - t['bits'])) - 1
            self.assertEqual(t['frame_mask'] & low, 0, t['name'])
            self.assertLess(t['opcode_mask'], 1 << t['bits'], t['name'])
            for label, (hi, lo) in t['fields'].items():
                self.assertTrue(0 <= lo <= hi < t['bits'], (t['name'], label))

    def test_15b_fixes_seven_bits(self):
        t = T.get_type('15b')
        self.assertEqual(t['fixed_bits'], 7)
        self.assertEqual(bin(t['opcode_mask']).count('1'), 7)

    def test_type2_width_is_bit_39(self):
        # PGR prefix 000000011 (SPEC-FINDINGS 3.4): Type 2 with bit 39 set is
        # the 32-bit 2b, clear is the 48-bit 2a; 1100 stays the 16-bit 2c.
        for words, name, bits in (([0x0193, 0x0421, 0], '2b', 32),
                                  ([0x0110, 0x0000, 0], '2a', 48),
                                  ([0xC012, 0x0000, 0], '2c', 16)):
            entry, _ = T.decode(words)
            self.assertEqual((entry['name'], entry['bits']), (name, bits), hex(words[0]))

    def test_10a_rel_decodes_in_visa(self):
        entry, _ = T.decode([0xE5E8, 0x0F04, 0x0000])
        self.assertEqual((entry['name'], entry['bits']), ('10a_rel', 48))


class DisasmTest(unittest.TestCase):
    def test_walk(self):
        t = T.get_type('17b')
        hi, lo = t['fields']['ureg[6:0]']
        data = encode('17b', 0x55 << lo) + encode('15b')
        recs = list(sharc_disasm.disassemble(data))
        self.assertEqual([(r.offset, r.type_name, r.length_bytes) for r in recs],
                         [(0, '17b', 4), (4, '15b', 4)])
        self.assertEqual(recs[0].fields['ureg[6:0]'], 0x55)

    def test_assembled_sequence(self):
        # r0 = 0x1234; r1 = r0 + r1; nop; jump start -- assembled from source
        # with an open-source SHARC+ assembler, linked at 0x180000.
        data = bytes.fromhex('800f34128001011101003e0618000000')
        recs = list(sharc_disasm.disassemble(data))
        self.assertEqual([(r.offset, r.type_name, r.length_bytes) for r in recs],
                         [(0, '17b', 4), (4, '2b', 4), (8, '21c', 2), (10, '8a_abs', 6)])

    def test_unknown_stops(self):
        data = encode('17b') + struct.pack('<HHH', undecodable_word(), 0, 0) + encode('17b')
        recs = list(sharc_disasm.disassemble(data))
        self.assertEqual([r.kind for r in recs], ['confident', 'unknown'])
        self.assertEqual(recs[-1].offset, 4)
        report = sharc_disasm.walk_and_report(data)
        self.assertEqual((report.end_offset, report.instructions), (4, 1))

    def test_buffer_ends_inside_an_instruction(self):
        wide = next(t['name'] for t in T.TYPES if t['bits'] == 48 and not t['uncertain']
                    and T.decode(struct.unpack('<3H', encode(t['name'])))[0] is not None
                    and T.decode(struct.unpack('<3H', encode(t['name'])))[0]['name'] == t['name'])
        recs = list(sharc_disasm.disassemble(encode(wide)[:4]))
        self.assertEqual(recs[-1].kind, 'unknown')

    def test_raise_and_empty(self):
        with self.assertRaises(sharc_disasm.Desync):
            list(sharc_disasm.disassemble(b'\x00', on_unknown='raise'))
        self.assertEqual(list(sharc_disasm.disassemble(b'')), [])


class CompareTest(unittest.TestCase):
    def test_names_agree(self):
        self.assertTrue(sharccompare.names_agree('8a', '8a_abs'))
        self.assertTrue(sharccompare.names_agree('15b', '15b'))
        self.assertFalse(sharccompare.names_agree('5a_move', '5a_swap'))
        self.assertFalse(sharccompare.names_agree('17b', '15b'))
        self.assertFalse(sharccompare.names_agree(None, '15b'))

    def test_sweeps_agree_on_built_words(self):
        data = encode('17b') + struct.pack('<H', undecodable_word()) + encode('15b')
        ours = sharccompare.sweep_ours(sharc_disasm, data, 0)
        spec = sharccompare.sweep_spec(data, 0)
        self.assertEqual(ours[0], (4, '17b'))
        self.assertEqual(ours[4], (None, None))
        self.assertEqual(sharccompare.compare(ours, spec, 5)['same_form'],
                         sharccompare.compare(ours, spec, 5)['common_instructions'])


if __name__ == '__main__':
    unittest.main()
