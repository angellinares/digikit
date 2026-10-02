//! The eDMA controller (MCF5441XRM chapter 19): shared register layout used
//! by every channel model in this crate (this file's own [`TxChannel`],
//! `dspi::Dspi2Link`, `ssi::Ssi0Dma`), plus the console-UART8-TX channel
//! (35) itself -- a direct port of `emu/edma.py`'s `TxChannel`.
//!
//! ## Register layout
//!
//! One 16 KiB slot at `0xFC04_4000` holds both the controller's own
//! registers (`0xFC04_4000`-`0xFC04_401F`, RM Table 19-3) and all 64
//! per-channel Transfer Control Descriptors (`TCDn` at
//! `0xFC04_5000 + n*0x20`, RM 19.4.17 p.384) -- the TCD area is `0x1000`
//! bytes into the same slot. `EdmaBank` (this file) owns that one
//! [`RegFile`]; every channel model that touches a TCD (this file's
//! `TxChannel`, `dspi::Dspi2Link`, `ssi::Ssi0Dma`) is handed `&mut RegFile`
//! rather than owning its own copy, exactly as they all read/write the same
//! guest memory region in Python (`self.m.uc.mem_read/mem_write`).
//!
//! `EDMA_SERQ`/`EDMA_CERQ` (RM 19.4.5/19.4.6, p.374-375) are the only two
//! eDMA control registers any model in this codebase reacts to (a write
//! arms/disarms a channel's request); every other TCD field is read
//! generically by field name (`SADDR`, `ATTR`, ... `CSR`), not hardcoded per
//! channel, matching the discipline `emu/edma.py`/`emu/dspi2.py`/`emu/ssi.py`
//! already follow (see their module docstrings: "duplicating firmware
//! struct layout is the mistake to avoid").
//!
//! `EDMA_SERQ`/`EDMA_CERQ`/`EDMA_CEEI`/`EDMA_CINT` are documented
//! **write-only: "Reads of this register return all zeroes"** (RM p.374,
//! p.375, p.377) -- real hardware, not exercised by any RD in the four
//! recorded traces (a read of these four bytes never appears), so this is
//! implemented from the manual with no oracle to check it against: an
//! `EdmaBank::read` of one of these four addresses returns 0, not the
//! `RegFile`'s echoed byte. Every other address in the slot (including every
//! TCD field this crate does not specifically interpret) is plain
//! `RegFile` passthrough, matching the Python oracle, which never
//! intercepts them at all.

use crate::regfile::RegFile;

pub const EDMA_BASE: u32 = 0xFC044000;
pub const SERQ: u32 = EDMA_BASE + 0x18;
pub const CERQ: u32 = EDMA_BASE + 0x19;
pub const SEEI: u32 = EDMA_BASE + 0x1A;
pub const CEEI: u32 = EDMA_BASE + 0x1B;
pub const CINT: u32 = EDMA_BASE + 0x1C;
pub const CERR: u32 = EDMA_BASE + 0x1D;
pub const SSRT: u32 = EDMA_BASE + 0x1E;
pub const CDNE: u32 = EDMA_BASE + 0x1F;

/// Registers documented "Reads of this register return all zeroes" (RM
/// p.374 EDMA_SERQ, p.375 EDMA_CERQ/EDMA_CEEI header, p.377 EDMA_CINT).
/// `EDMA_SEEI`/`EDMA_SSRT`/`EDMA_CERR`/`EDMA_CDNE` are the same family of
/// write-only command register (RM 19.4.7-19.4.12) but their read behaviour
/// is not transcribed here since none is exercised by any trace; they stay
/// plain RAM like everything else this crate does not specifically model.
pub const READ_ZERO: [u32; 4] = [SERQ, CERQ, CEEI, CINT];

pub const TCD_BASE: u32 = 0xFC045000;
/// TCDn's offset within `EdmaBank`'s one 16 KiB slot (`TCD_BASE - EDMA_BASE`).
pub const TCD_SLOT_OFFSET: usize = (TCD_BASE - EDMA_BASE) as usize;

