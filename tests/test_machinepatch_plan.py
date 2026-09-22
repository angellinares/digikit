"""Unit tests for tools/machinepatch.py's pure planner, `plan_b`."""

import dataclasses
import os
import struct
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))), 'tools'))

from emu import config

# tools/machinepatch.py inserts these same two paths itself, but they need
# to be present before the import statement below runs.
import machinepatch as mp
import machineprofile

MAIN_IMAGE_BASE = 0x40000400

# Golden output of today's (pre-MachineSpec) `patch_b`, all five parts, the
# default eighth value -- 18 lines, format 'ADDR  OLD -> NEW'.
GOLDEN_ALL5 = (
    '0x40303e5c  000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000 -> 202f00047207b2806608203c40303f5c4e757206b2806510123c002c4c0108000680429236444e75203c4292374c4e75',
    '0x40303f5c  0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000 -> 40303fa840303fc8000000f8000000f900000000000000fb000000fc000000fd00000000000000fe0000000a',
    '0x40303f9c  000000000000000000000000000000000000000000000000 -> 0000000b0000000bffffffff504c414345484f4c44455200',
    '0x40303fbc  0000000000000000000000000000000000 -> 0000000400000004ffffffff504c484400',
    '0x40303fdc  0000000000000000000000000000000000000000000000000000000000000000 -> 0000000000000001000000020000000300000006000000040000000500000007',
    '0x4005200c  401e1958 -> 40303fdc',
    '0x40052002  401e1974 -> 40303ffc',
    '0x4005d7ca  7206b2806604 -> 7207b2806504',
    '0x4030405c  000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000 -> 4022c7ab402398c800000000402111f94022c7b3000000004022c7b74023248d00000000402112264022c7bf000000004021124e4022c7c3000000004022a8e04022a8e0000000004022caa24022c7c74022c7cb',
    '0x403040b0  000000000000000000000000 -> 403040dc403040ec00000000',
    '0x403040dc  000000000000000000000000 -> 506c616365686f6c64657200',
    '0x403040ec  00000000 -> 504c4300',
    '0x400dcc51  06 -> 07',
    '0x400dcc62  401fbc50 -> 4030405c',
    '0x4030411c  00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000 -> 00000000000000000000000100000001000000020000000200000003000000030000000600000004000000040000000500000005000000060000000700000007',
    '0x403040fc  00000000000000000000000000000000000000000000 -> 2f7c4030411c00082f7c4030415c000c4ef940198948',
    '0x40051874  40198948 -> 403040fc',
    '0x400caf48  7206202f0004 -> 4ef940303e5c',
)


# These plans pin 1.15C bytes, so prefer the pinned 1.15C extract over
# config.main_image(), which follows whichever firmware sections/ holds.
PINNED_1_15C = os.path.join('out', 'sections', 'dt2-1.15C', 'section_3_MAIN_OS.bin')


def _try_main_image():
    path = PINNED_1_15C
    if not os.path.exists(path):
        try:
            path = config.main_image()
        except Exception:
            return None
    if not os.path.exists(path):
        return None
    try:
        with open(path, 'rb') as fh:
            return fh.read()
    except OSError:
        return None


_MAIN_IMAGE = _try_main_image()


class StaticImage:
    """A static MAIN OS image as a `read(addr, n) -> bytes` callable."""

    def __init__(self, data, base=MAIN_IMAGE_BASE):
        self.data = data
        self.base = base

    def read(self, addr, n):
        lo, hi = addr - self.base, addr - self.base + n
        if lo < 0 or hi > len(self.data):
            raise KeyError('0x%08x..0x%08x outside the static image' % (addr, addr + n))
        return self.data[lo:hi]


@unittest.skipUnless(_MAIN_IMAGE is not None,
                     'no MAIN OS image available (see emu.config.main_image)')
