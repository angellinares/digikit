"""eDMA channel 35: UART8 transmit.

Boot stalls 88 frames into the intro animation, and not on a semaphore.
`0x4000220c` is the firmware's "queue bytes for the console" routine, and it
opens with a spin loop waiting for room in a 4096-byte ring at `0x4FE1B000`:

    4000221c  d2 = [0x4094cd90] + len          ; bytes wanted
    40002232  d1 = w[0xFC045474]               ; TCD35.CITER
    40002238  d3 = w[0xFC04547C]               ; TCD35.BITER
    40002244  d1 = (d1 - d3) + ([cd88] - [cd94])
    40002246  d1 &= 0xfff                      ; -> bytes still in the ring
    40002252  if 0x1000 - d1 < d2: goto 4000221c

Nothing advanced that channel, so the ring never drained and the intro draw
task span there forever at priority 7, starving every other task. The
priority-7 busy-spin the handover blamed on `unblock=True` is downstream of
this: the intro never got far enough to finish.

The channel really is UART8 TX. TCD35 (base `0xFC045460`, in the ColdFire
eDMA layout where CITER sits at +0x14 and BITER at +0x1C) reads:

    SADDR  = 0x4FE1B000   the ring; ATTR = 0x6000 -> SMOD 12, source modulo 4096
    NBYTES = 1            one byte per request
    DADDR  = 0xEC07000C   UDR8, DOFF = 0

so this models it directly: a write of 35 to EDMA_SERQ runs the whole major
loop at once, the bytes land in `ev['uart_out']`, SADDR advances with the ring
modulo, CITER reloads from BITER (what hardware does at major-loop
completion), and vector 155 -- the channel-35 handler at `0x40001e7c` -- is
raised so the firmware's OWN ISR does the bookkeeping: `[cd94] = [cd88]`, then
either park the channel or chain the next transfer. Re-implementing that
bookkeeping in Python would duplicate firmware structure layout, which is the
mistake the Bitmap header cache already made once.

The completion interrupt is queued rather than raised inside the write hook.
The firmware writes SERQ with SR = 0x2700 -- the enqueue routine masks
interrupts around the ring update -- so hardware could not deliver it there
either, and changing PC from a Unicorn memory hook is not reliable in any
case. `deliver()` hands the vector over from a point that runs with
interrupts enabled, and only when the IPL actually allows it.
"""

# pyright: reportMissingImports=false
import struct

from unicorn import UC_HOOK_MEM_WRITE
from unicorn.m68k_const import UC_M68K_REG_SR

EDMA_BASE = 0xFC044000
SERQ = EDMA_BASE + 0x18  # set enable request, one byte, channel #
TCD_BASE = 0xFC045000  # TCD n at TCD_BASE + n * 0x20

TX_CHAN = 35
TX_VECTOR = 155  # -> 0x40001e7c, verified in the vector table
TX_STATE = 0x4094CD74  # firmware's own "a transfer is armed" flag
WAIT_LOOP = 0x4000221C  # head of the free-space spin loop

# ColdFire eDMA TCD layout. Note this is NOT the Kinetis order: CITER/DOFF and
# BITER/CSR are swapped relative to it, which is what makes CITER land at +0x14.
SADDR, ATTR, SOFF = 0x00, 0x04, 0x06
NBYTES, SLAST = 0x08, 0x0C
DADDR, CITER, DOFF = 0x10, 0x14, 0x16
DLAST, BITER, CSR = 0x18, 0x1C, 0x1E


