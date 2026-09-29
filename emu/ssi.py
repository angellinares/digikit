"""Opt-in SSI0/eDMA48/50 event model.

This is deliberately narrower than a generic SSI or eDMA implementation.  It
models the producer chain recovered from DT2 firmware:

    SSI0 request cadence -> TCD48/TCD50 minor loops -> TCD50 major interrupt
    -> vector 170 -> guest INTFRCH1[31] write -> vector 191

The request rate must be supplied explicitly.  DT2 selects an external
SSI_CLKIN, whose board frequency is not yet recovered; silently assuming an
audio sample rate would turn an exploratory model into false qualification.
RX destination bytes are preserved rather than inventing data from the
external SSI peer, unless an explicit `peer` supplies them (below).

## SSI0 peer hook

`Ssi0Dma(..., peer=None)` (default unchanged: RX untouched, TX bytes only
tracked internally) accepts an object with:

    peer.rx(nbytes: int) -> bytes   # exactly nbytes; this period's RX samples
    peer.tx(data: bytes) -> None    # this period's captured TX samples

called from `_run_minor` once per DMA period (one call = one minor-loop
element-group, i.e. one `_run_minor` invocation, not one 32-bit element) --
`rx()` before the RX channel's destination is written (its return value
*is* what gets written), `tx()` after the TX channel's source bytes are
captured. This is the audio-side half of the pairing described in
`emu/dspi2.py`'s "Lockstep interface for a future SHARC stepper": a stepper
implementing both `rx`/`tx` here and `exchange()` there is how a future
SHARC model answers both the periodic control frame and the sample stream
with one peer object.
"""

# pyright: reportMissingImports=false, reportAttributeAccessIssue=false
from __future__ import annotations

import collections
import struct
import zlib
from fractions import Fraction

from unicorn import UC_HOOK_MEM_READ, UC_HOOK_MEM_WRITE
from unicorn.m68k_const import UC_M68K_REG_A7, UC_M68K_REG_SR

from emu.edma import (
    ATTR,
    BITER,
    CITER,
    CSR,
    DADDR,
    DOFF,
    EDMA_BASE,
    NBYTES,
    SADDR,
    SERQ,
    SOFF,
    TCD_BASE,
)
from emu.pit import interrupt_level

CINT = EDMA_BASE + 0x1C
INTFRCH1 = 0xFC04C010
INTFRCH1_SOURCE63 = 0x80000000
RX_CHAN, TX_CHAN = 48, 50
RX_REGISTER, TX_REGISTER = 0xFC0BC008, 0xFC0BC000
RX_VECTOR, TX_VECTOR, FORCE_VECTOR = 168, 170, 191

# Audio-rate SSI0 request cadence, derived from the firmware's own SSI0
# register programming (docs/findings/04-coldfire-dsp-link.md, "The 1.16
# post-gesture checkpoint's SSI0 configuration is externally clocked") plus
# the public MCF5441x Reference Manual, and cross-checked against the
# firmware's own audio-block tick (below). Not a firmware-recovered board
# clock: SSI_CLKIN's real frequency is still unrecovered, so this stays an
# explicit, opt-in assumption -- see the module docstring.
#
#   SSIn_CCR = 0x16f00 (MCF5441XRM Figure 35-19/Table 35-12, p.1080-1081):
#     WL[16:13]=0b1011 -> 24-bit words; DC[12:8]=0b01111 -> DC+1 = 16 words
#     per network-mode frame (matches the documented "24-bit words, 16 words
#     per frame").
#   SSIn_TMASK = SSIn_RMASK = 0xffff0000 (MCF5441XRM Table 35-23/35-24,
#     p.1089-1090): bits 0-15 (the frame's 16 real time slots) are 0 =
#     "valid time slot", so all 16 slots are active, none masked.
#   SSIn_FCSR = 0x88 (MCF5441XRM Figure 35-20, p.1081): RFWM0=TFWM0=8 words,
#     matching the eDMA minor loop's 32 bytes / 4-byte elements = 8 elements
#     (docs/findings/04). With all 16 slots active and an 8-word watermark, a
#     DMA request fires twice per SSI audio frame (once per 8 of the 16
#     slots).
#   Assuming that SSI audio frame itself repeats at 48 kHz (the codec/SHARC
#   frame sync SSI0 is externally clocked from -- TCR/RCR select external
#   bit clock and frame sync; SSI_CLKIN's own frequency is not captured in
#   the firmware, see docs/findings/04), the request rate is
#   2 x 48,000 = 96,000 Hz.
#   TCD48/TCD50's major loop is 64 minors (docs/findings/04), so the TX
#   major-loop-complete interrupt (vector 170) then fires at
#   96,000 / 64 = 1,500 Hz -- which independently matches the SHARC's own
#   known block cadence (32 stereo samples/block at 48 kHz -> 1,500
#   blocks/s), and, checked live, the ColdFire's own per-block sample tick
#   `_DAT_40966500` (advanced by exactly 0x20 = 32 in the vector-191 handler,
#   `docs/findings/04`) -- three independently-derived numbers agreeing is
#   what makes 96,000 Hz a meaningfully better opt-in default than the
#   1,000 Hz / 48,000 Hz placeholders earlier tools used, though it remains
#   an assumption, not a recovered board clock.
AUDIO_SSI0_REQUEST_HZ = 96_000


