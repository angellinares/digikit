//! The four Programmable Interrupt Timers, MCF5441XRM chapter 38.
//!
//! Register layout (offsets relative to each channel's 16 KiB base --
//! `0xFC08_0000`, `0xFC08_4000`, `0xFC08_8000`, `0xFC08_C000`, one per
//! channel):
//!
//! ```text
//! 0x00  PCSRn  16  R/W  control/status (Figure 38-2)
//! 0x02  PMRn   16  R/W  modulus (reload value)
//! 0x04  PCNTRn 16  R    live counter (not modelled: unexercised by the
//!                       recorded traces, and the Python oracle never
//!                       computes it either)
//! ```
//!
//! PCSRn bits: 11-8 PRE (prescaler exponent), 6 DOZE, 5 DBG, 4 OVW, 3 PIE,
//! 2 PIF (write-1-to-clear), 1 RLD, 0 EN.
//!
//! Vectors 205-208 (INTC2 sources 13, 14, 15, 16) for PIT0-3
//! (`docs/findings`, MCF5441XRM p.349).
//!
//! Delivery order within a level is highest-source-first (RM SS17.3.1,
//! Table 17-19), which for PIT0/2/3 (sources 13/15/16) is `(3, 2, 0)` --
//! `emu.pit.Pits`' own `channels` default and the order this bank's
//! `service` walks when given that order.
//!
//! This is a direct, field-for-field port of `emu.pit.Pits`: same float
//! arithmetic (`next[]` are instruction-count floats, exactly as Python's
//! `done + p` produces), same "one tick per `service` call, never more"
//! rule, same pending/missed/cleared bookkeeping. See `emu/pit.py`'s module
//! docstring for the hardware reasoning; this file does not repeat it.

use crate::intc::IntcBank;
use crate::regfile::RegFile;
use crate::sr::SrTracker;

pub const BASES: [u32; 4] = [0xFC080000, 0xFC084000, 0xFC088000, 0xFC08C000];
pub const VECTORS: [u16; 4] = [205, 206, 207, 208];

const PCSR: usize = 0x00;
const PMR: usize = 0x02;

const EN: u16 = 0x01;
const RLD: u16 = 0x02;
const PIF: u16 = 0x04;
const PIE: u16 = 0x08;

/// MCF5441XRM Eqn. 38-1: the internal bus clock, fixed by `emu.pit`'s own
/// derivation from the reset PLL configuration (see `emu/pit.py::F_BUS`).
pub const F_BUS: f64 = 132_000_000.0;

/// A step with no armed channel at all: only matters for `step()`'s
/// fallback, which the replay harness does not use (see module docs).
pub const IDLE_STEP: u64 = 1_000_000;

#[derive(Clone, Default)]
struct Channel {
    /// Absolute instruction-count deadline, or None while off/unarmed.
    /// A float, like Python's `next[]`: periods are not integers
    /// (`prescale * (pmr+1) / F_BUS * ips`), and matching the trace exactly
    /// means matching Python's float arithmetic, not rounding early.
    next: Option<f64>,
    pending: bool,
    missed: u64,
    cleared: u64,
    fired: u64,
    transitions: u64,
}

pub struct PitBank {
    regs: [RegFile; 4],
    /// Priority order this bank services channels in; `(3, 2, 0)` matches
    /// the INTC's fixed source-number priority for PIT0/2/3, but is a field
    /// (not a constant) because a window's `MARK "setup"` record states the
    /// channel set the Python run actually used, which a replay must match
    /// exactly -- an unlisted channel gets no scheduling logic at all (plain
    /// RAM), only the listed ones do.
    channels: Vec<usize>,
    ips: f64,
    held: bool,
    ch: [Channel; 4],
}

impl Default for PitBank {
    fn default() -> Self {
        Self {
            regs: [
                RegFile::new(),
                RegFile::new(),
                RegFile::new(),
                RegFile::new(),
            ],
            channels: vec![3, 2, 0],
            ips: 4_680_000.0,
            held: false,
            ch: Default::default(),
        }
    }
}

impl PitBank {
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

    /// A guest write is plain RAM (see module docs): store verbatim, no
    /// write-1-to-clear filtering on the stored bytes -- the Python oracle
    /// does not filter it either, it only *observes* the write. What the
    /// oracle's `_on_pcsr` hook does with that observation, this mirrors
    /// exactly: if the write touches PCSR's PIF byte with bit 2 set, on a
    /// channel this bank is actively scheduling, a pending tick is
    /// discarded before the CPU ever sees it (`Pits._on_pcsr`; see the
    /// class docs in `emu/pit.py`, "A refused tick is held, not dropped" --
    /// the RTOS's own context-switch handler does this on every switch).
    pub fn write(&mut self, addr: u32, size: u8, value: u32) -> bool {
        let Some(ch) = Self::slot(addr) else {
            return false;
        };
        self.regs[ch].write(addr - BASES[ch], size, value);
        if self.channels.contains(&ch) {
            let target = BASES[ch] as i64 + 1; // PCSR's low byte (PIF is bit 2)
            let start = addr as i64;
            let end = start + size as i64; // exclusive
            if start <= target && target < end {
                let shift = (end - 1) - target;
                let byte = ((value >> (8 * shift as u32)) & 0xFF) as u16;
                self.note_pcsr_write(ch, true, byte & PIF != 0);
            }
        }
        true
    }

