"""Stepping of the SSI0 model and the timers: exact clock, coalescing, caches.

The eager model puts an `emu_start` boundary at every SSI0 request; these
tests pin what the faster paths must keep equal to it. Synthetic machines
only, except where a real Unicorn is needed to fire a guest-write hook.
"""

# pyright: reportMissingImports=false

import math
import random
import struct
import unittest
from fractions import Fraction

from unicorn.m68k_const import UC_M68K_REG_SR

from emu.edma import (
    ATTR,
    BITER,
    CITER,
    CSR,
    DADDR,
    DLAST,
    DOFF,
    NBYTES,
    SADDR,
    SLAST,
    SOFF,
    TCD_BASE,
)
from emu.ssi import (
    COALESCE_MAX_REQUESTS,
    RX_CHAN,
    RX_REGISTER,
    TX_CHAN,
    TX_REGISTER,
    RxHandoverPeer,
    Ssi0Dma,
)

RX_BANKS, TX_BANKS = (0x5000, 0x5800), (0x6000, 0x6800)
RX_SG, TX_SG = (0x3000, 0x3020), (0x3040, 0x3060)


class FakeUc:
    """Byte-addressed memory, registers and a record of installed hooks."""

    def __init__(self):
        self.memory = {}
        self.regs = {}
        self.hooks = {}
        self._next_handle = 1

    def hook_add(self, hook_type, callback, user_data=None, begin=1, end=0):
        handle = self._next_handle
        self._next_handle += 1
        self.hooks[handle] = (hook_type, callback, begin, end)
        return handle

    def hook_del(self, handle):
        del self.hooks[handle]

    def mem_read(self, address, size):
        return bytes(self.memory.get(address + i, 0) for i in range(size))

    def mem_write(self, address, data):
        for i, value in enumerate(data):
            self.memory[address + i] = value

    def reg_read(self, register):
        return self.regs.get(register, 0)

    def reg_write(self, register, value):
        self.regs[register] = value

    def guest_access(self, hook_type, address, size=4):
        """Fire every hook of `hook_type` whose range covers `address`."""
        for kind, callback, begin, end in list(self.hooks.values()):
            if kind & hook_type and begin <= address <= end:
                callback(self, hook_type, address, size, 0, None)


class FakeMachine:
    def __init__(self):
        self.uc = FakeUc()
        self.vectors = []
        self.now = 0

    def raise_vector(self, vector, level=None):
        self.vectors.append((self.now, vector, level))
        return True


def tcd(source, dest, soff, doff, link, csr, citer=64):
    raw = bytearray(0x20)
    struct.pack_into(">I", raw, SADDR, source)
    struct.pack_into(">H", raw, ATTR, 0x0202)
    struct.pack_into(">h", raw, SOFF, soff)
    struct.pack_into(">I", raw, NBYTES, 0x20)
    struct.pack_into(">i", raw, SLAST, 0)
    struct.pack_into(">I", raw, DADDR, dest)
    struct.pack_into(">H", raw, CITER, citer)
    struct.pack_into(">h", raw, DOFF, doff)
    struct.pack_into(">I", raw, DLAST, link)
    struct.pack_into(">H", raw, BITER, citer)
    struct.pack_into(">H", raw, CSR, csr)
    return bytes(raw)


class RecordingHandoverPeer(RxHandoverPeer):
    """The DT2 peer, logging every call; it takes the block path."""

    def __init__(self, machine):
        super().__init__(machine)
        self.log = []

    def rx_major(self, nbytes, major_start):
        data = super().rx_major(nbytes, major_start)
        self.log.append(("rx", data))
        return data

    def tx(self, data):
        self.log.append(("tx", data))


class RecordingPlainPeer:
    """Same answers without `rx_major`, which forces request-by-request."""

    def __init__(self, machine):
        self.inner = RxHandoverPeer(machine)
        self.log = []

    def rx(self, nbytes):
        data = self.inner.rx(nbytes)
        self.log.append(("rx", data))
        return data

    def tx(self, data):
        self.log.append(("tx", data))


