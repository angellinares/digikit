//! `DmaLink`: the eDMA/SSI0/DSPI2/DSPI1/FlexBus-DSP-FIFO lane as one unit --
//! the P4 "DMA + SSI + DSPI" counterpart of `machine::Timers` (see that
//! module's doc comment for the interface this follows: `read`/`write` for
//! guest MMIO, plus whatever a caller needs to drive scheduling). Wires:
//!
//! - one [`RegFile`] for the eDMA controller's 16 KiB slot (`0xFC04_4000`),
//!   shared by every channel model that touches a TCD -- `edma`'s
//!   [`edma::TxChannel`] (channel 35, UART8 console TX) and `dspi`'s
//!   [`dspi::Dspi2Link`] (channels 28/29). `ssi::Ssi0Dma` (channels 48/50)
//!   uses the same TCD area but is not wired in here -- see `ssi`'s module
//!   docs for why (no trace exercises it).
//! - one `RegFile` each for DSPI2's (`0xEC03_8000`) and DSPI1's
//!   (`0xFC03_C000`) 16 KiB slots -- plain passthrough (see `dspi`'s module
//!   docs on DSPI1).
//! - `dsp::Fifo`, the FlexBus coprocessor port's ready line, backed by a
//!   `RegFile` for its own slot (`0x8C00_0000`) for anything besides
//!   `STATUS`/`LATCH`, which `Fifo` computes dynamically (see its docs).
//! - a generic "forced MMIO" map: `emu.harness.Machine.install_mmio` forces
//!   a fixed 32-bit value on every read of a handful of addresses
//!   (`DSPI0_SR`, `DSPI2_SR` -- `emu/longrun.py` sets both unconditionally;
//!   `emu/mmiotrace.py`'s `STATE` records carry the live table as
//!   `state["mmio"]`, keyed by hex address string). A replay loads this map
//!   from each `STATE` record (`bin/replay.rs`); a live machine would
//!   instead pass in whatever `m.mmio` it starts with. Checked before any
//!   bank, exactly as `install_mmio`'s narrow read hooks run ahead of (and
//!   override) whatever the peripheral's own register bytes say.
//!
//! ## SERQ/CINT: the multi-model dispatch, and the two-step capture
//!
//! `EDMA_SERQ`/`EDMA_CINT` are one address each, but in Python multiple
//! independent models install a `UC_HOOK_MEM_WRITE` on them (`emu/edma.py`'s
//! `TxChannel`, `emu/dspi2.py`'s `Dspi2Link`; `emu/ssi.py`'s `Ssi0Dma` too,
//! not wired in here), each deciding for itself whether the channel number
//! written is one of its own. [`DmaLink::write`] does the same: one guest
//! write can trigger more than one model.
//!
//! A model that captures TX bytes (`TxChannel::run`, `Dspi2Link::capture`)
//! needs source bytes this peripheral-only crate cannot read on its own
//! (see `edma`/`dspi`'s module docs) -- in a replay, they come from the
//! trace's own next `HRD` record. [`DmaLink::write`] therefore does not run
//! the capture itself; it returns a [`SerqEffect`] saying which capture is
//! owed, and the caller (`bin/replay.rs`) finishes it once it has read that
//! `HRD` record, via [`DmaLink::finish_tx35_capture`] /
//! [`DmaLink::finish_dspi2_capture`]. `Dspi2Link`'s RX arm (channel 28,
//! always the second of the pair -- see `dspi`'s module docs) needs no
//! external bytes (`ZeroPeer`) and is completed by `write` itself, directly.

use std::collections::HashMap;

use crate::dsp;
use crate::dspi::{self, Peer};
use crate::edma;
use crate::regfile::RegFile;

pub const DSPI1_SLOT: u32 = dspi::DSPI1_BASE;
pub const DSPI2_SLOT: u32 = dspi::DSPI2_BASE;
pub const DSP_SLOT: u32 = dsp::BASE;

/// A capture a caller owes this link before its outcome is fully applied
/// (see the module docs, "the two-step capture").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SerqEffect {
    None,
    Tx35Capture,
    Dspi2Capture,
}

pub struct DmaLink {
    pub edma_regs: RegFile,
    pub dspi2_regs: RegFile,
    pub dspi1_regs: RegFile,
    pub dsp_regs: RegFile,
    pub tx35: edma::TxChannel,
    pub dspi2: dspi::Dspi2Link,
    pub dsp: dsp::Fifo,
    pub peer: Box<dyn Peer>,
    forced: HashMap<u32, u32>,
}

