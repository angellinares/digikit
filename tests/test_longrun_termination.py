# pyright: reportMissingImports=false
"""Termination sentinels must not be counted as completed work."""

import unittest

from emu.harness import Machine
from emu.longrun import run_until, spin


class _ZeroAfterStartUc:
    """Minimal Unicorn surface whose first emu_start returns the zero sentinel."""

    def __init__(self):
        self.pc = 0x40000000
        self.starts = 0

    def emu_start(self, pc, until, count=0):
        self.starts += 1
        self.pc = 0

    def reg_read(self, register):
        return self.pc


class _ZeroAfterStartMachine:
    def __init__(self):
        self.uc = _ZeroAfterStartUc()
        self.halt_vec = None


class _CountingUc:
    def __init__(self):
        self.pc = 0x40000000
        self.counts = []

    def emu_start(self, pc, until, count=0):
        self.counts.append(count)

    def reg_read(self, register):
        return self.pc


class _CountingMachine:
    def __init__(self):
        self.uc = _CountingUc()
        self.halt_vec = None


class _Timers:
    def __init__(self):
        self.now = 0
        self.steps = 0
        self.services = 0

    def step(self, done, remaining):
        self.steps += 1
        return 10

    def service(self, done):
        self.services += 1


class _Event:
    def __init__(self):
        self.next = 5
        self.services = []

    def step(self, done, remaining):
        return max(1, self.next - done)

    def service(self, done):
        self.services.append(done)
        if done >= self.next:
            self.next += 5


class LongrunTerminationTest(unittest.TestCase):
    def test_spin_pc_zero_credits_no_step_and_does_not_service_timers(self):
        timers = _Timers()
        pc, executed, stop = spin(Machine(), 0, 100, pits=timers)
        self.assertEqual((pc, executed, stop), (0, 0, "pc zero"))
        self.assertEqual(timers.steps, 0)
        self.assertEqual(timers.services, 0)

    def test_run_until_reports_pc_zero_sentinel(self):
        self.assertEqual(run_until(Machine(), 0), (0, "pc zero"))

    def test_spin_zero_returned_after_emu_start_credits_no_step_or_timer_service(self):
        timers = _Timers()
        machine = _ZeroAfterStartMachine()
        pc, executed, stop = spin(machine, machine.uc.pc, 100, pits=timers)
        self.assertEqual((pc, executed, stop), (0, 0, "pc zero"))
        self.assertEqual(machine.uc.starts, 1)
        self.assertEqual(timers.steps, 1)
        self.assertEqual(timers.services, 0)

    def test_async_source_subdivides_only_at_its_exact_deadlines(self):
        timers = _Timers()
        event = _Event()
        machine = _CountingMachine()

        pc, executed, stop = spin(
            machine, machine.uc.pc, 12, pits=timers, async_events=(event,)
        )

        self.assertEqual((pc, executed, stop), (machine.uc.pc, 15, "limit"))
        self.assertEqual(machine.uc.counts, [5, 5, 5])
        self.assertEqual(event.services, [5, 10, 15])


if __name__ == "__main__":
    unittest.main()
