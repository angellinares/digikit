"""Record peripheral traffic from a Python emulator run as a replayable trace.

The trace is the specification the native peripheral models (plan P4) are
developed against: every guest access to peripheral space, every interrupt
the host raises, every change a host-side model makes to guest memory or
registers, and periodic model state, all on the emulator's instruction clock.

Use it through ``emu.longrun.build(..., mmio_recorder=rec)`` (which attaches
the recorder right after the Machine exists, so its hooks run before the
models' hooks), then ``rec.start(...)`` once the timers exist, run, and
``rec.stop()``. ``tools/mmio_record.py`` does all of this and prints the
per-peripheral report. Recording only observes: it registers hooks that read
state and wraps five Unicorn/Machine methods with pass-through wrappers.
``tools/snapeq.py`` shows a recorded run ends in the same state as an
unrecorded one.

File format, version 2
======================

Version 2 adds the opt-in ``SR`` boundary sample below. Version 1 has no
such samples and remains readable by both readers.

All integers are little-endian.

    magic    8 bytes   b"DT2MMIO\\0"
    version  u16       1
    flags    u16       bit 0: the body is a zlib (RFC 1950) stream
    hdr_len  u32
    header   hdr_len bytes of UTF-8 JSON (see Recorder.start)
    body     records until EOF, compressed as a whole when flags bit 0 is set

Each record is a u8 tag followed by a fixed layout (`RECORDS` below lists
them with their struct formats). Records carry no timestamp of their own: a
TIME record sets the clock for every record after it.

    0x01 TIME   u64 clock        guest instruction count (the timers' clock)
    0x02 STEP   u32 pc, u32 count   one emu_start call: start pc, budget (0 = none)
    0x03 RD     u32 addr, u32 value, u32 pc, u8 size   guest read, value as returned
    0x04 WR     u32 addr, u32 value, u32 pc, u8 size   guest write
    0x05 IRQ    u16 vector, u8 level (0xff = none), u8 flags, u16 source,
                u32 pc_before, u32 pc_after, u16 frame_sr
                flags bit 0: taken (a frame was pushed), bit 1: synchronous
                (trap or fault raised by the instruction at pc_before).
                frame_sr is the interrupted SR the frame holds (0 when not
                taken or when the srtrap trampoline fills it later).
    0x06 RTE    u32 pc, u16 sr    rte resumed at pc with sr (from the frame)
    0x07 HWR    u16 source, u32 addr, u32 len, len bytes   host write to guest memory
    0x08 HRD    u16 source, u32 addr, u32 len, len bytes   host read of guest memory
    0x09 HREG   u16 source, u16 reg, u32 value   host write of a CPU register
                (reg is Unicorn's m68k register id: A0=1..A7=8, D0=9..D7=16, SR=17, PC=18)
    0x0a SRC    u16 id, u16 len, len bytes   names a source id ("module.function")
    0x0b STATE  u32 len, len bytes   JSON: host model state (see Recorder._state)
    0x0c PAGE   u32 base, u32 len, len bytes   a peripheral-space slot's bytes
    0x0d MARK   u32 len, len bytes   JSON annotation (input events, window notes)
    0x0e RATE   u64 ips   the timers' instructions-per-second changed
    0x0f END    u32 len, len bytes   JSON summary; last record
    0x10 SR     u16 sr   live guest status register at a service boundary
                Recorded only when the recorder's BoundarySampler is passed
                to ``spin(..., async_events=...)``. It precedes every other
                async event and PIT service at that boundary.

Semantics a replayer needs
--------------------------

* Clock. With ``icount=False`` (the default) the clock only moves between
  ``emu_start`` calls: accesses inside a step carry the clock at the step's
  start. That is also all the Python models know: they act at step
  boundaries (timer deadlines, SSI requests) and from code/memory hooks,
  never at an instruction offset inside a step. With ``icount=True`` a
  per-instruction counter makes the clock exact inside a step as well (much
  slower; the header says ``clock_resolution``).
* Order. Records are in the order they happened. Hooks are attached before
  the models', so a guest WR precedes the model's reaction (HWR/HREG/IRQ). A
  model's read hook answers before the load completes, so the HWR that set
  up the answer precedes the RD that observes it.
* RD values are what the guest received (a READ_AFTER hook), so a model is
  checked by comparing its answer with them.
* Merging. Consecutive HWR (or HRD) records of one source on contiguous
  addresses are merged into one record (a model copying a buffer a halfword
  at a time); any other record ends the run.
* Sources. HWR/HREG are recorded for every host writer except CPU-side
  helpers (``emu.harness`` exception frames and forced MMIO values,
  ``emu.hle``, ``emu.softfloat``), which are only counted (END). That
  includes the ``unblock`` semaphore HLE (``emu.longrun.satisfy``): it is
  oracle behaviour a whole-machine replay has to reproduce. HRD is recorded
  only for peripheral models (`PERIPHERAL_READERS`): what a DMA read out of
  guest memory, which a model replayed in isolation needs as input.
* IRQ records come from wrapping ``Machine.raise_vector``: the models decide
  masking themselves (most check the IPL against SR before raising), so an
  IRQ record is a delivery. A request held back by the mask shows up only in
  the model's STATE (``pending``).
* STATE records hold JSON of the checkpoint components (the same dicts a
  snapshot stores, except the eSDHC card overlay, reduced to its length),
  simple attributes of the other models, and ``m.mmio``. PAGE records after
  each STATE hold every non-zero 16 KiB slot of peripheral space, so a model
  can resynchronise its register file mid-trace.

A Rust reader: read 16 bytes, parse the header JSON (``serde_json``), wrap
the rest in ``flate2::read::ZlibDecoder`` when flags bit 0 is set, then read
tag bytes and fixed little-endian fields (``byteorder`` or
``u32::from_le_bytes``). Unknown tags are an error: every record's length is
determined by its tag, so a reader cannot skip one it does not know.
"""

