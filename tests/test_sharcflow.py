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


if __name__ == '__main__':
    unittest.main()
