"""The instruction time base shared by emu.pit, emu.dtim and emu.ssi."""

import struct
import unittest

from emu import dtim, pit


def machine():
    from emu.harness import Machine

    m = Machine()
    m.ensure(0x40000000)
    m.ensure(0xFC080000)
    return m


class TimeBaseConstantsTest(unittest.TestCase):
    def test_core_clock_is_twice_the_bus_clock(self):
        # MCF5441XRM Figure 8-1 note 4 and Eqn. 8-5: the internal bus clock
        # is fsys/2. The firmware's own bus constant is 132,000,000.
        self.assertEqual(pit.F_BUS, 132_000_000)
        self.assertEqual(pit.F_SYS, 2 * pit.F_BUS)

    def test_default_rate_is_still_the_legacy_one(self):
        self.assertEqual(pit.INSTR_PER_SEC_LEGACY, 4_680_000)
        self.assertEqual(pit.INSTR_PER_SEC, pit.INSTR_PER_SEC_LEGACY)

    def test_device_estimate_lies_between_the_audio_floor_and_the_core_clock(self):
        # The vector-191 audio handler executes ~33,900 instructions per
        # 1,500 Hz block, so a device slower than ~51M instructions a second
        # could not run it; a V4 core does not exceed one instruction per
        # core cycle by much.
        self.assertGreater(pit.DEVICE_INSTR_PER_SEC, 33_900 * 1_500)
        self.assertLessEqual(pit.DEVICE_INSTR_PER_SEC, pit.F_SYS)


class RateChangeTest(unittest.TestCase):
    """Setting `ips` after construction must rescale the cached periods.

    guirun's --ips-at and the GUI's post-intro rate change set `source.ips`
    on live timers; a period cached at the old rate would otherwise survive
    until the guest next writes that timer's registers.
    """

    def test_pit_period_follows_ips(self):
        m = machine()
        m.uc.mem_write(pit.BASES[2], struct.pack(">HH", 0x0609, 0x4323))
        pits = pit.Pits(m, channels=(2,), instr_per_sec=4_680_000)
        before = pits._period(2)
        pits.ips = 4 * 4_680_000
        self.assertEqual(pits._period(2), pits.period(2))
        self.assertEqual(pits._period(2), 4 * before)

    def test_dtim_period_follows_ips(self):
        m = machine()
        base = dtim.BASES[3]
        m.uc.mem_write(base + dtim.DTRR, struct.pack(">I", 0x43238))
        m.uc.mem_write(base + dtim.DTMR, struct.pack(">H", 0x001D))
        timers = dtim.Dtims(
            m, channels=(3,), instr_per_sec=4_680_000, clear_stale=False
        )
        before = timers._period(3)
        timers.ips = 132_000_000
        self.assertEqual(timers._period(3), timers.period(3))
        self.assertAlmostEqual(timers._period(3), before * 132_000_000 / 4_680_000)

    def test_unchanged_ips_keeps_the_cache(self):
        m = machine()
        m.uc.mem_write(pit.BASES[2], struct.pack(">HH", 0x0609, 0x4323))
        pits = pit.Pits(m, channels=(2,))
        pits._period(2)
        pits.ips = pits.ips
        self.assertIsNot(pits._periods[2], pit._STALE)


if __name__ == "__main__":
    unittest.main()
