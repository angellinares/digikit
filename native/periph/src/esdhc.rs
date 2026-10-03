//! MCF5441x eSDHC register and early-command model (RM chapter 25).
//!
//! This intentionally stops before controller DMA/eDMA integration.  The card
//! is supplied by the owner of that lane through [`CardPort`].

use std::collections::VecDeque;

use crate::{
    edma::{CSR_D_REQ, CSR_DONE, CSR_INT_MAJOR, TcdSnapshot},
    regfile::{RegFile, SLOT_SIZE},
};

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

/// The only eDMA channel attached to the eSDHC data port in the recorded
/// boot paths.
pub const DMA_CHANNEL: usize = 59;
/// A transfer helper must never turn a malformed descriptor into an
/// unbounded allocation or copy.
pub const MAX_DMA_BYTES: usize = 1024 * 1024;

/// Direction selected by an eMMC command for channel 59.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DmaDirection {
    /// CMD8 SEND_EXT_CSD or CMD18 READ_MULTIPLE_BLOCK: card to guest memory.
    CardToGuest,
    /// CMD25 WRITE_MULTIPLE_BLOCK: guest memory to card.
    GuestToCard,
}

/// Completion information the machine owner may use to queue its own eDMA
/// interrupt/disable-request work. This helper deliberately does not deliver
/// an ISR.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DmaCompletion {
    pub done: bool,
    pub major_interrupt: bool,
    pub disable_request: bool,
}

/// The result of a bounded channel-59 request. Apply `tcd` to the eDMA
/// register bank only after this function returns successfully.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DmaEffect {
    pub direction: DmaDirection,
    pub bytes: usize,
    pub tcd: TcdSnapshot,
    pub completion: DmaCompletion,
}

/// Why a pure eSDHC eDMA request could not be performed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DmaError {
    UnsupportedCommand(u8),
    TransferTooLarge { bytes: u64 },
    AddressOverflow { address: u32, bytes: usize },
    GuestOutOfRange { address: u32, bytes: usize },
    CardBufferTooSmall { available: usize, needed: usize },
}

/// External, preallocated storage for one request. `card` is the command's
/// selected data window: for CMD8 it is EXT_CSD, for CMD18/CMD25 it is the
/// selected consecutive card blocks. No file or card-object access occurs in
/// the helper.
pub struct DmaBuffers<'a> {
    pub guest_base: u32,
    pub guest: &'a mut [u8],
    pub card: &'a mut [u8],
}

impl DmaBuffers<'_> {
    fn guest_range(&self, address: u32, bytes: usize) -> Result<usize, DmaError> {
        // A caller-owned slice may span more address space than a 32-bit
        // guest pointer can name. Reject that range before touching either
        // guest or card data (notably the strided CMD25 source path).
        if address.checked_add(bytes as u32).is_none() {
            return Err(DmaError::AddressOverflow { address, bytes });
        }
        let Some(offset) = address.checked_sub(self.guest_base) else {
            return Err(DmaError::GuestOutOfRange { address, bytes });
        };
        let offset = offset as usize;
        if offset
            .checked_add(bytes)
            .is_none_or(|end| end > self.guest.len())
        {
            return Err(DmaError::GuestOutOfRange { address, bytes });
        }
        Ok(offset)
    }
}