    pub fn set_ips(&mut self, ips: f64) {
        self.ips = ips;
    }

    /// Set the active channel set and rate without disturbing register
    /// bytes or scheduling state -- a trace's `MARK "setup"` record can
    /// arrive after its initial `STATE` checkpoint (both are recorded before
    /// the first `STEP`, in that order), and a replay must not let
    /// configuring the bank discard a checkpoint it already loaded.
    pub fn configure(&mut self, channels: Vec<usize>, ips: f64) {
        self.channels = channels;
        self.ips = ips;
    }

    /// Change the time base, keeping each armed deadline's device time
    /// (`Pits.rescale`): `next = now + (next - now) * new/old`.
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

    /// MCF5441XRM Eqn. 38-1 / Table 38-3: instructions between interrupts,
    /// or None while off (`EN` or `PIE` clear). `Pits.period` exactly.
    pub fn period(&self, ch: usize) -> Option<f64> {
        let pcsr = self.regs[ch].u16_at(PCSR);
        let pmr = self.regs[ch].u16_at(PMR);
        if pcsr & EN == 0 || pcsr & PIE == 0 {
            return None;
        }
        let prescale = 1u32 << ((pcsr >> 8) & 0xF);
        Some((prescale as f64) * ((pmr as f64) + 1.0) / F_BUS * self.ips)
    }

    /// -> the earliest deadline among this bank's active channels, arming
    /// any that just turned on and disarming any just switched off (mirrors
    /// `Pits.deadline`; not used by the replay harness -- see module docs
    /// -- but is the interface a live CPU-driven loop calls).
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

    /// -> instructions to run before the next `service` call is due
    /// (`Pits.step`).
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

    /// Advance to instruction count `done`, delivering at most one tick per
    /// armed channel, in `channels` priority order, exactly as
    /// `Pits.service`/`deliver_pending` do. -> the vectors actually raised,
    /// in delivery order, each with its ICR level.
    pub fn service(&mut self, done: u64, intc: &IntcBank, sr: &mut SrTracker) -> Vec<(u16, u8)> {
        self.service_with(done, intc, sr, |_, _| true)
    }

    /// Service due PIT channels, retaining a pending tick when `offer`
    /// declines it. This lets a caller validate an interrupt handler before
    /// consuming the oracle-visible pending state.
    pub fn service_with(
        &mut self,
        done: u64,
        intc: &IntcBank,
        sr: &mut SrTracker,
        mut offer: impl FnMut(u16, u8) -> bool,
    ) -> Vec<(u16, u8)> {
        if self.held {
            return Vec::new();
        }
        let donef = done as f64;
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
                    if self.ch[ci].pending {
                        self.ch[ci].missed += 1;
                    } else {
                        self.ch[ci].pending = true;
                    }
                }
            }
        }
        if !self.ch.iter().any(|c| c.pending) {
            return Vec::new();
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
        raised
    }

    /// A guest write-1-to-clear of PIF discards a pending tick before the
    /// CPU takes it (`Pits._on_pcsr`): call this after routing a `write` to
    /// PCSR's address, with the byte(s) actually written and their offset
    /// within the 16-bit register, so the replay's `cleared` bookkeeping
    /// matches (informational; does not gate delivery -- see the class docs
    /// in `emu/pit.py`, "A refused tick is held, not dropped").
    pub fn note_pcsr_write(&mut self, ch: usize, byte_touches_pif: bool, pif_bit_set: bool) {
        if byte_touches_pif && pif_bit_set && self.ch[ch].pending {
            self.ch[ch].pending = false;
            self.ch[ch].cleared += 1;
        }
    }

    // -- checkpoint resync (from a trace `STATE` record's `Pits` blob) ------

    pub fn load_checkpoint(&mut self, next: &[Option<f64>; 4], pending: &[bool; 4], held: bool) {
        self.load_checkpoint_state(next, pending, held, &[0; 4], &[0; 4], &[0; 4]);
    }

    /// Restore timer scheduling state from a portable checkpoint. Register
    /// pages are intentionally loaded by the board's separate MMIO path.
    pub fn load_checkpoint_state(
        &mut self,
        next: &[Option<f64>; 4],
        pending: &[bool; 4],
        held: bool,
        fired: &[u64; 4],
        missed: &[u64; 4],
        cleared: &[u64; 4],
    ) {
        self.held = held;
        for i in 0..4 {
            self.ch[i].next = next[i];
            self.ch[i].pending = pending[i];
            self.ch[i].fired = fired[i];
            self.ch[i].missed = missed[i];
            self.ch[i].cleared = cleared[i];
        }
    }

    pub fn pending(&self, ch: usize) -> bool {
        self.ch[ch].pending
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
    pub fn set_channels(&mut self, channels: Vec<usize>) {
        self.channels = channels;
    }

    pub const PCSR_OFFSET: usize = PCSR;
    pub const PMR_OFFSET: usize = PMR;
    pub const PIF_BIT: u16 = PIF;
    pub const EN_BIT: u16 = EN;
    pub const RLD_BIT: u16 = RLD;
    pub const PIE_BIT: u16 = PIE;
}