def audio_machine(
    coalesce, request_hz=96_000, ips=4_680_000, citer=64, peer=RxHandoverPeer
):
    """The DT2 shape: RX and TX ping-pong through two scatter/gather banks,
    TX interrupting at each major loop, a marker-supplying RX peer."""
    machine = FakeMachine()
    uc = machine.uc
    uc.mem_write(0xFC04C06A, b"\x06")  # vector 170: INTC1 source 42, level 6
    uc.mem_write(0xFC04C07F, b"\x05")  # vector 191: INTC1 source 63, level 5
    rx = [tcd(RX_REGISTER, bank, 0, 4, 0, 0x10, citer) for bank in RX_BANKS]
    tx = [tcd(bank, TX_REGISTER, 4, 0, 0, 0x12, citer) for bank in TX_BANKS]
    for descriptors, pointers in ((rx, RX_SG), (tx, TX_SG)):
        for i, descriptor in enumerate(descriptors):
            linked = bytearray(descriptor)
            struct.pack_into(">I", linked, DLAST, pointers[(i + 1) % 2])
            uc.mem_write(pointers[i], linked)
    uc.mem_write(TCD_BASE + RX_CHAN * 0x20, uc.mem_read(RX_SG[0], 0x20))
    uc.mem_write(TCD_BASE + TX_CHAN * 0x20, uc.mem_read(TX_SG[0], 0x20))
    for bank in TX_BANKS:
        uc.mem_write(bank, bytes((bank + i) & 0xFF for i in range(0x800)))
    source = Ssi0Dma(
        machine,
        request_hz=request_hz,
        instr_per_sec=ips,
        peer=peer(machine),
        coalesce=coalesce,
    )
    source.arm_legacy()
    return machine, source


def drive(machine, source, until, deadlines=(), ipl=lambda done: 0, done=0):
    """spin()'s loop: step to the nearest deadline, run, service. -> done."""
    while done < until:
        step = source.step(done)
        for deadline in deadlines:
            if deadline > done:
                step = min(step, deadline - done)
        done += step
        machine.now = done
        machine.uc.reg_write(UC_M68K_REG_SR, ipl(done) << 8)
        source.service(done)
    return done


class FractionClock:
    """The pre-integer Ssi0Dma deadline arithmetic, kept as the reference."""

    def __init__(self, ips, request_hz, now):
        self.period = Fraction(ips, request_hz)
        self.next = Fraction(now) + self.period

    def step(self, done):
        return max(1, math.ceil(self.next - done))

    def service(self, done):
        if done >= self.next:
            self.next += self.period
            if self.next <= done:
                self.next = Fraction(done) + self.period


class IntegerClockTest(unittest.TestCase):
    def test_matches_fraction_reference_including_overshoot(self):
        rng = random.Random(1)
        for request_hz, ips in (
            (96_000, 4_680_000),
            (48_000, 4_680_000),
            (44_100, 1_000_003),
        ):
            source = Ssi0Dma(FakeMachine(), request_hz=request_hz, instr_per_sec=ips)
            source.enabled.update((RX_CHAN, TX_CHAN))  # zero TCDs: minors no-op
            source.align(12_345)
            reference = FractionClock(ips, request_hz, 12_345)
            done = 12_345
            for _ in range(3000):
                step = source.step(done)
                self.assertEqual(step, reference.step(done))
                # Mostly exact landings; now and then an overshoot of up to
                # several periods, which drops the backlog.
                done += step + (rng.randrange(400) if rng.random() < 0.05 else 0)
                source.service(done)
                reference.service(done)
                self.assertEqual(source.next, reference.next)

    def test_next_round_trips_and_rejects_off_grid_values(self):
        source = Ssi0Dma(FakeMachine(), request_hz=96_000, instr_per_sec=4_680_000)
        source.next = Fraction(4875, 100)
        self.assertEqual(source.next, Fraction(195, 4))
        source.next = None
        self.assertIsNone(source.next)
        with self.assertRaisesRegex(ValueError, "not a multiple"):
            source.next = Fraction(1, 7)