from __future__ import annotations

import collections
import json
import struct
import sys
import zlib
from collections.abc import Callable, Iterator
from typing import Any, BinaryIO, NamedTuple

from unicorn.m68k_const import UC_M68K_REG_A7, UC_M68K_REG_PC, UC_M68K_REG_SR

MAGIC = b"DT2MMIO\x00"
VERSION = 2
SUPPORTED_VERSIONS = frozenset((1, VERSION))
FLAG_ZLIB = 1

TIME, STEP, RD, WR, IRQ, RTE, HWR, HRD, HREG, SRC, STATE, PAGE, MARK, RATE, END, SR = (
    range(1, 17)
)

# tag -> (name, fixed-part struct, has trailing bytes)
RECORDS: dict[int, tuple[str, struct.Struct, bool]] = {
    TIME: ("TIME", struct.Struct("<Q"), False),
    STEP: ("STEP", struct.Struct("<II"), False),
    RD: ("RD", struct.Struct("<IIIB"), False),
    WR: ("WR", struct.Struct("<IIIB"), False),
    IRQ: ("IRQ", struct.Struct("<HBBHIIH"), False),
    RTE: ("RTE", struct.Struct("<IH"), False),
    HWR: ("HWR", struct.Struct("<HII"), True),
    HRD: ("HRD", struct.Struct("<HII"), True),
    HREG: ("HREG", struct.Struct("<HHI"), False),
    SRC: ("SRC", struct.Struct("<HH"), True),
    STATE: ("STATE", struct.Struct("<I"), True),
    PAGE: ("PAGE", struct.Struct("<II"), True),
    MARK: ("MARK", struct.Struct("<I"), True),
    RATE: ("RATE", struct.Struct("<Q"), False),
    END: ("END", struct.Struct("<I"), True),
    SR: ("SR", struct.Struct("<H"), False),
}
_PREAMBLE = struct.Struct("<8sHHI")

IRQ_TAKEN = 1
IRQ_SYNC = 2
NO_LEVEL = 0xFF

# Peripheral space hooked by default: the rapid-GPIO/FlexBus window the DSP
# port sits in, and everything from non-cacheable FlexBus space up through
# both peripheral bus controllers. DDR (0x4...) and the internal SRAM
# backdoor (0x80000000) are memory and are left alone.
DEFAULT_RANGES = ((0x8C000000, 0x8FFFFFFF), (0xC0000000, 0xFFFFFFFF))
SLOT = 0x4000
PAGE_SIZE = 0x100000

# Modules whose host reads of guest memory are recorded (HRD).
PERIPHERAL_READERS = frozenset(
    {
        "emu.edma",
        "emu.ssi",
        "emu.dspi2",
        "emu.esdhc",
        "emu.gpio",
        "emu.dsp",
        "emu.pit",
        "emu.dtim",
        "emu.panelin",
        "emu.serial",
        "emu.livesharc",
    }
)
# Modules whose host writes are only counted: CPU-side, not peripheral.
COUNT_ONLY = frozenset({"emu.harness", "emu.hle", "emu.softfloat"})
# Never recorded (restore/save plumbing and this module).
IGNORED = frozenset({"emu.snapshot", "emu.checkpoint", __name__})

EXCP_RTE = 0x100


# -- peripheral map ----------------------------------------------------------

