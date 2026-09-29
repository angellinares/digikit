# pyright: reportMissingImports=false
# fmt: off
"""The four programmable interval timers, gated on their own enable bits.

After the intro the system is correctly idle: every task blocks and nothing
delivers the interrupts that would wake them. The one that matters is PIT2.
Its ISR (vector 207, `0x40002a18`) does nothing but ack the timer and post
semaphore `0x47d9ade0`, which is the tick of the RTOS software-timer wheel.
The wheel task at `0x40002a46` then walks a callback list at `0x4094cdb8`,
firing each entry whose mask matches a rolling counter -- mask 1 every tick,
mask 4 every fourth, mask 0x80 every 128th. Seven callbacks are registered and
all seven point at real code, including `0x4012651e` in the display module.
That list is the OS heartbeat, and without PIT2 nothing turns it.

Rates come from the firmware's own registers rather than being hardcoded:

    prescaler = 1 << ((PCSR >> 8) & 0xF)
    period    = prescaler * (PMR + 1) bus cycles, at 132 MHz

That is MCF5441XRM Eqn. 38-1 and Table 38-3 (PRE = 0000 divides by 2^0 = 1),
and the firmware's own 1 us delay at `0x40136310` agrees: it programs PIT1
with PRE = 0 and PMR = 131 and counts PIF events, so one event is
(131 + 1) x 2^0 = 132 bus cycles = 1.000 us only under this formula. The
rates it gives: PIT0 10.0000 ms (100 Hz), PIT2 8.3336 ms (119.996 Hz), PIT3
66.6686 ms (14.9996 Hz) for the display and 33.3343 ms (29.999 Hz) for the
intro. An earlier `2^(PRE+1)` halved all of them; that they still landed on
round numbers is why round rates cannot tell the two formulas apart.

**PIT0 matters as much as PIT2, for a different reason.** It is the RTOS's
time-slice timer: the context switcher at `0x40000410` re-arms it on every
switch (`move.w #$53f,$fc080000`) and unmasks its INTC source (`and.l
#$ffffdfff,$fc050014` clears IMRL bit 13, source 13 = vector 205), and vector
205's handler *is* the switcher. Delivering PIT2 without PIT0 gives the RTOS
timer-wheel ticks while denying it preemption, and the result is not merely
slower -- it is unstable. Measured over 60M instructions from
`postintro.snap`, PIT2 alone crashed or aborted at five of six chunk sizes
tried; adding PIT0 removed every abort and every spurious exception, and left
faults at only two. So both are on by default.

**PIT3 is the display frame timer, and it is what the OS is waiting for.**
The intro switches PIT3 off on its way out, so it looks dead after the intro
and was left out of `channels` for a long time. It is not dead: the display
module's start routine at `0x40126004` re-points vector 208 from the intro's
handler to its own at `0x40125f3c`, reprograms PMR to `0x4323` and sets
EN|PIE, and that handler does one thing -- ack PIT3 and post semaphore
`0x44e2d148`. The prio-6 display task at `0x4012606a` pends on exactly that
semaphore. Deliver PIT0 and PIT2 but not PIT3 and the task blocks forever on
a semaphore nothing in the system ever posts: measured over 60M instructions
from `postintro.snap`, the task ran once, pended once, and never woke.
Adding PIT3 takes the same run from 2 spawned tasks to 6, runs the display
geometry setter at `0x40125f6a` eleven times, and reaches the first
`px_copy_to_bitmap`.

Delivery is gated on PCSR bit 0 (enable) and bit 3 (interrupt enable), so the
intro switching PIT3 off with its own `move.w d0,$fc08c000` stops frame
delivery without the emulator being told, and the display module switching
PIT3 back on at `0x40126004` starts it again. It is gated on the interrupt
controller too -- the source's ICR level, and its mask bit, both of which the
context switcher manipulates. And it is gated on the CPU's IPL: an interrupt
whose level is not above the current IPL cannot be taken.

**A refused tick is held, not dropped.** The hardware sets PIF when the
counter reaches zero and keeps the request asserted until the handler
write-1-clears PIF (MCF5441XRM Table 38-3), so a tick the IPL or the INTC
mask refuses is taken as soon as they allow. `service` does the same: a due
tick sets `pending`, and every boundary offers the pending channels until
the CPU takes them. A tick that comes due while its channel is still
pending is lost, as on the hardware (PIF is already set), and is counted in
`missed`. A guest write of 1 to PIF clears a pending tick (`cleared`): the
RTOS context switcher writes `0x053f` to PIT0's PCSR on every switch, which
acknowledges a time slice nobody took. A channel switched off drops its
pending tick too. The request can only be taken at an `emu_start` boundary,
so it waits for the first one after the IPL drops: about 10 us of device
time with the SSI0 model on, up to the next timer deadline without it.

Levels come from the INTC, not from us: vector 205 is INTC2 source 13, and
`0xFC050040 + source` reads 1 for PIT0, 3 for PIT2 and 3 for PIT3.

Pacing is an instruction count, not real time. `INSTR_PER_SEC` is 4.68M, from
the measured 312k instructions per intro frame when the intro was thought to
run at 15 fps (it runs at 30). It is a proxy, and only as good as the
assumption that instruction rate tracks wall clock -- which it does not
while a task spins. Good enough to turn the RTOS over; not cycle accuracy,
and it should not be described as such. It is not the device's rate either:
see `DEVICE_INSTR_PER_SEC` below. emu/gui.py and tools/guirun.py switch to
that rate when the intro hands over; instruction-budgeted tools keep this
one, because their budgets were calibrated on it.

**Deliver from a chunk boundary, and re-read PC afterwards.** `raise_vector`
moves PC, so a caller that captured PC before servicing must refresh it, or
emulation resumes at the interrupted address with an exception frame stranded
on the stack. The next `rts` then pops that frame as a return address and
jumps to nowhere -- observed as a jump to `0x033C2004` and a vector-4 fault
exactly one tick after the first. `emu/longrun.py:spin` does this correctly;
a hand-rolled loop is where it goes wrong.
"""
import collections
import math
import struct

