//! The three interrupt controllers (INTC0/1/2), MCF5441XRM chapter 17.
//!
//! Register map (offsets relative to each controller's 16 KiB base --
//! `0xFC048000`, `0xFC04C000`, `0xFC050000` -- manual Table 17-2):
//!
//! ```text
//! 0x00  IPRH      32  R    interrupt pending, sources 32-63
//! 0x04  IPRL      32  R    interrupt pending, sources 0-31
//! 0x08  IMRH      32  R/W  interrupt mask, sources 32-63 (reset all-1s)
//! 0x0C  IMRL      32  R/W  interrupt mask, sources 0-31  (reset all-1s)
//! 0x10  INTFRCH   32  R/W  software force, sources 32-63
//! 0x14  INTFRCL   32  R/W  software force, sources 0-31
//! 0x1A  ICONFIG   16  R/W  one copy machine-wide, INTC0 space only
//! 0x1C  SIMR       8  W    set IMR bits (SALL bit 6, fields bit 5-0)
//! 0x1D  CIMR       8  W    clear IMR bits (CALL bit 6)
//! 0x1E  CLMASK     8  R/W  one copy machine-wide, INTC0 space only
//! 0x1F  SLMASK     8  R/W  one copy machine-wide, INTC0 space only
//! 0x40+n ICRn      8  R/W  level 0-7 for source n (n = 0..63)
//! 0xE0  SWIACK     8  R    software IACK: highest pending vector
//! 0xE0+4n LnIACK   8  R    level-n IACK: highest pending vector at level n
//! ```
//!
//! Vector numbers: `64*index + source` (index 0/1/2 for INTC0/1/2, RM
//! section 17.3.1.3).
//!
//! **Oracle-compatible mode** (used by the replay harness, [`IntcBank::read`]
//! / [`IntcBank::write`]): the Python emulator (removed; see tag unicorn-reference-final) never intercepts a guest
//! access to any of these registers -- `unicorn-reference-final:emu/pit.py`'s `interrupt_level` only reads
//! IMR/ICR bytes it did not write, so from the guest's side every register
//! here, including IPR, INTFRC, SIMR and CIMR, is plain RAM (last write
//! wins). That is what `read`/`write` implement, so the replay's RD checks
//! match the trace by construction; [`IntcBank::level_for_vector`] is the
//! one piece of interpretation the oracle model needs, mirroring
//! `unicorn-reference-final:emu/pit.py`'s `interrupt_level` exactly.
//!
//! **Hardware-accurate mode** (manual-only; NOT used against these traces,
//! since the frozen oracle does not implement it -- see finding "the INTC
//! lane should model INTFRC" and MCF5441XRM SS17.2.3/17.2.5/17.2.6, pages
//! 339-341): [`IntcBank::hw_apply_simr_cimr`], [`IntcBank::hw_computed_ipr`]
//! and [`IntcBank::hw_iack`] implement SIMR/CIMR actually mutating IMR, IPR
//! reading as the OR of forced and asserted sources, and IACK vector
//! determination, all exactly as the manual specifies. A caller building the
//! eventual whole-machine INTC composes these; the oracle replay does not
//! call them, because the Python model's masking decisions are based on the
//! stale IMR SIMR/CIMR never touched.

use crate::regfile::RegFile;

pub const BASES: [u32; 3] = [0xFC048000, 0xFC04C000, 0xFC050000];
pub const VECTOR_BASE: [u16; 3] = [64, 128, 192];

pub const IPRH_OFFSET: usize = 0x00;
pub const IPRL_OFFSET: usize = 0x04;
const IMRH: usize = 0x08;
const IMRL: usize = 0x0C;
const INTFRCH: usize = 0x10;
const INTFRCL: usize = 0x14;
const ICONFIG: usize = 0x1A;
const SIMR: usize = 0x1C;
const CIMR: usize = 0x1D;
const CLMASK: usize = 0x1E;
const SLMASK: usize = 0x1F;
const ICR_BASE: usize = 0x40;
const SWIACK: usize = 0xE0;

