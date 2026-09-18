"""Opt-in SSI0/eDMA48/50 event model.

This is deliberately narrower than a generic SSI or eDMA implementation.  It
models the producer chain recovered from DT2 firmware:

    SSI0 request cadence -> TCD48/TCD50 minor loops -> TCD50 major interrupt
    -> vector 170 -> guest INTFRCH1[31] write -> vector 191

The request rate must be supplied explicitly.  DT2 selects an external
SSI_CLKIN, whose board frequency is not yet recovered; silently assuming an
audio sample rate would turn an exploratory model into false qualification.
RX destination bytes are preserved rather than inventing data from the
external SSI peer.
"""

# pyright: reportMissingImports=false, reportAttributeAccessIssue=false
from __future__ import annotations

from fractions import Fraction
import math
import struct
import zlib

from unicorn import UC_HOOK_MEM_WRITE
from unicorn.m68k_const import UC_M68K_REG_A7, UC_M68K_REG_SR

from emu.edma import (
    ATTR,
    BITER,
    CITER,
    CSR,
    DADDR,
    DLAST,
    DOFF,
    EDMA_BASE,
    NBYTES,
    SADDR,
    SERQ,
    SLAST,
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


def _signed(value, bits):
    sign = 1 << (bits - 1)
    return value - (1 << bits) if value & sign else value


class Ssi0Dma:
    """Exact-deadline SSI request source for the observed DT2 descriptors."""

    def __init__(self, machine, request_hz, instr_per_sec, at=None, force_rte=None):
        if request_hz <= 0 or instr_per_sec <= 0:
            raise ValueError("SSI request and instruction rates must be positive")
        self.m = machine
        self.request_hz = int(request_hz)
        self.ips = int(instr_per_sec)
        self.now = 0
        self.next = None
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

    def align(self, now):
        """Start a fresh SSI clock at an explicit legacy-upgrade boundary."""
        self.now = int(now)
        if self.enabled and (self.next is None or not self._checkpoint_restored):
            self.next = Fraction(self.now) + self.period

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
        if self.next is None:
            self.next = Fraction(self.now) + self.period

    def checkpoint_state(self):
        next_value = None
        if self.next is not None:
            next_value = [self.next.numerator, self.next.denominator]
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
        self.scatter_gathers = {
            int(k): v for k, v in state["scatter_gathers"].items()
        }
        self.tx_bytes = state["tx_bytes"]
        self.tx_crc32 = state["tx_crc32"]
        self.vector170 = state["vector170"]
        self.vector191 = state["vector191"]
        self._checkpoint_restored = True

    def step(self, done, remaining=None):
        self.now = int(done)
        if not self.enabled:
            return remaining
        if self.next is None:
            self.next = Fraction(done) + self.period
        step = max(1, math.ceil(self.next - done))
        return min(step, remaining) if remaining is not None else step

    def service(self, done):
        self.now = int(done)
        if self.next is not None and done >= self.next:
            self.requests += 1
            self._run_minor(RX_CHAN, capture_tx=False)
            self._run_minor(TX_CHAN, capture_tx=True)
            self.next += self.period
            if self.next <= done:
                self.next = Fraction(done) + self.period
        self._deliver_vector170()
        self._deliver_vector191()

    def _tcd(self, channel):
        return TCD_BASE + channel * 0x20

    def _u32(self, channel, offset):
        return struct.unpack(">I", self.m.uc.mem_read(self._tcd(channel) + offset, 4))[0]

    def _u16(self, channel, offset):
        return struct.unpack(">H", self.m.uc.mem_read(self._tcd(channel) + offset, 2))[0]

    def _w32(self, channel, offset, value):
        self.m.uc.mem_write(
            self._tcd(channel) + offset, struct.pack(">I", value & 0xFFFFFFFF)
        )

    def _w16(self, channel, offset, value):
        self.m.uc.mem_write(
            self._tcd(channel) + offset, struct.pack(">H", value & 0xFFFF)
        )

    def _run_minor(self, channel, capture_tx):
        if channel not in self.enabled:
            return False
        citer_raw = self._u16(channel, CITER)
        biter_raw = self._u16(channel, BITER)
        if citer_raw & 0x8000 or biter_raw & 0x8000:
            raise RuntimeError("Ssi0Dma does not support linked CITER/BITER")
        citer = citer_raw & 0x7FFF
        if not citer:
            return False
        attr = self._u16(channel, ATTR)
        source_size = 1 << (attr & 0x7)
        dest_size = 1 << ((attr >> 8) & 0x7)
        if source_size != 4 or dest_size != 4:
            raise RuntimeError("Ssi0Dma only supports the observed 32-bit transfers")
        nbytes = self._u32(channel, NBYTES)
        if not nbytes or nbytes % source_size:
            raise RuntimeError("invalid SSI eDMA minor-loop byte count")
        source = self._u32(channel, SADDR)
        dest = self._u32(channel, DADDR)
        source_offset = _signed(self._u16(channel, SOFF), 16)
        dest_offset = _signed(self._u16(channel, DOFF), 16)
        captured = bytearray()
        for _ in range(nbytes // source_size):
            if capture_tx:
                captured += self.m.uc.mem_read(source, source_size)
            source = (source + source_offset) & 0xFFFFFFFF
            dest = (dest + dest_offset) & 0xFFFFFFFF
        self._w32(channel, SADDR, source)
        self._w32(channel, DADDR, dest)
        citer -= 1
        self._w16(channel, CITER, citer)
        if captured:
            self.tx_bytes += len(captured)
            self.tx_crc32 = zlib.crc32(captured, self.tx_crc32)
        if citer:
            return False

        csr = self._u16(channel, CSR)
        source = (source + _signed(self._u32(channel, SLAST), 32)) & 0xFFFFFFFF
        self._w32(channel, SADDR, source)
        self.major_loops[channel] += 1
        if csr & 0x0010:  # E_SG
            pointer = self._u32(channel, DLAST)
            if pointer & 0x1F:
                raise RuntimeError("SSI scatter/gather pointer is not 32-byte aligned")
            descriptor = bytes(self.m.uc.mem_read(pointer, 0x20))
            self.m.uc.mem_write(self._tcd(channel), descriptor)
            self.scatter_gathers[channel] += 1
        else:
            dest = (dest + _signed(self._u32(channel, DLAST), 32)) & 0xFFFFFFFF
            self._w32(channel, DADDR, dest)
            self._w16(channel, CITER, biter_raw)
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
        self.enabled.update(ch for ch in channels if ch in (RX_CHAN, TX_CHAN))
        if self.enabled and self.next is None:
            self.next = Fraction(self.now) + self.period

    def _on_cint(self, uc, access, address, size, value, user_data):
        if size == 1 and (value & 0x40 or (value & 0x3F) == TX_CHAN):
            self.int50_asserted = False
            self.int50_delivered = False

    def _on_intfrch1(self, uc, access, address, size, value, user_data):
        current = bytearray(uc.mem_read(INTFRCH1, 4))
        offset = address - INTFRCH1
        current[offset:offset + size] = int(value).to_bytes(size, "big")
        asserted = bool(int.from_bytes(current, "big") & INTFRCH1_SOURCE63)
        if asserted and not self.force_asserted:
            self.force_delivered = False
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


def install(machine, at, events, request_hz, instr_per_sec, force_rte):
    source = Ssi0Dma(
        machine,
        request_hz=request_hz,
        instr_per_sec=instr_per_sec,
        at=at,
        force_rte=force_rte,
    )
    events["ssi0_dma"] = source
    return source
