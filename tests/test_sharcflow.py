"""tools/sharcflow.py call and return recognition (the Ghidra pass needs a project)."""

import os
import struct
import sys
import unittest

sys.path.insert(0, os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))), 'tools'))

import sharc_visa_tables as T  # noqa: E402
import sharcflow  # noqa: E402
from test_sharc_disasm import encode  # noqa: E402


def put(name, **stems):
    """-> bits for encode(): each field whose label is STEM or STEM[..] set to its value."""
    extra = 0
    for label, (hi, lo) in T.get_type(name)['fields'].items():
        stem = label.split('[')[0]
        if stem in stems:
            extra |= stems[stem] << lo
    return extra


def words(*ws):
    return struct.pack('<%dH' % len(ws), *ws)


def cjump(target):
    return encode('25a_direct', put('25a_direct') | (target & 0xFFFFFF))


def push3c():
    return words(0x9FF2)


def push3a():
    return encode('3a', put('3a', u=1, i=7, m=7, cond=31, g=0, d=1, l=0, ureg=2))


def store(value):
    return encode('16a', put('16a', i=7, m=7) | (value & 0xFFFFFFFF))


def load(ureg, value):
    return encode('17b', put('17b', ureg=ureg) | (value & 0xFFFF))


def call8a_rel(rel, b=1, a=0, cond=31, j=1, ci=0):
    """Type 8a PC-relative jump/call (SHARC+ Core Programming Reference, Type
    8a, p.357): b=1 is CALL, b=0 is JUMP; j is delayed (1) vs non-delayed (0),
    not a call/jump selector; cond=31 is unconditional."""
    return encode('8a_rel', put('8a_rel', b=b, a=a, cond=cond, j=j, ci=ci) | (rel & 0xFFFFFF))


def call8a_abs(addr, b=1, a=0, cond=31, j=1, ci=0):
    return encode('8a_abs', put('8a_abs', b=b, a=a, cond=cond, j=j, ci=ci) | (addr & 0xFFFFFF))


class SitesTest(unittest.TestCase):
    def test_call_with_push_and_store_in_its_delay_slots(self):
        # SW 0x1000 cjump, 0x1003 push, 0x1004 store (0x1006), return to 0x1007
        # SW 0x1007 load R4, 0x1009 cjump, 0x100c 3a push, 0x100f store (0x1011)
        data = (cjump(0x2000) + push3c() + store(0x1006)
                + load(4, 0xABC) + cjump(0x3000) + push3a() + store(0x1011))
        sites = sharcflow.find_sites(data, 0x1000, min_depth=1)
        self.assertEqual([(c['sw'], c['target'], c['slots'], c['linked'], c['returns_to'])
                          for c in sites['calls']],
                         [(0x1000, 0x2000, ['3c', '16a'], True, 0x1007),
                          (0x1009, 0x3000, ['3a', '16a'], True, 0x1012)])

    def test_store_must_hold_its_address_plus_2(self):
        data = cjump(0x2000) + push3c() + store(0xBF800000) + load(4, 1)
        sites = sharcflow.find_sites(data, 0x1000, min_depth=1)
        self.assertEqual([c['linked'] for c in sites['calls']], [False])

    def test_returns_and_indirect_calls(self):
        # SW 0x1000 return jump, 0x1002 load (epilogue), 0x1004 rframe; next code at 0x1005
        # SW 0x1005 indirect jump through M5 (DB), 0x1007 push, 0x1008 store (0x100a)
        data = (words(0x083F, 0x343F) + load(0, 0xABC) + words(0x1901)
                + words(0x083F, 0x2C3F) + push3c() + store(0x100A))
        sites = sharcflow.find_sites(data, 0x1000, min_depth=1)
        self.assertEqual(sites['returns'], [{'sw': 0x1000, 'slots': ['17b', '25c_rframe'],
                                             'after': 0x1005}])
        self.assertEqual(sites['indirect_calls'], [{'sw': 0x1005, 'slots': ['3c', '16a'],
                                                    'returns_to': 0x100B}])
        self.assertEqual(sites['aligned'][:3], [(0x1000, 4), (0x1002, 4), (0x1004, 2)])


