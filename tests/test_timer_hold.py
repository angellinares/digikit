"""Held timer ticks, the PIT prescaler, rescaling, and the idle-loop skip.

Each test runs a real Unicorn machine: the write-1-to-clear of PIF and REF
and the idle loop's hook only exist as guest-visible behaviour.
"""

import struct
import unittest
from fractions import Fraction

from unicorn import UC_HOOK_CODE
from unicorn.m68k_const import UC_M68K_REG_A7, UC_M68K_REG_PC, UC_M68K_REG_SR

from emu import dtim, pit
from emu.harness import VBR, Machine
from emu.longrun import IdleSpin, spin
from emu.ssi import Ssi0Dma

CODE = 0x40002000  # guest writes, each at a fresh address
HANDLER = 0x40001800
STACK = 0x40010000
INTC0, INTC2 = 0xFC048000, 0xFC050000


def machine():
    m = Machine()
    m.ensure(0x40000000)
    m.ensure(0xFC080000)  # the 1 MB page holding the INTCs, DTIMs and PITs
    # Supervisor, IPL 0, before A7: setting S swaps stack pointers. Reading
    # SR before anything has written it aborts the process in this Unicorn
    # build.
    m.uc.reg_write(UC_M68K_REG_SR, 0x2000)
    m.uc.reg_write(UC_M68K_REG_A7, STACK)
    m.uc.reg_write(UC_M68K_REG_PC, 0x40001000)
    return m


class GuestWriter:
    def __init__(self, m):
        self.m, self.pc = m, CODE

    def _run(self, code):
        # Unicorn keeps running an old translation of rewritten code, so
        # every write gets its own address.
        self.pc += 0x10
        self.m.uc.mem_write(self.pc, code + b"\x4e\x71")
        pc = self.m.uc.reg_read(UC_M68K_REG_PC)
        self.m.uc.emu_start(self.pc, 0, count=1)
        self.m.uc.reg_write(UC_M68K_REG_PC, pc)

    def word(self, address, value):  # move.w #value,(address).l
        self._run(struct.pack(">HHI", 0x33FC, value, address))

    def byte(self, address, value):  # move.b #value,(address).l
        self._run(struct.pack(">HHI", 0x13FC, value, address))


def ipl(m, level):
    m.uc.reg_write(UC_M68K_REG_SR, 0x2000 | (level << 8))


class PrescalerTest(unittest.TestCase):
    def test_pre_zero_divides_by_one(self):
        # The firmware's 1 us delay at 0x40136310: PCSR 0x0033, PMR 131.
        m = machine()
        m.uc.mem_write(pit.BASES[1], struct.pack(">HH", 0x0033 | pit.PIE, 131))
        pits = pit.Pits(m, channels=(1,), instr_per_sec=1_000_000)
        self.assertAlmostEqual(pits.period(1), 1.0)

    def test_rtos_tick_is_100_hz(self):
        # PIT0 as 0x40001290 programs it: PCSR 0x053f, PMR 41249.
        m = machine()
        m.uc.mem_write(pit.BASES[0], struct.pack(">HH", 0x053F, 41249))
        pits = pit.Pits(m, channels=(0,), instr_per_sec=pit.F_BUS)
        self.assertEqual(pits.period(0), pit.F_BUS / 100)