impl Default for DmaLink {
    fn default() -> Self {
        Self {
            edma_regs: RegFile::new(),
            dspi2_regs: RegFile::new(),
            dspi1_regs: RegFile::new(),
            dsp_regs: RegFile::new(),
            tx35: edma::TxChannel::new(35, 155),
            dspi2: dspi::Dspi2Link::new(dspi::TX_CHAN, dspi::RX_CHAN, true),
            dsp: dsp::Fifo::default(),
            peer: Box::new(dspi::ZeroPeer),
            forced: HashMap::new(),
        }
    }
}

impl DmaLink {
    pub fn owns(addr: u32) -> bool {
        Self::slot(addr).is_some()
    }

    fn slot(addr: u32) -> Option<u32> {
        [edma::EDMA_BASE, DSPI2_SLOT, DSPI1_SLOT, DSP_SLOT]
            .into_iter()
            .find(|&base| addr >= base && addr < base + crate::regfile::SLOT_SIZE as u32)
    }

    /// Load (or update) the forced-MMIO table from a trace `STATE` record's
    /// `mmio` object (`{"0xec03802c": value, ...}`) -- see the module docs.
    pub fn set_forced(&mut self, addr: u32, value: u32) {
        self.forced.insert(addr, value);
    }

    pub fn load_page(&mut self, base: u32, data: &[u8]) -> bool {
        match base {
            edma::EDMA_BASE => {
                self.edma_regs.load_page(data);
                true
            }
            DSPI2_SLOT => {
                self.dspi2_regs.load_page(data);
                true
            }
            DSPI1_SLOT => {
                self.dspi1_regs.load_page(data);
                true
            }
            DSP_SLOT => {
                self.dsp_regs.load_page(data);
                true
            }
            _ => false,
        }
    }

    fn forced_read(&self, addr: u32, size: u8) -> Option<u32> {
        for (&base, &value) in &self.forced {
            if addr >= base && addr + size as u32 <= base + 4 {
                let shift = 8 * (4 - (addr - base) - size as u32);
                let mask: u64 = if size == 4 {
                    0xFFFF_FFFF
                } else {
                    (1u64 << (8 * size)) - 1
                };
                return Some(((value as u64 >> shift) & mask) as u32);
            }
        }
        None
    }

    pub fn read(&mut self, addr: u32, size: u8) -> Option<u32> {
        if let Some(v) = self.forced_read(addr, size) {
            return Some(v);
        }
        let base = Self::slot(addr)?;
        if base == edma::EDMA_BASE {
            if size == 1 && edma::READ_ZERO.contains(&addr) {
                return Some(0); // RM p.374/375/377: "Reads... return all zeroes"
            }
            return Some(self.edma_regs.read(addr - base, size));
        }
        if base == DSP_SLOT {
            if dsp::Fifo::owns(addr)
                && let Some(v) = self.dsp.read_status(addr)
            {
                // The oracle's own echo write (`on_read`'s `uc.mem_write`,
                // recorded as an HWR from `emu.dsp.Fifo.__init__.<locals>.
                // on_read`) -- mirrored here so the returned bytes and
                // what a later plain read of this slot would see agree.
                self.dsp_regs.write((STATUS_OFFSET) as u32, 2, v as u32);
            }
            return Some(self.dsp_regs.read(addr - base, size));
        }
        let regs = if base == DSPI2_SLOT {
            &self.dspi2_regs
        } else {
            &self.dspi1_regs
        };
        Some(regs.read(addr - base, size))
    }

