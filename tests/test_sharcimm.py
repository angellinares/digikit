"""tools/sharcimm.py on instructions built from the table and a hand-built boot stream."""

import os
import struct
import sys
import unittest

sys.path.insert(0, os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))), 'tools'))

import sharc_disasm  # noqa: E402
import sharc_visa_tables as T  # noqa: E402
import sharcldr  # noqa: E402
import sharcimm  # noqa: E402
from test_sharc_disasm import encode  # noqa: E402
from test_sharcldr import block  # noqa: E402


def pack48(value):
    """Little-endian bytes of a raw 48-bit word, bypassing sharc_visa_tables
    (used for Type10a_rel/10a_abs below, which -- per tools/sharcfn.py's
    module docstring -- are absent from that table's VISA form set)."""
    words = [(value >> (48 - 16 * (i + 1))) & 0xFFFF for i in range(3)]
    return struct.pack('<3H', *words)


def put(name, **values):
    """-> bits for encode(): each field label set to its value."""
    fields = T.get_type(name)['fields']
    extra = 0
    for label, value in values.items():
        hi, lo = fields[label]
        extra |= value << lo
    return extra


def insn17a(ureg, value):
    return encode('17a', put('17a', **{'ureg[6:0]': ureg, 'data[31:16]': value >> 16,
                                       'data[15:0]': value & 0xFFFF}))


class NameTest(unittest.TestCase):
    def test_names(self):
        self.assertEqual(sharcimm.name_address(0x30024), 'CMMR_SYSCTL')
        self.assertEqual(sharcimm.name_address(0x31400), 'SHBTB_CFG')
        self.assertEqual(sharcimm.name_address(0x3E000), 'SHL1C_CFG')
        self.assertEqual(sharcimm.name_address(0x31030004), 'SPI2 CTL')
        self.assertEqual(sharcimm.name_address(0x3102D280), 'DMA27 (SPI2 RX) DSCPTR_NXT')
        self.assertEqual(sharcimm.name_address(0x310CA2E8), 'DAI1_GBL_SP_EN')
        self.assertEqual(sharcimm.name_address(0x310C9314), 'PCG0_SYNC1')
        self.assertEqual(sharcimm.name_address(0x3108C02C), 'RCU0+0x2c')
        self.assertIsNone(sharcimm.name_address(0x20000000))

    def test_values_pair_the_halves(self):
        fields = {'ureg[6:0]': 4, 'data[31:16]': 0x3103, 'data[15:0]': 0x0004}
        self.assertEqual(sharcimm.values_of(fields), [('data', 0x31030004, 32)])


class ScanTest(unittest.TestCase):
    def test_immediates(self):
        data = insn17a(4, 0x31030004) + insn17a(8, 0x802)
        hits = sharcimm.scan(data, 0x100, [sharcimm.PERIPHERAL_SPACE], {0x802})
        self.assertEqual([(h['sw'], h['form'], h['value'], h['depth'], h['sweep'], h['name'])
                          for h in hits],
                         [(0x100, '17a', 0x31030004, 1, True, 'SPI2 CTL'),
                          (0x103, '17a', 0x802, 2, True, None)])
        self.assertEqual(hits[0]['fields'], {'ureg[6:0]': 4})

    def test_min_depth(self):
        data = insn17a(4, 0x31030004) + insn17a(8, 0x31030008)
        hits = sharcimm.scan(data, 0, [sharcimm.PERIPHERAL_SPACE], set(), min_depth=2)
        self.assertEqual([h['value'] for h in hits], [0x31030008])

    def test_words(self):
        first = 1 << sharcldr.BFLAGS['FIRST']
        fill = 1 << sharcldr.FILL_BIT
        words = struct.pack('<4I', 0x3F000000, 0x31030000, 0xABC, 0)
        stream = (block(first, 0x100, 0)
                  + block(0, 0x28269250, len(words), payload=words)
                  + block(fill, 0x28269260, 64))
        hits = sharcimm.scan_words(stream, [sharcimm.PERIPHERAL_SPACE], {0xABC})
        self.assertEqual([(h['block'], h['addr'], h['data_ptr'], h['value'], h['name'])
                          for h in hits],
                         [(1, 0x28269254, 0x269254, 0x31030000, 'SPI2+0x0'),
                          (1, 0x28269258, 0x269258, 0xABC, None)])


