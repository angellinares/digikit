//! The four DMA timers, MCF5441XRM chapter 39.
//!
//! Register layout (offsets relative to each channel's 16 KiB base --
//! `0xFC07_0000`, `0xFC07_4000`, `0xFC07_8000`, `0xFC07_C000`):
//!
//! ```text
//! 0x00  DTMRn   16  R/W  mode
//! 0x02  DTXMRn   8  R/W  extended mode (DMAEN bit 7)
//! 0x03  DTERn    8  R/W  event, write-1-to-clear (bit 1 REF, bit 0 CAP)
//! 0x04  DTRRn   32  R/W  reference value
//! 0x08  DTCRn   32  R/W  capture (not modelled: unexercised)
//! 0x0C  DTCNn   32  R    free-running counter (not modelled: unexercised)
//! ```
//!
//! DTMRn bits: 15-8 PS (prescaler, divides by PS+1), 7-6 CE, 5 OM, 4 ORRI,
//! 3 FRR, 2-1 CLK (00 stop, 01 bus/1, 10 bus/16, 11 external pin), 0 RST.
//!
//! Vectors 96-99 (INTC0 sources 32-35) for DTIM0-3 (`emu/dtim.py`).
//! Highest-source-first within a level (RM SS17.3.1): DTIM3 (source 35)
//! before DTIM1 (source 33), matching `emu.dtim.Dtims`' own tie-break.
//!
//! A direct, field-for-field port of `emu.dtim.Dtims`, including the one
//! difference from `Pits`: on a due tick this **writes DTER's REF bit**
//! into the register file (`Dtims.service`'s `mem_read`/`mem_write` pair,
//! recorded as an `HWR` from `emu.dtim.Dtims.service`), so a guest read of
//! DTER after a tick sees REF=1 -- unlike PIT's PIF, which the Python model
//! never asserts in guest memory at all (see `pit.rs`). [`DtimBank::service`]
//! returns that write alongside any raised vectors so a replay can check it
//! against the trace's `HWR` records.

use crate::intc::IntcBank;
use crate::regfile::RegFile;
use crate::sr::SrTracker;

pub const BASES: [u32; 4] = [0xFC070000, 0xFC074000, 0xFC078000, 0xFC07C000];
pub const VECTORS: [u16; 4] = [96, 97, 98, 99];

const DTMR: usize = 0x00;
const DTXMR: usize = 0x02;
const DTER: usize = 0x03;
const DTRR: usize = 0x04;

const RST: u16 = 0x01;
const FRR: u16 = 0x08;
const ORRI: u16 = 0x10;
const REF: u8 = 0x02;
const DMAEN: u8 = 0x80;

pub const F_BUS: f64 = crate::pit::F_BUS;
pub const IDLE_STEP: u64 = crate::pit::IDLE_STEP;

/// `DtimBank::service`'s result: (vectors raised, in delivery order with
/// their ICR level; host writes performed as `(addr, byte)`).
pub type DtimServiceResult = (Vec<(u16, u8)>, Vec<(u32, u8)>);

#[derive(Clone, Default)]
struct Channel {
    next: Option<f64>,
    pending: bool,
    missed: u64,
    cleared: u64,
    fired: u64,
    transitions: u64,
}

pub struct DtimBank {
    regs: [RegFile; 4],
    /// See `PitBank::channels`: only these get scheduling logic; an
    /// unlisted DTIM channel's registers are plain RAM (matches
    /// `emu.dtim.Dtims`' default `channels=(3,)` -- DTIM0/1/2 are never
    /// serviced in the recorded configuration even though DTIM1 is
    /// physically armed in some windows, see the finding on DTIM1).
    channels: Vec<usize>,
    ips: f64,
    held: bool,
    /// Snapshot repair history from Python's `clear_stale`; informational
    /// only, but retained when a portable timer component is imported.
    stale: Vec<usize>,
    ch: [Channel; 4],
}