from unicorn import UC_HOOK_MEM_WRITE
from unicorn.m68k_const import UC_M68K_REG_SR

BASES   = (0xFC080000, 0xFC084000, 0xFC088000, 0xFC08C000)
VECTORS = (205, 206, 207, 208)
EN, PIF, PIE = 0x01, 0x04, 0x08       # PCSR bits 0, 2 and 3
F_BUS = 132_000_000
# The core clock. MCF5441XRM Figure 8-1 note 4 and Eqn. 8-5 fix the internal
# bus clock at fsys/2, so the firmware's 132 MHz bus constant means a 264 MHz
# core (above the manual's 250 MHz rating). The PLL is set by the reset
# configuration: MAIN OS never writes PLL_CR/PLL_DR (0xFC0C0000-0C).
F_SYS = 2 * F_BUS

# The default pacing rate every timer here converts device time with. 4.68M
# was chosen from ~312k emulated instructions per intro frame; it is not the
# device's rate, and in the audio-running state it starves the guest: one
# 1,500 Hz audio block is 3,120 instructions at this rate, and the
# vector-191 handler alone executes ~33,900 per block. Measured from
# drive3/loaded.snap with the SSI0 model: at 4.68M and 18.72M the CPU is at
# IPL 5 or above at every boundary and no PIT or DTIM tick is taken.
INSTR_PER_SEC_LEGACY = 4_680_000
INSTR_PER_SEC = INSTR_PER_SEC_LEGACY