# MCF5441x reference manual, Tables 1-3 and 1-4 (16 KiB slots), plus the
# rapid-GPIO window the firmware uses as the DSP coprocessor port. The lane
# column is plan P4's split of the Rust models.
SLOTS: dict[int, tuple[str, str]] = {
    0xFC004000: ("Crossbar switch", "system"),
    0xFC008000: ("FlexBus controller", "system"),
    0xFC020000: ("FlexCAN0", "other"),
    0xFC024000: ("FlexCAN1", "other"),
    0xFC038000: ("I2C1", "gpio-uart-panel"),
    0xFC03C000: ("DSPI1", "dma-ssi-dspi"),
    0xFC040000: ("SCM", "system"),
    0xFC044000: ("eDMA", "dma-ssi-dspi"),
    0xFC048000: ("INTC0", "timers-intc"),
    0xFC04C000: ("INTC1", "timers-intc"),
    0xFC050000: ("INTC2", "timers-intc"),
    0xFC054000: ("INTC IACK", "timers-intc"),
    0xFC058000: ("I2C0", "gpio-uart-panel"),
    0xFC05C000: ("DSPI0", "dma-ssi-dspi"),
    0xFC060000: ("UART0", "gpio-uart-panel"),
    0xFC064000: ("UART1", "gpio-uart-panel"),
    0xFC068000: ("UART2", "gpio-uart-panel"),
    0xFC06C000: ("UART3", "gpio-uart-panel"),
    0xFC070000: ("DTIM0", "timers-intc"),
    0xFC074000: ("DTIM1", "timers-intc"),
    0xFC078000: ("DTIM2", "timers-intc"),
    0xFC07C000: ("DTIM3", "timers-intc"),
    0xFC080000: ("PIT0", "timers-intc"),
    0xFC084000: ("PIT1", "timers-intc"),
    0xFC088000: ("PIT2", "timers-intc"),
    0xFC08C000: ("PIT3", "timers-intc"),
    0xFC090000: ("Edge port", "gpio-uart-panel"),
    0xFC094000: ("ADC", "other"),
    0xFC098000: ("DAC0", "other"),
    0xFC09C000: ("DAC1", "other"),
    0xFC0A8000: ("RTC", "system"),
    0xFC0AC000: ("SIM", "system"),
    0xFC0B0000: ("USB OTG", "other"),
    0xFC0B4000: ("USB host", "other"),
    0xFC0B8000: ("DDR controller", "system"),
    0xFC0BC000: ("SSI0", "dma-ssi-dspi"),
    0xFC0C0000: ("PLL", "system"),
    0xFC0C4000: ("RNG", "system"),
    0xFC0C8000: ("SSI1", "dma-ssi-dspi"),
    0xFC0CC000: ("eSDHC", "esdhc-card"),
    0xFC0D4000: ("MAC-NET0", "other"),
    0xFC0D8000: ("MAC-NET1", "other"),
    0xFC0DC000: ("L2 switch 0", "other"),
    0xFC0E0000: ("L2 switch 1", "other"),
    0xFC0FC000: ("NAND flash controller", "other"),
    0xEC008000: ("1-Wire", "other"),
    0xEC010000: ("I2C2", "gpio-uart-panel"),
    0xEC014000: ("I2C3", "gpio-uart-panel"),
    0xEC018000: ("I2C4", "gpio-uart-panel"),
    0xEC01C000: ("I2C5", "gpio-uart-panel"),
    0xEC038000: ("DSPI2", "dma-ssi-dspi"),
    0xEC03C000: ("DSPI3", "dma-ssi-dspi"),
    0xEC060000: ("UART4", "gpio-uart-panel"),
    0xEC064000: ("UART5", "gpio-uart-panel"),
    0xEC068000: ("UART6", "gpio-uart-panel"),
    0xEC06C000: ("UART7", "gpio-uart-panel"),
    0xEC070000: ("UART8", "gpio-uart-panel"),
    0xEC074000: ("UART9", "gpio-uart-panel"),
    0xEC088000: ("mcPWM", "other"),
    0xEC090000: ("CCM/reset/power", "system"),
    0xEC094000: ("GPIO (pin mux)", "gpio-uart-panel"),
    0x8C000000: ("DSP port (rapid-GPIO window)", "dma-ssi-dspi"),
}
EDMA_TCD_BASE = 0xFC045000
EDMA_TCD_SIZE = 0x20
# Channel uses the emulator knows about (emu/edma.py, emu/panelin.py,
# emu/dspi2.py, emu/ssi.py, emu/esdhc.py).
EDMA_CHANNELS = {
    28: "DSPI2 RX",
    29: "DSPI2 TX",
    34: "UART8 RX (panel)",
    35: "UART8 TX (console)",
    48: "SSI0 RX",
    50: "SSI0 TX",
    59: "eSDHC",
}
# Registers a Python model hooks, forces or reads, in the configuration
# tools/mmio_record.py runs (PITs 0/2/3, DTIM3, eDMA channels 28/29 with
# --audio, 34, 35 and 59; no SSI0 model), for the "unmodelled" flag. Ranges
# are inclusive. A register outside these behaves as plain RAM in the Python
# emulator: reads return the last value written.
_TCD = EDMA_TCD_BASE
MODELLED: tuple[tuple[int, int, str], ...] = (
    (0xFC080000, 0xFC080003, "emu.pit PIT0 PCSR/PMR"),
    (0xFC088000, 0xFC088003, "emu.pit PIT2 PCSR/PMR"),
    (0xFC08C000, 0xFC08C003, "emu.pit PIT3 PCSR/PMR"),
    (0xFC07C000, 0xFC07C00F, "emu.dtim DTIM3"),
    (0xFC044018, 0xFC044018, "eDMA SERQ (edma/dspi2/esdhc/ssi hooks)"),
    (0xFC04401C, 0xFC04401C, "eDMA CINT (dspi2/ssi hooks)"),
    (_TCD + 28 * 0x20, _TCD + 30 * 0x20 - 1, "emu.dspi2 TCD28/29"),
    (_TCD + 34 * 0x20, _TCD + 36 * 0x20 - 1, "emu.panelin/emu.edma TCD34/35"),
    (_TCD + 59 * 0x20, _TCD + 60 * 0x20 - 1, "emu.esdhc TCD59"),
    (0xFC048008, 0xFC04800F, "emu.pit.interrupt_level INTC0 IMR"),
    (0xFC048040, 0xFC04807F, "emu.pit.interrupt_level INTC0 ICR"),
    (0xFC04C008, 0xFC04C00F, "emu.pit.interrupt_level INTC1 IMR"),
    (0xFC04C040, 0xFC04C07F, "emu.pit.interrupt_level INTC1 ICR"),
    (0xFC050008, 0xFC05000F, "emu.pit.interrupt_level INTC2 IMR"),
    (0xFC050040, 0xFC05007F, "emu.pit.interrupt_level INTC2 ICR"),
    (0xFC0CC000, 0xFC0CFFFF, "emu.esdhc"),
    (0xEC09401A, 0xEC09401B, "emu.gpio.SdGate PPDSDR_C/D"),
    (0xEC094027, 0xEC094027, "emu.gpio.SdGate PCLRR_D"),
    (0xEC070004, 0xEC070007, "emu.longrun UART8 USR"),
    (0xEC07000C, 0xEC07000F, "emu.longrun UART8 UDR"),
    (0x8C000002, 0x8C000003, "emu.dsp DSP port status/data"),
    (0x8C00000A, 0x8C00000B, "emu.dsp DSP port latch"),
    (0xFC05C02C, 0xFC05C02F, "m.mmio forced (DSPI0 SR)"),
    (0xEC03802C, 0xEC03802F, "m.mmio forced (DSPI2 SR)"),
)