impl Default for DtimBank {
    fn default() -> Self {
        Self {
            regs: [
                RegFile::new(),
                RegFile::new(),
                RegFile::new(),
                RegFile::new(),
            ],
            channels: vec![3],
            ips: 4_680_000.0,
            held: false,
            stale: Vec::new(),
            ch: Default::default(),
        }
    }
}

impl DtimBank {
    pub fn new(channels: Vec<usize>, ips: f64, held: bool) -> Self {
        Self {
            channels,
            ips,
            held,
            ..Self::default()
        }
    }

    pub fn owns(addr: u32) -> bool {
        BASES
            .iter()
            .any(|b| addr >= *b && addr < b + crate::regfile::SLOT_SIZE as u32)
    }

    fn slot(addr: u32) -> Option<usize> {
        BASES
            .iter()
            .position(|b| addr >= *b && addr < b + crate::regfile::SLOT_SIZE as u32)
    }

    pub fn load_page(&mut self, base: u32, data: &[u8]) -> bool {
        match BASES.iter().position(|b| *b == base) {
            Some(ch) => {
                self.regs[ch].load_page(data);
                true
            }
            None => false,
        }
    }

    pub fn read(&self, addr: u32, size: u8) -> Option<u32> {
        let ch = Self::slot(addr)?;
        Some(self.regs[ch].read(addr - BASES[ch], size))
    }

    /// Plain RAM, like `PitBank::write`, plus the same write-1-to-clear
    /// observation on DTER's REF byte for an actively-scheduled channel
    /// (`Dtims._on_write`).
    pub fn write(&mut self, addr: u32, size: u8, value: u32) -> bool {
        let Some(ch) = Self::slot(addr) else {
            return false;
        };
        self.regs[ch].write(addr - BASES[ch], size, value);
        if self.channels.contains(&ch) {
            let target = BASES[ch] as i64 + DTER as i64;
            let start = addr as i64;
            let end = start + size as i64;
            if start <= target && target < end {
                let shift = (end - 1) - target;
                let byte = ((value >> (8 * shift as u32)) & 0xFF) as u8;
                self.note_dter_write(ch, byte & REF != 0);
            }
        }
        true
    }

    pub fn set_ips(&mut self, ips: f64) {
        self.ips = ips;
    }

    /// See `PitBank::configure`.
    pub fn configure(&mut self, channels: Vec<usize>, ips: f64) {
        self.channels = channels;
        self.ips = ips;
    }

    pub fn rescale(&mut self, now: f64, new_ips: f64) {
        if new_ips == self.ips {
            return;
        }
        for c in &mut self.ch {
            if let Some(n) = c.next {
                c.next = Some(now + (n - now) * new_ips / self.ips);
            }
        }
        self.ips = new_ips;
    }

    pub fn release(&mut self) {
        self.held = false;
    }

    /// `Dtims.period` exactly.
    pub fn period(&self, ch: usize) -> Option<f64> {
        let r = &self.regs[ch];
        let dtmr = r.u16_at(DTMR);
        let dtxmr = r.u8_at(DTXMR);
        let dtrr = r.u32_at(DTRR);
        if dtmr & RST == 0 || dtmr & ORRI == 0 || dtxmr & DMAEN != 0 {
            return None;
        }
        let clk = (dtmr >> 1) & 0x03;
        if clk == 0 || clk == 3 {
            return None;
        }
        let div = if clk == 1 { 1u32 } else { 16 };
        let ps = ((dtmr >> 8) & 0xFF) as f64;
        let ticks = ((dtrr as f64) + 1.0) * (ps + 1.0) * (div as f64);
        Some(ticks / F_BUS * self.ips)
    }

    pub fn deadline(&mut self, done: u64) -> Option<u64> {
        if self.held {
            return None;
        }
        let mut best: Option<f64> = None;
        for &ci in &self.channels.clone() {
            match self.period(ci) {
                None => {
                    if self.ch[ci].next.is_some() {
                        self.ch[ci].transitions += 1;
                    }
                    self.ch[ci].next = None;
                }
                Some(p) => {
                    if self.ch[ci].next.is_none() {
                        self.ch[ci].next = Some(done as f64 + p);
                        self.ch[ci].transitions += 1;
                    }
                    let n = self.ch[ci].next.unwrap();
                    if best.is_none() || n < best.unwrap() {
                        best = Some(n);
                    }
                }
            }
        }
        best.map(|b| b.ceil() as u64)
    }