// TCDn field offsets, relative to that channel's 32-byte descriptor
// (RM Figure 19-24 p.384, the ColdFire field order -- CITER at +0x14,
// BITER at +0x1C, NOT the Kinetis order).
pub const SADDR: usize = 0x00;
pub const ATTR: usize = 0x04;
pub const SOFF: usize = 0x06;
pub const NBYTES: usize = 0x08;
pub const SLAST: usize = 0x0C;
pub const DADDR: usize = 0x10;
pub const CITER: usize = 0x14;
pub const DOFF: usize = 0x16;
pub const DLAST: usize = 0x18;
pub const BITER: usize = 0x1C;
pub const CSR: usize = 0x1E;

// TCDn_CSR bits (RM Figure 19-36 / Table 19-31, p.390-391).
pub const CSR_DONE: u16 = 0x0080;
pub const CSR_ACTIVE: u16 = 0x0040;
pub const CSR_MAJOR_E_LINK: u16 = 0x0020;
pub const CSR_E_SG: u16 = 0x0010;
pub const CSR_D_REQ: u16 = 0x0008;
pub const CSR_INT_HALF: u16 = 0x0004;
pub const CSR_INT_MAJOR: u16 = 0x0002;
pub const CSR_START: u16 = 0x0001;

/// A detached eDMA TCD image.  Peripheral helpers use this instead of a
/// `RegFile` so their transfer effects are pure and can be applied by a
/// machine owner after its guest-memory operation succeeds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TcdSnapshot {
    pub saddr: u32,
    pub attr: u16,
    pub soff: i16,
    pub nbytes: u32,
    pub slast: i32,
    pub daddr: u32,
    /// The count with E_LINK removed.
    pub citer: u16,
    pub doff: i16,
    pub dlast: i32,
    /// The count with E_LINK removed.
    pub biter: u16,
    pub csr: u16,
}

/// -> the byte offset of channel `chan`'s TCD, within `EdmaBank`'s slot.
pub const fn tcd_offset(chan: usize) -> usize {
    TCD_SLOT_OFFSET + chan * 0x20
}

/// Sign-extend the low `bits` bits of `value` (`emu.edma`/`emu.dspi2`'s
/// `_signed`; `edma.py`'s `TxChannel.run` inlines the 16-bit case).
pub fn signed(value: u32, bits: u32) -> i64 {
    let sign = 1i64 << (bits - 1);
    let v = (value as i64) & ((1i64 << bits) - 1);
    if v & sign != 0 { v - (1i64 << bits) } else { v }
}

/// One eDMA channel's live TCD fields, generically read/written by name
/// (RM 19.4.17) -- the same fields every model in this crate reads instead
/// of hardcoding a layout. Backed by whichever `RegFile` slot the channel's
/// TCD physically lives in (always `EdmaBank`'s, for every channel this
/// crate touches).
pub struct TcdView<'a> {
    regs: &'a mut RegFile,
    base: usize,
}