def slot_of(addr: int) -> int:
    return addr & ~(SLOT - 1)


def peripheral_of(addr: int) -> tuple[str, str]:
    """-> (name, lane) for a peripheral-space address; eDMA TCDs by channel."""
    if EDMA_TCD_BASE <= addr < EDMA_TCD_BASE + 64 * EDMA_TCD_SIZE:
        chan = (addr - EDMA_TCD_BASE) // EDMA_TCD_SIZE
        use = EDMA_CHANNELS.get(chan)
        return ("eDMA TCD%d%s" % (chan, " (%s)" % use if use else ""), "dma-ssi-dspi")
    hit = SLOTS.get(slot_of(addr))
    if hit is not None:
        return hit
    if 0x8C000000 <= addr <= 0x8FFFFFFF:
        return ("rapid-GPIO window 0x%08x" % slot_of(addr), "other")
    return ("unmapped slot 0x%08x" % slot_of(addr), "unknown")


def modelled_by(addr: int) -> str | None:
    for lo, hi, what in MODELLED:
        if lo <= addr <= hi:
            return what
    return None


# -- recording ----------------------------------------------------------------


class Recorder:
    """Attach to a Machine, record while armed, write the trace file.

    ``attach(m)`` installs everything disarmed; ``start(...)`` writes the
    header and arms; ``stop()`` writes END, closes the file and removes the
    method wrappers (the Unicorn hooks stay, disarmed).
    """

    def __init__(
        self,
        path: str,
        *,
        ranges: tuple[tuple[int, int], ...] = DEFAULT_RANGES,
        icount: bool = False,
        state_every: int = 10_000_000,
        compress: bool = True,
        level: int = 6,
    ) -> None:
        self.path = path
        self.ranges = tuple(ranges)
        self.icount = icount
        self.state_every = state_every
        self.compress = compress
        self.level = level
        self.armed = False
        self.m: Any = None
        self.file: BinaryIO | None = None
        self._z: Any = None
        self._buf = bytearray()
        self._clock_fn: Callable[[], int] = lambda: 0
        self._rate_fn: Callable[[], int] | None = None
        self._last_clock: int | None = None
        self._last_rate: int | None = None
        self._next_state: int | None = None
        self._state_fn: Callable[[], dict] | None = None
        self._ic = 0
        self._in_step = False
        self._step_base = 0
        self._sources: dict[Any, tuple[int, str]] = {}
        self._source_names: list[str] = []
        self.counts: collections.Counter[str] = collections.Counter()
        self.count_only: collections.Counter[str] = collections.Counter()
        self.errors = 0
        self.first_error: str | None = None
        self._orig: dict[str, Any] = {}
        self.bytes_raw = 0
        # A host op waiting to be merged with the next one: [tag, sid, addr, data].
        self._pend: list | None = None

    # -- attach / arm --------------------------------------------------------
    def attach(self, m: Any) -> None:
        """Install hooks and wrappers on a fresh Machine, disarmed.

        Called by emu.longrun.build right after ``Machine()`` so that these
        hooks precede every model's hooks of the same type.
        """
        from unicorn import UC_HOOK_CODE, UC_HOOK_INTR, UC_HOOK_MEM_WRITE
        from unicorn.unicorn_py3.unicorn import (
            HOOK_MEM_ACCESS_CFUNC,
            uccallback,
        )

        try:
            from unicorn import UC_HOOK_MEM_READ_AFTER
        except ImportError as exc:  # pragma: no cover - pinned unicorn has it
            raise RuntimeError(
                "the MMIO recorder needs UC_HOOK_MEM_READ_AFTER"
            ) from exc
        if self.m is not None:
            raise RuntimeError("recorder already attached")
        self.m = m
        uc = m.uc
        self._reg_read = uc.reg_read
        self._mem_read_raw = uc.mem_read
        if self.icount:
            uc.hook_add(UC_HOOK_CODE, self._on_code)
        uc.hook_add(UC_HOOK_INTR, self._on_intr)
        # The Python binding does not map READ_AFTER in hook_add, so register
        # it through the binding's own plumbing (same callback type as the
        # other memory hooks: uc, access, address, size, value, key).
        read_cb = uccallback(uc, HOOK_MEM_ACCESS_CFUNC)(self._on_read)
        for begin, end in self.ranges:
            uc._Uc__do_hook_add(UC_HOOK_MEM_READ_AFTER, read_cb, begin, end)
            uc.hook_add(UC_HOOK_MEM_WRITE, self._on_write, begin=begin, end=end)
        self._wrap(uc, "mem_write", self._wrap_mem_write)
        self._wrap(uc, "mem_read", self._wrap_mem_read)
        self._wrap(uc, "reg_write", self._wrap_reg_write)
        self._wrap(uc, "emu_start", self._wrap_emu_start)
        self._wrap(m, "raise_vector", self._wrap_raise_vector)

    def _wrap(self, obj: Any, name: str, factory: Callable[[Any], Any]) -> None:
        orig = getattr(obj, name)
        self._orig[name] = (obj, orig)
        setattr(obj, name, factory(orig))

    def start(
        self,
        header: dict,
        clock: Callable[[], int],
        rate: Callable[[], int] | None = None,
        state: Callable[[], dict] | None = None,
    ) -> None:
        """Write the header and arm.

        ``header`` is caller-supplied JSON (snapshot and image hashes, build
        configuration, rates); the format fields are added here. ``clock``
        returns the guest instruction count (``timers.now``), ``rate`` the
        timers' instructions per second, ``state`` the model-state dict for
        STATE records.
        """
        if self.m is None:
            raise RuntimeError("attach() before start()")
        head = dict(header)
        head.update(
            {
                "format": "dt2-mmio-trace",
                "version": VERSION,
                "clock_resolution": "instruction" if self.icount else "step",
                "ranges": [[lo, hi] for lo, hi in self.ranges],
                "state_every": self.state_every,
                "records": {
                    "%#04x" % tag: [name, spec.format, trailing]
                    for tag, (name, spec, trailing) in RECORDS.items()
                },
            }
        )
        blob = json.dumps(head, sort_keys=True, default=_json_default).encode()
        self.file = open(self.path, "wb")  # noqa: SIM115 - closed by stop()
        self.file.write(
            _PREAMBLE.pack(MAGIC, VERSION, FLAG_ZLIB if self.compress else 0, len(blob))
        )
        self.file.write(blob)
        if self.compress:
            self._z = zlib.compressobj(self.level)
        self._clock_fn = clock
        self._rate_fn = rate
        self._state_fn = state
        self.armed = True
        self._tick()
        self._check_rate()
        self._emit_state()

    def stop(self, extra: dict | None = None) -> dict:
        """Write END, close the file, and remove the method wrappers."""
        if self.file is None:
            return {}
        self._tick()
        self._emit_state()
        if self._pend is not None:
            self._flush_pend()
        summary = {
            "counts": dict(self.counts),
            "count_only_sources": dict(self.count_only),
            "sources": self._source_names,
            "errors": self.errors,
            "first_error": self.first_error,
            "clock": self._clock(),
        }
        if extra:
            summary.update(extra)
        self._emit_blob(END, summary)
        self.armed = False
        self._flush(final=True)
        self.file.close()
        self.file = None
        for name, (obj, _orig) in self._orig.items():
            # Instance attributes shadow the class methods; deleting them
            # restores the originals exactly.
            if name in vars(obj):
                delattr(obj, name)
        self._orig.clear()
        return summary

    # -- output --------------------------------------------------------------
    def _put(self, tag: int, *fields: Any) -> None:
        self._buf += RECORDS[tag][1].pack(*fields)
        self.counts[RECORDS[tag][0]] += 1

    def _emit(self, tag: int, *fields: Any, data: bytes = b"") -> None:
        if self._pend is not None:
            self._flush_pend()
        self._buf.append(tag)
        self._put(tag, *fields)
        if data:
            self._buf += data
        if len(self._buf) >= 1 << 20:
            self._flush()

    def _host_op(self, tag: int, sid: int, address: int, raw: bytes) -> None:
        """HWR/HRD, merging a run of one source's operations on contiguous
        addresses (a model copying a buffer a halfword at a time) into one
        record. Any other record flushes the run first, so order holds."""
        address &= 0xFFFFFFFF
        pend = self._pend
        if (
            pend is not None
            and pend[0] == tag
            and pend[1] == sid
            and pend[2] + len(pend[3]) == address
            and len(pend[3]) < 1 << 16
        ):
            pend[3] += raw
            return
        if pend is not None:
            self._flush_pend()
        self._pend = [tag, sid, address, bytearray(raw)]

    def _flush_pend(self) -> None:
        pend = self._pend
        assert pend is not None
        self._pend = None
        tag, sid, address, data = pend
        self._emit(tag, sid, address, len(data), data=bytes(data))

    def _emit_blob(self, tag: int, obj: Any) -> None:
        raw = json.dumps(obj, sort_keys=True, default=_json_default).encode()
        self._emit(tag, len(raw), data=raw)

    def _flush(self, final: bool = False) -> None:
        if self.file is None:
            return
        data = bytes(self._buf)
        self._buf.clear()
        self.bytes_raw += len(data)
        if self._z is not None:
            out = self._z.compress(data)
            if final:
                out += self._z.flush()
            self.file.write(out)
        else:
            self.file.write(data)

    def _clock(self) -> int:
        if self._in_step and self.icount:
            return self._step_base + max(0, self._ic - 1)
        return int(self._clock_fn())

    def _tick(self) -> None:
        c = self._clock()
        if c != self._last_clock:
            self._last_clock = c
            self._emit(TIME, c)

    def _check_rate(self) -> None:
        if self._rate_fn is None:
            return
        r = int(self._rate_fn())
        if r != self._last_rate:
            self._last_rate = r
            self._emit(RATE, r)

    def _emit_state(self) -> None:
        if self._state_fn is not None:
            try:
                self._emit_blob(STATE, self._state_fn())
            except Exception as exc:  # observation must never stop the run
                self._error(exc)
        self._emit_pages()
        if self.state_every:
            self._next_state = self._clock() + self.state_every

    def _emit_pages(self) -> None:
        m = self.m
        for base in sorted(m.mapped):
            if not any(lo <= base <= hi for lo, hi in self.ranges):
                continue
            page = bytes(self._mem_read_raw(base, PAGE_SIZE))
            for off in range(0, PAGE_SIZE, SLOT):
                chunk = page[off : off + SLOT]
                if chunk.count(0) != SLOT:
                    self._emit(PAGE, base + off, SLOT, data=chunk)

    def mark(self, obj: Any) -> None:
        """Add a MARK record (input events, notes) at the current clock."""
        if self.armed:
            self._tick()
            self._emit_blob(MARK, obj)

    def _error(self, exc: BaseException) -> None:
        self.errors += 1
        if self.first_error is None:
            self.first_error = "%s: %s" % (type(exc).__name__, exc)

    def _source(self, frame: Any) -> tuple[int, str]:
        """-> (source id, module) for the Python frame that called a host op."""
        code = frame.f_code
        hit = self._sources.get(code)
        if hit is None:
            module = frame.f_globals.get("__name__", "?")
            name = "%s.%s" % (module, getattr(code, "co_qualname", code.co_name))
            sid = len(self._source_names)
            self._source_names.append(name)
            hit = (sid, module)
            self._sources[code] = hit
            raw = name.encode()
            self._emit(SRC, sid, len(raw), data=raw)
        return hit

    # -- hooks ---------------------------------------------------------------
    def _on_code(self, uc: Any, addr: int, size: int, data: Any) -> None:
        self._ic += 1

    def _on_read(
        self, uc: Any, access: int, addr: int, size: int, value: int, key: Any
    ) -> None:
        if not self.armed:
            return
        try:
            self._tick()
            self._emit(
                RD,
                addr & 0xFFFFFFFF,
                value & ((1 << (8 * size)) - 1) & 0xFFFFFFFF,
                self._reg_read(UC_M68K_REG_PC) & 0xFFFFFFFF,
                size,
            )
        except Exception as exc:
            self._error(exc)

    def _on_write(
        self, uc: Any, access: int, addr: int, size: int, value: int, data: Any
    ) -> None:
        if not self.armed:
            return
        try:
            self._tick()
            self._emit(
                WR,
                addr & 0xFFFFFFFF,
                value & ((1 << (8 * size)) - 1) & 0xFFFFFFFF,
                self._reg_read(UC_M68K_REG_PC) & 0xFFFFFFFF,
                size,
            )
        except Exception as exc:
            self._error(exc)

    def _on_intr(self, uc: Any, intno: int, data: Any) -> None:
        # Runs before install_exceptions' handler (attached first), so for
        # rte the frame is still at A7.
        if not self.armed or intno != EXCP_RTE:
            return
        try:
            sp = self._reg_read(UC_M68K_REG_A7)
            _fmt, sr, pc = struct.unpack(">HHI", self._mem_read_raw(sp, 8))
            self._tick()
            self._emit(RTE, pc, sr)
        except Exception as exc:
            self._error(exc)

    # -- wrappers --------------------------------------------------------------
    def _wrap_mem_write(self, orig: Any) -> Any:
        def mem_write(address: int, data: bytes) -> Any:
            if self.armed:
                try:
                    self._host_write(sys._getframe(1), address, data)
                except Exception as exc:
                    self._error(exc)
            return orig(address, data)

        return mem_write

    def _host_write(self, frame: Any, address: int, data: bytes) -> None:
        sid, module = self._source(frame)
        if module in IGNORED:
            return
        if module in COUNT_ONLY:
            self.count_only[self._source_names[sid]] += 1
            return
        self._tick()
        raw = bytes(data)
        self._host_op(HWR, sid, address, raw)

    def _wrap_mem_read(self, orig: Any) -> Any:
        def mem_read(address: int, size: int) -> Any:
            out = orig(address, size)
            if self.armed:
                try:
                    frame = sys._getframe(1)
                    sid, module = self._source(frame)
                    if module in PERIPHERAL_READERS:
                        self._tick()
                        raw = bytes(out)
                        self._host_op(HRD, sid, address, raw)
                except Exception as exc:
                    self._error(exc)
            return out

        return mem_read

    def _wrap_reg_write(self, orig: Any) -> Any:
        def reg_write(reg_id: int, value: Any) -> Any:
            if self.armed:
                try:
                    sid, module = self._source(sys._getframe(1))
                    if module in COUNT_ONLY:
                        self.count_only[self._source_names[sid] + " (reg)"] += 1
                    elif module not in IGNORED:
                        self._tick()
                        self._emit(HREG, sid, reg_id, int(value) & 0xFFFFFFFF)
                except Exception as exc:
                    self._error(exc)
            return orig(reg_id, value)

        return reg_write

    def boundary_sampler(self) -> Any:
        """A read-only ``spin`` async event that records live SR first.

        The caller passes this only for recording runs, and before any other
        async event. ``spin`` invokes async events before ``pits.service``,
        making the sample the replay contract for that service boundary.
        """
        return _BoundarySampler(self)

    def sample_sr_boundary(self) -> None:
        """Record live SR at the current service boundary without mutation."""
        if not self.armed:
            return
        try:
            self._tick()
            self._emit(SR, self._reg_read(UC_M68K_REG_SR) & 0xFFFF)
        except Exception as exc:
            self._error(exc)

    def _wrap_emu_start(self, orig: Any) -> Any:
        def emu_start(begin: int, until: int, timeout: int = 0, count: int = 0) -> Any:
            if not self.armed:
                return orig(begin, until, timeout, count)
            try:
                self._check_rate()
                if self._next_state is not None and self._clock() >= self._next_state:
                    self._tick()
                    self._emit_state()
                self._tick()
                self._emit(STEP, begin & 0xFFFFFFFF, count & 0xFFFFFFFF)
            except Exception as exc:
                self._error(exc)
            self._step_base = int(self._clock_fn())
            self._ic = 0
            self._in_step = True
            try:
                return orig(begin, until, timeout, count)
            finally:
                self._in_step = False

        return emu_start

    def _wrap_raise_vector(self, orig: Any) -> Any:
        def raise_vector(
            vec: int, from_instruction: bool = False, level: int | None = None
        ) -> bool:
            if not self.armed:
                return orig(vec, from_instruction=from_instruction, level=level)
            pc0 = self._reg_read(UC_M68K_REG_PC)
            # The frame goes at A7 - 8 of the interrupted context; A7 itself
            # may be a different stack once the handler's SR is installed.
            sp0 = self._reg_read(UC_M68K_REG_A7)
            taken = orig(vec, from_instruction=from_instruction, level=level)
            try:
                pc1 = self._reg_read(UC_M68K_REG_PC)
                sr = 0
                if taken and self.m.srtrap is None:
                    sr = struct.unpack(">H", self._mem_read_raw(sp0 - 6, 2))[0]
                sid, _module = self._source(sys._getframe(1))
                flags = (IRQ_TAKEN if taken else 0) | (
                    IRQ_SYNC if from_instruction else 0
                )
                self._tick()
                self._emit(
                    IRQ,
                    vec & 0xFFFF,
                    NO_LEVEL if level is None else level & 0xFF,
                    flags,
                    sid,
                    pc0 & 0xFFFFFFFF,
                    pc1 & 0xFFFFFFFF,
                    sr,
                )
            except Exception as exc:
                self._error(exc)
            return taken

        return raise_vector