# Estimated device rate: F_SYS at 2 cycles per instruction. The MCF5441XRM
# section 3.3.5 timing tables give 1.18 cycles per instruction for the
# firmware's own executed mix with zero-wait memory, so the device cannot
# exceed ~224M; cache misses to DDR2, branch mispredictions and pipeline
# stalls are not modelled, and 2.5 cycles would give ~106M. The audio
# handler's ~51M instructions a second set a hard floor. At this rate, from
# drive3/loaded.snap with the SSI0 model, the CPU is at IPL 5 or above at 37%
# of boundaries and every PIT and DTIM tick is taken (PIT0 100 Hz, PIT2
# 120 Hz, DTIM3 30 Hz, vector 191 1,500 a second). The GUI front ends use it
# after the intro; pass it explicitly (`instr_per_sec=`, `--ips`) elsewhere.
DEVICE_INSTR_PER_SEC = F_SYS // 2

# Each interrupt controller owns 64 vectors: INTC0 64-127, INTC1 128-191,
# INTC2 192-255. Per source there is an ICR byte at +0x40+source whose low
# three bits are the level, and a mask bit in IMRH/IMRL at +0x08/+0x0C.
INTC = ((0xFC048000, 64), (0xFC04C000, 128), (0xFC050000, 192))
ICR_BASE, IMR_BASE = 0x40, 0x08

# How far to run in one go when every timer is switched off and there is no
# deadline to aim at. Only the pacing of a dead clock depends on it.
IDLE_STEP = 1_000_000

PIT3_VECTOR_SLOT = 0x40000340     # VBR + 208*4, not build-specific

# `_STALE` marks a cached period that must be re-read from the registers.
_STALE = object()


def watch_writes(m, base, length, on_write):
    """Call `on_write()` on any guest write that can touch `length` bytes
    at `base`. -> the Unicorn hook handle.

    The hook runs before the store lands; callers only use it to drop a
    cache that is re-read at the next `emu_start` boundary, after it.
    Unicorn matches a write hook on the access's START address only, so an
    unaligned write that begins up to three bytes below `base` and spills
    into it would slip past a hook on the register range itself; the range
    is widened by three below to catch that. Host `mem_write`s never fire a
    hook: code that writes these registers from Python must drop the cache
    itself (`Pits.invalidate`, `Dtims.invalidate`).
    """
    return m.uc.hook_add(UC_HOOK_MEM_WRITE,
                         lambda uc, t, a, s, v, d: on_write(),
                         begin=base - 3, end=base + length - 1)


def interrupt_level(m, vec, respect_mask=True):
    """Return a vector's programmed level, or None if disabled/masked."""
    for base, first in INTC:
        if first <= vec < first + 64:
            src = vec - first
            break
    else:
        return None
    icr = m.uc.mem_read(base + ICR_BASE + src, 1)[0] & 0x07
    if not icr:
        return None
    if not respect_mask:
        return icr
    imrh, imrl = struct.unpack('>II', m.uc.mem_read(base + IMR_BASE, 8))
    masked = (imrl >> src) & 1 if src < 32 else (imrh >> (src - 32)) & 1
    return None if masked else icr


def intro_running(m, intro_isr):
    """-> True while the intro still owns PIT3.

    The intro paces its own frames off PIT3 (vector 208) and switches the
    timer off on its way out; the display module then claims the same vector
    for itself. So "vector 208 still points at the intro's handler and PIT3
    is enabled" is exactly the window in which the intro is live, and it
    distinguishes `boot400M.snap` (enabled) from `postintro.snap` (switched
    off) without either being told apart by name.

    `intro_isr` is the build's own intro PIT3 handler, resolved as
    `profile.intro_pit3_isr` in emu/symbols.py -- it is NOT hardcoded here.
    Hardcoding Digitakt's address was a real bug: on Digitone it made this
    return False for the whole intro, so the GUI released the timers into
    the middle of a running intro. If `intro_isr` did not resolve, this
    returns False, since there is then no way to tell.
    """
    if intro_isr is None:
        return False
    try:
        slot = struct.unpack('>I', m.uc.mem_read(PIT3_VECTOR_SLOT, 4))[0]
        pcsr = struct.unpack('>H', m.uc.mem_read(BASES[3], 2))[0]
    except Exception:
        return False
    return slot == intro_isr and bool(pcsr & EN)


