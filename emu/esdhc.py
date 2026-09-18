# pyright: reportMissingImports=false
"""The eSDHC controller and the eMMC behind it.

`0xFC0CC000`, 16 KB, PBC0 slot 51 (RM chapter 25). Reached only when
`build(sdgate=True)` satisfies the board loopback of `emu/gpio.py` -- without
that the driver is never entered at all. See HANDOVER section 6b.

What the firmware does with it, from the driver map:

  * `0x4011fe10` writes CMDARG then XFERTYP -- **writing XFERTYP issues the
    command** -- and then blocks on semaphore `0x44e26f38`, returning the
    status word `0x44e26f1c` that the ISR is supposed to have written.
  * `0x401208fe` reads blocks with CMD18, `0x40120ae4` writes with CMD25.
  * `XFERTYP[DMAEN]` is never set and DSADDR/ADMASAR are never referenced: the
    controller's own DMA is unused, and bulk data moves through the SoC's eDMA
    with `SADDR = DATPORT`.
  * Init ends with the eMMC **bus test**, and it is a real handshake, not a
    magic number. `0x40120242` writes `0x5A` to DATPORT under CMD19
    (BUSTEST_W), then CMD14 (BUSTEST_R) must read back a word whose low byte
    XORed with `0xA5` is zero (`0x401202c6`). `~0x5A == 0xA5`: the card returns
    the inverse of whatever the host sent, so the model inverts the captured
    pattern rather than hardcoding the constant.

Modelling notes that cost time:

**Reads are served from the backing store, not a read hook.** Every register
this model owns is kept up to date in guest memory as state changes, so an
ordinary read just works and no hook runs on the hot path. Only two addresses
carry write hooks -- SYSCTL, whose self-clearing bits have to be cleared, and
XFERTYP, which is the command trigger.

**`uc.mem_write` from Python does not fire write hooks** (HANDOVER trap 13),
so the model updating its own registers cannot recurse into itself.

**A write hook fires BEFORE the store lands.** Reading the register back from
inside one gives the OLD value, and anything written from inside it is then
overwritten by the store that is still to come. So a write hook must use its
own `value` argument, and a register whose value has to be *corrected* is
handled on READ instead -- which is what SYSCTL's self-clearing bits do here.
Getting this wrong looks exactly like the model not being installed: the INITA
spin stays at 36,988,467 reads and nothing else changes.

**Self-clearing bits are why the driver used to hang.** `SYSCTL` bit 27 INITA
sends 80 init clocks and clears itself; bits 24-26 RSTA/RSTC/RSTD are software
resets that do the same. Backed by plain RAM they read back whatever was
written, and `0x4012001e` spins forever -- measured at 36,988,467 reads.
"""
import struct

from emu.edma import (
    SERQ,
    TCD_BASE,
    SADDR,
    SOFF,
    NBYTES,
    SLAST,
    DADDR,
    CITER,
    DOFF,
    BITER,
    CSR,
)

BASE = 0xFC0CC000
SIZE = 0x1000

DSADDR, BLKATTR, CMDARG, XFERTYP = 0x00, 0x04, 0x08, 0x0C
CMDRSP0, CMDRSP1, CMDRSP2, CMDRSP3 = 0x10, 0x14, 0x18, 0x1C
DATPORT, PRSSTAT, PROCTL, SYSCTL = 0x20, 0x24, 0x28, 0x2C
IRQSTAT, IRQSTATEN, IRQSIGEN = 0x30, 0x34, 0x38
AUTOC12ERR, HOSTCAPBLT, WML = 0x3C, 0x40, 0x44
FEVT, ADMAESR, ADMASAR, VENDOR, HOSTVER = 0x50, 0x54, 0x58, 0xC0, 0xFC

NAME = {DSADDR: 'DSADDR', BLKATTR: 'BLKATTR', CMDARG: 'CMDARG',
        XFERTYP: 'XFERTYP', CMDRSP0: 'CMDRSP0', CMDRSP1: 'CMDRSP1',
        CMDRSP2: 'CMDRSP2', CMDRSP3: 'CMDRSP3', DATPORT: 'DATPORT',
        PRSSTAT: 'PRSSTAT', PROCTL: 'PROCTL', SYSCTL: 'SYSCTL',
        IRQSTAT: 'IRQSTAT', IRQSTATEN: 'IRQSTATEN', IRQSIGEN: 'IRQSIGEN',
        AUTOC12ERR: 'AUTOC12ERR', HOSTCAPBLT: 'HOSTCAPBLT', WML: 'WML',
        FEVT: 'FEVT', ADMAESR: 'ADMAESR', ADMASAR: 'ADMASAR',
        VENDOR: 'VENDOR', HOSTVER: 'HOSTVER'}