class CoalesceTest(unittest.TestCase):
    def assert_same_run(self, eager, coalesced):
        (em, es), (cm, cs) = eager, coalesced
        self.assertEqual(em.uc.memory, cm.uc.memory)
        self.assertEqual(em.vectors, cm.vectors)
        for name in (
            "requests",
            "major_loops",
            "scatter_gathers",
            "tx_bytes",
            "tx_crc32",
        ):
            self.assertEqual(getattr(es, name), getattr(cs, name), name)
        self.assertEqual(es.next, cs.next)

    def test_coalesced_run_matches_eager_run(self):
        eager, coalesced = audio_machine(False), audio_machine(True)
        # Timer deadlines that are not request boundaries cut spans short.
        deadlines = (1_000, 77_777, 150_001)
        end = drive(*coalesced, 200_000, deadlines)
        self.assertEqual(drive(*eager, end, deadlines), end)
        self.assert_same_run(eager, coalesced)
        self.assertGreater(coalesced[1].coalesced, 0)
        self.assertEqual(coalesced[1].coalesce_violations, {})
        self.assertGreater(len(eager[0].vectors), 50)

    def test_peer_sees_the_same_calls_in_the_same_order(self):
        for peer in (RecordingHandoverPeer, RecordingPlainPeer):
            eager = audio_machine(False, peer=peer)
            coalesced = audio_machine(True, peer=peer)
            end = drive(*coalesced, 20_000, (7_001,))
            self.assertEqual(drive(*eager, end, (7_001,)), end)
            self.assert_same_run(eager, coalesced)
            self.assertEqual(eager[1].peer.log, coalesced[1].peer.log, peer.__name__)
            self.assertGreater(len(eager[1].peer.log), 400)

    def test_refused_vector_170_is_retried_request_by_request(self):
        # IPL 7 blocks vector 170 for a window after each major loop; the
        # boundary at which it is finally taken must not move.
        def ipl(done):
            return 7 if done % 3120 < 700 else 0

        eager, coalesced = audio_machine(False), audio_machine(True)
        end = drive(*coalesced, 100_000, ipl=ipl)
        self.assertEqual(drive(*eager, end, ipl=ipl), end)
        self.assert_same_run(eager, coalesced)
        late = [v for v in eager[0].vectors if v[0] % 3120 >= 700]
        self.assertTrue(late, "the IPL window never deferred a delivery")

    def test_span_runs_to_the_next_major_loop(self):
        machine, source = audio_machine(True)
        step = source.step(0)
        # 64 requests of 48.75 instructions: the 64th is due at 3120.
        self.assertEqual(step, 3120)
        self.assertEqual(COALESCE_MAX_REQUESTS, 64)
        machine.uc.reg_write(UC_M68K_REG_SR, 0x2700)  # refuses vector 170
        source.service(step)  # closes the span; the major loop asserts 170
        self.assertTrue(source.int50_asserted)
        self.assertFalse(source.int50_delivered)
        self.assertEqual(source.step(step), 49)  # 65 * 48.75 = 3168.75

    def test_guest_access_counts_only_when_a_boundary_was_skipped(self):
        from unicorn import UC_HOOK_MEM_READ, UC_HOOK_MEM_WRITE

        machine, source = audio_machine(True)
        source.step(0)
        machine.uc.guest_access(UC_HOOK_MEM_READ, RX_BANKS[0] + 0x40)
        source.service(40)  # before the first request: nothing was skipped
        self.assertEqual(source.coalesce_violations, {})
        self.assertEqual(source.requests, 0)

        source.step(40)
        machine.uc.guest_access(UC_HOOK_MEM_READ, RX_BANKS[0] + 0x40)
        machine.uc.guest_access(UC_HOOK_MEM_WRITE, TX_BANKS[0] + 0x7FC)
        machine.uc.guest_access(UC_HOOK_MEM_WRITE, TCD_BASE + TX_CHAN * 0x20 + CITER)
        machine.uc.guest_access(UC_HOOK_MEM_WRITE, TX_BANKS[1])  # other bank
        source.service(1000)  # 20 requests came due, 19 boundaries skipped
        self.assertEqual(source.requests, 20)
        self.assertEqual(dict(source.coalesce_violations), {"rx": 1, "tx": 1, "tcd": 1})
        # Span hooks are removed at the span's end; the TCD hooks stay.
        kinds = sorted((b, e) for _t, _c, b, e in machine.uc.hooks.values())
        self.assertNotIn((RX_BANKS[0], RX_BANKS[0] + 0x7FF), kinds)

    def test_only_reads_of_advancing_tcd_fields_count(self):
        from unicorn import UC_HOOK_MEM_READ

        machine, source = audio_machine(True)
        base = TCD_BASE + TX_CHAN * 0x20
        source.step(0)
        for offset in (NBYTES, SLAST, DOFF, DLAST, BITER, CSR):  # static
            machine.uc.guest_access(UC_HOOK_MEM_READ, base + offset, 2)
        machine.uc.guest_access(UC_HOOK_MEM_READ, base + CITER, 2)
        machine.uc.guest_access(UC_HOOK_MEM_READ, base + SADDR, 4)
        source.service(3120)
        self.assertEqual(dict(source.coalesce_violations), {"tcd": 2})

    def test_timer_arming_inside_a_span_is_counted(self):
        class Timer:
            transitions = 0

        machine, source = audio_machine(True)
        timer = Timer()
        source.watch = (timer,)
        source.step(0)
        timer.transitions += 1  # the timers noticed a guest write at 3120
        source.service(3120)
        source.step(3120)
        self.assertEqual(source.coalesce_violations["timers"], 1)

    def test_checking_off_installs_no_span_hooks(self):
        machine, source = audio_machine(True)
        source.coalesce_check = False
        before = len(machine.uc.hooks)
        source.step(0)
        self.assertEqual(len(machine.uc.hooks), before)
        source.service(3120)
        self.assertEqual(source.requests, 64)


