"""tools/sharcinv.py: boundaries, feature vectors and labels on synthetic code.

No real firmware is used (it is Elektron's copyright and is not committed);
words are built directly from tools/sharc_visa_tables.py, the same way
tests/test_sharcflow.py builds calls and returns.
"""

import os
import struct
import sys
import unittest

sys.path.insert(0, os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))), 'tools'))

import sharc_disasm  # noqa: E402
import sharc_visa_tables as T  # noqa: E402
import sharcflow  # noqa: E402
import sharcinv  # noqa: E402
from test_sharc_disasm import encode  # noqa: E402
from test_sharcflow import call8a_rel, cjump, load, push3c, store, words  # noqa: E402


def field_insn(name, **values):
    """A `name` instruction with each of its fields set from `values` (by
    merged base name, e.g. reladdr=0x1234 covers both reladdr[22:16] and
    reladdr[15:0]) -- generalizes compute23's per-chunk placement below to
    any form whose fields are split across non-overlapping bit ranges, such
    as Type12a's data/reladdr."""
    t = T.get_type(name)
    insn = t['opcode_value']
    for label, (hi, lo) in t['fields'].items():
        base = label.split('[')[0]
        if base not in values:
            continue
        if '[' in label:
            chi, clo = label[label.index('[') + 1:-1].split(':')
            chi, clo = int(chi), int(clo)
            chunk = (values[base] >> clo) & ((1 << (chi - clo + 1)) - 1)
        else:
            chunk = values[base] & ((1 << (hi - lo + 1)) - 1)
        insn |= chunk << lo
    nwords = t['bits'] // 16
    ws = [(insn >> (t['bits'] - 16 * (i + 1))) & 0xFFFF for i in range(nwords)]
    return struct.pack('<%dH' % nwords, *ws)


def loop_insn(sw, count, reladdr, form='12a_imm'):
    """(sw, Instruction) for a Type12a hardware-loop setup at `sw`, trip
    count `count` (12a_imm) and end offset `reladdr` short-words ahead of
    the loop body's start (PRM Table 12a: reladdr is PC-relative from the
    setup instruction itself, see sharcinv.compute_vector)."""
    data = field_insn(form, data=count, reladdr=reladdr & 0x7FFFFF, mode=0) \
        if form == '12a_imm' else field_insn(form, ureg=count, reladdr=reladdr & 0x7FFFFF, mode=0)
    insn = next(sharc_disasm.disassemble(data))
    return sw, insn


def compute23(name, field23):
    """A `name` instruction (2a/2a_short/... ) with its 23-bit compute field
    (bits 22:0 of compute[22:16]/compute[15:0]) set to `field23`, the fixed
    bits from the opcode table otherwise -- put() in test_sharcflow.py can't
    place a value that's split across two field chunks, so this does it by
    hand from each chunk's (hi, lo)."""
    t = T.get_type(name)
    insn = t['opcode_value']
    for label, (hi, lo) in t['fields'].items():
        if label.split('[')[0] != 'compute':
            continue
        chi, clo = label[label.index('[') + 1:-1].split(':')
        chunk = (field23 >> int(clo)) & ((1 << (int(chi) - int(clo) + 1)) - 1)
        insn |= chunk << lo
    nwords = t['bits'] // 16
    ws = [(insn >> (t['bits'] - 16 * (i + 1))) & 0xFFFF for i in range(nwords)]
    return struct.pack('<%dH' % nwords, *ws)


def rframe():
    return encode('25c_rframe')


def ret():
    """9b_abs return jump (raw 0x083F343F), split into its two 16-bit words."""
    return words(sharcinv.RETURN_JUMP >> 16, sharcinv.RETURN_JUMP & 0xFFFF)


DUAL_ADD_SUB_FIXED = (0x7 << 16) | (0xA << 12) | (0x1 << 8) | (0x2 << 4) | 0x3
# cu=0 (bits 22:20 = 000), opcode[19:16]=0111 (dual add/sub, fixed),
# opcode[15:12]=Rs=0xA, Ra(rn)=1, Rx=2, Ry=3 -- PRM Table 18-10.


