"""Opt-in regression: the "MaGj" factory-table record
`tools/plusdrive.py` writes at sector 0x458000 passes DT2 1.16's own
validator (`FUN_4002ccd0`), which gates `FUN_4015a450` (the +Drive mount)
succeeding past its superblock check -- see
docs/findings/14-plus-drive-format.md.

Uses the same bounded, isolated-call technique as
`tests/test_esdhc_identity.py`: `FUN_4002ccd0` only reads ordinary RAM (the
record this test writes there itself, standing in for the CMD18 read
`FUN_4012deda` would otherwise do) and calls a handful of pure-computation
helpers (`FUN_4013e06c`'s CRC-32), so a minimal `emu.harness.Machine` with
just the MAIN_OS image and that one RAM buffer mapped is enough.
"""

import os
import unittest

import tools.plusdrive as pd

_MAIN_IMG_BASE = 0x40000400
FUN_4002ccd0 = 0x4002CCD0
RECORD_RAM_ADDR = 0x47E203CC  # DAT_47e203cc, the buffer FUN_4002ccd0 reads


@unittest.skipUnless(
    os.environ.get("DT2_SYX"), "set DT2_SYX=Digitakt_II_OS1.16.syx to run this"
)
class FactoryTableRecordTest(unittest.TestCase):
    def test_record_passes_the_real_validator(self):
        from emu import config, harness

        with open(config.main_image(), "rb") as fh:
            image = fh.read()

        m = harness.Machine()
        page = harness.PAGE
        end = _MAIN_IMG_BASE + len(image)
        for base in range(_MAIN_IMG_BASE & ~(page - 1), end, page):
            m.ensure(base)
        m.uc.mem_write(_MAIN_IMG_BASE, image)

        record = pd.build_factory_table_record()
        base = RECORD_RAM_ADDR & ~(page - 1)
        for off in range(0, len(record), page):
            m.ensure(base + off)
        m.uc.mem_write(RECORD_RAM_ADDR, record)

        # FUN_4002ccd0(param_1) -- param_1 is an optional output pointer;
        # pass 0 (NULL), matching the "don't care" case in its own
        # decompile (`if (param_1 != 0) { ... }`).
        d0 = harness.call(m, FUN_4002ccd0, [0], limit=2_000_000)
        self.assertEqual(
            d0,
            1,
            "FUN_4002ccd0 rejected the built factory-table record (D0=0x%08x); "
            "FUN_4015a450 (mount) would then never succeed" % d0,
        )


if __name__ == "__main__":
    unittest.main()