/// Execute the data movement and TCD writeback performed by
/// `emu.esdhc.Esdhc._dma_out`/`_dma_in` for eDMA channel 59.
///
/// CMD8 and CMD18 copy the supplied card window to `DADDR`, then advance
/// `DADDR` by the major-loop byte count. CMD25 copies each guest minor loop
/// from `SADDR`, advances by `SOFF`, then applies `SLAST`. All successful
/// transfers reload `CITER` from `BITER` and set CSR.DONE. The helper is
/// bounded by [`MAX_DMA_BYTES`] and operates only on caller-provided slices.
pub fn transfer_dma59(
    command: u8,
    tcd: TcdSnapshot,
    buffers: &mut DmaBuffers<'_>,
) -> Result<DmaEffect, DmaError> {
    let direction = match command {
        8 | 18 => DmaDirection::CardToGuest,
        25 => DmaDirection::GuestToCard,
        _ => return Err(DmaError::UnsupportedCommand(command)),
    };
    let bytes64 = u64::from(tcd.citer) * u64::from(tcd.nbytes);
    if bytes64 > MAX_DMA_BYTES as u64 {
        return Err(DmaError::TransferTooLarge { bytes: bytes64 });
    }
    let bytes = bytes64 as usize;
    if buffers.card.len() < bytes {
        return Err(DmaError::CardBufferTooSmall {
            available: buffers.card.len(),
            needed: bytes,
        });
    }

    // `_dma_out` returns before its TCD/CSR writes when CITER is zero.
    if bytes == 0 && direction == DmaDirection::CardToGuest {
        return Ok(DmaEffect {
            direction,
            bytes: 0,
            tcd,
            completion: DmaCompletion {
                done: false,
                major_interrupt: false,
                disable_request: false,
            },
        });
    }

    let mut after = tcd;
    match direction {
        DmaDirection::CardToGuest => {
            let next_daddr =
                tcd.daddr
                    .checked_add(bytes as u32)
                    .ok_or(DmaError::AddressOverflow {
                        address: tcd.daddr,
                        bytes,
                    })?;
            let dst = buffers.guest_range(tcd.daddr, bytes)?;
            buffers.guest[dst..dst + bytes].copy_from_slice(&buffers.card[..bytes]);
            after.daddr = next_daddr;
        }
        DmaDirection::GuestToCard => {
            let mut src = i64::from(tcd.saddr);
            let nbytes = tcd.nbytes as usize;
            // Validate every strided source before mutating the card window,
            // so an address error has no partial transfer effect.
            for _ in 0..usize::from(tcd.citer) {
                let addr = u32::try_from(src).map_err(|_| DmaError::AddressOverflow {
                    address: tcd.saddr,
                    bytes,
                })?;
                buffers.guest_range(addr, nbytes)?;
                src += i64::from(tcd.soff);
            }
            src = i64::from(tcd.saddr);
            for minor in 0..usize::from(tcd.citer) {
                let offset = buffers.guest_range(src as u32, nbytes)?;
                let card_offset = minor * nbytes;
                buffers.card[card_offset..card_offset + nbytes]
                    .copy_from_slice(&buffers.guest[offset..offset + nbytes]);
                src += i64::from(tcd.soff);
            }
            // `_dma_in` masks the final source pointer after SLAST to u32;
            // unlike CMD18's DADDR writeback, that wrap is intentional.
            after.saddr = (src + i64::from(tcd.slast)) as u32;
        }
    }
    after.citer = tcd.biter;
    after.csr |= CSR_DONE;
    Ok(DmaEffect {
        direction,
        bytes,
        tcd: after,
        completion: DmaCompletion {
            done: true,
            major_interrupt: tcd.csr & CSR_INT_MAJOR != 0,
            disable_request: tcd.csr & CSR_D_REQ != 0,
        },
    })
}

/// The card-side contract exercised by early commands. Storage/eDMA needs
/// a separate, fallible bounded-transfer contract when it is implemented;
/// do not silently discard a card's transfer-size errors here.
pub trait CardPort {
    fn command(&mut self, idx: u8, arg: u32) -> [u32; 4];
    fn read_word(&mut self, idx: u8, pattern: u32) -> u32;
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

    /// Give a machine owner the same attached card for its fallible eDMA
    /// payload transfer; command/identity behavior remains in this core.
    pub fn card_mut(&mut self) -> &mut P {
        &mut self.card
    }

    /// Shared access to the attached card (snapshot save).
    pub fn card_ref(&self) -> &P {
        &self.card
    }

    /// Restore the DATPORT bus-test word retained by Python `Esdhc` v1.
    /// Register pages are loaded separately; this is host-only state.
    pub fn restore_pattern(&mut self, pattern: u32) {
        self.pattern = pattern;
    }

    #[inline]
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

impl<P> Esdhc<P> {
    /// Register file, DATPORT pattern and queued host writes; the card is
    /// saved by its owner.
    pub fn snap_save(&self, w: &mut crate::snap::Writer) {
        self.regs.snap_save(w);
        w.u32(self.pattern);
        w.u64(self.host_writes.len() as u64);
        for (addr, bytes) in &self.host_writes {
            w.u32(*addr);
            w.raw(bytes);
        }
    }
    pub fn snap_load(&mut self, r: &mut crate::snap::Reader) -> crate::snap::Result<()> {
        self.regs.snap_load(r)?;
        self.pattern = r.u32()?;
        let n = r.len(1 << 20)?;
        self.host_writes.clear();
        for _ in 0..n {
            let addr = r.u32()?;
            let bytes: [u8; 4] = r.raw(4)?.try_into().unwrap();
            self.host_writes.push_back((addr, bytes));
        }
        Ok(())
    }
}
