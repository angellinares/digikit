"""Opt-in regression: emu.dspboot.run's Esdhc gets a real sd_dma_sem.

`emu.dspboot.run`'s `Esdhc(...)` construction passed `cmd_sem`/`data_sem`
but omitted `dma_sem` entirely, unlike `emu.longrun.build`'s own
construction of the same class. `Esdhc._post(None)` is a silent no-op (see
its own docstring), so any multi-block CMD25 write issued during the
*initial* cold-boot pass (the only place `emu.dspboot.run` is used) left
the guest's own `sem_pend(sd_dma_sem)` inside `FUN_4012e0c0` blocked
forever -- confirmed live: this is exactly where the +Drive-mount boot
task hung once an earlier fix (the eMMC-identity whitelist, see
docs/findings/14-plus-drive-format.md) let it reach that code at all.

This only checks the wiring (a real, uncommitted firmware image is needed
even to construct the Machine), not the full write-completes-and-posts
behavior -- that's already covered, at the `Esdhc` model level, by
`tests/test_esdhc.py`'s `test_cmd25_consumes_host_buffer_and_is_visible_to_cmd18`.
"""

import os
import unittest


@unittest.skipUnless(
    os.environ.get("DT2_SYX"), "set DT2_SYX=Digitakt_II_OS1.16.syx to run this"
)
class DspbootEsdhcDmaSemWiringTest(unittest.TestCase):
    def test_dma_sem_is_wired(self):
        import emu.dspboot as db
        from emu import config, symbols

        with open(config.main_image(), "rb") as fh:
            main_img = fh.read()
        profile = symbols.resolve(main_img)
        self.assertIsNotNone(
            profile.sd_dma_sem, "profile has no sd_dma_sem to wire up at all"
        )

        m, st, stop = db.run(
            config.firmware(None),
            main_img,
            limit=1,
            sdgate=True,
            esdhc=True,
        )
        self.assertEqual(m.esdhc.dma_sem, profile.sd_dma_sem)
