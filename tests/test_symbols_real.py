"""Local image-pinned resolver checks for the two DT2 firmware builds."""

import unittest
from pathlib import Path

from emu import symbols


# The pinned per-version extracts, not the shared sections/ directory, which
# holds whichever firmware was extracted last.
DT15 = Path("out/sections/dt2-1.15C/section_3_MAIN_OS.bin")
DT16 = Path("out/sections/dt2-1.16/section_3_MAIN_OS.bin")


@unittest.skipUnless(DT15.exists() and DT16.exists(), "DT2 1.15C/1.16 images absent")
class DigitaktResolverTest(unittest.TestCase):
    def test_scheduler_uart8_and_ssi0_symbols_resolve_in_both_images(self):
        expected = {
            DT15: (0x4000044A, 0x47D9ADB4, 0x4094CD74, 0x4000221C, 0x4002CCC2),
            DT16: (0x4000044A, 0x47DB3F64, 0x40964D74, 0x4000221C, 0x4002D36A),
        }
        for path, values in expected.items():
            with self.subTest(path=path):
                profile = symbols.resolve(path.read_bytes())
                self.assertEqual(
                    (
                        profile.ctx_switch_load,
                        profile.current_tcb,
                        profile.uart8_tx_state,
                        profile.uart8_tx_wait,
                        profile.ssi0_dma_force_rte,
                    ),
                    values,
                )

    def test_display_semaphore_resolves_in_both_images(self):
        # Unresolved on 1.16, the display semaphore was faked and the
        # "INITIALIZING +DRIVE..." screen starved the job worker.
        expected = {
            DT15: (0x40125F4E, 0x44E2D148),
            DT16: (0x4013352A, 0x44E460D8),
        }
        for path, values in expected.items():
            with self.subTest(path=path):
                profile = symbols.resolve(path.read_bytes())
                self.assertEqual((profile.display_frame_post, profile.display_sem), values)
                self.assertNotEqual(profile.display_sem, profile.frame_sem)

    def test_worker_done_semaphore_resolves_in_both_images(self):
        # display_sem+8: a plain software completion semaphore a background
        # worker (e.g. the "Factory reset" BgWorker that formats +Drive)
        # gives on finishing. Confirmed by scanning both images for the
        # distinct "pea IMM32; jsr sem_pend; addq.l #4,sp; rts" wrapper
        # shape, which appears exactly twice per image: once at
        # frame_sem+8 (the intro's own analogous park) and once here.
        # `unblock` force-satisfying this one let a caller proceed ~250M
        # instructions before the real completion on 1.16, and the frozen
        # "FACTORY PROJECT >> +DRIVE..." screen followed once the worker's
        # job-pump later parked for real at `pump_wait`.
        expected = {
            DT15: 0x44E2D150,
            DT16: 0x44E460E0,
        }
        for path, values in expected.items():
            with self.subTest(path=path):
                profile = symbols.resolve(path.read_bytes())
                self.assertEqual(profile.worker_done_sem, values)
                self.assertEqual(profile.worker_done_sem, profile.display_sem + 8)


if __name__ == "__main__":
    unittest.main()
