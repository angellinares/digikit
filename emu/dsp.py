"""The 0x8C000000 coprocessor port's ready line.

`0x8C000000` is a FlexBus-attached coprocessor addressed in 4 KB pages. The
transfer primitive `0x400cf4a8(word)` writes four bytes most-significant
first, each as two 16-bit writes to `0x8C000002` -- `(byte << 8) | 0x80`
then `(byte << 8)` -- so bit 7 of the low byte is a write clock strobe and
the data rides in the high byte. Before the burst it writes `0x80` to
`0x8C00000A` and then spins at `0x400cf4ec` until bit 0 of a 16-bit read of
`0x8C000002` is set. That bit is the device's ready line.

`0x400cfd40(cmd, buf, swap)` locks the scheduler, sends `cmd` as a
four-byte header, then sends 4096 bytes from `buf` in four-byte groups,
then sleeps 100 microseconds through `0x40128c7c`. A `cmd` of `0xFFFFFFFF`
marks a command block; anything else is a page index, from
`0x40146148(addr, src, len)` which writes `len` bytes as 4 KB pages.

Nothing in the emulator backed that address, so bit 0 read as zero forever
and the priority-3 job worker `0x400f1fce` wedged in the spin on the very
first burst -- measured as 9.7M of 60M instructions in a two-instruction
loop, with `0x400cf4a8` entered exactly once and never returning. Because
that worker never finished its first job, none of the five queued jobs
(`KitActive::updateSingleMirror`, `saveProjectToMmc(tempProject)`,
`Migrate presets`, `Update MMC Caches`, `Load all samples`) ever ran.

State honestly what this model is and is not: it makes the ready line
readable, and **the pacing comes from the firmware's own 100 microsecond
sleep between 4 KB transfers, not from us**. We do not know the device's
real word-accept rate, so a `poll_delay` knob is provided (report ready
only after that many consecutive status polls) and defaults to 0, meaning
always ready. Nothing beyond the ready line is modelled: writes are accepted and
discarded unless `log_path` is given.

`log_path` (opt-in, off by default, e.g. `tools/guirun.py --flexbus-log`):
records the raw byte stream actually placed on the wire, one byte per
`0x400cf4a8` byte-send (the write where the strobe bit is set, so each byte
is captured once, not twice). Nothing else is added -- no markers, no
instruction counts -- because the framing is already fixed by the firmware
protocol: every `0x400cfd40` push is 4 bytes (a page index or `0xFFFFFFFF`)
followed by 4096 bytes (1024 little- or big-endian-swapped longwords), so
the logged stream splits evenly into 4100-byte calls in order. `self.words`
and `self.bursts` (already kept for reporting) give the total byte and
burst count independent of whether logging is on.
"""
import struct

from unicorn import UC_HOOK_MEM_READ, UC_HOOK_MEM_WRITE

BASE   = 0x8C000000
STATUS = BASE + 0x02        # 16-bit: read = status (bit 0 ready), write = data
LATCH  = BASE + 0x0A        # written 0x80 to open a burst
READY  = 0x0001
STROBE = 0x0080             # low byte of a STATUS write when data is valid


class Fifo:
    """The ready line of the 0x8C000000 coprocessor port."""

    def __init__(self, m, poll_delay=0, log_path=None):
        self.m = m
        self.poll_delay = poll_delay
        self.polls = 0            # consecutive status reads since the last write
        self.words = 0            # data words accepted, for reporting
        # One latch write per FOUR-BYTE GROUP, not per 4KB transfer:
        # 0x400cf4a8 opens the port for each group, so a single
        # 0x400cfd40 push of 4096 bytes shows up as 1025 of these.
        self.bursts = 0
        self.log_path = log_path
        self._log_fh = open(log_path, 'wb') if log_path else None

        # A scoped hook is used rather than Machine.mmio because the value
        # is computed, not constant, and the reads are 16-bit so only two
        # bytes are written back -- Machine.install_mmio writes four and
        # would clobber 0x8C000004.
        def on_read(uc, typ, addr, size, val, data):
            self.polls += 1
            ready = READY if self.polls > self.poll_delay else 0
            uc.mem_write(STATUS, struct.pack('>H', ready))
        m.uc.hook_add(UC_HOOK_MEM_READ, on_read, begin=STATUS, end=STATUS + 1)

        def on_write(uc, typ, addr, size, val, data):
            self.words += 1
            self.polls = 0
            if self._log_fh is not None and (val & STROBE):
                self._log_fh.write(bytes(((val >> 8) & 0xFF,)))
        m.uc.hook_add(UC_HOOK_MEM_WRITE, on_write, begin=STATUS, end=STATUS + 1)

        def on_latch(uc, typ, addr, size, val, data):
            self.bursts += 1
        m.uc.hook_add(UC_HOOK_MEM_WRITE, on_latch, begin=LATCH, end=LATCH + 1)

    def close(self):
        """Flush and close the opt-in byte log, if one was requested."""
        if self._log_fh is not None:
            self._log_fh.close()
            self._log_fh = None


def install(m, ev=None, poll_delay=0, log_path=None):
    """Model the coprocessor port's ready line. -> the Fifo, also ev['dsp']."""
    fifo = Fifo(m, poll_delay, log_path=log_path)
    if ev is not None:
        ev['dsp'] = fifo
    return fifo