def _signed(value, bits):
    sign = 1 << (bits - 1)
    return value - (1 << bits) if value & sign else value


# One eDMA TCD in the ColdFire field order of emu/edma.py: SADDR ATTR SOFF
# NBYTES SLAST DADDR CITER DOFF DLAST BITER CSR. One read per minor loop
# instead of one per field: the binding allocates a buffer per `mem_read`,
# and at 96 kHz that allocation was the largest single cost of this model.
_TCD = struct.Struct(">IHhIiIHhIHH")
_SADDR, _ATTR, _SOFF, _NBYTES, _SLAST, _DADDR = range(6)
_CITER, _DOFF, _DLAST, _BITER, _CSR = range(6, 11)
# CITER at +0x14 and BITER at +0x1C, read together.
_CITER_BITER = struct.Struct(">H6xH")

# Coalescing never batches more requests than one major loop; see
# `Ssi0Dma.coalesce`.
COALESCE_MAX_REQUESTS = 64


class Ssi0Dma:
    """Exact-deadline SSI request source for the observed DT2 descriptors.

    ## Deadlines

    Request `k` is due at instruction `ceil(k * ips / request_hz)` after the
    clock's origin. `step` returns the distance to the next one and `service`
    runs it, so every request lands on its own `emu_start` boundary. The
    clock is held as an integer count of `1 / request_hz` instruction units
    (`self._q`), which is exactly the rational `self.next` of the earlier
    Fraction implementation: `next == Fraction(_q, request_hz)`. The
    `next` property keeps that interface for checkpoints and callers.

    ## What the guest can observe at a request

    Per request the model reads one TCD, writes the RX destination words
    (only when a `peer` supplies them), reads the TX source words, and writes
    the advanced TCD back. Only the request that completes a major loop does
    anything else: it reloads or scatter/gathers the TCD and asserts the
    channel-50 major interrupt, which `service` delivers as vector 170 at
    that same boundary. A vector that the CPU's IPL or the INTC mask refuses
    stays pending and is retried at every later boundary, as is a forced
    vector 191 that its RTE hook could not deliver.

    ## Coalescing (opt-in, `coalesce=True`)

    With coalescing on, `step` does not stop at every request. It runs to the
    request that completes the next major loop (at most
    `COALESCE_MAX_REQUESTS` requests), and `service` then runs every request
    that has come due, in order, before delivering. Timer deadlines still cut
    a span short; `service` then runs the requests due so far. While vector
    170 is pending it steps request by request, exactly as without
    coalescing: the boundary at which a refused vector 170 is finally taken
    depends on which boundaries exist, and in the audio-running state that
    retry does succeed at later boundaries. A pending forced vector 191 does
    NOT stop coalescing: its boundary retries are skipped and it is retried
    only at span ends, and at its RTE hook as before.

    That is exact only if the guest neither observes nor changes what the
    skipped boundaries would have done. `coalesce_violations` counts every
    span in which that may have failed and can be seen from here:

      * `tcd`: a guest write of TCD48 or TCD50, or a read of a field the
        requests advance (SADDR, DADDR, CITER), which returns the span's
        starting value instead of the eager one;
      * `rx`: a guest read or write of the RX words the span's requests write;
      * `tx`: a guest write of the TX words the span's requests read. This
        one changes only what the peer receives (`peer.tx`, `tx_crc32`),
        never the ColdFire's own state;
      * `serq`: a guest SERQ write that enabled a channel;
      * `force`: INTFRCH1[31] set with the live IPL below vector 191's level,
        which the next skipped boundary would have delivered;
      * `late191`: a vector 191 pending through a span and delivered at its
        end, which a skipped boundary may have delivered earlier;
      * `timers`: a timer in `watch` armed or disarmed at the span's end,
        which the eager model notices at the first boundary after the
        guest's write.

    What it cannot see is a vector 191 pending through a span while the IPL
    dips below its level and comes back up by guest SR writes alone: the
    eager model would have taken it at a boundary inside the dip. That is
    why coalescing is opt-in, and why its equivalence claim is a
    tools/snapeq.py comparison against the eager model on the same window,
    not this counter. `coalesce_check=False` drops the per-span range hooks
    and the counter with them.

    It also works against the idle skip (`emu.longrun.IdleSpin`) at the
    device rate. An exact span starts where the last one delivered an
    interrupt, so it almost never starts in the idle loop, and the idle
    stretch inside it runs pass by pass. Measured from drive3/loaded.snap at
    132M, 0.3 s of device time: exact 0.22x real time without coalescing,
    0.019x with it; with `spin(fast=True)` 0.24x and 0.28x. Held timer ticks
    also wait for a span end, where the RTOS has often acknowledged them
    already (PIT0 18 of 30 cleared by the guest). So it stays opt-in.
    """

    def __init__(
        self,
        machine,
        request_hz,
        instr_per_sec,
        at=None,
        force_rte=None,
        peer=None,
        coalesce=False,
        coalesce_check=True,
    ):
        if request_hz <= 0 or instr_per_sec <= 0:
            raise ValueError("SSI request and instruction rates must be positive")
        self.m = machine
        self.request_hz = int(request_hz)
        self.ips = int(instr_per_sec)
        # Optional peer supplying RX samples / consuming TX samples once per
        # DMA period (one `_run_minor` call); see the module docstring,
        # "SSI0 peer hook". None (default): unchanged from before this
        # parameter existed -- RX destination bytes are left untouched, and
        # captured TX bytes are tracked in self.tx_bytes/tx_crc32 only.
        self.peer = peer
        self.now = 0
        # The next request's deadline in units of 1/request_hz instructions;
        # see the class docstring, "Deadlines".
        self._q = None
        self.enabled = set()
        self.int50_asserted = False
        self.int50_delivered = False
        self.force_asserted = False
        self.force_delivered = False
        self.requests = 0
        self.major_loops = {RX_CHAN: 0, TX_CHAN: 0}
        self.scatter_gathers = {RX_CHAN: 0, TX_CHAN: 0}
        self.tx_bytes = 0
        self.tx_crc32 = 0
        self.vector170 = 0
        self.vector191 = 0
        self._checkpoint_restored = False

        self.coalesce = bool(coalesce)
        self.coalesce_check = bool(coalesce_check)
        self.coalesced = 0  # requests run at a boundary not their own
        self.coalesce_violations = collections.Counter()
        self.watch = ()  # timer sources checked for `timers`
        self._span = False  # a coalesced span is running
        self._span_hooks = []
        self._span_counts = collections.Counter()
        self._span_marks = None
        self._span_open_marks = ()
        self._tcd_hooks = False

        machine.uc.hook_add(UC_HOOK_MEM_WRITE, self._on_serq, begin=SERQ, end=SERQ)
        machine.uc.hook_add(UC_HOOK_MEM_WRITE, self._on_cint, begin=CINT, end=CINT)
        machine.uc.hook_add(
            UC_HOOK_MEM_WRITE,
            self._on_intfrch1,
            begin=INTFRCH1,
            end=INTFRCH1 + 3,
        )
        if at is not None and force_rte is not None:
            at(force_rte, self._on_force_rte)

    @property
    def period(self):
        return Fraction(self.ips, self.request_hz)

    @property
    def next(self):
        """The next request's deadline, as the exact rational it always was."""
        return None if self._q is None else Fraction(self._q, self.request_hz)

    @next.setter
    def next(self, value):
        if value is None:
            self._q = None
            return
        q = Fraction(value) * self.request_hz
        if q.denominator != 1:
            raise ValueError(
                "SSI deadline %r is not a multiple of 1/%d" % (value, self.request_hz)
            )
        self._q = int(q)

    def _first(self, done):
        return int(done) * self.request_hz + self.ips

    def rescale(self, ips):
        """Change the time base, keeping the next request's device time.

        The counterpart of `emu.pit.Pits.rescale`, for a checkpoint saved at
        another rate or a rate change mid-run. The deadline stays on the
        `1 / request_hz` grid, rounded up.
        """
        ips = int(ips)
        if ips <= 0:
            raise ValueError("SSI instruction rate must be positive")
        if self._q is not None and ips != self.ips:
            origin = self.now * self.request_hz
            self._q = origin - (origin - self._q) * ips // self.ips
        self.ips = ips

    def align(self, now):
        """Start a fresh SSI clock at an explicit legacy-upgrade boundary."""
        self.now = int(now)
        if self.enabled and (self._q is None or not self._checkpoint_restored):
            self._q = self._first(self.now)

    def arm_legacy(self):
        """Claim the already-programmed DT2 descriptors at an explicit upgrade."""
        expected = {
            RX_CHAN: (RX_REGISTER, 0x0202, 0, 0x20, 4, 0x10),
            TX_CHAN: (TX_REGISTER, 0x0202, 4, 0x20, 0, 0x12),
        }
        for channel in (RX_CHAN, TX_CHAN):
            peripheral = (
                self._u32(channel, SADDR)
                if channel == RX_CHAN
                else self._u32(channel, DADDR)
            )
            actual = (
                peripheral,
                self._u16(channel, ATTR),
                _signed(self._u16(channel, SOFF), 16),
                self._u32(channel, NBYTES),
                _signed(self._u16(channel, DOFF), 16),
                self._u16(channel, CSR) & 0x12,
            )
            if actual != expected[channel]:
                raise RuntimeError(
                    f"SSI legacy upgrade TCD{channel} shape mismatch: "
                    f"actual={actual!r} expected={expected[channel]!r}"
                )
            if not self._u16(channel, CITER) or not self._u16(channel, BITER):
                raise RuntimeError(f"SSI legacy upgrade found inactive TCD{channel}")
        self.enabled.update((RX_CHAN, TX_CHAN))
        if self._q is None:
            self._q = self._first(self.now)

    def checkpoint_state(self):
        next_value = None
        if self._q is not None:
            value = self.next
            next_value = [value.numerator, value.denominator]
        return {
            "type": "Ssi0Dma",
            "version": 1,
            "request_hz": self.request_hz,
            "ips": self.ips,
            "now": self.now,
            "next": next_value,
            "enabled": sorted(self.enabled),
            "int50_asserted": self.int50_asserted,
            "int50_delivered": self.int50_delivered,
            "force_asserted": self.force_asserted,
            "force_delivered": self.force_delivered,
            "requests": self.requests,
            "major_loops": dict(self.major_loops),
            "scatter_gathers": dict(self.scatter_gathers),
            "tx_bytes": self.tx_bytes,
            "tx_crc32": self.tx_crc32,
            "vector170": self.vector170,
            "vector191": self.vector191,
        }

    def restore_checkpoint_state(self, state):
        if state.get("type") != "Ssi0Dma" or state.get("version") != 1:
            raise RuntimeError("unsupported Ssi0Dma checkpoint state")
        if state.get("request_hz") != self.request_hz:
            raise RuntimeError("Ssi0Dma request-rate mismatch")
        self.ips = state["ips"]
        self.now = state["now"]
        raw_next = state["next"]
        self.next = None if raw_next is None else Fraction(*raw_next)
        self.enabled = set(state["enabled"])
        self.int50_asserted = state["int50_asserted"]
        self.int50_delivered = state["int50_delivered"]
        self.force_asserted = state["force_asserted"]
        self.force_delivered = state["force_delivered"]
        self.requests = state["requests"]
        self.major_loops = {int(k): v for k, v in state["major_loops"].items()}
        self.scatter_gathers = {int(k): v for k, v in state["scatter_gathers"].items()}
        self.tx_bytes = state["tx_bytes"]
        self.tx_crc32 = state["tx_crc32"]
        self.vector170 = state["vector170"]
        self.vector191 = state["vector191"]
        self._checkpoint_restored = True

    def step(self, done, remaining=None):
        self.now = int(done)
        if not self.enabled:
            return remaining
        hz = self.request_hz
        if self._q is None:
            self._q = self._first(done)
        end = self._q
        if self.coalesce:
            self._check_timers()
            if not (self.int50_asserted and not self.int50_delivered):
                batch = self._batch()
                if batch > 1:
                    end += (batch - 1) * self.ips
                    self._open_span(batch)
        # ceil((end - done*hz) / hz), in integers.
        step = max(1, -((done * hz - end) // hz))
        return min(step, remaining) if remaining is not None else step

    def service(self, done):
        self.now = int(done)
        due = done * self.request_hz
        late = False
        if self._span:
            late = self._close_span(done)
        if self._q is not None and due >= self._q:
            count = (due - self._q) // self.ips + 1 if self.coalesce else 1
            ran = self._run_requests(count)
            self.requests += ran
            self._q += ran * self.ips
            if ran > 1:
                self.coalesced += ran - 1
            if self._q <= due and not self.coalesce:
                # Overshot by more than a period (fast mode): drop the
                # backlog, do not chase it.
                self._q = due + self.ips
        self._deliver_vector170()
        if self._deliver_vector191() and late:
            self.coalesce_violations["late191"] += 1

    # -- coalescing ------------------------------------------------------

    def _batch(self):
        """-> requests until the next major-loop completion, capped."""
        batch = COALESCE_MAX_REQUESTS
        for channel in (RX_CHAN, TX_CHAN):
            if channel not in self.enabled:
                continue
            citer, _biter = _CITER_BITER.unpack(
                self.m.uc.mem_read(self._tcd(channel) + CITER, 10)
            )
            citer &= 0x7FFF
            if citer:
                batch = min(batch, citer)
        return batch

    def _marks(self):
        return tuple(getattr(source, "transitions", 0) for source in self.watch)

    def _check_timers(self):
        # A timer notices a guest arm or disarm at the first boundary after
        # the write, and the eager model has one at every request. The span
        # that skipped them ends before the timers are serviced, so compare
        # once they have had their boundary: here, at the next step.
        if self._span_marks is not None and self._marks() != self._span_marks:
            self.coalesce_violations["timers"] += 1
        self._span_marks = None

    def _open_span(self, batch):
        if self._span_hooks:  # a span an exception cut short
            self._close_span(0)
        self._span = True
        self._span_marks = None
        if not self.coalesce_check:
            return
        uc = self.m.uc
        if not self._tcd_hooks:
            # Unicorn matches a hook on an access's first byte, so each range
            # starts three bytes early to catch an unaligned access that
            # spills into it. Reads matter only of the fields a request
            # advances (SADDR; DADDR and CITER); any write matters.
            for channel in (RX_CHAN, TX_CHAN):
                base = self._tcd(channel)
                for hook_type, low, high in (
                    (UC_HOOK_MEM_WRITE, 0x00, 0x1F),
                    (UC_HOOK_MEM_READ, 0x00, 0x03),
                    (UC_HOOK_MEM_READ, 0x10, 0x15),
                ):
                    uc.hook_add(
                        hook_type,
                        self._on_span_tcd,
                        begin=base + low - 3,
                        end=base + high,
                    )
            self._tcd_hooks = True
        self._span_open_marks = self._marks()
        for channel, hook_type, hook in (
            (RX_CHAN, UC_HOOK_MEM_READ | UC_HOOK_MEM_WRITE, self._on_span_rx),
            (TX_CHAN, UC_HOOK_MEM_WRITE, self._on_span_tx),
        ):
            if channel not in self.enabled or (
                channel == RX_CHAN and self.peer is None
            ):
                continue  # without a peer, RX requests write nothing
            tcd = _TCD.unpack(uc.mem_read(self._tcd(channel), 0x20))
            if channel == RX_CHAN:
                start, offset = tcd[_DADDR], tcd[_DOFF]
            else:
                start, offset = tcd[_SADDR], tcd[_SOFF]
            # The words every request of the span moves. A request that turns
            # out to be due only at the span's end ran on time; counting it
            # anyway keeps the check conservative.
            length = batch * (tcd[_NBYTES] // 4) * offset
            if not length:
                continue
            if length > 0:
                low, high = start, start + length - 1
            else:
                low, high = start + length + 4, start + 3
            self._span_hooks.append(uc.hook_add(hook_type, hook, begin=low, end=high))

    def _on_span_tcd(self, uc, access, address, size, value, user_data):
        if self._span:
            self._span_counts["tcd"] += 1

    def _on_span_rx(self, uc, access, address, size, value, user_data):
        if self._span:
            self._span_counts["rx"] += 1

    def _on_span_tx(self, uc, access, address, size, value, user_data):
        if self._span:
            self._span_counts["tx"] += 1

    def _close_span(self, done):
        """End a span at boundary `done`. -> True if vector 191 was pending
        through it and it skipped a boundary the eager model would have had."""
        self._span = False
        for handle in self._span_hooks:
            self.m.uc.hook_del(handle)
        self._span_hooks = []
        # The eager model has a boundary at ceil(q / hz) for each request q;
        # the span skipped one if the first pending request is due before
        # `done`, i.e. q <= (done - 1) * hz.
        skipped = self._q is not None and self._q <= (done - 1) * self.request_hz
        counts, self._span_counts = self._span_counts, collections.Counter()
        if not skipped:
            return False
        self.coalesce_violations.update(counts)
        if self.coalesce_check:
            self._span_marks = self._span_open_marks
        return self.coalesce_check and self.force_asserted and not self.force_delivered

    # -- DMA requests ----------------------------------------------------

    def _run_requests(self, count):
        """Run up to `count` due requests back to back. -> how many ran.

        Stops after a request that completes a major loop, so its interrupt
        is delivered with the DMA state as it was at that request.
        """
        ran = self._run_block(count - 1) if count > 1 else 0
        while ran < count:
            ran += 1
            major = self._run_minor(RX_CHAN, capture_tx=False)
            major = self._run_minor(TX_CHAN, capture_tx=True) or major
            if major:
                break
        return ran

    def _run_block(self, limit):
        """Run up to `limit` requests that complete no major loop as one
        block. -> how many ran (0 when the shape needs `_run_minor`).

        Same bytes, same order as that many `_run_minor` pairs: one TX read
        and one RX write for the whole block instead of one per request, and
        the TCDs written back once. Only for the observed shape: both
        channels enabled with contiguous 32-bit elements, no ranges that
        overlap each other or the TCDs, and a peer that is either absent or
        can be told where a major loop starts (`rx_major`) instead of
        reading the TCD from guest memory mid-block.
        """
        if RX_CHAN not in self.enabled or TX_CHAN not in self.enabled:
            return 0
        peer = self.peer
        rx_major = getattr(peer, "rx_major", None)
        if peer is not None and (
            rx_major is None or getattr(peer, "channel", RX_CHAN) != RX_CHAN
        ):
            return 0
        uc = self.m.uc
        rx = list(_TCD.unpack(uc.mem_read(self._tcd(RX_CHAN), 0x20)))
        tx = list(_TCD.unpack(uc.mem_read(self._tcd(TX_CHAN), 0x20)))
        for tcd in (rx, tx):
            if (tcd[_CITER] | tcd[_BITER]) & 0x8000 or tcd[_ATTR] & 0x0707 != 0x0202:
                return 0
            if tcd[_NBYTES] != 0x20:
                return 0
        if tx[_SOFF] != 4 or (peer is not None and rx[_DOFF] != 4):
            return 0
        count = min(limit, rx[_CITER] - 1, tx[_CITER] - 1)
        if count <= 0:
            return 0
        nbytes = count * 0x20
        source, dest = tx[_SADDR], rx[_DADDR]
        ranges = [
            (source, nbytes),
            (TCD_BASE + RX_CHAN * 0x20, 0x20),
            (TCD_BASE + TX_CHAN * 0x20, 0x20),
        ]
        if peer is not None:
            ranges.append((dest, nbytes))
        ranges.sort()
        for (low, size), (high, _size) in zip(ranges, ranges[1:], strict=False):
            if low + size > high:
                return 0
        if source + nbytes > 1 << 32 or dest + nbytes > 1 << 32:
            return 0

        captured = bytes(uc.mem_read(source, nbytes))
        written = bytearray()
        biter = rx[_BITER] & 0x7FFF
        for i in range(count):
            if peer is not None:
                provided = rx_major(0x20, rx[_CITER] - i == biter)
                if len(provided) != 0x20:
                    raise ValueError(
                        "Ssi0Dma peer.rx returned %d bytes, expected 32" % len(provided)
                    )
                written += provided
            chunk = captured[i * 0x20 : (i + 1) * 0x20]
            self.tx_crc32 = zlib.crc32(chunk, self.tx_crc32)
            if peer is not None:
                peer.tx(chunk)
        self.tx_bytes += nbytes
        if peer is not None:
            uc.mem_write(dest, bytes(written))
        rx[_DADDR] = (dest + rx[_DOFF] * 8 * count) & 0xFFFFFFFF
        rx[_SADDR] = (rx[_SADDR] + rx[_SOFF] * 8 * count) & 0xFFFFFFFF
        tx[_SADDR] = (source + nbytes) & 0xFFFFFFFF
        tx[_DADDR] = (tx[_DADDR] + tx[_DOFF] * 8 * count) & 0xFFFFFFFF
        rx[_CITER] -= count
        tx[_CITER] -= count
        uc.mem_write(self._tcd(RX_CHAN), _TCD.pack(*rx))
        uc.mem_write(self._tcd(TX_CHAN), _TCD.pack(*tx))
        return count

    # -- one DMA request -------------------------------------------------

    def _tcd(self, channel):
        return TCD_BASE + channel * 0x20

    def _u32(self, channel, offset):
        return struct.unpack(">I", self.m.uc.mem_read(self._tcd(channel) + offset, 4))[
            0
        ]

    def _u16(self, channel, offset):
        return struct.unpack(">H", self.m.uc.mem_read(self._tcd(channel) + offset, 2))[
            0
        ]

    def _w32(self, channel, offset, value):
        self.m.uc.mem_write(
            self._tcd(channel) + offset, struct.pack(">I", value & 0xFFFFFFFF)
        )

    def _w16(self, channel, offset, value):
        self.m.uc.mem_write(
            self._tcd(channel) + offset, struct.pack(">H", value & 0xFFFF)
        )

    def _run_minor(self, channel, capture_tx):
        """Run one request on `channel`. -> True if it completed a major loop."""
        if channel not in self.enabled:
            return False
        uc = self.m.uc
        base = self._tcd(channel)
        tcd = list(_TCD.unpack(uc.mem_read(base, 0x20)))
        citer_raw, biter_raw = tcd[_CITER], tcd[_BITER]
        if citer_raw & 0x8000 or biter_raw & 0x8000:
            raise RuntimeError("Ssi0Dma does not support linked CITER/BITER")
        citer = citer_raw & 0x7FFF
        if not citer:
            return False
        attr = tcd[_ATTR]
        source_size = 1 << (attr & 0x7)
        dest_size = 1 << ((attr >> 8) & 0x7)
        if source_size != 4 or dest_size != 4:
            raise RuntimeError("Ssi0Dma only supports the observed 32-bit transfers")
        nbytes = tcd[_NBYTES]
        if not nbytes or nbytes % source_size:
            raise RuntimeError("invalid SSI eDMA minor-loop byte count")
        source, dest = tcd[_SADDR], tcd[_DADDR]
        source_offset, dest_offset = tcd[_SOFF], tcd[_DOFF]
        # `self.peer`, if given, supplies this period's RX samples and
        # receives this period's TX samples -- see the module docstring,
        # "SSI0 peer hook". `provided` is fetched once per `_run_minor` call
        # (one DMA period), not per element, since the peer answers for the
        # whole nbytes-sized chunk in one call.
        provided = None
        if self.peer is not None and not capture_tx:
            provided = self.peer.rx(nbytes)
            if len(provided) != nbytes:
                raise ValueError(
                    "Ssi0Dma peer.rx returned %d bytes, expected %d"
                    % (len(provided), nbytes)
                )
        # Contiguous elements move as one block: the same bytes as one
        # access per element, for one binding call instead of eight.
        elements = nbytes // source_size
        captured = b""
        if capture_tx:
            if source_offset == source_size and source + nbytes <= 1 << 32:
                captured = bytes(uc.mem_read(source, nbytes))
            else:
                parts = bytearray()
                address = source
                for _ in range(elements):
                    parts += uc.mem_read(address, source_size)
                    address = (address + source_offset) & 0xFFFFFFFF
                captured = bytes(parts)
        elif provided is not None:
            if dest_offset == dest_size and dest + nbytes <= 1 << 32:
                uc.mem_write(dest, bytes(provided))
            else:
                address = dest
                for pos in range(0, nbytes, dest_size):
                    uc.mem_write(address, provided[pos : pos + dest_size])
                    address = (address + dest_offset) & 0xFFFFFFFF
        source = (source + source_offset * elements) & 0xFFFFFFFF
        dest = (dest + dest_offset * elements) & 0xFFFFFFFF
        citer -= 1
        tcd[_SADDR], tcd[_DADDR], tcd[_CITER] = source, dest, citer
        uc.mem_write(base, _TCD.pack(*tcd))
        if captured:
            self.tx_bytes += len(captured)
            self.tx_crc32 = zlib.crc32(captured, self.tx_crc32)
            if self.peer is not None:
                self.peer.tx(captured)
        if citer:
            return False

        csr = tcd[_CSR]
        tcd[_SADDR] = (source + tcd[_SLAST]) & 0xFFFFFFFF
        self.major_loops[channel] += 1
        if csr & 0x0010:  # E_SG
            uc.mem_write(base, _TCD.pack(*tcd))
            pointer = tcd[_DLAST]
            if pointer & 0x1F:
                raise RuntimeError("SSI scatter/gather pointer is not 32-byte aligned")
            descriptor = bytes(uc.mem_read(pointer, 0x20))
            uc.mem_write(base, descriptor)
            self.scatter_gathers[channel] += 1
        else:
            tcd[_DADDR] = (dest + _signed(tcd[_DLAST], 32)) & 0xFFFFFFFF
            tcd[_CITER] = biter_raw
            uc.mem_write(base, _TCD.pack(*tcd))
        if channel == TX_CHAN and csr & 0x0002:  # INT_MAJOR
            self.int50_asserted = True
            self.int50_delivered = False
        return True

    def _deliver_vector170(self):
        if not self.int50_asserted or self.int50_delivered:
            return False
        level = interrupt_level(self.m, TX_VECTOR)
        if level is None:
            return False
        sr = self.m.uc.reg_read(UC_M68K_REG_SR)
        if ((sr >> 8) & 0x07) >= level:
            return False
        if self.m.raise_vector(TX_VECTOR, level=level):
            self.int50_delivered = True
            self.vector170 += 1
            return True
        return False

    def _on_serq(self, uc, access, address, size, value, user_data):
        if size != 1 or value & 0x80:
            return
        channels = (RX_CHAN, TX_CHAN) if value & 0x40 else (value & 0x3F,)
        added = {ch for ch in channels if ch in (RX_CHAN, TX_CHAN)} - self.enabled
        if added and self._span:
            self._span_counts["serq"] += 1
        self.enabled.update(added)
        if self.enabled and self._q is None:
            self._q = self._first(self.now)

    def _on_cint(self, uc, access, address, size, value, user_data):
        if size == 1 and (value & 0x40 or (value & 0x3F) == TX_CHAN):
            self.int50_asserted = False
            self.int50_delivered = False

    def _on_intfrch1(self, uc, access, address, size, value, user_data):
        current = bytearray(uc.mem_read(INTFRCH1, 4))
        offset = address - INTFRCH1
        current[offset : offset + size] = int(value).to_bytes(size, "big")
        asserted = bool(int.from_bytes(current, "big") & INTFRCH1_SOURCE63)
        if asserted and not self.force_asserted:
            self.force_delivered = False
            if self._span:
                # The eager model would retry at the next request's boundary
                # with the IPL live then; if the IPL already admits it, that
                # boundary is where it would most likely have been taken.
                level = interrupt_level(self.m, FORCE_VECTOR, respect_mask=False)
                ipl = (uc.reg_read(UC_M68K_REG_SR) >> 8) & 0x07
                if level is not None and ipl < level:
                    self._span_counts["force"] += 1
        self.force_asserted = asserted
        if not asserted:
            self.force_delivered = False

    def _on_force_rte(self, uc, address, size, user_data):
        if not self.force_asserted or self.force_delivered:
            return
        # INTFRCH requests explicitly bypass the INTC mask registers. At this
        # hook the channel-50 ISR is about to restore the interrupted SR; take
        # the pending source as the post-RTE interrupt, via a nested frame that
        # returns to this same RTE after vector 191 clears the force bit.
        level = interrupt_level(self.m, FORCE_VECTOR, respect_mask=False)
        if level is None:
            return
        sp = uc.reg_read(UC_M68K_REG_A7)
        saved_sr = struct.unpack(">H", uc.mem_read(sp + 2, 2))[0]
        if ((saved_sr >> 8) & 0x07) < level and self.m.raise_vector(
            FORCE_VECTOR, level=level
        ):
            self.force_delivered = True
            self.vector191 += 1

    def _deliver_vector191(self):
        """Retry a software-forced source that the interrupted IPL blocked."""
        if not self.force_asserted or self.force_delivered:
            return False
        level = interrupt_level(self.m, FORCE_VECTOR, respect_mask=False)
        if level is None:
            return False
        sr = self.m.uc.reg_read(UC_M68K_REG_SR)
        if ((sr >> 8) & 0x07) >= level:
            return False
        if self.m.raise_vector(FORCE_VECTOR, level=level):
            self.force_delivered = True
            self.vector191 += 1
            return True
        return False


class RxHandoverPeer:
    """Supplies the external RX synchronization marker the generic vector-170
    handler `0x400d2f98` scans for, so vector 170 can hand itself over to the
    real per-block ISR `0x4002d322` (`emu.symbols` calls its RTE point
    `ssi0_dma_force_rte`) without any host-side vector forcing.

    docs/findings/04-coldfire-dsp-link.md ("The same checkpoint has no
    0x007fffff row head...") documents the mechanism this peer answers: at
    every RX (channel 48) major-loop completion, `0x400d2f98` checks byte
    offset 0 of the bank it just filled for the literal `0x007fffff`; only
    while that marker is present does its counter at `0x43153a20` advance,
    and only once that counter exceeds 63 does it replace vector 170's own
    RAM vector slot (`0x400002a8`, i.e. vector 170 = INTC1 source 42) with
    `0x4002d322`. Every vector-170 delivery after that point runs
    `0x4002d322` directly out of the guest's own vector table -- a real,
    firmware-driven handoff, not a host-forced one.

    Without a marker (`Ssi0Dma`'s default of leaving RX destination bytes
    untouched), that handoff counter never advances and `0x400002a8` keeps
    pointing at the generic handler forever: this is the root cause Lane I1
    measured on `running.snap` ("Ssi0Dma delivered vector 170 400 times ...
    force_asserted never went True") once the RTOS is genuinely running,
    independent of the request rate used.

    Marker placement is derived from live TCD state, not a private counter,
    so it survives a checkpoint resume mid-major-loop: element 0 of a major
    loop is exactly the call where the channel's CITER (read before this
    call's own decrement) still equals BITER -- the pre-decrement reload
    value -- which is true immediately after a fresh arm and after every
    major-loop reload, and only there.
    """

    MARKER = 0x007FFFFF

    def __init__(self, machine, channel=RX_CHAN):
        self.m = machine
        self.channel = channel

    def _at_major_start(self):
        base = TCD_BASE + self.channel * 0x20
        citer, biter = _CITER_BITER.unpack(self.m.uc.mem_read(base + CITER, 10))
        return citer & 0x7FFF == biter & 0x7FFF

    def rx(self, nbytes):
        return self.rx_major(nbytes, self._at_major_start())

    def rx_major(self, nbytes, major_start):
        """`rx`, told by the caller whether this is a major loop's first
        request instead of reading the TCD (see `Ssi0Dma._run_block`)."""
        payload = bytearray(nbytes)
        if major_start:
            struct.pack_into(">I", payload, 0, self.MARKER)
        return bytes(payload)

    def tx(self, data):
        pass


def install(
    machine, at, events, request_hz, instr_per_sec, force_rte, peer=None, coalesce=False
):
    source = Ssi0Dma(
        machine,
        request_hz=request_hz,
        instr_per_sec=instr_per_sec,
        at=at,
        force_rte=force_rte,
        peer=peer,
        coalesce=coalesce,
    )
    events["ssi0_dma"] = source
    return source