def _json_default(obj: Any) -> Any:
    if isinstance(obj, (set, frozenset, tuple)):
        return list(obj)
    if isinstance(obj, (bytes, bytearray)):
        return obj.hex()
    return repr(obj)


# -- model state for STATE records ----------------------------------------------


def _simple_attrs(obj: Any) -> dict:
    out: dict[str, Any] = {"type": type(obj).__name__}
    for key, value in vars(obj).items():
        if key.startswith("_"):
            continue
        if isinstance(value, (int, float, str, bool)) or value is None:
            out[key] = value
        elif (
            isinstance(value, (dict, list, tuple, collections.Counter))
            and len(value) <= 64
        ):
            try:
                json.dumps(value, default=_json_default)
                out[key] = value
            except (TypeError, ValueError):
                pass
    return out


def component_state(component: Any) -> Any:
    """A component's checkpoint dict, cheap enough to take every few million
    instructions: the eSDHC card overlay (one dict entry per written byte) is
    reduced to its length."""
    card = getattr(component, "card", None)
    if card is not None and hasattr(card, "overlay"):
        return {
            "type": type(component).__name__,
            "pattern": getattr(component, "pattern", None),
            "armed": getattr(component, "armed", None),
            "dma_bytes": getattr(component, "dma_bytes", None),
            "card_blocks": card.blocks,
            "card_rca": card.rca,
            "card_selected": card.selected,
            "card_overlay_len": len(card.overlay),
        }
    if hasattr(component, "checkpoint_state"):
        return component.checkpoint_state()
    if isinstance(component, collections.deque):
        return {"type": "deque", "values": list(component)}
    return _simple_attrs(component)