/// IMR resets to all-ones (every source masked); reset only matters for a
/// fresh controller, since a replay always resyncs from a trace `PAGE`
/// before relying on any value.
const IMR_RESET: u32 = 0xFFFF_FFFF;

pub struct IntcBank {
    ctrl: [RegFile; 3],
}

impl Default for IntcBank {
    fn default() -> Self {
        let mut ctrl = [RegFile::new(), RegFile::new(), RegFile::new()];
        for c in &mut ctrl {
            c.set_u32_at(IMRH, IMR_RESET);
            c.set_u32_at(IMRL, IMR_RESET);
        }
        Self { ctrl }
    }
}

impl IntcBank {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fresh unmodelled INTC backing as created by the Python oracle's
    /// zero-mapped MMIO fault path. This deliberately differs from hardware
    /// reset: the oracle starts with every register byte, including IMR,
    /// clear until guest code writes it.
    pub fn oracle_zeroed() -> Self {
        Self {
            ctrl: [RegFile::new(), RegFile::new(), RegFile::new()],
        }
    }

    /// -> (controller index, offset within its slot), or None if `addr` is
    /// outside all three controllers' 16 KiB slots.
    #[inline]
    fn locate(addr: u32) -> Option<(usize, u32)> {
        for (i, base) in BASES.iter().enumerate() {
            if addr >= *base && addr < base + crate::regfile::SLOT_SIZE as u32 {
                return Some((i, addr - base));
            }
        }
        None
    }

    #[inline]
    pub fn owns(addr: u32) -> bool {
        Self::locate(addr).is_some()
    }

    /// Resync one controller's slot from a recorded `PAGE`.
    pub fn load_page(&mut self, base: u32, data: &[u8]) -> bool {
        if let Some(i) = BASES.iter().position(|b| *b == base) {
            self.ctrl[i].load_page(data);
            true
        } else {
            false
        }
    }

    // -- oracle-compatible passthrough (matched the Python emulator (removed; see tag unicorn-reference-final)) --------

    pub fn read(&self, addr: u32, size: u8) -> Option<u32> {
        let (i, off) = Self::locate(addr)?;
        Some(self.ctrl[i].read(off, size))
    }

    pub fn write(&mut self, addr: u32, size: u8, value: u32) -> bool {
        match Self::locate(addr) {
            Some((i, off)) => {
                self.ctrl[i].write(off, size, value);
                true
            }
            None => false,
        }
    }

    /// -> the source's ICR level (1-7), or None if level 0 (disabled) or
    /// masked in IMR. Same computation as `unicorn-reference-final:emu/pit.py`'s `interrupt_level` /
    /// `Pits.level` / `Dtims.level`: read-only, no side effect.
    pub fn level_for_vector(&self, vector: u16) -> Option<u8> {
        let (ctrl_idx, first) = VECTOR_BASE
            .iter()
            .enumerate()
            .find(|(_, base)| vector >= **base && vector < **base + 64)?;
        let src = (vector - first) as u32;
        let c = &self.ctrl[ctrl_idx];
        let icr = c.u8_at(ICR_BASE + src as usize) & 0x07;
        if icr == 0 {
            return None;
        }
        let masked = if src < 32 {
            (c.u32_at(IMRL) >> src) & 1
        } else {
            (c.u32_at(IMRH) >> (src - 32)) & 1
        };
        if masked != 0 { None } else { Some(icr) }
    }

    // -- hardware-accurate extras (manual-only; not used in oracle replay) --

    /// Whether an access touches either mask-command byte in any controller.
    /// Includes accesses starting at a command with invalid size zero, so Device
    /// callers can reject unsupported widths before passthrough or cache mutation.
    pub fn hw_touches_simr_cimr(addr: u32, size: u8) -> bool {
        BASES.iter().any(|base| {
            let first = u64::from(*base) + SIMR as u64;
            let last = u64::from(*base) + CIMR as u64;
            let start = u64::from(addr);
            (first..=last).contains(&start) || start < first && start + u64::from(size) > first
        })
    }