class PitHoldTest(unittest.TestCase):
    PERIOD = 64 * 1001  # PRE 6, PMR 1000, one instruction per bus cycle

    def setUp(self):
        m = self.m = machine()
        for ch, source in ((2, 15), (3, 16)):
            m.uc.mem_write(INTC2 + pit.ICR_BASE + source, b"\x03")
            m.uc.mem_write(VBR + pit.VECTORS[ch] * 4, struct.pack(">I", HANDLER))
        m.uc.mem_write(pit.BASES[2], struct.pack(">HH", 0x0609, 1000))
        self.pits = pit.Pits(m, channels=(3, 2, 0), instr_per_sec=pit.F_BUS)
        self.guest = GuestWriter(m)
        self.assertEqual(self.pits.deadline(0), self.PERIOD)

    def test_refused_tick_is_taken_when_the_ipl_drops(self):
        ipl(self.m, 7)
        self.pits.service(self.PERIOD)
        self.assertEqual(self.pits.pending, {2})
        self.assertEqual(self.pits.fired[2], 0)
        ipl(self.m, 0)
        self.pits.service(self.PERIOD + 10)
        self.assertEqual(self.pits.pending, set())
        self.assertEqual(self.pits.fired[2], 1)
        self.assertEqual(self.m.uc.reg_read(UC_M68K_REG_PC), HANDLER)
        self.assertEqual((self.m.uc.reg_read(UC_M68K_REG_SR) >> 8) & 7, 3)

    def test_tick_due_while_pending_is_lost(self):
        ipl(self.m, 7)
        self.pits.service(self.PERIOD)
        self.pits.service(2 * self.PERIOD)
        self.assertEqual(self.pits.pending, {2})
        self.assertEqual(self.pits.missed[2], 1)

    def test_guest_write_one_to_pif_clears_the_pending_tick(self):
        ipl(self.m, 7)
        self.pits.service(self.PERIOD)
        self.guest.word(pit.BASES[2], 0x0609)  # PIF written 0: no effect
        self.assertEqual(self.pits.pending, {2})
        self.guest.word(pit.BASES[2], 0x0609 | pit.PIF)
        self.assertEqual(self.pits.pending, set())
        self.assertEqual(self.pits.cleared[2], 1)
        ipl(self.m, 0)
        self.pits.service(self.PERIOD + 10)
        self.assertEqual(self.pits.fired[2], 0)

    def test_masked_tick_waits_for_the_unmask(self):
        imrl = INTC2 + pit.IMR_BASE + 4
        self.m.uc.mem_write(imrl, struct.pack(">I", 1 << 15))
        self.pits.service(self.PERIOD)
        self.assertEqual(self.pits.pending, {2})
        self.m.uc.mem_write(imrl, struct.pack(">I", 0))
        self.pits.service(self.PERIOD + 10)
        self.assertEqual(self.pits.fired[2], 1)

    def test_switching_the_timer_off_drops_the_pending_tick(self):
        ipl(self.m, 7)
        self.pits.service(self.PERIOD)
        self.guest.word(pit.BASES[2], 0x0000)
        ipl(self.m, 0)
        self.pits.service(self.PERIOD + 10)
        self.assertEqual(self.pits.pending, set())
        self.assertEqual(self.pits.fired[2], 0)

    def test_same_level_collision_takes_both_in_intc_order(self):
        # PIT3 is INTC2 source 16, PIT2 source 15: the higher source first,
        # and the other at the first boundary after its handler returns.
        self.m.uc.mem_write(pit.BASES[3], struct.pack(">HH", 0x0609, 1000))
        self.pits.invalidate()
        self.pits.next[3] = self.PERIOD
        self.pits.service(self.PERIOD)
        self.assertEqual(dict(self.pits.fired), {3: 1})
        self.assertEqual(self.pits.pending, {2})
        ipl(self.m, 0)  # the PIT3 handler's rte
        self.pits.service(self.PERIOD + 10)
        self.assertEqual(dict(self.pits.fired), {3: 1, 2: 1})

    def test_rescale_keeps_each_deadline_in_device_time(self):
        self.pits.now = 1000
        self.pits.rescale(2 * pit.F_BUS)
        self.assertEqual(self.pits.next[2], 1000 + 2 * (self.PERIOD - 1000))
        self.assertEqual(self.pits.ips, 2 * pit.F_BUS)
        self.assertEqual(self.pits.period(2), 2 * self.PERIOD)


