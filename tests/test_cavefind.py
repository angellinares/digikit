"""tools/cavefind.py against the three real images.

Skips whatever image isn't present on disk (firmware and anything derived
from it is Elektron's copyright and never committed -- see CLAUDE.md), so
this passes trivially in a checkout with no firmware and does real work in
one that has the extracted sections.

The boot-map assertions and the known-good-cave / rejected-cave assertions
pin the facts recorded by hand before this tool existed (see
tools/cavefind.py's module docstring and the HANDOVER this shipped with):
Digitakt II 1.16's reset path copies [0x40312000, 0x4031ff60) to 0x80000000
and zero-fills [0x40312000, 0x47e28470); 1.15C's equivalents are
[0x402fa000, 0x40307f60) and [0x402fa000, 0x47e0f2c0). 1.16 has a known-good
2108-byte cave at 0x403117c4; 0x4031041c (referenced as a stored pointer)
and eight 0xFF runs (each referenced once) are known-bad and must not
appear.
"""
import os
import sys
import time
import unittest

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, REPO)
sys.path.insert(0, os.path.join(REPO, 'tools'))

import cavefind as cf  # noqa: E402

IMAGES = {
    'dt2-1.16': os.path.join(REPO, 'out', 'sections', 'dt2-1.16', 'section_3_MAIN_OS.bin'),
    'dt2-1.15C': os.path.join(REPO, 'out', 'sections', 'dt2-1.15C', 'section_3_MAIN_OS.bin'),
    'dn2-1.11': os.path.join(REPO, 'out', 'sections', 'dn2-1.11', 'section_3_MAIN_OS.bin'),
}

# Known-bad candidates on 1.16, hand-verified before this tool existed: one
# pointer-literal run and eight 0xFF runs, each excluded for a reference
# found somewhere in [start-64, start+size).
DT2_116_REJECTED_STARTS = (
    0x4031041c,             # 3045 zero bytes, referenced as a stored pointer
    0x402e5f5c, 0x402c5390, 0x402eec7c, 0x402f03dc,
    0x402faddc, 0x4030025c, 0x40305384, 0x403075d8,
)


def _load(key):
    path = IMAGES[key]
    if not os.path.exists(path):
        return None
    with open(path, 'rb') as f:
        return f.read()


def _run(key, **kwargs):
    img = _load(key)
    bmap = cf.boot_map(img)
    candidates = cf.find_candidates(
        img, cf.LOAD_ADDR, bmap['survive_start'], bmap['survive_end'],
        kwargs.get('min_size', 256))
    kept, rejected = [], []
    for c in candidates:
        kept_range, evidence = cf.apply_exclusions(c, img, cf.LOAD_ADDR)
        if kept_range is None:
            rejected.append(dict(c, evidence=evidence))
            continue
        start, end = kept_range
        kept.append(dict(c, start=start, end=end, size=end - start, evidence=evidence))
    return img, bmap, kept, rejected


class TestBootMap(unittest.TestCase):
    def test_dt2_116(self):
        img = _load('dt2-1.16')
        if img is None:
            self.skipTest('dt2-1.16 image not present')
        bmap = cf.boot_map(img)
        self.assertEqual(bmap['copy_ranges'], [(0x40312000, 0x4031ff60)])
        self.assertEqual(bmap['clear_start'], 0x40312000)
        self.assertEqual(bmap['clear_end'], 0x47e28470)
        self.assertEqual(bmap['survive_start'], cf.LOAD_ADDR)
        self.assertEqual(bmap['survive_end'], 0x40312000)

    def test_dt2_115c(self):
        img = _load('dt2-1.15C')
        if img is None:
            self.skipTest('dt2-1.15C image not present')
        bmap = cf.boot_map(img)
        self.assertEqual(bmap['copy_ranges'], [(0x402fa000, 0x40307f60)])
        self.assertEqual(bmap['clear_start'], 0x402fa000)
        self.assertEqual(bmap['clear_end'], 0x47e0f2c0)

    def test_dn2_111_resolves(self):
        # DN2 1.11's reset path was not hand-verified byte-for-byte before
        # this tool existed (unlike the two Digitakt images above), so this
        # only pins that the masked signature finds it at all -- a
        # regression here means the signature stopped covering a build it
        # used to.
        img = _load('dn2-1.11')
        if img is None:
            self.skipTest('dn2-1.11 image not present')
        bmap = cf.boot_map(img)
        self.assertLess(bmap['survive_start'], bmap['survive_end'])
        self.assertEqual(bmap['clear_start'], bmap['copy_ranges'][0][0])