    /// -> `(owned, effect, host_write)`. `host_write` is `Dspi2Link::arm_rx`'s
    /// RX delivery, if this write completed an exchange -- check it against
    /// the trace's `HWR` from `_deliver`.
    pub fn write(
        &mut self,
        addr: u32,
        size: u8,
        value: u32,
    ) -> (bool, SerqEffect, Option<(u32, Vec<u8>)>) {
        let Some(base) = Self::slot(addr) else {
            return (false, SerqEffect::None, None);
        };
        if base == edma::EDMA_BASE {
            self.edma_regs.write(addr - base, size, value);
            let mut effect = SerqEffect::None;
            let mut hw = None;
            if addr == edma::SERQ {
                let v = value as u8;
                // `emu/edma.py`'s `on_serq`: no size or NOP-bit check at
                // all -- but `run()` itself no-ops (and reads nothing but
                // its own TCD's CITER) when CITER is 0, so no `HRD` for the
                // big block read is coming in that case either; checked
                // here so `pending_capture` is never armed waiting for one.
                if (v & 0x40 != 0 || (v & 0x3F) as usize == self.tx35.chan)
                    && edma::TcdView::new(&mut self.edma_regs, self.tx35.chan).citer() != 0
                {
                    effect = SerqEffect::Tx35Capture;
                }
                // `emu/dspi2.py`'s `_on_serq`: requires a single byte write
                // with the NOP bit clear.
                if size == 1 && v & 0x80 == 0 && self.dspi2.touches(v) {
                    if self.dspi2.arms_tx(v) {
                        if edma::TcdView::new(&mut self.edma_regs, self.dspi2.tx_chan).citer() != 0
                        {
                            effect = SerqEffect::Dspi2Capture;
                        } else {
                            // `_capture` with CITER==0 still sets `_tx_ready
                            // = b""` in Python (an empty, definite capture,
                            // not "no capture yet") -- reproduced directly,
                            // no `HRD` involved, since nothing external was
                            // read either way.
                            self.dspi2.capture(&mut self.edma_regs, &[]);
                        }
                    }
                    if self.dspi2.arms_rx(v) {
                        hw = self
                            .dspi2
                            .arm_rx(&mut self.edma_regs, self.peer.as_mut())
                            .map(|w| (w.addr, w.data));
                    }
                }
            } else if addr == edma::CINT && size == 1 {
                self.dspi2.on_cint(value as u8);
            }
            return (true, effect, hw);
        }
        if base == DSPI2_SLOT {
            self.dspi2_regs.write(addr - base, size, value);
            return (true, SerqEffect::None, None);
        }
        if base == DSPI1_SLOT {
            self.dspi1_regs.write(addr - base, size, value);
            return (true, SerqEffect::None, None);
        }
        if base == DSP_SLOT {
            self.dsp.write(addr);
            self.dsp_regs.write(addr - base, size, value);
            return (true, SerqEffect::None, None);
        }
        (false, SerqEffect::None, None)
    }

    /// Complete a [`SerqEffect::Tx35Capture`]: `source` is the trace's `HRD`
    /// payload for `emu.edma.TxChannel.run`'s read (or a live bus's own
    /// read, for a future caller).
    pub fn finish_tx35_capture(&mut self, source: &[u8]) {
        self.tx35.run(&mut self.edma_regs, source);
    }

    /// Complete a [`SerqEffect::Dspi2Capture`]: `source` is the trace's
    /// `HRD` payload for `emu.dspi2.Dspi2Link._capture`'s read. -> the
    /// logical TX frame (PUSHR tags stripped) -- the gate's "byte-exact"
    /// check is a caller comparing this against the known TRIG-mask bytes.
    pub fn finish_dspi2_capture(&mut self, source: &[u8]) -> Vec<u8> {
        self.dspi2.capture(&mut self.edma_regs, source)
    }

    /// `SERQ` may have armed RX before the caller supplied TX source RAM.
    /// Exchange only when both halves are ready, then return its host write.
    pub fn finish_dspi2_exchange(&mut self) -> Option<(u32, Vec<u8>)> {
        self.dspi2
            .exchange_after_capture(&mut self.edma_regs, self.peer.as_mut())
            .map(|w| (w.addr, w.data))
    }

    /// Mirror an arbitrary host write (`HWR`) from a source this link does
    /// not itself model (e.g. `emu.panelin.feed` advancing TCD34's DADDR --
    /// eDMA channel 34, the panel-input UART ring, owned by `emu/panelin.py`,
    /// not this crate) into whichever slot owns `addr`, byte for byte,
    /// exactly like a guest write would be stored -- so a later `RD` of that
    /// same register still matches the oracle. A no-op for any address
    /// outside this link's slots (most host writes land in guest DDR, which
    /// this peripheral-only crate does not model at all). -> whether it
    /// landed in one of this link's slots.
    pub fn mirror_write(&mut self, addr: u32, data: &[u8]) -> bool {
        let Some(base) = Self::slot(addr) else {
            return false;
        };
        let regs = match base {
            b if b == edma::EDMA_BASE => &mut self.edma_regs,
            b if b == DSPI2_SLOT => &mut self.dspi2_regs,
            b if b == DSPI1_SLOT => &mut self.dspi1_regs,
            _ => &mut self.dsp_regs,
        };
        for (i, &byte) in data.iter().enumerate() {
            let a = addr.wrapping_add(i as u32);
            if a < base || a >= base + crate::regfile::SLOT_SIZE as u32 {
                break; // spilled past this slot; nothing else here owns it
            }
            regs.write(a - base, 1, byte as u32);
        }
        true
    }
}

const STATUS_OFFSET: usize = (dsp::STATUS - dsp::BASE) as usize;