class TimerCacheTest(unittest.TestCase):
    """Cached periods must follow guest writes, which need a real Unicorn."""

    CODE = 0x40000000

    def machine(self):
        from emu.harness import Machine

        m = Machine()
        m.ensure(self.CODE)
        m.ensure(0xFC080000)
        return m

    def guest_write16(self, m, address, value):
        # move.w #value,(address).l, each at a fresh address: Unicorn keeps
        # running an old translation of code rewritten from the host.
        self.pc = getattr(self, "pc", self.CODE) + 0x10
        code = struct.pack(">HHI", 0x33FC, value, address)
        m.uc.mem_write(self.pc, code + b"\x4e\x71")
        m.uc.emu_start(self.pc, 0, count=1)

    def test_pit_period_follows_guest_writes(self):
        from emu import pit

        m = self.machine()
        base = pit.BASES[2]
        m.uc.mem_write(base, struct.pack(">HH", 0x0209, 0x1000))  # EN|PIE, PMR
        pits = pit.Pits(m, channels=(2,))
        self.assertEqual(pits._period(2), pits.period(2))
        first = pits.deadline(0)
        self.assertEqual(pits.transitions, 1)

        self.guest_write16(m, base + 2, 0x2000)  # PMR
        self.assertEqual(pits._period(2), pits.period(2))
        self.assertNotEqual(pits._period(2), first)

        self.guest_write16(m, base, 0x0000)  # switched off
        self.assertIsNone(pits._period(2))
        self.assertIsNone(pits.deadline(10))
        self.assertEqual(pits.transitions, 2)

        # A host write is invisible to the hook until invalidated.
        m.uc.mem_write(base, struct.pack(">H", 0x0209))
        self.assertIsNone(pits._period(2))
        pits.invalidate(2)
        self.assertEqual(pits._period(2), pits.period(2))

    def test_dtim_period_follows_guest_writes(self):
        from emu import dtim

        m = self.machine()
        base = dtim.BASES[3]
        m.uc.mem_write(base + dtim.DTRR, struct.pack(">I", 999))
        m.uc.mem_write(base + dtim.DTMR, struct.pack(">H", 0x001D))
        timers = dtim.Dtims(m, channels=(3,), clear_stale=False)
        self.assertEqual(timers._period(3), timers.period(3))
        self.assertIsNotNone(timers._period(3))

        self.guest_write16(m, base + dtim.DTMR, 0x0000)
        self.assertIsNone(timers._period(3))
        self.guest_write16(m, base + dtim.DTRR + 2, 0x0063)  # DTRR low half
        self.guest_write16(m, base + dtim.DTMR, 0x001D)
        self.assertEqual(timers._period(3), timers.period(3))

    def test_clear_stale_drops_the_cache(self):
        from emu import dtim

        m = self.machine()
        base = dtim.BASES[3]
        m.uc.mem_write(base + dtim.DTRR, struct.pack(">I", 999))
        m.uc.mem_write(base + dtim.DTMR, struct.pack(">H", 0x001D))
        timers = dtim.Dtims(m, channels=(3,))  # clear_stale stops it
        self.assertEqual(timers.stale, [3])
        self.assertIsNone(timers._period(3))


if __name__ == "__main__":
    unittest.main()