class TestCandidates(unittest.TestCase):
    def test_dt2_116_known_good_cave_present(self):
        img = _load('dt2-1.16')
        if img is None:
            self.skipTest('dt2-1.16 image not present')
        _, _, kept, _ = _run('dt2-1.16')
        matches = [c for c in kept if c['start'] == 0x403117c4 and c['end'] == 0x40312000]
        self.assertEqual(len(matches), 1, 'expected exactly one kept candidate at 0x403117c4-0x40312000, got %r'
                          % [(hex(c['start']), hex(c['end'])) for c in kept])
        self.assertEqual(matches[0]['size'], 2108)
        self.assertEqual(matches[0]['fill'], 0x00)

    def test_dt2_116_rejects_known_bad_candidates(self):
        img = _load('dt2-1.16')
        if img is None:
            self.skipTest('dt2-1.16 image not present')
        _, _, kept, rejected = _run('dt2-1.16')
        kept_starts = {c['start'] for c in kept}
        rejected_starts = {c['start'] for c in rejected}
        for start in DT2_116_REJECTED_STARTS:
            self.assertNotIn(start, kept_starts, '0x%08x should have been excluded, not kept' % start)
            self.assertIn(start, rejected_starts,
                          '0x%08x should appear as a raw candidate (and then be rejected); '
                          'it is missing from the byte-run scan entirely' % start)

    def test_min_size_filters_small_runs(self):
        img = _load('dt2-1.16')
        if img is None:
            self.skipTest('dt2-1.16 image not present')
        _, _, kept_small, _ = _run('dt2-1.16', min_size=32)
        _, _, kept_default, _ = _run('dt2-1.16', min_size=256)
        self.assertGreaterEqual(len(kept_small), len(kept_default))
        self.assertTrue(all(c['size'] >= 256 for c in kept_default))


class TestControlWrites(unittest.TestCase):
    def test_dt2_116_cacr_acr0(self):
        img = _load('dt2-1.16')
        if img is None:
            self.skipTest('dt2-1.16 image not present')
        writes = cf.find_control_writes(img)
        by_reg = {w['reg']: w for w in writes}
        self.assertIn('CACR', by_reg)
        self.assertIn('ACR0', by_reg)
        self.assertEqual(by_reg['CACR']['value'], 0xa50ce100)
        self.assertEqual(by_reg['ACR0']['value'], 0x4007e020)
        acr0 = cf.decode_acrn(by_reg['ACR0']['value'])
        self.assertEqual(acr0['region'], (0x40000000, 0x48000000))
        self.assertFalse(acr0['W_write_protected'])
        cacr = cf.decode_cacr(by_reg['CACR']['value'])
        self.assertTrue(cacr['DEC'])
        self.assertTrue(cacr['IEC'])
        self.assertTrue(cacr['DCINVA'])
        self.assertTrue(cacr['ICINVA'])

    def test_same_write_on_all_three_images(self):
        # BSP-level bring-up code, not application code -- expected
        # byte-identical (same address, same values) on every build.
        addrs = set()
        for key in IMAGES:
            img = _load(key)
            if img is None:
                continue
            writes = cf.find_control_writes(img)
            by_reg = {w['reg']: w['value'] for w in writes}
            self.assertEqual(by_reg.get('CACR'), 0xa50ce100)
            self.assertEqual(by_reg.get('ACR0'), 0x4007e020)
            addrs.add(tuple(w['addr'] for w in writes))
        if len(addrs) > 1:
            self.fail('control-register write addresses differ between images: %r' % addrs)


class TestPerformance(unittest.TestCase):
    def test_runtime_under_two_seconds_per_image(self):
        ran = False
        for key in IMAGES:
            img = _load(key)
            if img is None:
                continue
            ran = True
            t0 = time.time()
            bmap = cf.boot_map(img)
            candidates = cf.find_candidates(img, cf.LOAD_ADDR, bmap['survive_start'],
                                            bmap['survive_end'], 256)
            for c in candidates:
                cf.apply_exclusions(c, img, cf.LOAD_ADDR)
            cf.find_control_writes(img)
            elapsed = time.time() - t0
            self.assertLess(elapsed, 2.0, '%s took %.2fs, want < 2s' % (key, elapsed))
        if not ran:
            self.skipTest('no images present')


if __name__ == '__main__':
    unittest.main()
