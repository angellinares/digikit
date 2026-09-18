"""Local image-pinned resolver checks for the two DT2 firmware builds."""

import unittest
from pathlib import Path

from emu import symbols


DT15 = Path("sections/section_3_MAIN_OS.bin")
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


if __name__ == "__main__":
    unittest.main()