class BoundariesTest(unittest.TestCase):
    def _block(self, data, base_sw, min_depth=1):
        sites = sharcflow.find_sites(data, base_sw, min_depth)
        insns = sharcflow.aligned(data, min_depth)
        block = {'base_sw': base_sw, 'sites': sites, 'insns': insns}
        block['_insn_sw'] = [base_sw + off // 2 for off, _ in insns]
        return block

    def test_two_functions_split_at_the_return(self):
        # fn A: a call, then a return with its delay slots
        # fn B: one dual add/subtract compute op, then a return
        data = (cjump(0x2000) + push3c() + store(0x1007)
                + ret() + load(0, 0) + rframe()
                + compute23('2a_short', DUAL_ADD_SUB_FIXED) + ret() + load(0, 0) + rframe())
        block = self._block(data, 0x1000)
        spans = sharcinv.function_bounds(block)
        self.assertEqual(len(spans), 2)
        (a_entry, a_exit, a_kind), (b_entry, b_exit, b_kind) = spans
        self.assertEqual(a_entry, 0x1000)
        self.assertEqual(b_entry, a_exit)
        self.assertEqual(a_kind, 'return_boundary')
        self.assertEqual(b_kind, 'return_boundary')

        b_insns = sharcinv.instructions_in(block, b_entry, b_exit)
        v = sharcinv.compute_vector(b_insns, {}, {})
        self.assertEqual(v['dual_add_sub'], 1)
        self.assertEqual(v['int_alu'], 0)
        self.assertEqual(v['float_alu'], 0)

        fv = sharcinv.finalize_vector(v)
        label, conf, reasons = sharcinv.label_function(fv, len(b_insns), 0, 0, True)
        # A single dual add/subtract with no corroborating spectral tell is
        # a plain paired sum/difference, not an FFT claim.
        self.assertEqual(label, 'paired sum/difference (coefficient combine)')
        self.assertTrue(reasons)

    def test_call_target_inside_a_span_splits_it(self):
        # One return-delimited span containing two calls: one to an address
        # inside itself (should split it), one further out (should not).
        inner_target = 0x1006  # lands right after the two calls below
        outer_target = 0x9000
        data = (cjump(inner_target) + push3c() + store(0x1007)
                + cjump(outer_target) + push3c() + store(0x100A)
                + load(0, 0) + ret() + load(0, 0) + rframe())
        block = self._block(data, 0x1000)
        spans = sharcinv.function_bounds(block)
        entries = [e for e, _, _ in spans]
        self.assertIn(inner_target, entries)
        kinds = {e: k for e, _, k in spans}
        self.assertEqual(kinds[0x1000], 'return_boundary')
        self.assertEqual(kinds[inner_target], 'interior_call_target')

    def test_type8a_call_target_inside_a_span_also_splits_it(self):
        # Same shape as above, but the split is driven by a Type 8a CALL
        # (the decoder gap this module's docstring describes) rather than a
        # CJUMP -- sharcinv needs no code change to pick it up, since it only
        # reads sites['calls']['target'].
        inner_target = 0x1003  # right after the 8a call's own 3 short words
        data = (call8a_rel(inner_target - 0x1000, j=0) + load(0, 0) + ret() + load(0, 0) + rframe())
        block = self._block(data, 0x1000)
        spans = sharcinv.function_bounds(block)
        entries = [e for e, _, _ in spans]
        self.assertIn(inner_target, entries)
        kinds = {e: k for e, _, k in spans}
        self.assertEqual(kinds[0x1000], 'return_boundary')
        self.assertEqual(kinds[inner_target], 'interior_call_target')

    def test_type8a_branch_does_not_split_a_span(self):
        # b=0 is a JUMP, not a CALL: it must not appear in sites['calls'], so
        # it must not open a new function inside the span either.
        target = 0x1003
        data = (call8a_rel(target - 0x1000, b=0, j=0) + load(0, 0) + ret() + load(0, 0) + rframe())
        block = self._block(data, 0x1000)
        spans = sharcinv.function_bounds(block)
        entries = [e for e, _, _ in spans]
        self.assertNotIn(target, entries)
        self.assertEqual(len(spans), 1)


class ComputeClassifyTest(unittest.TestCase):
    def test_plain_integer_add(self):
        field = (0 << 20) | (0x01 << 12) | (1 << 8) | (2 << 4) | 3  # RN=RX+RY
        cu, d = sharcinv.classify_compute(field)
        self.assertEqual(cu, 'ALU')
        self.assertFalse(d['is_float'])
        self.assertNotIn('is_dual_addsub', d)

    def test_float_add(self):
        field = (0 << 20) | (0x81 << 12)  # FN = FX + FY
        cu, d = sharcinv.classify_compute(field)
        self.assertEqual(cu, 'ALU')
        self.assertTrue(d['is_float'])

    def test_dual_add_subtract_fixed(self):
        cu, d = sharcinv.classify_compute(DUAL_ADD_SUB_FIXED)
        self.assertEqual(cu, 'ALU')
        self.assertTrue(d['is_dual_addsub'])
        self.assertFalse(d['is_float'])

    def test_dual_add_subtract_float(self):
        field = (0xF << 16) | (0xA << 12) | (1 << 8) | (2 << 4) | 3
        cu, d = sharcinv.classify_compute(field)
        self.assertEqual(cu, 'ALU')
        self.assertTrue(d['is_dual_addsub'])
        self.assertTrue(d['is_float'])

    def test_mac_accumulate(self):
        # cu=1 (MULT), top2=2 ("acc + RX*RY"), signed/int -> a MAC
        opcode = (2 << 6) | (0 << 3) | 0  # top2=10, F=0
        field = (1 << 20) | (opcode << 12)
        cu, d = sharcinv.classify_compute(field)
        self.assertEqual(cu, 'MULT')
        self.assertTrue(d['is_mac'])

    def test_multifunction_mul_dual_addsub(self):
        field = (6 << 20)  # bits[22:20]=110: MUL + dual add/subtract, fixed
        cu, d = sharcinv.classify_compute(field)
        self.assertEqual(cu, 'MULTIFN')
        self.assertTrue(d['is_mac'])
        self.assertTrue(d['is_dual_addsub'])
        self.assertFalse(d['is_float'])

    def test_zero_field_is_no_compute(self):
        self.assertEqual(sharcinv.classify_compute(0), (None, {}))


class FieldMergeTest(unittest.TestCase):
    def test_merges_split_fields(self):
        merged = sharcinv.merge_fields({'data[31:16]': 0x1234, 'data[15:0]': 0x5678, 'g': 1})
        self.assertEqual(merged, {'data': 0x12345678, 'g': 1})

    def test_float32_bit_pattern(self):
        self.assertAlmostEqual(sharcinv.float32(0x3F800000), 1.0)


class LiteralRegionTest(unittest.TestCase):
    def test_named_table(self):
        self.assertEqual(sharcinv.classify_literal(0x8055C440), 'named:cosine_a')

    def test_param_frame(self):
        self.assertEqual(sharcinv.classify_literal(0x2559000), 'other')  # out of range on purpose
        self.assertEqual(sharcinv.classify_literal(0x255900), 'param_frame')

    def test_audio_ring(self):
        self.assertEqual(sharcinv.classify_literal(0x262138), 'audio_ring')

    def test_external_table_space(self):
        self.assertEqual(sharcinv.classify_literal(0x80123456), 'external_0x80xxxxxx')

    def test_generic_dm(self):
        self.assertEqual(sharcinv.classify_literal(0x210000), 'dm_0x2xxxxx')


class SwBaseTest(unittest.TestCase):
    def test_alias_window(self):
        self.assertEqual(sharcinv.sw_base_for_target(0x282403F0), 0x1201F8)

    def test_l2_window(self):
        self.assertEqual(sharcinv.sw_base_for_target(0x20000000), sharcinv.L2_SW_BASE)

    def test_outside_any_window(self):
        self.assertIsNone(sharcinv.sw_base_for_target(0x10000000))


class LoopFeatureTest(unittest.TestCase):
    """loop_pow2_uniform and nested_loops -- the two new corroborating
    features derived (in finalize_vector) from Type12a's reladdr, the same
    end_sw formula tools/sharc_trace.py uses at runtime, computed here
    statically from the instruction alone."""

    def test_pow2_uniform_true_when_every_literal_count_is_a_power_of_two(self):
        insns = [loop_insn(0x1000, 32, 0x10), loop_insn(0x1100, 16, 0x10)]
        fv = sharcinv.finalize_vector(sharcinv.compute_vector(insns, {}, {}))
        self.assertEqual(fv['loop_pow2_uniform'], 1)

    def test_pow2_uniform_false_with_one_non_power_of_two(self):
        # blk93@0x1c5615's real shape: literal trip counts [32, 15, 32] --
        # the 15 disqualifies the whole function (see the module docstring).
        insns = [loop_insn(0x1000, 32, 0x10), loop_insn(0x1100, 15, 0x10),
                 loop_insn(0x1200, 32, 0x10)]
        fv = sharcinv.finalize_vector(sharcinv.compute_vector(insns, {}, {}))
        self.assertEqual(fv['loop_pow2_uniform'], 0)

    def test_pow2_uniform_false_with_no_literal_loops(self):
        fv = sharcinv.finalize_vector(sharcinv.compute_vector([], {}, {}))
        self.assertEqual(fv['loop_pow2_uniform'], 0)

    def test_nested_loops_detected(self):
        # outer: setup at 0x1000, body [0x1003, 0x1020); inner: setup at
        # 0x1010 (inside the outer body), body [0x1013, 0x1015) -- strictly
        # inside the outer span.
        outer = loop_insn(0x1000, 4, 0x20)
        inner = loop_insn(0x1010, 8, 0x5)
        fv = sharcinv.finalize_vector(sharcinv.compute_vector([outer, inner], {}, {}))
        self.assertEqual(fv['nested_loops'], 1)

    def test_flat_sequential_loops_are_not_nested(self):
        # blk69@0xb8063e's real shape: 17 loops, all flat -- disjoint spans,
        # one after another, never one inside another.
        a = loop_insn(0x1000, 4, 0x8)
        b = loop_insn(0x1010, 4, 0x8)
        fv = sharcinv.finalize_vector(sharcinv.compute_vector([a, b], {}, {}))
        self.assertEqual(fv['nested_loops'], 0)


class DualAddSubLabelTest(unittest.TestCase):
    """label_function's strengthened dual-add/subtract rule: the plain,
    accurate label by default, escalating to an FFT claim only once two or
    more of the four corroborating tells also show up."""

    def _fv(self, **overrides):
        fv = sharcinv.finalize_vector(sharcinv.empty_vector())
        fv.update(overrides)
        return fv

    def test_dual_addsub_alone_is_paired_sum_difference_not_fft(self):
        fv = self._fv(dual_add_sub=2, compute_total=2)
        label, conf, reasons = sharcinv.label_function(fv, 50, 0, 0, True)
        self.assertEqual(label, 'paired sum/difference (coefficient combine)')
        self.assertTrue(reasons)

    def test_one_corroborator_is_not_enough_to_claim_fft(self):
        # blk93@0x1cb647's real shape: dual add/subtract plus a single
        # power-of-two loop (the block-average case in the module
        # docstring) -- flagged as worth a look, but not labelled FFT-like.
        fv = self._fv(dual_add_sub=3, compute_total=3, loop_pow2_uniform=1,
                      loop_literal_values=[32])
        label, conf, reasons = sharcinv.label_function(fv, 168, 0, 0, True)
        self.assertEqual(label, 'paired sum/difference (coefficient combine)')
        self.assertIn('uncorroborated', ' '.join(reasons))

    def test_two_corroborators_escalates_to_fft_like(self):
        fv = self._fv(dual_add_sub=2, compute_total=2, bitrev_addr=1, nested_loops=1)
        label, conf, reasons = sharcinv.label_function(fv, 50, 0, 0, True)
        self.assertEqual(label, 'FFT-like (dual add/subtract + spectral tell)')
        self.assertTrue(reasons)

    def test_table_touch_and_nesting_together_also_escalate(self):
        fv = self._fv(dual_add_sub=2, compute_total=2, nested_loops=2,
                      named_tables_touched=['cosine_a'])
        label, conf, reasons = sharcinv.label_function(fv, 50, 0, 0, True)
        self.assertEqual(label, 'FFT-like (dual add/subtract + spectral tell)')

    def test_dual_addsub_not_dominant_does_not_reach_the_rule_at_all(self):
        # one dual add/subtract in a function whose compute is otherwise
        # large (>20) never triggers either label -- see 0x1cd286 in
        # --ground-truth, an honest abstention this rule must not disturb.
        fv = self._fv(dual_add_sub=1, compute_total=40, bitrev_addr=1, nested_loops=1)
        label, conf, reasons = sharcinv.label_function(fv, 148, 1, 0, False)
        self.assertNotIn('dual add/subtract', label)
        self.assertNotIn('paired sum/difference', label)


if __name__ == '__main__':
    unittest.main()