# XFERTYP bits
DMAEN, BCEN, AC12EN = 1 << 0, 1 << 1, 1 << 2
DTDSEL, MSBSEL = 1 << 4, 1 << 5
DPSEL = 1 << 21

# PRSSTAT bits
CIHB, CDIHB, DLA, SDSTB = 1 << 0, 1 << 1, 1 << 2, 1 << 3
BWEN, BREN = 1 << 10, 1 << 11
CINS = 1 << 16
CLSL = 1 << 23
DLSL0 = 1 << 24

# IRQSTAT bits
CC, TC, BGE, DINT = 1 << 0, 1 << 1, 1 << 2, 1 << 3
BWR, BRR = 1 << 4, 1 << 5
CINS_IRQ = 1 << 6

# SYSCTL self-clearing bits
RSTA, RSTC, RSTD, INITA = 1 << 24, 1 << 25, 1 << 26, 1 << 27

# Reset values, RM Table 25-2. PRSSTAT's documented reset is 0xFF8800F8; we add
# CINS because a card IS inserted, which is the whole point of the model.
RESET = {
    DSADDR: 0, BLKATTR: 0x00010000, CMDARG: 0, XFERTYP: 0,
    CMDRSP0: 0, CMDRSP1: 0, CMDRSP2: 0, CMDRSP3: 0, DATPORT: 0,
    PRSSTAT: 0xFF8800F8 | CINS,
    PROCTL: 0x00000020, SYSCTL: 0x00008008,
    IRQSTAT: 0, IRQSTATEN: 0x117F013F, IRQSIGEN: 0,
    AUTOC12ERR: 0, HOSTCAPBLT: 0x07F30000, WML: 0x08100810,
    ADMAESR: 0, ADMASAR: 0, VENDOR: 1, HOSTVER: 0x00001201,
}

# The driver's own status word, written by the ISR on real hardware. This is
# the DIGITAKT value, kept as the default; per-image callers should instead
# pass drv_status resolved from emu/symbols.py's sd_status (Digitone's is
# 0x44459054). Using Digitakt's literal on Digitone leaves the guest's status
# word never cleared, so every bge/blt check after CMD0/CMD1/CMD19/CMD14/CMD8
# reads a stale "in progress" value.
# 0x4011fe10 sets it to 1 before issuing and returns it after the wait.
DRV_STATUS = 0x44E26F1C

# EXT_CSD is 512 bytes, DMA'd to this buffer by eDMA channel 59; the driver
# programs DADDR at 0x40120302 and arms the channel at 0x4012037e.
EXTCSD_BUF = 0x4FE49100
DMA_CHAN = 59

# Offsets INTO EXT_CSD that this firmware reads. `SLC_OK` is the one that
# matters: 0x4fe49198 is EXTCSD_BUF + 0x98, so the "MMC NOT IN SLC MODE" flag
# of section 1 is simply EXT_CSD byte 152, and build(slc=True) has been poking
# into this buffer all along.
#
# Byte 152 is inside GP_SIZE_MULT in the JEDEC map, which is not an obvious
# home for an SLC flag, so treat the JEDEC identity as UNCONFIRMED and the
# offset as the fact. What is certain is 0x401204a4 reads it and wants 1.
SLC_OK = 0x98
PARTITIONS_ATTRIBUTE = 0x9C          # 156; also read at 0x4fe4919c-9e
SEC_COUNT = 0xD4                     # 212..215, little-endian, in 512B sectors
ERASE_GROUP_DEF = 0xAF               # 175
BUS_WIDTH = 0xB7                     # 183
HS_TIMING = 0xB9                     # 185


def ext_csd(sectors=0x00760000, slc=True):
    """-> 512 bytes of EXT_CSD.

    Deliberately sparse: only the fields this firmware has been observed to
    read are set, so anything else showing up as a dependency will announce
    itself as a spin or a rejected card rather than hiding behind a plausible
    value.
    """
    b = bytearray(512)
    b[SLC_OK] = 1 if slc else 0
    b[SEC_COUNT:SEC_COUNT + 4] = struct.pack('<I', sectors)
    b[ERASE_GROUP_DEF] = 1
    b[BUS_WIDTH] = 1
    b[HS_TIMING] = 1
    b[PARTITIONS_ATTRIBUTE] = 1
    return bytes(b)


