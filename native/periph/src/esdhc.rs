//! MCF5441x eSDHC register and early-command model (RM chapter 25).
//!
//! This intentionally stops before controller DMA/eDMA integration.  The card
//! is supplied by the owner of that lane through [`CardPort`].

use std::collections::VecDeque;

use crate::regfile::{RegFile, SLOT_SIZE};

pub const BASE: u32 = 0xFC0C_C000;
pub const SIZE: u32 = SLOT_SIZE as u32;
pub const CMDARG: u32 = 0x08;
pub const XFERTYP: u32 = 0x0C;
pub const CMDRSP0: u32 = 0x10;
pub const CMDRSP1: u32 = 0x14;
pub const CMDRSP2: u32 = 0x18;
pub const CMDRSP3: u32 = 0x1C;
pub const DATPORT: u32 = 0x20;
pub const PRSSTAT: u32 = 0x24;
pub const SYSCTL: u32 = 0x2C;
pub const IRQSTAT: u32 = 0x30;

const DPSEL: u32 = 1 << 21;
const DTDSEL: u32 = 1 << 4;
const BREN: u32 = 1 << 11;
const BWEN: u32 = 1 << 10;
const CC: u32 = 1 << 0;
const TC: u32 = 1 << 1;
const BWR: u32 = 1 << 4;
const BRR: u32 = 1 << 5;
const SELF_CLEAR: u32 = 0x0F00_0000; // RSTA/RSTC/RSTD/INITA

/// The card-side contract needed by the controller.  Storage and eDMA are
/// deliberately outside this module; `data_for`/`write_data` are retained so
/// that a machine can connect them without changing command semantics.
pub trait CardPort {
    fn command(&mut self, idx: u8, arg: u32) -> [u32; 4];
    fn read_word(&mut self, idx: u8, pattern: u32) -> u32;
    fn data_for(&mut self, idx: u8, arg: u32, len: usize) -> Option<Vec<u8>>;
    fn write_data(&mut self, idx: u8, arg: u32, payload: &[u8]);
}

/// Guest-write behavior for registers where the Python oracle intentionally
/// differs from device hardware.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RegisterPolicy {
    /// Match `emu.esdhc.Esdhc`: guest writes store directly in IRQSTAT.
    #[default]
    Oracle,
    /// Apply the RM's device behavior: IRQSTAT guest writes are W1C.
    Device,
}

/// Pure register model.  Command completion is synchronous, as in the
/// oracle's early eMMC model; interrupt delivery and DMA are machine duties.
pub struct Esdhc<P> {
    regs: RegFile,
    card: P,
    policy: RegisterPolicy,
    pattern: u32,
    host_writes: VecDeque<(u32, [u8; 4])>,
}

impl<P: CardPort> Esdhc<P> {
    /// Construct an oracle-compatible controller. Use [`Self::with_policy`]
    /// for hardware register behavior.
    pub fn new(card: P) -> Self {
        Self::with_policy(card, RegisterPolicy::Oracle)
    }

    pub fn with_policy(card: P, policy: RegisterPolicy) -> Self {
        let mut regs = RegFile::new();
        // RM Table 25-2 reset values; CINS is asserted because this model has
        // a CardPort attached, matching the inserted-card board configuration.
        for (off, value) in [
            (0x04, 0x0001_0000),
            (PRSSTAT, 0xFF89_00F8),
            (0x28, 0x0000_0020),
            (SYSCTL, 0x0000_8008),
            (0x34, 0x117F_013F),
            (0x40, 0x07F3_0000),
            (0x44, 0x0810_0810),
            (0xC0, 1),
            (0xFC, 0x0000_1201),
        ] {
            regs.set_u32_at(off as usize, value);
        }
        Self {
            regs,
            card,
            policy,
            pattern: 0,
            host_writes: VecDeque::new(),
        }
    }

    pub fn owns(addr: u32) -> bool {
        (BASE..BASE + SIZE).contains(&addr)
    }

    pub fn read(&mut self, addr: u32, size: u8) -> Option<u32> {
        if !Self::owns(addr) {
            return None;
        }
        if addr - BASE == SYSCTL {
            let value = self.regs.u32_at(SYSCTL as usize);
            if value & SELF_CLEAR != 0 {
                self.put(SYSCTL, value & !SELF_CLEAR);
            }
        }
        Some(self.regs.read(addr - BASE, size))
    }

    /// Host-side register writes caused by a controller side effect, in
    /// oracle order. Guest MMIO writes are deliberately not included.
    pub fn take_host_write(&mut self) -> Option<(u32, [u8; 4])> {
        self.host_writes.pop_front()
    }

    fn put(&mut self, off: u32, value: u32) {
        self.regs.set_u32_at(off as usize, value);
        self.host_writes
            .push_back((BASE + off, value.to_be_bytes()));
    }

    pub fn write(&mut self, addr: u32, size: u8, value: u32) -> bool {
        if !Self::owns(addr) {
            return false;
        }
        let off = addr - BASE;
        if off == IRQSTAT && size == 4 && self.policy == RegisterPolicy::Device {
            // RM Table 25-2: hardware IRQSTAT is write-one-to-clear. The
            // Python oracle deliberately does not hook this guest write.
            let current = self.regs.u32_at(IRQSTAT as usize);
            self.regs.set_u32_at(IRQSTAT as usize, current & !value);
            return true;
        }
        self.regs.write(off, size, value);
        if off == DATPORT && size == 4 {
            self.pattern = value;
        } else if off == XFERTYP && size == 4 {
            self.issue_command(value);
        }
        true
    }

    pub fn load_page(&mut self, base: u32, data: &[u8]) -> bool {
        if base != BASE {
            return false;
        }
        self.regs.load_page(data);
        true
    }

    fn issue_command(&mut self, xfer: u32) {
        let idx = ((xfer >> 24) & 0x3F) as u8;
        let arg = self.regs.u32_at(CMDARG as usize);
        let response = self.card.command(idx, arg);
        for (off, value) in [CMDRSP0, CMDRSP1, CMDRSP2, CMDRSP3]
            .into_iter()
            .zip(response)
        {
            self.put(off, value);
        }
        let mut status = self.regs.u32_at(PRSSTAT as usize) & !0x7;
        self.put(PRSSTAT, status);
        let mut irq = self.regs.u32_at(IRQSTAT as usize) | CC | TC;
        self.put(IRQSTAT, irq);
        if xfer & DPSEL != 0 {
            if xfer & DTDSEL != 0 {
                status |= BREN;
                irq |= BRR;
                self.put(PRSSTAT, status);
                irq |= BRR;
                self.put(IRQSTAT, irq);
                let word = self.card.read_word(idx, self.pattern);
                self.put(DATPORT, word);
            } else {
                status |= BWEN;
                irq |= BWR;
                self.put(PRSSTAT, status);
                self.put(IRQSTAT, irq);
            }
        }
    }
}
