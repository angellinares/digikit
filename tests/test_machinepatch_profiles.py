"""`tools/machinepatch.py` planning against a profile, not against 1.15C.

`tests/test_machinepatch_plan.py` pins the exact 18 writes `plan_b` produces on
Digitakt II 1.15C, and that golden is what proves the profile refactor changed
no behaviour on the image the tool was built against. This file covers the
other half: that a second image plans with *its own* addresses.

The property worth testing is not a second golden blob -- it is that nothing
from one image leaks into another's plan. A hard-coded address that survives
the refactor still produces a plausible-looking 18-write plan; it just writes
1.15C addresses into a 1.16 image. `test_trampoline_carries_its_own_descriptor`
is the case that caught exactly that, in `build_trampoline`, which sits outside
`plan_b` and so was missed by a first pass that only threaded `plan_b`'s body.
"""

import os
import struct
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))), 'tools'))

import machinepatch as mp
import machineprofile as prof

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
IMAGES = {
    '6a6a887b0573a557b71badf32cd9392777c60b4d1f33dfae12bb8346a014a37b':
        os.path.join(REPO, 'sections', 'section_3_MAIN_OS.bin'),
    '57bb4dfa8df07d846adc72fdb4fb0d3cd3c5680c524bf498338460207e008e7d':
        os.path.join(REPO, 'out', 'sections', 'dt2-1.16', 'section_3_MAIN_OS.bin'),
}
# The five parts that do not need anchors Digitone II lacks.
PARTS = ('list', 'dispatch', 'group', 'name', 'rank')

# Every constant this module used to hard-code to 1.15C, and which Anchors must
# reproduce exactly or the golden in test_machinepatch_plan.py is meaningless.
LEGACY = (
    'DISPATCH', 'DISPATCH_WANT', 'END_SITE', 'END_WANT', 'START_SITE',
    'START_WANT', 'TABLE_D_LO', 'TABLE_D_HI', 'GROUP_ADDR', 'GROUP_WANT',
    'GROUP_NEW', 'NAME_ADDR', 'NAME_HEAD_WANT', 'NAME_LEA_ADDR',
    'NAME_LEA_WANT', 'NAME_TABLE_SRC', 'NAME_TABLE_ROWS',
    'NAME_TABLE_ROW_BYTES', 'RANK_CALL', 'RANK_CALL_WANT', 'RANK_INSERT',
    'RANK_GUARD', 'PERMIT_BOUND_ADDR', 'PERMIT_BOUND_WANT', 'PERMIT_LEA_ADDR',
    'PERMIT_LEA_WANT', 'PERMIT_TABLE_SRC', 'PERTYPE_TABLE_SRC',
    'PERTYPE_SITES', 'LABEL_FUNCS', 'DESCRIPTOR_BASE', 'DESCRIPTOR_STRIDE',
    'FALLBACK_DESCRIPTOR', 'DEFAULT_CAVE', 'DEFAULT_CAVE_B', 'NEW_TYPE',
)


def available():
    """-> [(sha, profile, image path)] for the images actually on disk."""
    found = []
    for sha, path in IMAGES.items():
        if os.path.exists(path) and prof.sha256_file(path) == sha:
            found.append((sha, prof.PROFILES[sha], path))
    return found


AVAILABLE = available()


class AnchorsTest(unittest.TestCase):
    def test_default_profile_reproduces_the_old_constants(self):
        a = mp.anchors_for()
        for name in LEGACY:
            self.assertEqual(getattr(a, name), getattr(mp, name),
                             '%s differs from the module constant' % name)

    def test_new_type_follows_the_machine_count(self):
        for profile in (prof.DT2_115C, prof.DT2_116, prof.DN2_111):
            a = mp.anchors_for(profile)
            self.assertEqual(a.NEW_TYPE, profile['machine_count'])
        self.assertEqual(mp.anchors_for(prof.DN2_111).count, 5)

    def test_digitone_refuses_the_parts_it_has_no_anchors_for(self):
        a = mp.anchors_for(prof.DN2_111)
        for part, attrs in (('rank', ('RANK_CALL', 'RANK_INSERT')),
                            ('permit', ('PERMIT_BOUND_ADDR', 'PERMIT_LEA_ADDR')),
                            ('pertype', ('PERTYPE_TABLE_SRC',)),
                            ('group', ('GROUP_ADDR',))):
            with self.assertRaises(SystemExit) as caught:
                a.require(part, *attrs)
            self.assertIn('Digitone II 1.11', str(caught.exception))
            self.assertIn(part, str(caught.exception))

    def test_a_profile_that_disagrees_with_its_own_checks_is_rejected(self):
        broken = dict(prof.DT2_115C)
        broken['dispatch'] = prof.DT2_115C['dispatch'] + 2
        with self.assertRaises(SystemExit):
            mp.anchors_for(broken)