class TxChannel:
    """One eDMA channel whose destination is a fixed register, run eagerly."""

    def __init__(self, m, out, chan=TX_CHAN, vector=TX_VECTOR):
        self.m, self.out = m, out
        self.chan, self.vector = chan, vector
        self.tcd = TCD_BASE + chan * 0x20
        self.pending = 0  # queued major-loop completions
        self.bytes = 0
        self.transfers = 0
        self._checkpoint_restored = False

    def checkpoint_state(self):
        return {
            "type": "TxChannel",
            "version": 1,
            "chan": self.chan,
            "vector": self.vector,
            "pending": self.pending,
            "bytes": self.bytes,
            "transfers": self.transfers,
        }

    def restore_checkpoint_state(self, state):
        if state.get("type") != "TxChannel" or state.get("version") != 1:
            raise RuntimeError("unsupported TxChannel checkpoint state")
        if (state["chan"], state["vector"]) != (self.chan, self.vector):
            raise RuntimeError("TxChannel checkpoint configuration mismatch")
        self.pending = state["pending"]
        self.bytes = state["bytes"]
        self.transfers = state["transfers"]
        self._checkpoint_restored = True

    def _u32(self, off):
        return struct.unpack(">I", self.m.uc.mem_read(self.tcd + off, 4))[0]

    def _u16(self, off):
        return struct.unpack(">H", self.m.uc.mem_read(self.tcd + off, 2))[0]

    def _s16(self, off):
        return struct.unpack(">h", self.m.uc.mem_read(self.tcd + off, 2))[0]

    def _w32(self, off, v):
        self.m.uc.mem_write(self.tcd + off, struct.pack(">I", v & 0xFFFFFFFF))

    def _w16(self, off, v):
        self.m.uc.mem_write(self.tcd + off, struct.pack(">H", v & 0xFFFF))

    def run(self):
        """Run the whole major loop now. -> bytes moved.

        Bit 15 of CITER/BITER is ELINK, not part of the count.
        """
        citer = self._u16(CITER) & 0x7FFF
        biter = self._u16(BITER) & 0x7FFF
        if not citer:
            return 0
        src, soff, nbytes = self._u32(SADDR), self._s16(SOFF), self._u32(NBYTES)
        smod = (self._u16(ATTR) >> 11) & 0x1F
        mask = (1 << smod) - 1 if smod else 0
        base = src & ~mask if mask else 0

        data = bytearray()
        for _ in range(citer):
            for _ in range(nbytes):
                data.append(self.m.uc.mem_read(src, 1)[0])
                src += soff
                if mask:
                    src = base | (src & mask)
        src += self._u32(SLAST)
        if mask:
            src = base | (src & mask)

        self._w32(SADDR, src)
        self._w16(CITER, biter)  # major-loop completion reloads CITER
        self.out += data
        self.bytes += len(data)
        self.transfers += 1
        self.pending += 1
        return len(data)

    def deliver(self):
        """Give the CPU a queued completion, if the current IPL allows. -> bool."""
        if not self.pending:
            return False
        if (self.m.uc.reg_read(UC_M68K_REG_SR) & 0x0700) == 0x0700:
            return False
        self.pending -= 1
        return self.m.raise_vector(self.vector)


def install(m, at, ev, chan=TX_CHAN, vector=TX_VECTOR,
            wait_loop=WAIT_LOOP):
    """Model the UART8 TX channel. -> the TxChannel, also at ev['edma_tx']."""
    ch = TxChannel(m, ev["uart_out"], chan, vector)
    ev["edma_tx"] = ch

    def on_serq(uc, typ, addr, size, val, data):
        # bit 6 = "set all channels"; the low 6 bits are the channel number.
        if val & 0x40 or (val & 0x3F) == chan:
            ch.run()

    m.uc.hook_add(UC_HOOK_MEM_WRITE, on_serq, begin=SERQ, end=SERQ)

    # The free-space spin loop runs with interrupts enabled and is exactly
    # where hardware would take the completion, so deliver it there.
    if wait_loop is not None:
        at(wait_loop, lambda uc, a, s, d: ch.deliver())
    return ch


def needs_legacy_kick(ch):
    """Whether a snapshot lacks model state that makes kicking unsafe."""
    return not ch._checkpoint_restored


def kick(m, ch, tx_state=TX_STATE):
    """Drain a transfer a snapshot resumed with already armed.

    A legacy snapshot restores the firmware's image-specific "transfer is in
    flight" flag without restoring EDMA_ERQ, so the channel would sit
    half-started forever and the first free-space check would block. Running
    it once hands the chain back to the firmware's ISR. New callers pass the
    address resolved from the image; the default preserves direct users of
    the original 1.15C-only helper.
    """
    try:
        armed = struct.unpack(">I", m.uc.mem_read(tx_state, 4))[0]
    except Exception:
        return 0
    return ch.run() if armed else 0