class Type8aCallTest(unittest.TestCase):
    """Type 8a (SHARC+ Core Programming Reference, Type 8a, p.357): b=1 is
    CALL, b=0 is JUMP -- a plain branch, never a call, however its `j` (delay)
    and `cond` bits are set. Unlike CJUMP, an 8a call is a genuine hardware
    call (return address on the PC stack): its delay slots are ordinary
    instructions, not a push+store idiom, so 'linked' is always False."""

    def test_delayed_call_is_recorded_with_its_condition(self):
        # SW 0x1000 8a_rel call (DB) to 0x1010, delay slots at 0x1003/0x1005,
        # return address 0x1007 (after both delay slots).
        data = call8a_rel(0x10) + load(0, 0xAAA) + load(4, 0xBBB)
        sites = sharcflow.find_sites(data, 0x1000, min_depth=1)
        self.assertEqual(len(sites['calls']), 1)
        call = sites['calls'][0]
        self.assertEqual((call['sw'], call['target'], call['kind'], call['cond'],
                          call['conditional'], call['delayed'], call['linked'],
                          call['slots'], call['returns_to']),
                         (0x1000, 0x1010, '8a', 31, False, True, False,
                          ['17b', '17b'], 0x1007))

    def test_branch_b0_is_not_a_call(self):
        data = call8a_rel(0x10, b=0) + load(0, 1) + load(4, 2)
        sites = sharcflow.find_sites(data, 0x1000, min_depth=1)
        self.assertEqual(sites['calls'], [])

    def test_conditional_call_is_kept_with_its_condition_recorded(self):
        data = call8a_rel(0x8, cond=5) + load(0, 1) + load(4, 2)
        sites = sharcflow.find_sites(data, 0x1000, min_depth=1)
        self.assertEqual(len(sites['calls']), 1)
        call = sites['calls'][0]
        self.assertEqual((call['target'], call['cond'], call['conditional']),
                         (0x1008, 5, True))

    def test_non_delayed_call_returns_right_after_itself(self):
        # j=0: no delay slots execute, so the return address is simply the
        # instruction after the (3-short-word) call itself.
        data = call8a_abs(0x2000, j=0) + load(0, 1) + load(4, 2)
        sites = sharcflow.find_sites(data, 0x1000, min_depth=1)
        self.assertEqual(len(sites['calls']), 1)
        call = sites['calls'][0]
        self.assertEqual((call['target'], call['delayed'], call['slots'], call['returns_to']),
                         (0x2000, False, None, 0x1003))

    def test_absolute_form_target_is_the_addr_field(self):
        data = call8a_abs(0x1CB4B2) + load(0, 1) + load(4, 2)
        sites = sharcflow.find_sites(data, 0x1000, min_depth=1)
        self.assertEqual(sites['calls'][0]['target'], 0x1CB4B2)


class PcrelTargetTest(unittest.TestCase):
    """sharcflow.pcrel_target: CJUMP's and Type 8a's shared reladdr formula
    (own short-word address + sign-extended reladdr, wrapped to 24 bits)."""

    def test_forward_offset(self):
        self.assertEqual(sharcflow.pcrel_target(0x1000, 0x10), 0x1010)

    def test_negative_offset_sign_extends(self):
        self.assertEqual(sharcflow.pcrel_target(0x1010, 0xFFFFF0), 0x1000)  # -16

    def test_wraps_past_the_top_of_the_24bit_space(self):
        # blk69's real 8a_rel calls to the 0x1c06ba reciprocal primitive: sw
        # 0xb8031c + reladdr 0x64039e overflows 0xFFFFFF unmasked (0x11c06ba)
        # and must wrap to land on the real target.
        self.assertEqual(sharcflow.pcrel_target(0xB8031C, 0x64039E), 0x1C06BA)

    def test_wraps_below_zero(self):
        self.assertEqual(sharcflow.pcrel_target(0x2, 0xFFFFFA), 0xFFFFFC)  # -6


if __name__ == '__main__':
    unittest.main()