class PlanBTest(unittest.TestCase):
    def setUp(self):
        self.img = StaticImage(_MAIN_IMAGE)

    def test_default_plan_matches_verified_patch(self):
        writes = mp.plan_b(self.img.read, 0x40303e5c,
                            parts=('list', 'dispatch', 'group', 'name', 'rank'))
        lines = tuple('%#010x  %s -> %s' % (addr, old.hex(), new.hex())
                      for addr, old, new in writes)
        self.assertEqual(lines, GOLDEN_ALL5)

    def test_permit_raises_bound_and_relocates_mask_table(self):
        cave_b = 0x40303e5c
        writes = mp.plan_b(self.img.read, cave_b, parts=('permit',))
        self.assertEqual(len(writes), 3)

        slot = cave_b + mp.PERMIT_TABLE_OFF
        expected_new = (self.img.read(0x401fbda6, 28)
                         + self.img.read(0x401fbda6 + 24, 4))
        self.assertEqual(writes[0], (slot, b'\x00' * 32, expected_new))

        self.assertEqual(writes[1], (
            0x400dcad0,
            bytes.fromhex('41f9401fbda6'),
            bytes.fromhex('41f9') + struct.pack('>I', slot)))

        self.assertEqual(writes[2], (
            0x400dcaba, bytes.fromhex('7406'), bytes.fromhex('7407')))

    def test_all_parts_plan_ends_with_permit_writes(self):
        cave_b = 0x40303e5c
        writes = mp.plan_b(self.img.read, cave_b,
                            parts=('list', 'dispatch', 'group', 'name',
                                   'rank', 'permit'))
        self.assertEqual(len(writes), 21)
        lines = tuple('%#010x  %s -> %s' % (addr, old.hex(), new.hex())
                      for addr, old, new in writes[:18])
        self.assertEqual(lines, GOLDEN_ALL5)

        permit_writes = mp.plan_b(self.img.read, cave_b, parts=('permit',))
        self.assertEqual(writes[18:], permit_writes)

    def test_hint_fills_short_name_and_hint_accessors(self):
        cave_b = 0x40303e5c
        base = mp.plan_b(self.img.read, cave_b, parts=('name',))
        writes = mp.plan_b(self.img.read, cave_b, parts=('name', 'hint'))
        self.assertEqual(writes[:len(base)], base)

        extra = writes[len(base):]
        self.assertEqual([(a, o, n) for a, o, n in extra[:-1]], [
            (0x400dcc77, b'\x06', b'\x07'),
            (0x400dcc8a, struct.pack('>I', 0x401fbc54),
             struct.pack('>I', cave_b + 0x204)),
            (0x400dcc9d, b'\x06', b'\x07'),
            (0x400dccb0, struct.pack('>I', 0x401fbc58),
             struct.pack('>I', cave_b + 0x208)),
        ])
        last_addr, last_old, last_new = extra[-1]
        self.assertEqual(last_addr, cave_b + 0x25c)
        self.assertEqual(last_new, struct.pack('>I', 0x4022c7cb))

        with self.assertRaises(SystemExit):
            mp.plan_b(self.img.read, cave_b, parts=('hint',))

    def test_pertype_relocates_byte_table(self):
        cave_b = 0x40303e5c
        writes = mp.plan_b(self.img.read, cave_b, parts=('pertype',))
        self.assertEqual(writes[0], (
            cave_b + mp.PERTYPE_TABLE_OFF, bytes(8),
            bytes.fromhex('cdd6dfe8f100fbfb')))

        bounds_leas = (
            (0x400166fc, 0x40016702), (0x400178c6, 0x400178d0),
            (0x40017d46, 0x40017d50), (0x40017d9a, 0x40017da8),
            (0x4001709e, 0x400170a4), (0x40016628, 0x40016648))
        expected = [(
            cave_b + mp.PERTYPE_TABLE_OFF, bytes(8),
            bytes.fromhex('cdd6dfe8f100fbfb'))]
        for bound, lea in bounds_leas:
            expected.append((
                bound + 1, b'\x06', b'\x07'))
            expected.append((
                lea + 2, struct.pack('>I', 0x401d9f30),
                struct.pack('>I', cave_b + mp.PERTYPE_TABLE_OFF)))
        self.assertEqual(len(writes), 13)
        self.assertEqual(writes, expected)

    def test_clone_shims_for_slice(self):
        cave_b = 0x40303e5c
        writes = mp.plan_b(self.img.read, cave_b, parts=('clone',))
        self.assertEqual(len(writes), 12)

        self.assertEqual(writes[0], (
            cave_b + 0x300, bytes(30),
            bytes.fromhex('7206b280' '67000012' '0c8000000007' '67000008'
                          '4ef94005f1a8' '4ef94005f288')))
        self.assertEqual(writes[1], (
            0x4005f1a0, bytes.fromhex('7206b280670000e2'),
            bytes.fromhex('4ef9') + struct.pack('>I', cave_b + 0x300)
            + bytes.fromhex('4e71')))
        self.assertEqual(writes[3], (
            0x4005eeac, bytes.fromhex('7206b2806622'),
            bytes.fromhex('4ef9') + struct.pack('>I', cave_b + 0x31e)))
        self.assertEqual(writes[8][0], cave_b + 0x378)
        self.assertEqual(writes[8][2], bytes.fromhex(
            '7006b082' '67000012' '0c8200000007' '67000008'
            '4ef94005bf04' '4ef94005bef0'))
        self.assertEqual(writes[10], (
            cave_b + 0x396, bytes(34),
            bytes.fromhex('0c8000000007' '6602' '7006'
                          '72fdc0817204b280' '67000008'
                          '4ef94005d020' '4ef94005d0b6')))
        self.assertEqual(writes[11], (
            0x4005d014, bytes.fromhex('72fdc0817204b28067000098'),
            bytes.fromhex('4ef9') + struct.pack('>I', cave_b + 0x396)
            + bytes.fromhex('4e71') * 3))

    def test_clone_sites_decode_to_the_measured_1_15c_tuples(self):
        eq, mask = mp.clone_sites(self.img.read,
                                  machineprofile.DT2_115C['clone_sites'])
        self.assertEqual(eq, [
            (0x4005f1a0, '7206b280670000e2', '7206b280', 0, 0x4005f288, 0x4005f1a8),
            (0x4005eeac, '7206b2806622', '7206b280', 0, 0x4005eeb2, 0x4005eed4),
            (0x40030766, '7206b2806600016c', '7206b280', 0, 0x4003076e, 0x400308d8),
            (0x400488ec, '588fbc806622', '588fbc80', 0, 0x400488f2, 0x40048914),
            (0x4005beea, '7006b0826614', '7006b082', 2, 0x4005bef0, 0x4005bf04),
        ])
        self.assertEqual(mask, (0x4005d014, '72fdc0817204b28067000098',
                                0x4005d0b6, 0x4005d020))

    def test_clone_without_known_sites_writes_nothing(self):
        cave_b = 0x40303e5c
        spec = mp.MachineSpec(clone_of=3, fields=(0,) * 9)
        writes = mp.plan_b(self.img.read, cave_b, parts=('clone',), spec=spec)
        self.assertEqual(list(writes), [])

    def test_position_zero_puts_new_machine_first(self):
        spec = mp.MachineSpec(position=0)
        writes = mp.plan_b(self.img.read, 0x40303e5c, spec=spec)
        by_addr = {addr: new for addr, old, new in writes}

        order = list(mp.ORIGINAL_TABLE)
        order.insert(0, mp.NEW_TYPE)

        table_bytes = by_addr[0x40303e5c + mp.TABLE_B_OFF]
        self.assertEqual(struct.unpack('>8I', table_bytes), tuple(order))

        rank_bytes = by_addr[0x40303e5c + mp.RANK_TABLE_OFF]
        pairs = [struct.unpack_from('>II', rank_bytes, i * 8)
                for i in range(len(order))]
        expected_pairs = [(t, i) for i, t in enumerate(order)]
        self.assertEqual(pairs, expected_pairs)
        self.assertEqual(pairs[0], (mp.NEW_TYPE, 0))

    def test_custom_names_land_in_cave(self):
        spec = mp.spec_from_arg('Lofi:LOF:3:0')
        spec = dataclasses.replace(spec, fields=(1, 2, 3, 4, 5, 6, 7, 8, 9))
        cave_b = 0x40303e5c
        writes = mp.plan_b(self.img.read, cave_b, spec=spec)
        by_addr = {addr: new for addr, old, new in writes}

        long_addr = cave_b + mp.LONGSTR_OFF
        short_addr = cave_b + mp.SHORTSTR_OFF
        self.assertEqual(by_addr[long_addr], b'Lofi\x00')
        self.assertEqual(by_addr[short_addr], b'LOF\x00')

        lrep = by_addr[cave_b + mp.LNAME_OFF]
        srep = by_addr[cave_b + mp.SNAME_OFF]
        self.assertIn(b'LOFI', lrep)
        self.assertIn(b'LOF', srep)

        desc = by_addr[cave_b + mp.DESC_OFF]
        fields = struct.unpack('>9I', desc[8:])
        self.assertEqual(fields, (1, 2, 3, 4, 5, 6, 7, 8, 9))

    def test_rejects_bad_specs(self):
        with self.assertRaises(SystemExit):
            mp.validate_spec(mp.MachineSpec(name='x' * 16))
        with self.assertRaises(SystemExit):
            mp.validate_spec(mp.MachineSpec(desc_name='x' * 20))
        with self.assertRaises(SystemExit):
            mp.validate_spec(mp.MachineSpec(clone_of=7))
        with self.assertRaises(SystemExit):
            mp.validate_spec(mp.MachineSpec(position=8))
        with self.assertRaises(SystemExit):
            mp.validate_spec(mp.MachineSpec(fields=(1, 2, 3, 4, 5, 6, 7, 8)))
        with self.assertRaises(SystemExit):
            mp.plan_b(self.img.read, 0x40303e5c, parts=('bogus',))
        with self.assertRaises(SystemExit):
            mp.spec_from_arg('Only')

    def test_clone_other_than_6_on_static_image_needs_fields(self):
        with self.assertRaises(KeyError):
            mp.plan_b(self.img.read, 0x40303e5c, spec=mp.MachineSpec(clone_of=3))


if __name__ == '__main__':
    unittest.main()