    /// MCF54418RM p0341/p0342: a byte SIMR sets its selected IMR bit;
    /// CIMR clears it. Bit 6 acts on both halves and ignores the source field.
    /// Sources 0..31 select IMRL, 32..63 select IMRH. Reserved bit 7 and
    /// non-command addresses reject before mutation. Device callers separately
    /// validate access width and return zero on reads; Oracle read/write remain
    /// passthrough and never call this helper.
    pub fn hw_apply_simr_cimr(&mut self, addr: u32, value: u8) -> bool {
        let Some((i, off)) = Self::locate(addr) else {
            return false;
        };
        let off = off as usize;
        if !matches!(off, SIMR | CIMR) || value & 0x80 != 0 {
            return false;
        }
        let set = off == SIMR;
        let c = &mut self.ctrl[i];
        if value & 0x40 != 0 {
            let mask = if set { IMR_RESET } else { 0 };
            c.set_u32_at(IMRL, mask);
            c.set_u32_at(IMRH, mask);
        } else {
            let source = value & 0x3f;
            let register = if source < 32 { IMRL } else { IMRH };
            let bit = 1u32 << (source % 32);
            let mask = c.u32_at(register);
            c.set_u32_at(register, if set { mask | bit } else { mask & !bit });
        }
        true
    }

    /// MCF5441XRM SS17.2.1/17.2.3 (p.336, 339): IPR is the logical OR of the
    /// real per-source signal and the software force register. This crate
    /// only tracks sources in its own lane (PIT/DTIM), so `asserted` (the
    /// non-force bits this bank should contribute, e.g. a PIT channel's
    /// `pending` flag) must be supplied by the caller; other lanes' sources
    /// read as zero here.
    pub fn hw_computed_ipr(
        &self,
        ctrl_idx: usize,
        asserted_low: u32,
        asserted_high: u32,
    ) -> (u32, u32) {
        let c = &self.ctrl[ctrl_idx];
        let iprl = asserted_low | c.u32_at(INTFRCL);
        let iprh = asserted_high | c.u32_at(INTFRCH);
        (iprl, iprh)
    }

    /// Select from externally asserted (low, high) masks and INTFRC for all
    /// three controllers, ignoring CPU IPL. Read-only: entry does not clear a
    /// request. MCF54418RM local p0352 specifies higher source first within a
    /// level; p0353 describes force recognition and vector mapping; p0354
    /// specifies INTC0 > INTC1 > INTC2 at equal level. This is candidate
    /// selection only, not IACK, controller level masking or level-7 timing.
    pub fn hw_candidate(&self, asserted: [(u32, u32); 3]) -> Option<(u16, u8)> {
        let mut best: Option<(u16, u8)> = None;
        // Visit equal-level candidates in priority order and retain the first.
        for (i, (low, high)) in asserted.into_iter().enumerate() {
            let (low, high) = self.hw_computed_ipr(i, low, high);
            for source in (0u32..64).rev() {
                let pending = if source < 32 {
                    low >> source
                } else {
                    high >> (source - 32)
                } & 1;
                if pending == 0 {
                    continue;
                }
                let vector = VECTOR_BASE[i] + source as u16;
                if let Some(level) = self.level_for_vector(vector)
                    && best.is_none_or(|(_, best_level)| level > best_level)
                {
                    best = Some((vector, level));
                }
            }
        }
        best
    }