class DtimHoldTest(unittest.TestCase):
    PERIOD = 1000 * 16  # DTRR 999, bus clock / 16, no prescale

    def setUp(self):
        m = self.m = machine()
        m.uc.mem_write(INTC0 + pit.ICR_BASE + 35, b"\x02")
        m.uc.mem_write(VBR + dtim.VECTORS[3] * 4, struct.pack(">I", HANDLER))
        base = dtim.BASES[3]
        m.uc.mem_write(base + dtim.DTRR, struct.pack(">I", 999))
        m.uc.mem_write(base + dtim.DTMR, struct.pack(">H", 0x001D))
        self.dtims = dtim.Dtims(
            m, channels=(3,), instr_per_sec=pit.F_BUS, clear_stale=False
        )
        self.guest = GuestWriter(m)
        self.assertEqual(self.dtims.deadline(0), self.PERIOD)

    def test_refused_tick_is_held_and_ref_cleared_by_the_guest(self):
        ipl(self.m, 7)
        self.dtims.service(self.PERIOD)
        self.assertEqual(self.dtims.pending, {3})
        dter = dtim.BASES[3] + dtim.DTER
        self.assertEqual(self.m.uc.mem_read(dter, 1)[0] & dtim.REF, dtim.REF)
        self.guest.byte(dter, dtim.REF)
        self.assertEqual(self.dtims.pending, set())
        self.assertEqual(self.dtims.cleared[3], 1)

    def test_refused_tick_is_taken_when_the_ipl_drops(self):
        ipl(self.m, 7)
        self.dtims.service(self.PERIOD)
        ipl(self.m, 1)
        self.dtims.service(self.PERIOD + 10)
        self.assertEqual(self.dtims.fired[3], 1)
        self.assertEqual(self.m.uc.reg_read(UC_M68K_REG_PC), HANDLER)


class Ssi0RescaleTest(unittest.TestCase):
    def test_next_request_keeps_its_device_time(self):
        source = Ssi0Dma(machine(), request_hz=96_000, instr_per_sec=4_680_000)
        source.now = 1000
        source.next = Fraction(1000) + Fraction(4_680_000, 96_000)
        source.rescale(9_360_000)
        self.assertEqual(source.next, Fraction(1000) + Fraction(9_360_000, 96_000))
        self.assertEqual(source.ips, 9_360_000)


IDLE, YIELD, COUNTER = 0x40001000, 0x40001100, 0x40003000


def idle_machine(skip, every=5):
    """A CPU in `bra.b *` whose reschedule handler counts in memory."""
    m = machine()
    m.install_exceptions()  # implements `rte`
    m.uc.mem_write(IDLE, b"\x60\xfe")
    # addq.l #1,(COUNTER).l; rte
    m.uc.mem_write(YIELD, struct.pack(">HI", 0x52B9, COUNTER) + b"\x4e\x73")
    m.uc.mem_write(VBR + 32 * 4, struct.pack(">I", YIELD))
    ipl(m, 0)
    idle = IdleSpin(m, [IDLE], None, every)
    idle.skip_enabled = skip
    m.uc.hook_add(UC_HOOK_CODE, idle.on_spin, begin=IDLE, end=IDLE)
    m._idle = idle
    return m, idle


def guest_state(m, idle):
    regs = [m.uc.reg_read(r) for r in (UC_M68K_REG_PC, UC_M68K_REG_A7)]
    sr = m.uc.reg_read(UC_M68K_REG_SR) & 0xFF00
    return regs, sr, bytes(m.uc.mem_read(COUNTER, 4)), idle.count["n"]


class IdleSkipTest(unittest.TestCase):
    def test_skip_stops_one_pass_short_of_the_reschedule(self):
        _m, idle = idle_machine(True, every=5)
        self.assertEqual(idle.skip(100), 4)
        self.assertEqual(idle.count["n"], 4)
        idle.count["n"] = 5
        self.assertEqual(idle.skip(3), 3)

    def test_skipping_matches_running_the_loop(self):
        # Steps of 7 instructions, a reschedule every 50 passes: most steps
        # start in the loop, and some end inside the reschedule handler.
        runs = []
        for skip in (False, True):
            m, idle = idle_machine(skip, every=50)
            _pc, done, stop = spin(m, IDLE, 1000, chunk=7)
            runs.append((guest_state(m, idle), done, stop))
            if skip:
                self.assertGreater(idle.skipped, 500)
        self.assertEqual(runs[0], runs[1])
        self.assertGreater(struct.unpack(">I", runs[0][0][2])[0], 10)

    def test_a_queued_uart_completion_disables_the_skip(self):
        m, idle = idle_machine(True)
        idle.tx = type("Tx", (), {"pending": 1, "deliver": lambda self: False})()
        self.assertEqual(idle.skip(100), 0)

    def test_fast_stepping_skips_and_clears_its_stop_flag(self):
        m, idle = idle_machine(True)
        spin(m, IDLE, 200, chunk=7, fast=True)
        self.assertGreater(idle.skipped, 0)
        self.assertFalse(idle.stop_on_entry)


if __name__ == "__main__":
    unittest.main()