    pub fn step(&mut self, done: u64, remaining: Option<u64>) -> u64 {
        let d = self.deadline(done);
        let mut n = match d {
            None => IDLE_STEP,
            Some(d) => (d.saturating_sub(done)).max(1),
        };
        if let Some(r) = remaining {
            n = n.min(r);
        }
        n.max(1)
    }

    /// Advance to instruction count `done`. -> (vectors raised, in delivery
    /// order with their ICR level; host writes performed, `(addr, byte)`).
    pub fn service(&mut self, done: u64, intc: &IntcBank, sr: &mut SrTracker) -> DtimServiceResult {
        self.service_with(done, intc, sr, |_, _| true)
    }

    /// Offer due DTIM interrupts atomically to a CPU owner. A declined offer
    /// leaves the tick pending; the timer's REF host write still occurred.
    pub fn service_with(
        &mut self,
        done: u64,
        intc: &IntcBank,
        sr: &mut SrTracker,
        mut offer: impl FnMut(u16, u8) -> bool,
    ) -> DtimServiceResult {
        if self.held {
            return (Vec::new(), Vec::new());
        }
        let donef = done as f64;
        let mut writes = Vec::new();
        for channel_index in 0..self.channels.len() {
            let ci = self.channels[channel_index];
            match self.period(ci) {
                None => {
                    if self.ch[ci].next.is_some() {
                        self.ch[ci].transitions += 1;
                    }
                    self.ch[ci].next = None;
                    self.ch[ci].pending = false;
                    continue;
                }
                Some(p) => {
                    if self.ch[ci].next.is_none() {
                        self.ch[ci].next = Some(donef + p);
                        self.ch[ci].transitions += 1;
                        continue;
                    }
                    let next = self.ch[ci].next.unwrap();
                    if donef < next {
                        continue;
                    }
                    let mut nn = next + p;
                    if nn <= donef {
                        nn = donef + p;
                    }
                    self.ch[ci].next = Some(nn);
                    // Set DTER's REF bit on the due tick, mirroring
                    // `Dtims.service`'s read-modify-write (module docs).
                    let addr = BASES[ci] + DTER as u32;
                    let dter = self.regs[ci].u8_at(DTER);
                    let new_dter = dter | REF;
                    self.regs[ci].set_u8_at(DTER, new_dter);
                    writes.push((addr, new_dter));
                    if self.ch[ci].pending {
                        self.ch[ci].missed += 1;
                    } else {
                        self.ch[ci].pending = true;
                    }
                }
            }
        }
        if !self.ch.iter().any(|c| c.pending) {
            return (Vec::new(), writes);
        }
        let mut raised = Vec::new();
        for channel_index in 0..self.channels.len() {
            let ci = self.channels[channel_index];
            if !self.ch[ci].pending {
                continue;
            }
            let vec = VECTORS[ci];
            let Some(lvl) = intc.level_for_vector(vec) else {
                continue;
            };
            if sr.ipl() >= lvl {
                continue;
            }
            if !offer(vec, lvl) {
                continue;
            }
            sr.on_taken(Some(lvl), 0);
            self.ch[ci].pending = false;
            self.ch[ci].fired += 1;
            raised.push((vec, lvl));
        }
        (raised, writes)
    }

    /// A guest write-1-to-clear of DTER's REF bit discards a pending tick
    /// (`Dtims._on_write`); see `PitBank::note_pcsr_write`.
    pub fn note_dter_write(&mut self, ch: usize, ref_bit_set: bool) {
        if ref_bit_set && self.ch[ch].pending {
            self.ch[ch].pending = false;
            self.ch[ch].cleared += 1;
        }
    }