class Card:
    """A minimal eMMC. Only what the identification sequence asks for.

    `capacity_blocks` is in 512-byte sectors. CMD3 assigns RCA 2 (a
    host-assigned RCA is illegal for SD and standard for eMMC, which is how we
    know this is an eMMC and not a card).
    """

    def __init__(self, image=None, capacity_blocks=0x00760000, slc=True):
        self.image = image
        self.blocks = capacity_blocks
        self.ext_csd = ext_csd(capacity_blocks, slc)
        self.rca = 0
        self.selected = False
        # OCR: bit31 power-up done, bit30 sector addressing, voltage window.
        self.ocr = 0xC0FF8080
        self.overlay = {}
        # CID/CSD as four longwords each, R2 order {RSP3[23:0],RSP2,RSP1,RSP0}.
        self.cid = [0x00000000, 0x00000000, 0x00000000, 0x00110000]
        self.csd = [0x00000000, 0x00000000, 0x00000000, 0x00000000]

    def command(self, idx, arg):
        """-> (resp0, resp1, resp2, resp3). R1 is a card-status word."""
        r1 = 0x00000900          # state=transfer(4), READY_FOR_DATA
        if idx == 0:             # GO_IDLE_STATE, no response
            self.selected = False
            return (0, 0, 0, 0)
        if idx == 1:             # SEND_OP_COND -> OCR, bit31 must end set
            return (self.ocr, 0, 0, 0)
        if idx in (2, 9, 10):    # ALL_SEND_CID / SEND_CSD / SEND_CID -> R2
            src = self.csd if idx == 9 else self.cid
            return tuple(src)
        if idx == 3:             # SET_RELATIVE_ADDR (eMMC: host assigns)
            self.rca = (arg >> 16) & 0xFFFF
            return (r1, 0, 0, 0)
        if idx == 7:             # SELECT/DESELECT_CARD
            self.selected = ((arg >> 16) & 0xFFFF) == self.rca
            return (r1, 0, 0, 0)
        return (r1, 0, 0, 0)

    def read_word(self, idx, pattern):
        """-> the next word the host will read out of DATPORT.

        CMD14 is BUSTEST_R: the eMMC returns the bitwise inverse of the
        pattern the host sent under CMD19, which is why the driver's check is
        `read ^ 0xA5 == 0` after writing 0x5A.
        """
        if idx == 14:
            return (~pattern) & 0xFFFFFFFF
        return 0

    def data_for(self, idx, arg=0, length=None):
        """-> the block of data a data-read command hands back, or None."""
        if idx == 8:                 # SEND_EXT_CSD
            return self.ext_csd
        if idx == 18:                # READ_MULTIPLE_BLOCK, sector addressed
            start = arg * 512
            if length is None:
                length = max(0, len(self.image) - start) if self.image else 0
            if self.image is None:
                data = bytearray(length)
            else:
                data = bytearray(self.image[start:start + length])
                data.extend(b'\0' * (length - len(data)))
            for offset in range(length):
                value = self.overlay.get(start + offset)
                if value is not None:
                    data[offset] = value
            return bytes(data)
        return None

    def write_data(self, idx, arg, payload):
        """Accept block data sent by the host, retaining a sparse overlay."""
        if idx != 25:                 # WRITE_MULTIPLE_BLOCK
            return
        start = arg * 512
        self.overlay.update((start + offset, value)
                            for offset, value in enumerate(payload))


