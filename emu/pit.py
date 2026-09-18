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

    prescaler = 1 << (((PCSR >> 8) & 0xF) + 1)
    period    = prescaler * (PMR + 1) bus cycles, at 132 MHz

which reproduces all three known rates exactly -- PIT0 20.0000 ms (50 Hz),
PIT2 16.6672 ms (59.998 Hz), PIT3 66.6686 ms (14.9996 Hz). Three independent
timers landing on round numbers is what validates the 132 MHz bus clock.

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
whose level is not above the current IPL cannot be taken. A tick the hardware
could not have taken is counted as missed, never forced.

Levels come from the INTC, not from us: vector 205 is INTC2 source 13, and
`0xFC050040 + source` reads 1 for PIT0, 3 for PIT2 and 3 for PIT3.

Pacing is an instruction count, not real time. `INSTR_PER_SEC` is 4.68M, from
the measured 312k instructions per intro frame at 15 fps. It is a proxy, and
only as good as the assumption that instruction rate tracks wall clock --
which it does not while a task spins. Good enough to turn the RTOS over; not
cycle accuracy, and it should not be described as such.

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

from unicorn.m68k_const import UC_M68K_REG_SR

BASES   = (0xFC080000, 0xFC084000, 0xFC088000, 0xFC08C000)
VECTORS = (205, 206, 207, 208)
EN, PIE = 0x01, 0x08                  # PCSR bit 0 and bit 3
F_BUS = 132_000_000
INSTR_PER_SEC = 4_680_000             # 312k instructions per frame at 15 fps

# Each interrupt controller owns 64 vectors: INTC0 64-127, INTC1 128-191,
# INTC2 192-255. Per source there is an ICR byte at +0x40+source whose low
# three bits are the level, and a mask bit in IMRH/IMRL at +0x08/+0x0C.
INTC = ((0xFC048000, 64), (0xFC04C000, 128), (0xFC050000, 192))
ICR_BASE, IMR_BASE = 0x40, 0x08

# How far to run in one go when every timer is switched off and there is no
# deadline to aim at. Only the pacing of a dead clock depends on it.
IDLE_STEP = 1_000_000

PIT3_VECTOR_SLOT = 0x40000340     # VBR + 208*4, not build-specific


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

    **The order is load-bearing, and it is a stopgap.** `service` walks the
    list, and the first timer taken raises IPL to its own level, which
    refuses any later one at the same level. PIT3's period is exactly eight
    times PIT2's, so once their deadlines line up they collide on the same
    instruction for ever, and both sources are INTC level 3. Measured from
    `boot400M.snap` over 250M instructions: listed as `(0, 2, 3)`, PIT3 is
    due 281 times and taken **none** of them and the display task never
    wakes; listed as `(3, 2, 0)`, PIT3 is taken 130 times, at the cost of
    PIT2's misses rising from 29 to 160. Losing 6% of timer-wheel ticks is a
    much smaller price than never running the display timer, so PIT3 leads.

    What this really wants is a latched PIF per channel -- a refused tick
    held and delivered when IPL drops, which is what the hardware does --
    rather than an arbitrary tie-break. That was tried both ways and both
    are worse: retrying at the next timer deadline lands the interrupt
    wherever that deadline falls and halts the run on an unhandled vector at
    58.7M instructions, and delivering from the `rte` handler is correct but
    puts a Python callback on every `rte`, including the RTOS's trap-based
    yields, which is far too slow to finish a run. Note also that the real
    tie-break between two same-level sources is the INTC's, by source
    number, and we have not read the MCF5441x manual to find out which way
    it goes -- PIT2 is INTC2 source 15 and PIT3 is source 16.

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
        self.missed = collections.Counter()

    def release(self):
        """Start delivering. Safe to call more than once."""
        self.held = False

    def checkpoint_state(self):
        return {'type': 'Pits', 'version': 1, 'channels': self.channels,
                'ips': self.ips, 'next': list(self.next), 'now': self.now,
                'held': self.held, 'fired': dict(self.fired),
                'missed': dict(self.missed)}

    def restore_checkpoint_state(self, state):
        if state.get('type') != 'Pits' or state.get('version') != 1:
            raise RuntimeError('unsupported Pits checkpoint state')
        if tuple(state['channels']) != self.channels or state['ips'] != self.ips:
            raise RuntimeError('Pits checkpoint configuration mismatch')
        self.next = list(state['next']); self.now = state['now']
        self.held = state['held']; self.fired = collections.Counter(state['fired'])
        self.missed = collections.Counter(state['missed'])

    def period(self, ch):
        """-> instructions between interrupts, or None while the timer is off."""
        pcsr, pmr = struct.unpack('>HH', self.m.uc.mem_read(BASES[ch], 4))
        if not (pcsr & EN) or not (pcsr & PIE):
            return None
        prescale = 1 << (((pcsr >> 8) & 0xF) + 1)
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
            p = self.period(ch)
            if p is None:
                self.next[ch] = None       # off; restart the clock if it returns
                continue
            if self.next[ch] is None:
                self.next[ch] = done + p
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

        `channels` is walked in the order given, and that order is the
        tie-break when two timers come due on the same instruction: the first
        one taken raises IPL to its own level, which refuses any later one at
        the same level. That is not a hypothetical -- see the class docstring
        on PIT3.
        """
        if self.held:
            return
        for ch in self.channels:
            p = self.period(ch)
            if p is None:
                self.next[ch] = None       # off; restart the clock if it returns
                continue
            if self.next[ch] is None:
                self.next[ch] = done + p
                continue
            if done < self.next[ch]:
                continue
            self.next[ch] += p
            if self.next[ch] <= done:      # overshot by more than a period
                self.next[ch] = done + p   # drop the backlog, do not chase it
            vec = VECTORS[ch]
            lvl = self.level(vec)
            sr = self.m.uc.reg_read(UC_M68K_REG_SR)
            if lvl is None or ((sr >> 8) & 0x07) >= lvl:
                self.missed[ch] += 1       # the hardware could not take it either
            elif self.m.raise_vector(vec, level=lvl):
                # Taking an interrupt raises the mask to its own level, so the
                # handler cannot be re-entered by the same source. That is done
                # by the entry trampoline, in guest code: writing SR from here
                # would install a stale condition-code byte over the flags of
                # the code being interrupted. See Machine.install_srtrap.
                self.fired[ch] += 1
