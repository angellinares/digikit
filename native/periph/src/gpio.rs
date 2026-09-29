//! GPIO port-D-to-port-C SD continuity gate (MCF5441x RM chapter 15).
//!
//! Only the board wiring used by `emu::gpio::SdGate` is modelled: PPDSDR_D
//! bit 4 drives PPDSDR_C bit 3. All other bytes retain ordinary register-file
//! behaviour so PAGE checkpoints continue to seed unmodelled GPIO state.

use crate::regfile::{RegFile, SLOT_SIZE};

pub const GPIO_BASE: u32 = 0xEC09_4000;
pub const PPDSDR_C: u32 = GPIO_BASE + 0x1A;
pub const PPDSDR_D: u32 = GPIO_BASE + 0x1B;
pub const PCLRR_D: u32 = GPIO_BASE + 0x27;
const DRIVE_BIT: u8 = 0x10;
const SENSE_BIT: u8 = 0x08;

/// The narrow board loopback which makes the eSDHC probe pass.
#[derive(Default)]
pub struct SdGate {
    regs: RegFile,
    driven: bool,
}

impl SdGate {
    pub fn owns(addr: u32) -> bool {
        addr >= GPIO_BASE && addr < GPIO_BASE + SLOT_SIZE as u32
    }

    /// Seed raw GPIO bytes from a trace checkpoint. This deliberately does
    /// not infer `driven` from PPDSDR_D: the hook's state is separate from
    /// the backing bytes in the oracle and is supplied by `load_state`.
    pub fn load_page(&mut self, base: u32, data: &[u8]) -> bool {
        if base != GPIO_BASE {
            return false;
        }
        self.regs.load_page(data);
        true
    }

    /// Seed the hook state recorded in a trace STATE record.
    pub fn load_state(&mut self, driven: bool) {
        self.driven = driven;
    }

    /// Guest writes retain their raw register value, with the two hook
    /// addresses also changing the board's driven side.
    pub fn write(&mut self, addr: u32, size: u8, value: u32) {
        self.regs.write(addr, size, value);
        if size != 1 {
            return;
        }
        if addr == PPDSDR_D && value as u8 & DRIVE_BIT != 0 {
            self.driven = true;
        } else if addr == PCLRR_D && value as u8 & DRIVE_BIT == 0 {
            self.driven = false;
        }
    }

    /// The host's read hook writes the sensed value before the guest read.
    /// Returning it lets replay check the subsequent RD independently of the
    /// recorded HWR; unrelated port-C bits are retained.
    pub fn sense(&mut self) -> u8 {
        let value = (self.regs.u8_at((PPDSDR_C - GPIO_BASE) as usize) & !SENSE_BIT)
            | if self.driven { SENSE_BIT } else { 0 };
        self.regs.set_u8_at((PPDSDR_C - GPIO_BASE) as usize, value);
        value
    }

    /// Only the sensed pin is a gate prediction. Other GPIO bytes remain
    /// trace-seeded backing RAM and are intentionally outside this slice.
    pub fn read(&self, addr: u32, size: u8) -> Option<u32> {
        (addr == PPDSDR_C && size == 1).then(|| self.regs.read(addr, size))
    }
}