class Esdhc:
    """The controller. `log` collects (command index, argument) in order."""

    def __init__(self, m, card=None, trace=False, drv_status=None,
                 cmd_sem=None, data_sem=None, dma_sem=None):
        from unicorn import UC_HOOK_MEM_WRITE
        self.m = m
        self.uc = m.uc
        self.card = card or Card()
        self.trace = trace
        self.drv_status = DRV_STATUS if drv_status is None else drv_status
        self.cmd_sem = cmd_sem
        self.data_sem = data_sem
        self.dma_sem = dma_sem
        self.log = []
        self.pattern = 0           # last word the host wrote to DATPORT
        self.armed = None          # eDMA channel armed via SERQ for this cmd
        self.dma_bytes = 0
        # Machine.ensure(addr) maps the 1MB page containing addr. Guest
        # accesses to an unmapped page go through Machine._fault, which maps
        # the page and resumes -- but host-side uc.mem_write from Python does
        # not fire _fault, so it raises UC_ERR_WRITE_UNMAPPED instead. On the
        # longrun.py resume path this never showed up, because restore_into
        # pre-maps every page the earlier boot touched. On the cold-boot path
        # (dspboot.py) nothing has touched this page yet, so without this the
        # _put loop below died immediately seeding its own reset values. This
        # is what makes the model usable cold, not just on a snapshot resume.
        m.ensure(BASE)
        for off, val in RESET.items():
            self._put(off, val)
        from unicorn import UC_HOOK_MEM_READ
        # SYSCTL is corrected on READ: see the note above about write hooks
        # running before the store.
        self.uc.hook_add(UC_HOOK_MEM_READ, self._on_sysctl_read,
                         begin=BASE + SYSCTL, end=BASE + SYSCTL + 3)
        self.uc.hook_add(UC_HOOK_MEM_WRITE, self._on_xfertyp,
                         begin=BASE + XFERTYP, end=BASE + XFERTYP + 3)
        self.uc.hook_add(UC_HOOK_MEM_WRITE, self._on_datport_write,
                         begin=BASE + DATPORT, end=BASE + DATPORT + 3)
        # The driver arms an eDMA channel BEFORE issuing the command, so the
        # transfer has to run when the command is issued, not when SERQ is
        # written. emu/edma.py hooks the same byte for channel 35; Unicorn is
        # happy with more than one hook on an address.
        self.uc.hook_add(UC_HOOK_MEM_WRITE, self._on_serq,
                         begin=SERQ, end=SERQ)

    def checkpoint_state(self):
        """Preserve card state that is not represented in guest memory."""
        return {
            'type': 'Esdhc',
            'version': 1,
            'pattern': self.pattern,
            'armed': self.armed,
            'dma_bytes': self.dma_bytes,
            'card_blocks': self.card.blocks,
            'card_rca': self.card.rca,
            'card_selected': self.card.selected,
            'card_overlay': dict(self.card.overlay),
        }

    def restore_checkpoint_state(self, state):
        if state.get('type') != 'Esdhc' or state.get('version') != 1:
            raise RuntimeError('unsupported Esdhc checkpoint state')
        if state.get('card_blocks') != self.card.blocks:
            raise RuntimeError('Esdhc card capacity mismatch')
        overlay = state.get('card_overlay')
        if not isinstance(overlay, dict) or any(
            type(offset) is not int
            or offset < 0
            or type(value) is not int
            or not 0 <= value <= 0xFF
            for offset, value in overlay.items()
        ):
            raise RuntimeError('invalid Esdhc card overlay')
        armed = state.get('armed')
        if armed is not None and (type(armed) is not int or not 0 <= armed < 64):
            raise RuntimeError('invalid Esdhc armed channel')
        for key in ('pattern', 'dma_bytes', 'card_rca'):
            if type(state.get(key)) is not int or state[key] < 0:
                raise RuntimeError('invalid Esdhc checkpoint field %s' % key)
        if type(state.get('card_selected')) is not bool:
            raise RuntimeError('invalid Esdhc selected state')
        self.pattern = state['pattern']
        # Early v1 checkpoints recorded every SERQ writer, including UART8
        # channel 35. Only channel 59 can belong to this controller.
        self.armed = armed if armed == DMA_CHAN else None
        self.dma_bytes = state['dma_bytes']
        self.card.rca = state['card_rca']
        self.card.selected = state['card_selected']
        self.card.overlay = dict(overlay)

    def _post(self, sem):
        """Post an RTOS semaphore, as the eSDHC ISR does on real hardware.

        On real hardware the eSDHC ISR does two things on command/transfer
        completion: writes the driver's status word (modelled above via
        drv_status) and posts the driver's completion semaphore. This is the
        second half, previously unmodelled -- which is why the command
        primitive (FUN_4011d5b4 on Digitone) issued its command and then
        blocked forever in sem_pend on the cold-boot path. Mirrors exactly
        what emu/longrun.py's `unblock` does for pends generically -- it
        writes 1 when the count is <= 0 -- but scoped to the two semaphores
        this controller owns, so the model is correct on the cold-boot path
        too and does not depend on a global bypass. When `unblock` is also
        active this is simply a no-op, since it only acts on counts <= 0.
        """
        if sem is None:
            return
        try:
            self.m.ensure(sem)
            count = struct.unpack('>i', bytes(self.uc.mem_read(sem, 4)))[0]
            if count <= 0:
                self.uc.mem_write(sem, struct.pack('>i', 1))
        except Exception:
            pass

    # -- register access -------------------------------------------------
    def _put(self, off, val):
        self.uc.mem_write(BASE + off, struct.pack('>I', val & 0xFFFFFFFF))

    def _get(self, off):
        return struct.unpack('>I', bytes(self.uc.mem_read(BASE + off, 4)))[0]

    def _set_bits(self, off, bits):
        self._put(off, self._get(off) | bits)

    def _clr_bits(self, off, bits):
        self._put(off, self._get(off) & ~bits)

    # -- hooks -----------------------------------------------------------
    def _on_sysctl_read(self, uc, typ, addr, size, val, data):
        """INITA and the three software resets have already self-cleared.

        Done on read rather than on write because a write hook runs before the
        store: anything cleared there is put straight back by the store that
        follows. The driver sets INITA and then polls, so the first poll sees
        it clear, which is the behaviour the manual describes.
        """
        cur = self._get(SYSCTL)
        if cur & (INITA | RSTA | RSTC | RSTD):
            self._put(SYSCTL, cur & ~(INITA | RSTA | RSTC | RSTD))

    def _on_xfertyp(self, uc, typ, addr, size, val, data):
        """Writing XFERTYP issues the command.

        `val` is the value being stored; the store has not happened yet, so
        reading XFERTYP back here would give the previous command.
        """
        xfer = val & 0xFFFFFFFF if size == 4 else self._get(XFERTYP)
        idx = (xfer >> 24) & 0x3F
        arg = self._get(CMDARG)
        self.log.append((idx, arg))
        r0, r1, r2, r3 = self.card.command(idx, arg)
        self._put(CMDRSP0, r0)
        self._put(CMDRSP1, r1)
        self._put(CMDRSP2, r2)
        self._put(CMDRSP3, r3)
        # Command completes instantly: never leave the inhibit bits set, or
        # the driver's `(PRSSTAT & 3) == 0` waits never finish.
        self._clr_bits(PRSSTAT, CIHB | CDIHB | DLA)
        self._set_bits(IRQSTAT, CC | TC)
        # A data command has to make the buffer look ready, or the driver
        # spins on PRSSTAT: BWEN at 0x40120236 before it writes, BREN at
        # 0x401202b2 before it reads.
        if xfer & DPSEL:
            if xfer & DTDSEL:                       # card -> host
                self._set_bits(PRSSTAT, BREN)
                self._set_bits(IRQSTAT, BRR)
                self._put(DATPORT, self.card.read_word(idx, self.pattern))
                payload = self.card.data_for(idx, arg, self._dma_size())
                if payload is not None and self.armed is not None:
                    n = self._dma_out(payload)
                    self.armed = None
                    # Channel 59's completion ISR posts this before the
                    # eSDHC transfer-complete ISR posts data_sem.  Model the
                    # two producers separately instead of satisfying every
                    # blocked wait globally.
                    self._post(self.dma_sem)
                    if self.trace:
                        print('[esdhc]   CMD%d -> %d bytes by eDMA' % (idx, n))
                # The bring-up pends on this one after the EXT_CSD DMA read.
                self._post(self.data_sem)
            else:                                   # host -> card
                self._set_bits(PRSSTAT, BWEN)
                self._set_bits(IRQSTAT, BWR)
                if self.armed is not None:
                    payload = self._dma_in()
                    self.card.write_data(idx, arg, payload)
                    self.armed = None
                    self._post(self.dma_sem)
                    if self.trace:
                        print('[esdhc]   CMD%d <- %d bytes by eDMA'
                              % (idx, len(payload)))
                self._post(self.data_sem)
        # The ISR's bookkeeping. 0x4011fe10 pre-sets this to 1 and returns it
        # after the wait; `unblock` satisfies the wait, so without this the
        # caller always sees "still in progress".
        # Host-side mem_write does not demand-map; see __init__'s note on
        # Machine.ensure. This word lives in SDRAM the guest may not have
        # touched yet at this point.
        self.m.ensure(self.drv_status)
        self.uc.mem_write(self.drv_status, struct.pack('>I', 0))
        # The command-completion half of the ISR: this is what lets
        # FUN_4011d5b4 return from its sem_pend on the cold-boot path.
        self._post(self.cmd_sem)
        if self.trace:
            print('[esdhc] CMD%-2d arg=%#010x xfertyp=%#010x -> %#010x'
                  % (idx, arg, xfer, r0))

    def _on_serq(self, uc, typ, addr, size, val, data):
        """Remember when the eSDHC's channel is armed. Bit 6 means all."""
        if not (val & 0x40) and (val & 0x3F) == DMA_CHAN:
            self.armed = DMA_CHAN

    def _dma_out(self, payload):
        """Push `payload` through the armed channel's TCD, as the eDMA would.

        SOFF is zero for these transfers -- the source is the DATPORT register
        read over and over -- and DOFF equals NBYTES, so the destination is
        contiguous and this is a straight copy plus TCD bookkeeping.
        """
        if self.armed is None:
            return 0
        tcd = TCD_BASE + self.armed * 0x20
        def u32(o):
            return struct.unpack('>I', bytes(self.uc.mem_read(tcd + o, 4)))[0]
        def u16(o):
            return struct.unpack('>H', bytes(self.uc.mem_read(tcd + o, 2)))[0]
        citer, nbytes = u16(CITER) & 0x7FFF, u32(NBYTES)
        total = citer * nbytes
        if not total:
            return 0
        dst = u32(DADDR)
        chunk = payload[:total].ljust(total, b'\x00')
        # Host-side mem_write does not demand-map; see __init__'s note on
        # Machine.ensure. dst is the firmware's EXT_CSD buffer, which the
        # guest has not necessarily written to yet.
        self.m.ensure(dst)
        self.uc.mem_write(dst, chunk)
        self.uc.mem_write(tcd + DADDR, struct.pack('>I', dst + total))
        self.uc.mem_write(tcd + CITER, struct.pack('>H', u16(BITER) & 0x7FFF))
        # The bring-up routine's EXT_CSD read (CMD8 SEND_EXT_CSD) does not
        # poll any eSDHC register for completion -- it arms eDMA channel 59,
        # issues the command, then polls TCD59's own CSR bit 7 (DONE) with a
        # bound of 20 retries of 1000 ticks each. Without this the poll
        # always times out, the routine bails to its error exit, and the
        # storage flag at the end of the routine is never set -- so storage
        # never comes up even though every eSDHC register was serviced
        # correctly. This is the one poll in the whole routine that has a
        # timeout, which is why the symptom was a silent failure rather than
        # a hang.
        self.uc.mem_write(tcd + CSR, struct.pack('>H', u16(CSR) | 0x80))
        self.dma_bytes += total
        return total

    def _dma_size(self):
        """Return the armed channel's remaining major-loop byte count."""
        if self.armed is None:
            return 0
        tcd = TCD_BASE + self.armed * 0x20
        citer = struct.unpack(
            '>H', bytes(self.uc.mem_read(tcd + CITER, 2))
        )[0] & 0x7FFF
        nbytes = struct.unpack(
            '>I', bytes(self.uc.mem_read(tcd + NBYTES, 4))
        )[0]
        return citer * nbytes

    def _dma_in(self):
        """Pull the armed channel's host buffer into the card."""
        if self.armed is None:
            return b''
        tcd = TCD_BASE + self.armed * 0x20

        def u32(o):
            return struct.unpack('>I', bytes(self.uc.mem_read(tcd + o, 4)))[0]

        def s32(o):
            return struct.unpack('>i', bytes(self.uc.mem_read(tcd + o, 4)))[0]

        def u16(o):
            return struct.unpack('>H', bytes(self.uc.mem_read(tcd + o, 2)))[0]

        def s16(o):
            return struct.unpack('>h', bytes(self.uc.mem_read(tcd + o, 2)))[0]

        citer, nbytes = u16(CITER) & 0x7FFF, u32(NBYTES)
        src, soff = u32(SADDR), s16(SOFF)
        payload = bytearray()
        for _ in range(citer):
            payload.extend(self.uc.mem_read(src, nbytes))
            src += soff
        src += s32(SLAST)
        self.uc.mem_write(tcd + SADDR, struct.pack('>I', src & 0xFFFFFFFF))
        self.uc.mem_write(tcd + CITER, struct.pack('>H', u16(BITER) & 0x7FFF))
        self.uc.mem_write(tcd + CSR, struct.pack('>H', u16(CSR) | 0x80))
        self.dma_bytes += len(payload)
        return bytes(payload)

    def _on_datport_write(self, uc, typ, addr, size, val, data):
        """Capture what the host puts in the buffer, for the bus test."""
        self.pattern = val & 0xFFFFFFFF

    def __repr__(self):
        return 'Esdhc(commands=%d, %s)' % (
            len(self.log), ' '.join('CMD%d' % c for c, _ in self.log[:16]))