    /// The software-forced source (INTFRCH/INTFRCL) that would be offered
    /// next, ignoring the CPU IPL: the highest (level, vector) among forced,
    /// unmasked sources with a nonzero ICR level. `skip_vector` is a source
    /// another model already delivers (INTC1 source 63, vector 191, is the
    /// SSI model's). A forced source stays asserted until the guest clears
    /// its bit, so the caller offers it only above the live IPL.
    pub fn forced_candidate(&self, skip_vector: u16) -> Option<(u16, u8)> {
        let mut best: Option<(u16, u8)> = None;
        for (i, c) in self.ctrl.iter().enumerate() {
            let (lo, hi) = (c.u32_at(INTFRCL), c.u32_at(INTFRCH));
            if lo == 0 && hi == 0 {
                continue;
            }
            for src in 1u32..64 {
                let bit = if src < 32 {
                    lo >> src
                } else {
                    hi >> (src - 32)
                } & 1;
                if bit == 0 {
                    continue;
                }
                let vector = VECTOR_BASE[i] + src as u16;
                if vector == skip_vector {
                    continue;
                }
                let Some(level) = self.level_for_vector(vector) else {
                    continue;
                };
                if best.is_none_or(|(bv, bl)| (level, vector) > (bl, bv)) {
                    best = Some((vector, level));
                }
            }
        }
        best
    }

    pub fn hw_intfrc_bit(&self, ctrl_idx: usize, src: u32) -> bool {
        let c = &self.ctrl[ctrl_idx];
        if src < 32 {
            (c.u32_at(INTFRCL) >> src) & 1 != 0
        } else {
            (c.u32_at(INTFRCH) >> (src - 32)) & 1 != 0
        }
    }

    /// MCF5441XRM SS17.2.10 (p.351-352): a level-n IACK returns the highest
    /// source number pending & unmasked at exactly level `level` within
    /// `ctrl_idx`, or 0x18 (24, spurious) if none. `asserted` is the same
    /// per-lane signal as [`Self::hw_computed_ipr`] (OR'd with INTFRC here).
    pub fn hw_iack(&self, ctrl_idx: usize, level: u8, asserted_low: u32, asserted_high: u32) -> u8 {
        let (iprl, iprh) = self.hw_computed_ipr(ctrl_idx, asserted_low, asserted_high);
        let c = &self.ctrl[ctrl_idx];
        let imrl = c.u32_at(IMRL);
        let imrh = c.u32_at(IMRH);
        let mut best: Option<u32> = None;
        for src in (0u32..64).rev() {
            let (pending, masked) = if src < 32 {
                ((iprl >> src) & 1, (imrl >> src) & 1)
            } else {
                ((iprh >> (src - 32)) & 1, (imrh >> (src - 32)) & 1)
            };
            if pending == 0 || masked != 0 {
                continue;
            }
            if c.u8_at(ICR_BASE + src as usize) & 0x07 == level {
                best = Some(src);
                break;
            }
        }
        match best {
            Some(src) => (VECTOR_BASE[ctrl_idx] as u32 + src) as u8,
            None => 0x18,
        }
    }

    pub fn icr(&self, ctrl_idx: usize, src: u32) -> u8 {
        self.ctrl[ctrl_idx].u8_at(ICR_BASE + src as usize) & 0x07
    }

    /// Read-only ICONFIG/CLMASK/SLMASK (one copy machine-wide, RM SS17.2.4/
    /// .7/.8): always resolved through INTC0's slot regardless of which
    /// controller's `addr` targeted them, matching "all reads and writes to
    /// this register must be made to the INTC0 memory space" -- a write to
    /// INTC1/2's alias is simply unmodelled RAM in their own slot.
    pub fn iconfig(&self) -> u16 {
        self.ctrl[0].u16_at(ICONFIG)
    }
    pub fn clmask(&self) -> u8 {
        self.ctrl[0].u8_at(CLMASK) & 0x0F
    }
    pub fn slmask(&self) -> u8 {
        self.ctrl[0].u8_at(SLMASK) & 0x0F
    }
    pub fn swiack_offset() -> usize {
        SWIACK
    }
}

impl IntcBank {
    pub fn snap_save(&self, w: &mut crate::snap::Writer) {
        for c in &self.ctrl {
            c.snap_save(w);
        }
    }
    pub fn snap_load(&mut self, r: &mut crate::snap::Reader) -> crate::snap::Result<()> {
        for c in &mut self.ctrl {
            c.snap_load(r)?;
        }
        Ok(())
    }
}