class NeverAlignedFormsTest(unittest.TestCase):
    """decode_all() must never treat Type10a_rel/10a_abs as a real decoded
    instruction (tools/sharcimm.py's _NEVER_ALIGNED_FORMS comment):
    tools/sharcfn.py's module docstring says both are absent from the real
    VISA form set entirely, so any bit-pattern match is a disassembler
    desync, not a genuine instruction. Before this fix, decode_all() fed
    such a match straight into the depth/sweep alignment sweep, which is
    exactly what corrupted DT2 1.16's real disassembly around sw 0x1c4b99
    -0x1c4bb5 (a Type10a_rel "hit" at sw 0x1c4b9b hid the real 64-bit
    voice-record store at sw 0x1c4ba0)."""

    def test_pure_type10a_rel_pattern_is_excluded(self):
        insn = next(sharc_disasm.disassemble(pack48(0xE00000000000), 0, count=1))
        self.assertEqual(insn.type_name, '10a_rel')  # sanity: it does decode
        table = sharcimm.decode_all(pack48(0xE00000000000))
        self.assertEqual(table, {})

    def test_10a_abs_is_also_in_the_never_aligned_set(self):
        # Type10a_abs never occurs for real in DT2 1.16 (0 raw matches across
        # the whole image, vs Type10a_rel's 4808) so there is no all-zero
        # byte pattern that decodes uniquely to it rather than a more
        # specific competing form; check the exclusion set directly instead.
        self.assertIn('10a_abs', sharcimm._NEVER_ALIGNED_FORMS)

    def test_a_real_instruction_right_after_is_unaffected(self):
        data = pack48(0xE00000000000) + insn17a(4, 0x1234)
        table = sharcimm.decode_all(data)
        self.assertNotIn(0, table)
        self.assertEqual(table[6].type_name, '17a')


class WidthPreferenceTest(unittest.TestCase):
    """tools/sharc_isa.py's select_frame() ranks matching forms by leading
    fixed bits, so Type2b (32-bit, 9 leading fixed bits, all inside word0)
    always outranks Type2c (16-bit, 4 fixed bits) on any word0 whose top 9
    bits happen to fit both -- even though Type2b's word1 is entirely free
    (its mask never touches it), so it "matches" no matter what comes next.
    This is DT2 1.16 sw 0x1c4b99 and sw 0x1c4e10 (see sharcimm.py's
    _WIDTH_LOOKAHEAD comment): decode_all() must prefer Type2c whenever
    Type2b's own successor chain breaks down, or a Type2c-led reading
    independently walks back onto one of Type2b's own successor offsets.
    Built from tools/sharc_visa_tables.py's public field layout only (never
    from firmware bytes)."""

    def test_broken_successor_prefers_narrower_form(self):
        # word0 = Type2c with an all-zero compute field, which happens to
        # also satisfy Type2b's mask (0xC000's top 9 bits are 0b110000000).
        word0 = encode('2c', 0)
        naive = next(sharc_disasm.disassemble(word0 + b'\x00' * 24, 0, count=1))
        self.assertEqual(naive.type_name, '2b')  # sanity: select_frame alone picks the wide form

        # Landing on offset+4 with real code following only ever lines up
        # with a full instruction's *middle*, so it decodes uncertain/unknown
        # -- exactly the DT2 1.16 sw 0x1c4b99 break (there it was a
        # _NEVER_ALIGNED_FORMS trap instead; either way the wide reading's
        # own chain fails to stay confident).
        filler = b''.join(insn17a(4 + (i % 8), 0x1000 + i) for i in range(12))
        table = sharcimm.decode_all(word0 + filler)
        self.assertEqual(table[0].type_name, '2c')
        self.assertEqual(table[0].length_bytes, 2)
        self.assertEqual(table[2].type_name, '17a')

    def test_reconverging_successor_prefers_narrower_form(self):
        # word1 = Type21c, a fully-fixed 16-bit form (all 16 bits pinned, no
        # fields) that wins its own leading-fixed-bits tie outright, so
        # reading word0+word1 as two 16-bit instructions is unambiguous on
        # its own. It also happens to be exactly the free word1 Type2b's
        # mask lets through, so the two readings consume the identical 4
        # bytes and land on the identical successor offset -- proof Type2b's
        # extra word added no information a 16-bit reading lacked (DT2 1.16
        # sw 0x1c4e10: both widths kept decoding confidently for thousands
        # of instructions afterwards, so a broken-successor check alone
        # cannot tell them apart; only this reconvergence can).
        word0 = encode('2c', 0)
        word1 = struct.pack('<H', 1)
        filler = b''.join(insn17a(4 + (i % 8), 0x2000 + i) for i in range(10))
        data = word0 + word1 + filler

        naive = next(sharc_disasm.disassemble(data, 0, count=1))
        self.assertEqual((naive.type_name, naive.length_bytes), ('2b', 4))  # sanity

        table = sharcimm.decode_all(data)
        self.assertEqual(table[0].type_name, '2c')
        self.assertEqual(table[0].length_bytes, 2)
        self.assertEqual(table[2].type_name, '21c')


if __name__ == '__main__':
    unittest.main()
