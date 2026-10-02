//! A peripheral's raw register bytes: one 16 KiB slot, big-endian, exactly
//! as `emu/mmiotrace.py`'s `PAGE` records capture it and as the MCF5441x
//! bus presents it.
//!
//! For every register this crate's models do not specifically interpret,
//! reading back the last written bytes IS the correct behaviour: the
//! Python/Unicorn oracle has no special handling for them either (see
//! `docs/findings` on "unmodelled registers" -- they behave as plain RAM).
//! `PitBank`/`DtimBank`/`IntcBank` layer field-level semantics (masking,
//! prescaler math, write-1-to-clear) on top of one of these per instance.

/// MCF5441x peripherals are mapped in 16 KiB (0x4000-byte) slots (RM Tables
/// 1-3/1-4), and `emu/mmiotrace.py`'s `PAGE` records dump exactly one slot.
pub const SLOT_SIZE: usize = 0x4000;

#[derive(Clone)]
pub struct RegFile {
    bytes: Box<[u8; SLOT_SIZE]>,
}

impl Default for RegFile {
    fn default() -> Self {
        Self {
            bytes: Box::new([0u8; SLOT_SIZE]),
        }
    }
}

impl RegFile {
    pub fn new() -> Self {
        Self::default()
    }

    /// Resync from a recorded `PAGE` record's bytes (must be exactly one
    /// slot; the recorder only ever dumps whole, non-zero slots).
    pub fn load_page(&mut self, data: &[u8]) {
        let n = data.len().min(SLOT_SIZE);
        self.bytes[..n].copy_from_slice(&data[..n]);
        for b in &mut self.bytes[n..] {
            *b = 0;
        }
    }

    #[inline]
    fn off(addr_low: u32) -> usize {
        (addr_low as usize) & (SLOT_SIZE - 1)
    }

    /// Big-endian read of `size` bytes (1, 2 or 4) at `addr_low` (the
    /// address's low bits within the slot; callers pass the full guest
    /// address, this just masks it).
    pub fn read(&self, addr_low: u32, size: u8) -> u32 {
        let o = Self::off(addr_low);
        match size {
            1 => self.bytes[o] as u32,
            2 => u16::from_be_bytes([self.bytes[o], self.bytes[o + 1]]) as u32,
            4 => u32::from_be_bytes([
                self.bytes[o],
                self.bytes[o + 1],
                self.bytes[o + 2],
                self.bytes[o + 3],
            ]),
            _ => panic!("unsupported register access size {size}"),
        }
    }

    pub fn write(&mut self, addr_low: u32, size: u8, value: u32) {
        let o = Self::off(addr_low);
        match size {
            1 => self.bytes[o] = value as u8,
            2 => self.bytes[o..o + 2].copy_from_slice(&(value as u16).to_be_bytes()),
            4 => self.bytes[o..o + 4].copy_from_slice(&value.to_be_bytes()),
            _ => panic!("unsupported register access size {size}"),
        }
    }

    /// Convenience accessors for field-level code (offsets relative to a
    /// channel/controller base, not full guest addresses).
    pub fn u8_at(&self, off: usize) -> u8 {
        self.bytes[off]
    }
    pub fn set_u8_at(&mut self, off: usize, v: u8) {
        self.bytes[off] = v;
    }
    pub fn u16_at(&self, off: usize) -> u16 {
        u16::from_be_bytes([self.bytes[off], self.bytes[off + 1]])
    }
    pub fn set_u16_at(&mut self, off: usize, v: u16) {
        self.bytes[off..off + 2].copy_from_slice(&v.to_be_bytes());
    }
    pub fn u32_at(&self, off: usize) -> u32 {
        u32::from_be_bytes([
            self.bytes[off],
            self.bytes[off + 1],
            self.bytes[off + 2],
            self.bytes[off + 3],
        ])
    }
    pub fn set_u32_at(&mut self, off: usize, v: u32) {
        self.bytes[off..off + 4].copy_from_slice(&v.to_be_bytes());
    }

    pub fn raw(&self) -> &[u8; SLOT_SIZE] {
        &self.bytes
    }
}

impl RegFile {
    pub fn snap_save(&self, w: &mut crate::snap::Writer) {
        w.raw(&self.bytes[..]);
    }
    pub fn snap_load(&mut self, r: &mut crate::snap::Reader) -> crate::snap::Result<()> {
        self.bytes.copy_from_slice(r.raw(SLOT_SIZE)?);
        Ok(())
    }
}