@unittest.skipUnless(AVAILABLE, 'no MAIN OS image with a machine profile on disk')
class PlanPerImageTest(unittest.TestCase):
    def test_byte_checks_pass(self):
        for sha, profile, path in AVAILABLE:
            with self.subTest(profile['name']):
                failures, checked = prof.verify(profile, prof.image_reader(path))
                self.assertTrue(checked, 'profile defines no byte checks')
                self.assertEqual(failures, [])

    def test_plan_is_eighteen_writes(self):
        for sha, profile, path in AVAILABLE:
            with self.subTest(profile['name']):
                writes = mp.plan_b(prof.image_reader(path), profile['cave_b'],
                                   parts=PARTS, profile=profile)
                self.assertEqual(len(writes), 18)

    def test_trampoline_carries_its_own_descriptor(self):
        """No other image's descriptor array may appear in this one's plan."""
        others = [p for p in prof.PROFILES.values()]
        for sha, profile, path in AVAILABLE:
            with self.subTest(profile['name']):
                writes = mp.plan_b(prof.image_reader(path), profile['cave_b'],
                                   parts=PARTS, profile=profile)
                trampoline = writes[0][2]
                for key in ('descriptor_base', 'fallback_descriptor'):
                    self.assertIn(struct.pack('>I', profile[key]), trampoline,
                                  '%s missing from the trampoline' % key)
                    for other in others:
                        if other is profile or other.get(key) is None:
                            continue
                        self.assertNotIn(
                            struct.pack('>I', other[key]), trampoline,
                            "%s's %s leaked into %s's trampoline"
                            % (other['name'], key, profile['name']))

    def test_every_patch_site_belongs_to_this_image(self):
        """A write either lands in the cave or at an anchor of this profile."""
        for sha, profile, path in AVAILABLE:
            with self.subTest(profile['name']):
                a = mp.anchors_for(profile)
                cave = profile['cave_b']
                writes = mp.plan_b(prof.image_reader(path), cave,
                                   parts=PARTS, profile=profile)
                anchors = {getattr(a, n) for n in LEGACY
                           if isinstance(getattr(a, n, None), int)}
                for addr, old, new in writes:
                    if cave <= addr < cave + 0x400:
                        continue
                    near = any(base <= addr <= base + 0x20 for base in anchors)
                    self.assertTrue(
                        near, '%#010x is neither in the cave nor near an anchor '
                        'of %s' % (addr, profile['name']))

    def test_clone_plan_matches_1_16_sites(self):
        for sha, profile, path in AVAILABLE:
            if profile is not prof.DT2_116:
                continue
            with self.subTest(profile['name']):
                writes = mp.plan_b(prof.image_reader(path), profile['cave_b'],
                                   parts=('clone',), profile=profile)
                self.assertEqual(len(writes), 12)
                cave = profile['cave_b']
                sites = {addr for addr, old, new in writes
                         if not cave <= addr < cave + 0x400}
                expected = {site for _f, site in profile['clone_sites']}
                self.assertEqual(sites, expected)

    def test_dn2_clone_plan_writes_nothing(self):
        spec = mp.MachineSpec(clone_of=0, fields=(0,) * 9, position=0)
        writes = mp.plan_b(lambda addr, n: bytes(n), prof.DN2_111['cave_b'],
                           parts=('clone',), spec=spec, profile=prof.DN2_111)
        self.assertEqual(list(writes), [])

    def test_the_plan_reads_only_this_image(self):
        """`read` outside the image raises, so a stray 1.15C address on 1.16
        would fault rather than silently produce a plausible plan."""
        for sha, profile, path in AVAILABLE:
            with self.subTest(profile['name']):
                seen = []
                base = prof.image_reader(path)

                def watched(addr, n):
                    seen.append(addr)
                    return base(addr, n)

                mp.plan_b(watched, profile['cave_b'], parts=PARTS,
                          profile=profile)
                self.assertTrue(seen)


if __name__ == '__main__':
    unittest.main()