def machine_state(m: Any, ev: dict, extra: dict | None = None) -> dict:
    """The STATE record for a longrun.build machine: checkpoint components,
    the other models' simple attributes, and the forced MMIO values."""
    comps = ev.get("checkpoint_components", {})
    state: dict[str, Any] = {
        "components": {name: component_state(c) for name, c in comps.items()},
        "mmio": {"%#010x" % a: v for a, v in m.mmio.items()},
    }
    others = {}
    for key in ("dspi2", "sdgate", "idle"):
        obj = ev.get(key)
        if obj is not None and key not in comps:
            others[key] = _simple_attrs(obj)
    state["models"] = others
    if extra:
        state.update(extra)
    return state


# -- reading ---------------------------------------------------------------------


class _BoundarySampler:
    """``spin``-compatible no-deadline event used only by Recorder mode."""

    def __init__(self, recorder: Recorder) -> None:
        self.recorder = recorder

    def step(self, now: int, _limit: Any) -> None:
        return None

    def service(self, _now: int) -> None:
        self.recorder.sample_sr_boundary()


class Record(NamedTuple):
    tag: int
    name: str
    clock: int
    fields: tuple
    data: bytes


class Reader:
    """Iterate a trace file's records. ``header`` is the parsed JSON header;
    ``sources`` fills in as SRC records are read."""

    def __init__(self, path: str) -> None:
        self.path = path
        with open(path, "rb") as fh:
            pre = fh.read(_PREAMBLE.size)
            magic, version, flags, hlen = _PREAMBLE.unpack(pre)
            if magic != MAGIC:
                raise ValueError("%s: not a DT2MMIO trace" % path)
            if version not in SUPPORTED_VERSIONS:
                raise ValueError(
                    "%s: trace version %d, reader supports %s"
                    % (path, version, sorted(SUPPORTED_VERSIONS))
                )
            self.version = version
            self.flags = flags
            self.header = json.loads(fh.read(hlen))
            self._body_at = _PREAMBLE.size + hlen
        self.sources: dict[int, str] = {}

    def _body(self) -> Iterator[bytes]:
        with open(self.path, "rb") as fh:
            fh.seek(self._body_at)
            z = zlib.decompressobj() if self.flags & FLAG_ZLIB else None
            while True:
                chunk = fh.read(1 << 20)
                if not chunk:
                    break
                yield z.decompress(chunk) if z is not None else chunk
            if z is not None:
                tail = z.flush()
                if tail:
                    yield tail

    def __iter__(self) -> Iterator[Record]:
        buf = bytearray()
        pos = 0
        clock = 0
        body = self._body()

        def need(n: int) -> bool:
            nonlocal buf, pos
            while len(buf) - pos < n:
                try:
                    more = next(body)
                except StopIteration:
                    return False
                if pos:
                    del buf[:pos]
                    pos = 0
                buf += more
            return True

        while need(1):
            tag = buf[pos]
            spec = RECORDS.get(tag)
            if spec is None:
                raise ValueError("unknown record tag %#x" % tag)
            name, st, trailing = spec
            if not need(1 + st.size):
                raise ValueError("truncated %s record" % name)
            fields = st.unpack_from(buf, pos + 1)
            pos += 1 + st.size
            data = b""
            if trailing:
                n = fields[-1]
                if not need(n):
                    raise ValueError("truncated %s data" % name)
                data = bytes(buf[pos : pos + n])
                pos += n
            if tag == TIME:
                clock = fields[0]
            elif tag == SRC:
                self.sources[fields[0]] = data.decode()
            yield Record(tag, name, clock, fields, data)

    def blobs(self, tag: int) -> Iterator[tuple[int, Any]]:
        """-> (clock, parsed JSON) for every STATE/MARK/END record."""
        for rec in self:
            if rec.tag == tag:
                yield rec.clock, json.loads(rec.data)