class Pits:
    """Deliver PIT interrupts on an instruction-count clock.

    `channels` defaults to PIT3, PIT2 and PIT0: the display frame timer, the
    timer-wheel tick, and the time slice. Delivering only some of them is
    measurably worse than delivering all -- see the module docstring.
    Listing a channel does not deliver it: `period` returns None while the
    firmware has the timer switched off, so an unused channel costs two
    memory reads per step and nothing else.

    **The order is the INTC's priority within a level.** `service` offers
    the pending channels in this order, and the first one taken raises IPL
    to its own level, which refuses any later one at the same level until
    its handler returns. Within one level the INTC serves the higher source
    number first (MCF5441XRM section 17.3.1, Table 17-19), and PIT3, PIT2
    and PIT0 are INTC2 sources 16, 15 and 13, so `(3, 2, 0)` is the
    hardware's order.

    The order used to decide which timer starved. Before ticks were held, a
    refused tick was dropped, and PIT3's period was exactly eight times
    PIT2's, so once their deadlines lined up they collided on the same
    instruction for ever at the same level: from `boot400M.snap` over 250M
    instructions, `(0, 2, 3)` took PIT3 none of 281 times and `(3, 2, 0)`
    took it 130 times, at the cost of PIT2's misses rising from 29 to 160.
    With `pending`, the loser of a collision is taken at a later boundary,
    so the order now only decides which of the two runs first. (An earlier
    attempt at holding ticks retried only at timer deadlines and halted on
    an unhandled vector at 58.7M instructions. This one also drops a pending
    tick on the guest's write-1-to-clear, and `tools/guirun.py --exact`
    from `dt2-1.16/boot400M.snap` runs 500M instructions without a fault.)

    **Do not deliver anything while the intro is still running.** The intro is
    driven by `unblock` force-satisfying its frame semaphore, and a real PIT3
    tick posts that same semaphore a second time: the draw loop then never
    reaches its exit test and the intro never ends. PIT2 is worse in a quieter
    way -- the intro exits, but the six OS tasks that should spawn afterwards
    never do. Measured from `boot400M.snap` over 90M instructions: channels
    `()` and `(0,)` both reach six tasks, and `(2,)`, `(3,)`, `(0, 2)` and
    `(0, 2, 3)` all reach zero. Construct with `hold=intro_running(m, isr)` and
    call `release()` from a hook on the intro's exit point, `profile.intro_done`.
    """

    def __init__(self, m, channels=(3, 2, 0), instr_per_sec=INSTR_PER_SEC,
                 hold=False):
        self.m = m
        self.channels = tuple(channels)
        self.held = bool(hold)
        self.ips = instr_per_sec
        self.next = [None] * 4
        # Instruction count at the last step, and the epoch a fresh `spin`
        # resumes from. Deadlines in `self.next` are absolute counts measured
        # against it, so a caller that makes repeated short `spin` calls with
        # one `Pits` keeps a continuous clock instead of restarting it.
        self.now = 0
        self.fired = collections.Counter()
        # A tick that came due while its channel was still pending (PIF
        # already set), and a pending tick the guest cleared; see the
        # module docstring, "A refused tick is held, not dropped".
        self.missed = collections.Counter()
        self.cleared = collections.Counter()
        self.pending = set()
        # Times a channel went from armed to off or back, which happens at
        # the first boundary after the guest's write -- see emu.ssi's
        # coalescing, which watches it.
        self.transitions = 0
        # `period(ch)` per channel, re-read only after a guest write to that
        # channel's PCSR/PMR. `deadline` and `service` run at every
        # `emu_start` boundary (96,000 a guest second with the SSI0 model
        # on), and re-reading three channels' registers twice per boundary
        # was a large share of the whole emulator's time.
        self._periods = [_STALE] * 4
        for ch in self.channels:
            watch_writes(m, BASES[ch], 4,
                         (lambda c: lambda: self.invalidate(c))(ch))
            m.uc.hook_add(UC_HOOK_MEM_WRITE,
                          (lambda c: lambda uc, t, a, s, v, d:
                           self._on_pcsr(c, a, s, v))(ch),
                          begin=BASES[ch] - 3, end=BASES[ch] + 1)

    def _on_pcsr(self, ch, address, size, value):
        # PIF is PCSR bit 2, in the byte at BASES[ch] + 1; writing 1 clears it.
        shift = address + size - 1 - (BASES[ch] + 1)
        if (0 <= shift < size and (value >> (8 * shift)) & PIF
                and ch in self.pending):
            self.pending.discard(ch)
            self.cleared[ch] += 1

    @property
    def ips(self):
        """Instructions per second of device time."""
        return self._ips

    @ips.setter
    def ips(self, value):
        # Cached periods are in instructions, so a new rate makes them stale.
        if getattr(self, '_ips', None) != value:
            self._ips = value
            self._periods = [_STALE] * 4

    def rescale(self, ips):
        """Change the time base, keeping each deadline's device time.

        Setting `ips` alone changes the period from the next tick on and
        leaves the deadlines already armed at the old rate. This scales the
        time left to each one, which a checkpoint saved at another rate
        needs.
        """
        _rescale(self, ips)

    def invalidate(self, ch=None):
        """Drop the cached period of `ch` (every channel if None).

        Guest writes do this through a hook; only a host-side write to a
        PIT register needs to call it.
        """
        if ch is None:
            self._periods = [_STALE] * 4
        else:
            self._periods[ch] = _STALE

    def _period(self, ch):
        p = self._periods[ch]
        if p is _STALE:
            p = self._periods[ch] = self.period(ch)
        return p

    def release(self):
        """Start delivering. Safe to call more than once."""
        self.held = False

    def checkpoint_state(self):
        return {'type': 'Pits', 'version': 1, 'channels': self.channels,
                'ips': self.ips, 'next': list(self.next), 'now': self.now,
                'held': self.held, 'fired': dict(self.fired),
                'missed': dict(self.missed), 'cleared': dict(self.cleared),
                'pending': sorted(self.pending)}

    def restore_checkpoint_state(self, state):
        if state.get('type') != 'Pits' or state.get('version') != 1:
            raise RuntimeError('unsupported Pits checkpoint state')
        if tuple(state['channels']) != self.channels or state['ips'] != self.ips:
            raise RuntimeError('Pits checkpoint configuration mismatch')
        self.next = list(state['next']); self.now = state['now']
        self.held = state['held']; self.fired = collections.Counter(state['fired'])
        self.missed = collections.Counter(state['missed'])
        # Absent from checkpoints saved before ticks were held: none pending.
        self.cleared = collections.Counter(state.get('cleared', {}))
        self.pending = set(state.get('pending', ()))
        self._periods = [_STALE] * 4

    def period(self, ch):
        """-> instructions between interrupts, or None while the timer is off."""
        pcsr, pmr = struct.unpack('>HH', self.m.uc.mem_read(BASES[ch], 4))
        if not (pcsr & EN) or not (pcsr & PIE):
            return None
        prescale = 1 << ((pcsr >> 8) & 0xF)        # MCF5441XRM Eqn. 38-1
        return prescale * (pmr + 1) / F_BUS * self.ips

    def level(self, vec):
        """-> the source's interrupt level, or None if masked or disabled.

        Read from the INTC rather than assumed, because the RTOS changes it:
        the context switcher unmasks PIT0's source on every switch.
        """
        return interrupt_level(self.m, vec)

    def deadline(self, done):
        """-> the instruction count at which the next channel is due.

        Arms any channel that is enabled but unarmed, and disarms any that
        the firmware has switched off, so the answer always reflects the
        registers as they are now. None while every channel is off.
        """
        if self.held:
            return None
        best = None
        for ch in self.channels:
            p = self._period(ch)
            if p is None:
                if self.next[ch] is not None:
                    self.transitions += 1
                self.next[ch] = None       # off; restart the clock if it returns
                continue
            if self.next[ch] is None:
                self.next[ch] = done + p
                self.transitions += 1
            if best is None or self.next[ch] < best:
                best = self.next[ch]
        return best

    def step(self, done, remaining=None):
        """-> instructions to run before the next `service` call is due.

        Run this many and a timer lands on the instruction it was due at,
        instead of at whatever chunk boundary happens to follow it. That is
        the whole point: with a fixed chunk the same 60M instructions from
        the same snapshot produces different fault counts, different display
        callback counts and different surviving task counts depending only
        on the chunk size, which makes every post-intro measurement an
        artifact of the harness rather than a property of the firmware.

        `remaining` bounds the step to what is left of a caller's budget.
        Leave it None to let the step run to the deadline and overshoot the
        budget instead, which is what a caller wants when it is spending its
        budget in several calls: truncating the last step of each call puts
        an emu_start boundary somewhere no timer was due, and boundaries in
        the wrong place are exactly what deadline stepping exists to avoid.
        The overshoot is under one timer period. Arbitrary subdivisions are
        unsupported because Unicorn can change guest condition-code behavior
        across `emu_start` boundaries.
        """
        d = self.deadline(done)
        try:
            n = IDLE_STEP if d is None else max(1, int(math.ceil(d - done)))
        except (TypeError, ValueError, OverflowError) as exc:
            raise RuntimeError('invalid PIT deadline') from exc
        if d is None and remaining is not None:
            n = remaining
        if remaining is not None:
            n = min(n, remaining)
        return max(1, n)

    def service(self, done):
        """Call at a chunk boundary with the instruction count so far.

        The caller MUST re-read PC afterwards -- see the module docstring.

        A channel whose deadline has passed becomes pending, and the pending
        channels are then offered to the CPU in `channels` order, which is
        the INTC's -- see the class docstring.
        """
        if self.held:
            return
        for ch in self.channels:
            p = self._period(ch)
            if p is None:
                if self.next[ch] is not None:
                    self.transitions += 1
                self.next[ch] = None       # off; restart the clock if it returns
                self.pending.discard(ch)   # and drop its request
                continue
            if self.next[ch] is None:
                self.next[ch] = done + p
                self.transitions += 1
                continue
            if done < self.next[ch]:
                continue
            self.next[ch] += p
            if self.next[ch] <= done:      # overshot by more than a period
                self.next[ch] = done + p   # drop the backlog, do not chase it
            if ch in self.pending:
                self.missed[ch] += 1       # PIF is still set: this tick is lost
            else:
                self.pending.add(ch)
        if self.pending:
            deliver_pending(self, VECTORS)


def deliver_pending(source, vectors):
    """Offer a timer source's pending channels to the CPU, in `channels`
    order. Shared by `Pits` and `emu.dtim.Dtims`."""
    for ch in source.channels:
        if ch not in source.pending:
            continue
        vec = vectors[ch]
        lvl = source.level(vec)
        if lvl is None:
            continue                       # masked: held until unmasked
        sr = source.m.uc.reg_read(UC_M68K_REG_SR)
        if ((sr >> 8) & 0x07) >= lvl:
            continue                       # held until the IPL drops
        if source.m.raise_vector(vec, level=lvl):
            # Taking an interrupt raises the mask to its own level, so the
            # handler cannot be re-entered by the same source, and a later
            # channel at the same level waits for this handler's return.
            source.pending.discard(ch)
            source.fired[ch] += 1


def _rescale(source, ips):
    """`Pits.rescale` and `Dtims.rescale`: keep each deadline's device time."""
    old = source.ips
    if ips == old:
        return
    now = source.now
    source.next = [None if d is None else now + (d - now) * ips / old
                   for d in source.next]
    source.ips = ips