impl<'a> TcdView<'a> {
    pub fn new(regs: &'a mut RegFile, chan: usize) -> Self {
        Self {
            regs,
            base: tcd_offset(chan),
        }
    }
    pub fn saddr(&self) -> u32 {
        self.regs.u32_at(self.base + SADDR)
    }
    pub fn set_saddr(&mut self, v: u32) {
        self.regs.set_u32_at(self.base + SADDR, v)
    }
    pub fn daddr(&self) -> u32 {
        self.regs.u32_at(self.base + DADDR)
    }
    pub fn set_daddr(&mut self, v: u32) {
        self.regs.set_u32_at(self.base + DADDR, v)
    }
    pub fn attr(&self) -> u16 {
        self.regs.u16_at(self.base + ATTR)
    }
    pub fn soff(&self) -> i64 {
        signed(self.regs.u16_at(self.base + SOFF) as u32, 16)
    }
    pub fn doff(&self) -> i64 {
        signed(self.regs.u16_at(self.base + DOFF) as u32, 16)
    }
    pub fn nbytes(&self) -> u32 {
        self.regs.u32_at(self.base + NBYTES)
    }
    pub fn slast(&self) -> i64 {
        signed(self.regs.u32_at(self.base + SLAST), 32)
    }
    pub fn dlast(&self) -> i64 {
        signed(self.regs.u32_at(self.base + DLAST), 32)
    }
    pub fn dlast_raw(&self) -> u32 {
        self.regs.u32_at(self.base + DLAST)
    }
    /// Bit 15 of CITER/BITER is E_LINK, not part of the count (`& 0x7FFF`,
    /// matching every Python model's own masking).
    pub fn citer(&self) -> u16 {
        self.regs.u16_at(self.base + CITER) & 0x7FFF
    }
    pub fn citer_raw(&self) -> u16 {
        self.regs.u16_at(self.base + CITER)
    }
    pub fn biter(&self) -> u16 {
        self.regs.u16_at(self.base + BITER) & 0x7FFF
    }
    pub fn biter_raw(&self) -> u16 {
        self.regs.u16_at(self.base + BITER)
    }
    pub fn set_citer(&mut self, v: u16) {
        self.regs.set_u16_at(self.base + CITER, v)
    }
    pub fn csr(&self) -> u16 {
        self.regs.u16_at(self.base + CSR)
    }
    pub fn smod_mask(&self) -> u32 {
        let smod = (self.attr() >> 11) & 0x1F;
        if smod == 0 { 0 } else { (1u32 << smod) - 1 }
    }
    pub fn dmod_mask(&self) -> u32 {
        let dmod = (self.attr() >> 3) & 0x1F;
        if dmod == 0 { 0 } else { (1u32 << dmod) - 1 }
    }
    /// Read a full 32-byte descriptor (for a scatter/gather reload's source
    /// pointer verification, or `RegFile::load_page` cross-checks).
    pub fn base_offset(&self) -> usize {
        self.base
    }

    /// Copy the live descriptor into a detached value for a pure peripheral
    /// transfer helper. E_LINK is intentionally not carried in its counts;
    /// the oracle masks it before its eSDHC channel-59 bookkeeping too.
    pub fn snapshot(&self) -> TcdSnapshot {
        TcdSnapshot {
            saddr: self.saddr(),
            attr: self.attr(),
            soff: self.soff() as i16,
            nbytes: self.nbytes(),
            slast: self.slast() as i32,
            daddr: self.daddr(),
            citer: self.citer(),
            doff: self.doff() as i16,
            dlast: self.dlast() as i32,
            biter: self.biter(),
            csr: self.csr(),
        }
    }

    /// Apply only the three writes the eSDHC oracle performs on completion:
    /// the moving address, reloaded CITER, and DONE-set CSR. The remaining
    /// snapshot fields are inputs, not writebacks (or extra HWR events).
    pub fn apply_dma59_writeback(&mut self, tcd: TcdSnapshot, card_to_guest: bool) {
        if card_to_guest {
            self.set_daddr(tcd.daddr);
        } else {
            self.set_saddr(tcd.saddr);
        }
        self.set_citer(tcd.citer);
        self.regs.set_u16_at(self.base + CSR, tcd.csr);
    }
}