def summarize(path: str) -> dict:
    """Per-peripheral traffic of one trace: guest reads/writes per slot and per
    register, host writes/reads per source, IRQs per vector and source."""
    rd = Reader(path)
    per: dict[str, collections.Counter] = collections.defaultdict(collections.Counter)
    regs: dict[str, collections.Counter] = collections.defaultdict(collections.Counter)
    unmod: dict[str, collections.Counter] = collections.defaultdict(collections.Counter)
    host_src: collections.Counter = collections.Counter()
    host_bytes: collections.Counter = collections.Counter()
    host_regions: dict[str, collections.Counter] = collections.defaultdict(
        collections.Counter
    )
    irqs: collections.Counter = collections.Counter()
    irq_untaken: collections.Counter = collections.Counter()
    tags: collections.Counter = collections.Counter()
    tag_bytes: collections.Counter = collections.Counter()
    first_clock = last_clock = None
    end: Any = None
    for rec in rd:
        size = 1 + RECORDS[rec.tag][1].size + len(rec.data)
        tags[rec.name] += 1
        tag_bytes[rec.name] += size
        if first_clock is None:
            first_clock = rec.clock
        last_clock = rec.clock
        if rec.tag in (RD, WR):
            addr = rec.fields[0]
            name, lane = peripheral_of(addr)
            key = "%s|%s" % (name, lane)
            per[key]["reads" if rec.tag == RD else "writes"] += 1
            per[key]["bytes"] += size
            regs[key][
                "%#010x %s%d" % (addr, "R" if rec.tag == RD else "W", rec.fields[3] * 8)
            ] += 1
            if modelled_by(addr) is None:
                unmod[key][
                    "%#010x %s%d"
                    % (addr, "R" if rec.tag == RD else "W", rec.fields[3] * 8)
                ] += 1
        elif rec.tag in (HWR, HRD):
            src = rd.sources.get(rec.fields[0], "?")
            kind = "write" if rec.tag == HWR else "read"
            host_src["%s %s" % (src, kind)] += 1
            host_bytes["%s %s" % (src, kind)] += rec.fields[2]
            addr = rec.fields[1]
            region = (
                peripheral_of(addr)[0]
                if any(lo <= addr <= hi for lo, hi in DEFAULT_RANGES)
                else "RAM %#x" % (addr & ~0xFFFFF)
            )
            host_regions["%s %s" % (src, kind)][region] += 1
        elif rec.tag == HREG:
            host_src["%s reg" % rd.sources.get(rec.fields[0], "?")] += 1
        elif rec.tag == IRQ:
            vec, _lvl, flags, sid = rec.fields[:4]
            key = "%d %s%s" % (
                vec,
                rd.sources.get(sid, "?"),
                " sync" if flags & IRQ_SYNC else "",
            )
            irqs[key] += 1
            if not flags & IRQ_TAKEN:
                irq_untaken[key] += 1
        elif rec.tag == END:
            end = json.loads(rec.data)
    return {
        "header": rd.header,
        "clock": [first_clock, last_clock],
        "tags": dict(tags),
        "tag_bytes": dict(tag_bytes),
        "peripherals": {k: dict(v) for k, v in per.items()},
        "registers": {k: dict(v.most_common()) for k, v in regs.items()},
        "unmodelled": {k: dict(sorted(v.items())) for k, v in unmod.items()},
        "host": {k: {"n": v, "bytes": host_bytes[k]} for k, v in host_src.items()},
        "host_regions": {k: dict(v) for k, v in host_regions.items()},
        "irqs": dict(irqs),
        "irq_untaken": dict(irq_untaken),
        "end": end,
    }