    pub fn load_checkpoint(&mut self, next: &[Option<f64>; 4], pending: &[bool; 4], held: bool) {
        self.load_checkpoint_state(next, pending, held, &[0; 4], &[0; 4], &[0; 4], Vec::new());
    }

    /// Restore scheduling and representable bookkeeping state. Register
    /// pages are intentionally loaded by the board's separate MMIO path.
    pub fn load_checkpoint_state(
        &mut self,
        next: &[Option<f64>; 4],
        pending: &[bool; 4],
        held: bool,
        fired: &[u64; 4],
        missed: &[u64; 4],
        cleared: &[u64; 4],
        stale: Vec<usize>,
    ) {
        self.held = held;
        self.stale = stale;
        for i in 0..4 {
            self.ch[i].next = next[i];
            self.ch[i].pending = pending[i];
            self.ch[i].fired = fired[i];
            self.ch[i].missed = missed[i];
            self.ch[i].cleared = cleared[i];
        }
    }

    pub fn is_held(&self) -> bool {
        self.held
    }

    pub fn pending(&self, ch: usize) -> bool {
        self.ch[ch].pending
    }
    /// Highest INTC level among pending channels that `service_with` would
    /// consider (0 for a pending channel without a deliverable level). While
    /// the CPU's IPL is at or above this and no deadline is reached,
    /// `service_with` changes nothing.
    pub fn max_pending_level(&self, intc: &IntcBank) -> u8 {
        self.channels
            .iter()
            .filter(|&&ci| self.ch[ci].pending)
            .map(|&ci| intc.level_for_vector(VECTORS[ci]).unwrap_or(0))
            .max()
            .unwrap_or(0)
    }
    pub fn next_deadline(&self, ch: usize) -> Option<f64> {
        self.ch[ch].next
    }
    pub fn missed(&self, ch: usize) -> u64 {
        self.ch[ch].missed
    }
    pub fn fired(&self, ch: usize) -> u64 {
        self.ch[ch].fired
    }
    pub fn cleared(&self, ch: usize) -> u64 {
        self.ch[ch].cleared
    }
    pub fn channels(&self) -> &[usize] {
        &self.channels
    }
    pub fn ips(&self) -> f64 {
        self.ips
    }
    pub fn stale(&self) -> &[usize] {
        &self.stale
    }

    pub const DTMR_OFFSET: usize = DTMR;
    pub const DTXMR_OFFSET: usize = DTXMR;
    pub const DTER_OFFSET: usize = DTER;
    pub const DTRR_OFFSET: usize = DTRR;
    pub const REF_BIT: u8 = REF;
    pub const RST_BIT: u16 = RST;
    pub const ORRI_BIT: u16 = ORRI;
    pub const FRR_BIT: u16 = FRR;
    pub const DMAEN_BIT: u8 = DMAEN;
}

impl DtimBank {
    pub fn snap_save(&self, w: &mut crate::snap::Writer) {
        w.bool(self.held);
        w.u64(self.stale.len() as u64);
        for s in &self.stale {
            w.u64(*s as u64);
        }
        for (regs, ch) in self.regs.iter().zip(&self.ch) {
            regs.snap_save(w);
            w.opt_f64(ch.next);
            w.bool(ch.pending);
            w.u64(ch.missed);
            w.u64(ch.cleared);
            w.u64(ch.fired);
            w.u64(ch.transitions);
        }
    }
    pub fn snap_load(&mut self, r: &mut crate::snap::Reader) -> crate::snap::Result<()> {
        self.held = r.bool()?;
        let n = r.len(1 << 16)?;
        self.stale = (0..n)
            .map(|_| r.u64().map(|v| v as usize))
            .collect::<Result<_, _>>()?;
        for (regs, ch) in self.regs.iter_mut().zip(&mut self.ch) {
            regs.snap_load(r)?;
            ch.next = r.opt_f64()?;
            ch.pending = r.bool()?;
            ch.missed = r.u64()?;
            ch.cleared = r.u64()?;
            ch.fired = r.u64()?;
            ch.transitions = r.u64()?;
        }
        Ok(())
    }
}