/// One eDMA channel whose destination is a fixed peripheral register and
/// whose major loop runs eagerly on `EDMA_SERQ`, run entirely inside a
/// register write with the CPU's own instruction stream paused -- a direct
/// port of `emu/edma.py`'s `TxChannel` (see that module's docstring for the
/// hardware reasoning: eDMA channel 35, the UART8 console TX ring).
///
/// The major loop's *source* bytes (`0x4FE1B000`'s ring, outside this
/// crate's own register file) are not something a peripheral-only replay
/// can read independently -- there is no guest DDR model here, only the
/// peripheral slots the trace hooks. A live machine (a future
/// `native/machine`, with a real memory bus) calls [`TxChannel::run`]
/// directly with the source bytes it read itself; a trace replay instead
/// takes them from the trace's own merged `HRD` record for this exact call
/// (`emu.edma.TxChannel.run read` -- `docs/plan-native-emulator.md` P4,
/// "host memory-write records HWR" mentions this class of record; the read
/// counterpart is `HRD`, recorded "only for peripheral modules... the DMA
/// source data a model replayed in isolation needs as input",
/// `scratchpad/p4-mmio-recorder.md`). Either way the bytes are handed to
/// `run`, which does not care where they came from -- only the register
/// side effects (SADDR advance, CITER reload) are computed here, and those
/// *are* checked, against `HWR`/`RD`, by a replay.
pub struct TxChannel {
    pub chan: usize,
    pub vector: u16,
    /// Queued major-loop completions, delivered from a PC-triggered hook in
    /// Python (`at(wait_loop, ...)`) that this peripheral-only crate cannot
    /// reproduce (see `bin/replay.rs`'s module docs, "PC-triggered
    /// completions"): a replay counts these, it does not predict their
    /// instruction-count boundary.
    pub pending: u64,
    pub bytes: u64,
    pub transfers: u64,
}

impl TxChannel {
    pub fn new(chan: usize, vector: u16) -> Self {
        Self {
            chan,
            vector,
            pending: 0,
            bytes: 0,
            transfers: 0,
        }
    }

    /// Run the whole major loop now, given the source bytes it moved
    /// (`citer * 1` bytes, one byte per minor loop -- `emu.edma.TxChannel`
    /// always uses `NBYTES` bytes per minor loop, but the console driver's
    /// own TCD always programs `NBYTES=1`; `source` may be longer, only the
    /// first `citer * nbytes` bytes are used). -> `false` (no-op) if CITER
    /// is 0, matching Python's early return.
    pub fn run(&mut self, regs: &mut RegFile, source: &[u8]) -> bool {
        let mut tcd = TcdView::new(regs, self.chan);
        let citer = tcd.citer();
        if citer == 0 {
            return false;
        }
        let nbytes = tcd.nbytes();
        let want = citer as usize * nbytes as usize;
        debug_assert!(
            source.len() >= want,
            "TxChannel::run: source has {} bytes, TCD wants {}",
            source.len(),
            want
        );
        let biter = tcd.biter();
        let mut src = tcd.saddr();
        let soff = tcd.soff();
        let mask = tcd.smod_mask();
        let base = if mask != 0 { src & !mask } else { 0 };
        for _ in 0..want {
            src = ((src as i64 + soff) & 0xFFFF_FFFF) as u32;
            if mask != 0 {
                src = base | (src & mask);
            }
        }
        src = ((src as i64 + tcd.slast()) & 0xFFFF_FFFF) as u32;
        if mask != 0 {
            src = base | (src & mask);
        }
        tcd.set_saddr(src);
        tcd.set_citer(biter); // major-loop completion reloads CITER
        self.bytes += want as u64;
        self.transfers += 1;
        self.pending += 1;
        true
    }

    /// One queued completion delivered (from `deliver()`'s replay-side
    /// counterpart -- see the struct docs). -> whether one was available.
    pub fn consume_pending(&mut self) -> bool {
        if self.pending == 0 {
            return false;
        }
        self.pending -= 1;
        true
    }
}

impl TxChannel {
    pub fn snap_save(&self, w: &mut crate::snap::Writer) {
        w.u64(self.pending);
        w.u64(self.bytes);
        w.u64(self.transfers);
    }
    pub fn snap_load(&mut self, r: &mut crate::snap::Reader) -> crate::snap::Result<()> {
        self.pending = r.u64()?;
        self.bytes = r.u64()?;
        self.transfers = r.u64()?;
        Ok(())
    }
}
