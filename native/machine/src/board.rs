//! Firmware-agnostic sparse RAM board and synchronous eMMC DMA59 seam.
use std::collections::BTreeMap;

use coldfire::{Bus, BusError};
use emmc_card::{
    Card, CardError,
    dma::{StorageDmaError, service_dma59},
};
use periph::{
    DmaLink,
    dtim::{BASES as DTIM_BASES, DtimBank},
    edma,
    esdhc::{self, DmaCompletion, Esdhc, RegisterPolicy},
    intc::BASES as INTC_BASES,
    pit::BASES as PIT_BASES,
    regfile::SLOT_SIZE,
};

use crate::{host_state::HostState, time::Time};

const PAGE_SIZE: usize = 1024 * 1024;
const PAGE_SHIFT: u32 = 20;
const PAGE_COUNT: usize = 4096;
const PAGE_MASK: u32 = PAGE_SIZE as u32 - 1;
type RamPage = Box<[u8]>;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SemaphoreAddresses {
    pub dma_sem: Option<u32>,
    pub data_sem: Option<u32>,
    pub cmd_sem: Option<u32>,
    pub status: Option<u32>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompletionPolicy {
    Oracle,
    Device,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompletionEvent {
    Dma59 {
        policy: CompletionPolicy,
        completion: DmaCompletion,
    },
    Data {
        policy: CompletionPolicy,
        command: u8,
    },
    Command {
        policy: CompletionPolicy,
        command: u8,
    },
}
/// The detailed cause of a failed Board guest write.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BoardWriteError {
    Bus(BusError),
    Dma(StorageDmaError),
    CompletionAddress { address: u32 },
}

/// One successful access made through the public ColdFire [`Bus`] interface.
/// Hosts delimit a guest instruction by clearing these before `Machine::step`
/// and taking them afterwards; setup/import helpers do not use this recorder.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GuestAccess {
    pub address: u32,
    pub size: u8,
    pub value: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GuestAccessKind {
    Read,
    Write,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GuestBusAccess {
    pub kind: GuestAccessKind,
    pub access: GuestAccess,
}

/// Caller-mapped 1 MiB RAM pages.  The tagged 4096-entry table is indexed by
/// address bits 31:20, so ordinary 1/2/4-byte RAM accesses take one table
/// lookup.  MMIO is always dispatched before this table.
pub struct Board {
    pages: Box<[Option<RamPage>; PAGE_COUNT]>,
    pub dma: DmaLink,
    pub esdhc: Esdhc<Card>,
    semaphores: SemaphoreAddresses,
    policy: CompletionPolicy,
    armed_dma59: bool,
    scratch_guest: RamPage,
    scratch_card: RamPage,
    events: Vec<CompletionEvent>,
    dma_written: Vec<(u32, usize)>,
    last_error: Option<BoardWriteError>,
    /// Optional PIT/INTC facade. Its owned MMIO is never backed by sparse RAM.
    time: Option<Time>,
    /// Oracle read hooks: each value overlays four big-endian bytes before a
    /// guest read, without changing the backing page or later guest writes.
    forced_mmio: BTreeMap<u32, u32>,
    /// Opt-in only: ordinary long-running execution must not retain every Bus access.
    capture_guest_accesses: bool,
    guest_reads: Vec<GuestAccess>,
    guest_writes: Vec<GuestAccess>,
    guest_ordered: Vec<GuestBusAccess>,
    /// Python's unmapped-memory callback zero-maps pages on first touch.
    /// Opt-in and SDRAM-only here; never treat missing MMIO as RAM.
    oracle_sdram_faults: bool,
    oracle_fault_pages: u8,
}
impl Board {
    pub fn new(card: Card, semaphores: SemaphoreAddresses, policy: CompletionPolicy) -> Self {
        let register_policy = match policy {
            CompletionPolicy::Oracle => RegisterPolicy::Oracle,
            CompletionPolicy::Device => RegisterPolicy::Device,
        };
        Self {
            pages: Box::new(std::array::from_fn(|_| None)),
            dma: DmaLink::default(),
            esdhc: Esdhc::with_policy(card, register_policy),
            semaphores,
            policy,
            armed_dma59: false,
            scratch_guest: vec![0; PAGE_SIZE].into_boxed_slice(),
            scratch_card: vec![0; PAGE_SIZE].into_boxed_slice(),
            events: vec![],
            dma_written: vec![],
            last_error: None,
            time: None,
            forced_mmio: BTreeMap::new(),
            capture_guest_accesses: false,
            guest_reads: vec![],
            guest_writes: vec![],
            guest_ordered: vec![],
            oracle_sdram_faults: false,
            oracle_fault_pages: 0,
        }
    }

    /// Route PIT and INTC MMIO through the supplied timer facade.
    pub fn attach_time(&mut self, time: Time) {
        self.time = Some(time);
    }

    pub fn time_mut(&mut self) -> Option<&mut Time> {
        self.time.as_mut()
    }

    /// Temporarily remove the timer facade for an atomic CPU interrupt offer.
    pub fn take_time(&mut self) -> Option<Time> {
        self.time.take()
    }

    pub fn restore_time(&mut self, time: Time) {
        debug_assert!(self.time.is_none());
        self.time = Some(time);
    }

    /// Restore validated host-only storage state after checkpoint pages load.
    /// Guest register files and eDMA TCDs come from the mapped MSTATE pages.
    pub(crate) fn restore_storage_state(&mut self, state: &HostState) -> Result<(), CardError> {
        self.esdhc.card_mut().restore_checkpoint(&state.card)?;
        self.esdhc.restore_pattern(state.pattern);
        self.armed_dma59 = state.armed_dma59;
        Ok(())
    }

    fn owned_mmio(addr: u32) -> bool {
        Time::owns(addr) || DmaLink::owns(addr) || Esdhc::<Card>::owns(addr)
    }

    /// Validate and install Oracle forced read words. A forced word may
    /// overlay one owned register (the Python hook does this before the
    /// peripheral read), but may not cross into or out of owned MMIO.
    pub fn install_forced_mmio(&mut self, words: BTreeMap<u32, u32>) -> Result<(), ()> {
        if words.keys().any(|&base| {
            let Some(end) = base.checked_add(3) else {
                return true;
            };
            let owned = (base..=end).map(Self::owned_mmio).collect::<Vec<_>>();
            owned.iter().any(|owned| *owned) && owned.iter().any(|owned| !owned)
        }) {
            return Err(());
        }
        self.forced_mmio = words;
        Ok(())
    }

    /// Forced bytes available at the start of a guest read.  If the read
    /// extends past the four-byte hook write, its suffix must come from the
    /// normal bus (as Unicorn observes after the hook has written the word).
    fn forced_prefix(&self, addr: u32, size: u8) -> Option<(u32, u8)> {
        self.forced_mmio.iter().find_map(|(&base, &word)| {
            let word_end = base.checked_add(4)?;
            (addr >= base && addr < word_end).then(|| {
                let count = size.min((word_end - addr) as u8);
                let bytes = word.to_be_bytes();
                let value = bytes
                    [(addr - base) as usize..(addr - base + u32::from(count)) as usize]
                    .iter()
                    .fold(0, |value, byte| (value << 8) | u32::from(*byte));
                (value, count)
            })
        })
    }

    /// Map a zero-filled 1 MiB RAM page.  This is deliberately separate from
    /// importing bytes so snapshot mapped-but-zero pages remain observable.
    pub fn map_zeroed_ram_page(&mut self, addr: u32) -> Result<(), BusError> {
        if addr & PAGE_MASK != 0 {
            return Err(Self::bus_error(addr, true));
        }
        self.pages[(addr >> PAGE_SHIFT) as usize]
            .get_or_insert_with(|| vec![0; PAGE_SIZE].into_boxed_slice());
        Ok(())
    }

    /// Compatibility spelling for callers that map ordinary guest RAM.
    pub fn map_ram_page(&mut self, addr: u32) -> Result<(), BusError> {
        self.map_zeroed_ram_page(addr)
    }

    /// Replace one already mapped snapshot RAM page.  The fixed one-page
    /// bound prevents a malformed state blob from allocating or copying an
    /// unbounded guest range.
    pub fn import_ram_page(&mut self, addr: u32, data: &[u8]) -> Result<(), BusError> {
        if addr & PAGE_MASK != 0 || data.len() != PAGE_SIZE {
            return Err(Self::bus_error(addr, true));
        }
        self.map_zeroed_ram_page(addr)?;
        self.pages[Self::page_index(addr)]
            .as_mut()
            .expect("page just mapped")
            .copy_from_slice(data);
        // MSTATE pages are 1 MiB while the peripheral facade consumes 16 KiB
        // slots. Restore a slot at its address within the page, not only when
        // the MSTATE page base happens to equal the peripheral base.
        if let Some(time) = self.time.as_mut() {
            for slot in PIT_BASES
                .iter()
                .chain(DTIM_BASES.iter())
                .chain(INTC_BASES.iter())
            {
                if *slot & !PAGE_MASK == addr {
                    let offset = (*slot & PAGE_MASK) as usize;
                    let _ = time.load_page(*slot, &data[offset..offset + SLOT_SIZE]);
                }
            }
        }
        Ok(())
    }

    /// Apply a DTIM-generated REF byte to the guest backing page without
    /// routing it through the guest's W1C MMIO write hook. The timer bank has
    /// already updated its own register before returning this host write.
    pub fn apply_timer_host_write(&mut self, addr: u32, byte: u8) -> Result<(), BusError> {
        if !DtimBank::owns(addr) {
            return Err(Self::bus_error(addr, true));
        }
        self.map_zeroed_ram_page(addr & !PAGE_MASK)?;
        if !self.ram_write(addr, 1, u32::from(byte)) {
            return Err(Self::bus_error(addr, true));
        }
        Ok(())
    }

    /// True only when every byte in `addr..addr+len` is mapped sparse RAM,
    /// never an owned peripheral. Used to preflight ColdFire exception frames
    /// before the CPU mutates SR or either stack pointer.
    pub fn can_write_ram_range(&self, addr: u32, len: usize) -> bool {
        let Ok(len) = u32::try_from(len) else {
            return false;
        };
        let Some(end) = addr.checked_add(len) else {
            return false;
        };
        (addr..end).all(|byte| !Self::owned_mmio(byte) && self.page(byte).is_some())
    }

    /// Read one mapped RAM page without going through MMIO dispatch.
    pub fn read_ram_page(&self, addr: u32, out: &mut [u8]) -> Result<(), BusError> {
        if addr & PAGE_MASK != 0 || out.len() != PAGE_SIZE {
            return Err(Self::bus_error(addr, false));
        }
        let page = self.page(addr).ok_or(Self::bus_error(addr, false))?;
        out.copy_from_slice(page);
        Ok(())
    }
    pub fn completion_events(&self) -> &[CompletionEvent] {
        &self.events
    }
    pub fn take_completion_events(&mut self) -> Vec<CompletionEvent> {
        std::mem::take(&mut self.events)
    }
    pub fn dma59_armed(&self) -> bool {
        self.armed_dma59
    }
    pub fn completion_policy(&self) -> CompletionPolicy {
        self.policy
    }
    /// External DMA invalidates these CPU decode pages before the next
    /// instruction fetch; only successful card-to-guest transfers appear.
    pub fn take_dma_written_ranges(&mut self) -> Vec<(u32, usize)> {
        std::mem::take(&mut self.dma_written)
    }
    pub fn last_write_error(&self) -> Option<&BoardWriteError> {
        self.last_error.as_ref()
    }
    /// Enable bounded, caller-delimited Bus effect capture for a differential gate.
    /// Disabling it discards any unconsumed entries.
    pub fn set_guest_access_capture(&mut self, enabled: bool) {
        self.clear_guest_accesses();
        self.capture_guest_accesses = enabled;
    }
    /// Discard accesses collected outside the next guest instruction.
    pub fn clear_guest_accesses(&mut self) {
        self.guest_reads.clear();
        self.guest_writes.clear();
        self.guest_ordered.clear();
    }
    fn record_guest_access(&mut self, kind: GuestAccessKind, access: GuestAccess) {
        self.guest_ordered.push(GuestBusAccess { kind, access });
        match kind {
            GuestAccessKind::Read => self.guest_reads.push(access),
            GuestAccessKind::Write => self.guest_writes.push(access),
        }
    }
    /// Return interleaved, instruction-local read/write order for parity gates.
    pub fn take_guest_accesses(&mut self) -> Vec<GuestBusAccess> {
        std::mem::take(&mut self.guest_ordered)
    }
    /// Reproduce the Oracle's first-touch zero-fill for absent SDRAM pages,
    /// bounded to 16 new pages. Device semantics and peripheral holes differ.
    pub fn enable_oracle_sdram_faults(&mut self) {
        assert_eq!(self.policy, CompletionPolicy::Oracle);
        self.oracle_sdram_faults = true;
    }
    /// Return ordered successful Bus reads since the last clear/take.
    pub fn take_guest_reads(&mut self) -> Vec<GuestAccess> {
        std::mem::take(&mut self.guest_reads)
    }
    /// Return ordered successful Bus writes since the last clear/take.
    pub fn take_guest_writes(&mut self) -> Vec<GuestAccess> {
        std::mem::take(&mut self.guest_writes)
    }
    /// Fallible Board entry point used by hosts that need the DMA/configuration
    /// cause rather than the ColdFire trait's address-only `BusError`.
    pub fn write_guest(&mut self, addr: u32, size: u8, value: u32) -> Result<(), BoardWriteError> {
        self.write_inner(addr, size, value).inspect_err(|&error| {
            self.last_error = Some(error);
        })
    }
    fn bus_error(addr: u32, write: bool) -> BusError {
        BusError { addr, write }
    }
    #[inline]
    fn page_index(addr: u32) -> usize {
        (addr >> PAGE_SHIFT) as usize
    }
    #[inline]
    fn page_offset(addr: u32) -> usize {
        (addr & PAGE_MASK) as usize
    }
    #[inline]
    fn page(&self, addr: u32) -> Option<&RamPage> {
        self.pages[Self::page_index(addr)].as_ref()
    }
    #[inline]
    fn page_mut(&mut self, addr: u32) -> Option<&mut RamPage> {
        self.pages[Self::page_index(addr)].as_mut()
    }
    fn ensure_oracle_sdram(&mut self, addr: u32, size: u8) -> bool {
        if !self.oracle_sdram_faults {
            return true;
        }
        let Some(end) = addr.checked_add(u32::from(size)) else {
            return false;
        };
        if addr < 0x4000_0000 || end > 0x4800_0000 {
            return true;
        }
        let first = addr & !PAGE_MASK;
        let last = (end - 1) & !PAGE_MASK;
        for base in [first, last] {
            if self.page(base).is_none() {
                if self.oracle_fault_pages >= 16 || self.map_ram_page(base).is_err() {
                    return false;
                }
                self.oracle_fault_pages += 1;
            }
        }
        true
    }
    fn ram_read(&self, addr: u32, size: u8) -> Option<u32> {
        let end = addr.checked_add(size as u32)?;
        if end <= ((addr & !PAGE_MASK).checked_add(PAGE_SIZE as u32)?) {
            let p = self.page(addr)?;
            let o = Self::page_offset(addr);
            return Some(match size {
                1 => p[o] as u32,
                2 => u16::from_be_bytes([p[o], p[o + 1]]) as u32,
                4 => u32::from_be_bytes([p[o], p[o + 1], p[o + 2], p[o + 3]]),
                _ => return None,
            });
        }
        let mut value = 0;
        for byte in addr..end {
            value = (value << 8) | self.page(byte)?[Self::page_offset(byte)] as u32;
        }
        Some(value)
    }
    fn ram_write(&mut self, addr: u32, size: u8, value: u32) -> bool {
        let Some(end) = addr.checked_add(size as u32) else {
            return false;
        };
        if end <= (addr & !PAGE_MASK).saturating_add(PAGE_SIZE as u32) {
            let Some(p) = self.page_mut(addr) else {
                return false;
            };
            let o = Self::page_offset(addr);
            match size {
                1 => p[o] = value as u8,
                2 => p[o..o + 2].copy_from_slice(&(value as u16).to_be_bytes()),
                4 => p[o..o + 4].copy_from_slice(&value.to_be_bytes()),
                _ => return false,
            };
            return true;
        }
        if (addr..end).any(|a| self.page(a).is_none()) {
            return false;
        }
        for (i, a) in (addr..end).enumerate() {
            self.page_mut(a).unwrap()[Self::page_offset(a)] =
                (value >> (8 * (size as usize - 1 - i))) as u8;
        }
        true
    }
    fn drain_esdhc_host_writes(&mut self) {
        while self.esdhc.take_host_write().is_some() {}
    }
    fn completion_addresses(&self) -> [Option<u32>; 4] {
        [
            self.semaphores.dma_sem,
            self.semaphores.data_sem,
            self.semaphores.status,
            self.semaphores.cmd_sem,
        ]
    }
    /// Oracle copies `Machine.ensure` for configured completion words.
    fn preflight_completion_addresses(&mut self) -> Result<(), BoardWriteError> {
        for address in self.completion_addresses().into_iter().flatten() {
            match self.policy {
                CompletionPolicy::Oracle => {
                    let end = address
                        .checked_add(3)
                        .ok_or(BoardWriteError::CompletionAddress { address })?;
                    let first = address & !PAGE_MASK;
                    self.map_ram_page(first).map_err(BoardWriteError::Bus)?;
                    if end & !PAGE_MASK != first {
                        self.map_ram_page(end & !PAGE_MASK)
                            .map_err(BoardWriteError::Bus)?;
                    }
                }
                CompletionPolicy::Device => {
                    if self.ram_read(address, 4).is_none() {
                        return Err(BoardWriteError::CompletionAddress { address });
                    }
                }
            }
        }
        Ok(())
    }
    fn post(&mut self, address: Option<u32>) {
        if let Some(a) = address
            && let Some(value) = self.ram_read(a, 4)
            && (value as i32) <= 0
        {
            assert!(self.ram_write(a, 4, 1));
        }
    }
    fn complete(&mut self, command: u8, data: bool, dma: Option<DmaCompletion>) {
        if let Some(completion) = dma {
            if self.policy == CompletionPolicy::Oracle {
                self.post(self.semaphores.dma_sem);
            }
            self.events.push(CompletionEvent::Dma59 {
                policy: self.policy,
                completion,
            });
        }
        if data {
            if self.policy == CompletionPolicy::Oracle {
                self.post(self.semaphores.data_sem);
            }
            self.events.push(CompletionEvent::Data {
                policy: self.policy,
                command,
            });
        }
        if self.policy == CompletionPolicy::Oracle {
            if let Some(address) = self.semaphores.status {
                assert!(self.ram_write(address, 4, 0));
            }
            self.post(self.semaphores.cmd_sem);
        }
        self.events.push(CompletionEvent::Command {
            policy: self.policy,
            command,
        });
    }
    fn dma_window(&mut self, command: u8) -> Result<(u32, usize), StorageDmaError> {
        let t = edma::TcdView::new(&mut self.dma.edma_regs, esdhc::DMA_CHANNEL).snapshot();
        let bytes = u64::from(t.citer) * u64::from(t.nbytes);
        if bytes > PAGE_SIZE as u64 {
            return Err(StorageDmaError::Dma(esdhc::DmaError::TransferTooLarge {
                bytes,
            }));
        }
        let n = bytes as usize;
        if matches!(command, 8 | 18) {
            t.daddr.checked_add(n as u32).ok_or(StorageDmaError::Dma(
                esdhc::DmaError::AddressOverflow {
                    address: t.daddr,
                    bytes: n,
                },
            ))?;
            return Ok((t.daddr, n));
        }
        let (mut lo, mut hi, mut current) = (t.saddr, t.saddr, i64::from(t.saddr));
        for _ in 0..t.citer {
            let start = u32::try_from(current).map_err(|_| {
                StorageDmaError::Dma(esdhc::DmaError::AddressOverflow {
                    address: t.saddr,
                    bytes: n,
                })
            })?;
            let end = start.checked_add(t.nbytes).ok_or(StorageDmaError::Dma(
                esdhc::DmaError::AddressOverflow {
                    address: start,
                    bytes: n,
                },
            ))?;
            lo = lo.min(start);
            hi = hi.max(end);
            current += i64::from(t.soff);
        }
        let span = hi.wrapping_sub(lo) as usize;
        if span > PAGE_SIZE {
            return Err(StorageDmaError::ScratchTooSmall {
                needed: span,
                available: PAGE_SIZE,
            });
        }
        Ok((lo, span))
    }
    fn service_dma59(&mut self, command: u8) -> Result<DmaCompletion, StorageDmaError> {
        let argument = self.esdhc.read(esdhc::BASE + esdhc::CMDARG, 4).unwrap_or(0);
        self.drain_esdhc_host_writes();
        let (base, len) = self.dma_window(command)?;
        if command == 25 {
            for i in 0..len {
                let address = base.checked_add(i as u32).ok_or(StorageDmaError::Dma(
                    esdhc::DmaError::AddressOverflow {
                        address: base,
                        bytes: len,
                    },
                ))?;
                self.scratch_guest[i] = self.page(address).ok_or(StorageDmaError::Dma(
                    esdhc::DmaError::GuestOutOfRange {
                        address,
                        bytes: len,
                    },
                ))?[Self::page_offset(address)];
            }
        }
        let effect = service_dma59(
            self.esdhc.card_mut(),
            &mut self.dma.edma_regs,
            command,
            argument,
            base,
            &mut self.scratch_guest[..len],
            &mut self.scratch_card[..],
        )?;
        if effect.direction == esdhc::DmaDirection::CardToGuest && effect.completion.done {
            for i in 0..len {
                let value = self.scratch_guest[i];
                let address = base + i as u32;
                self.page_mut(address).unwrap()[Self::page_offset(address)] = value;
            }
            self.dma_written.push((base, effect.bytes));
        }
        Ok(effect.completion)
    }
    fn update_dma59_request(&mut self, addr: u32, size: u8, value: u32) {
        if size != 1 {
            return;
        }
        let byte = value as u8;
        match self.policy {
            // `emu.edma` only recognizes the channel form of SERQ. Its CERQ
            // writes and SERQ all-channel form are inert for this seam.
            CompletionPolicy::Oracle => {
                if addr == edma::SERQ
                    && byte & 0x40 == 0
                    && (byte & 0x3f) as usize == esdhc::DMA_CHANNEL
                {
                    self.armed_dma59 = true;
                }
            }
            // MCF5441xRM 19.4.5-19.4.6 (p.374-375): bit 7 is NOP; bit 6
            // selects all channels, otherwise the low six bits select one.
            CompletionPolicy::Device => {
                if byte & 0x80 != 0 {
                    return;
                }
                let selects_dma59 =
                    byte & 0x40 != 0 || (byte & 0x3f) as usize == esdhc::DMA_CHANNEL;
                if selects_dma59 {
                    match addr {
                        edma::SERQ => self.armed_dma59 = true,
                        edma::CERQ => self.armed_dma59 = false,
                        _ => {}
                    }
                }
            }
        }
    }
    fn read_inner(&mut self, addr: u32, size: u8) -> Result<u32, BusError> {
        if let Some((mut value, prefix)) = self.forced_prefix(addr, size) {
            for offset in prefix..size {
                value = (value << 8) | self.read_inner_unforced(addr + u32::from(offset), 1)?;
            }
            return Ok(value);
        }
        self.read_inner_unforced(addr, size)
    }

    fn read_inner_unforced(&mut self, addr: u32, size: u8) -> Result<u32, BusError> {
        if Time::owns(addr) {
            return self
                .time
                .as_ref()
                .and_then(|time| time.read(addr, size))
                .ok_or(Self::bus_error(addr, false));
        }
        if let Some(value) = self.dma.read(addr, size) {
            return Ok(value);
        }
        if let Some(value) = self.esdhc.read(addr, size) {
            self.drain_esdhc_host_writes();
            return Ok(value);
        }
        if !self.ensure_oracle_sdram(addr, size) {
            return Err(Self::bus_error(addr, false));
        }
        self.ram_read(addr, size)
            .ok_or(Self::bus_error(addr, false))
    }
    fn write_inner(&mut self, addr: u32, size: u8, value: u32) -> Result<(), BoardWriteError> {
        self.last_error = None;
        if Time::owns(addr) {
            if let Some(time) = self.time.as_mut()
                && time.write(addr, size, value)
            {
                return Ok(());
            }
            return Err(BoardWriteError::Bus(Self::bus_error(addr, true)));
        }
        if DmaLink::owns(addr) {
            let _ = self.dma.write(addr, size, value);
            self.update_dma59_request(addr, size, value);
            return Ok(());
        }
        if !Esdhc::<Card>::owns(addr) {
            if !self.ensure_oracle_sdram(addr, size) {
                return Err(BoardWriteError::Bus(Self::bus_error(addr, true)));
            }
            return self
                .ram_write(addr, size, value)
                .then_some(())
                .ok_or(BoardWriteError::Bus(Self::bus_error(addr, true)));
        }
        let xfer = (addr == esdhc::BASE + esdhc::XFERTYP && size == 4).then_some(value);
        let command = xfer.map(|v| ((v >> 24) & 0x3f) as u8);
        let data = xfer.is_some_and(|v| v & (1 << 21) != 0);
        // Preflight before the controller command so a configured Device
        // completion word cannot leave a partial eSDHC/DMA completion behind.
        if xfer.is_some() && self.policy == CompletionPolicy::Oracle {
            self.preflight_completion_addresses()?;
        }
        let read = xfer.is_some_and(|v| v & (1 << 4) != 0);
        let direction_ok = matches!((command, read), (Some(8 | 18), true) | (Some(25), false));
        if data && direction_ok && self.armed_dma59 {
            let (base, len) = self
                .dma_window(command.unwrap())
                .map_err(BoardWriteError::Dma)?;
            if len != 0 {
                let end = base.checked_add(len as u32).ok_or(BoardWriteError::Dma(
                    StorageDmaError::Dma(esdhc::DmaError::AddressOverflow {
                        address: base,
                        bytes: len,
                    }),
                ))?;
                let mut page = base & !PAGE_MASK;
                loop {
                    if self.policy == CompletionPolicy::Oracle {
                        self.map_ram_page(page).map_err(BoardWriteError::Bus)?;
                    } else if self.page(page).is_none() {
                        return Err(BoardWriteError::Dma(StorageDmaError::Dma(
                            esdhc::DmaError::GuestOutOfRange {
                                address: page,
                                bytes: len,
                            },
                        )));
                    }
                    let next = page.checked_add(PAGE_SIZE as u32);
                    if next.is_none_or(|p| p >= end) {
                        break;
                    }
                    page = next.unwrap();
                }
            }
        }
        let _ = self.esdhc.write(addr, size, value);
        self.drain_esdhc_host_writes();
        if let Some(command) = command {
            let dma_completion = if data && direction_ok && self.armed_dma59 {
                match self.service_dma59(command) {
                    Ok(completion) => {
                        if self.policy == CompletionPolicy::Oracle
                            || (completion.done && completion.disable_request)
                        {
                            self.armed_dma59 = false;
                        }
                        Some(completion)
                    }
                    Err(error) => return Err(BoardWriteError::Dma(error)),
                }
            } else {
                None
            };
            self.complete(command, data, dma_completion);
        }
        Ok(())
    }
}
impl Bus for Board {
    fn read8(&mut self, addr: u32) -> Result<u8, BusError> {
        let result = self.read_inner(addr, 1).map(|value| value as u8);
        if self.capture_guest_accesses
            && let Ok(value) = result
        {
            self.record_guest_access(
                GuestAccessKind::Read,
                GuestAccess {
                    address: addr,
                    size: 1,
                    value: u32::from(value),
                },
            );
        }
        result
    }
    fn read16(&mut self, addr: u32) -> Result<u16, BusError> {
        let result = self.read_inner(addr, 2).map(|value| value as u16);
        if self.capture_guest_accesses
            && let Ok(value) = result
        {
            self.record_guest_access(
                GuestAccessKind::Read,
                GuestAccess {
                    address: addr,
                    size: 2,
                    value: u32::from(value),
                },
            );
        }
        result
    }
    /// Fetches share normal board dispatch but are not guest data reads.
    fn fetch16(&mut self, addr: u32) -> Result<u16, BusError> {
        self.read_inner(addr, 2).map(|value| value as u16)
    }
    fn read32(&mut self, addr: u32) -> Result<u32, BusError> {
        let result = self.read_inner(addr, 4);
        if self.capture_guest_accesses
            && let Ok(value) = result
        {
            self.record_guest_access(
                GuestAccessKind::Read,
                GuestAccess {
                    address: addr,
                    size: 4,
                    value,
                },
            );
        }
        result
    }
    fn write8(&mut self, addr: u32, value: u8) -> Result<(), BusError> {
        let result = self.write_inner(addr, 1, value as u32).map_err(|e| {
            self.last_error = Some(e);
            Self::bus_error(addr, true)
        });
        if self.capture_guest_accesses && result.is_ok() {
            self.record_guest_access(
                GuestAccessKind::Write,
                GuestAccess {
                    address: addr,
                    size: 1,
                    value: u32::from(value),
                },
            );
        }
        result
    }
    fn write16(&mut self, addr: u32, value: u16) -> Result<(), BusError> {
        let result = self.write_inner(addr, 2, value as u32).map_err(|e| {
            self.last_error = Some(e);
            Self::bus_error(addr, true)
        });
        if self.capture_guest_accesses && result.is_ok() {
            self.record_guest_access(
                GuestAccessKind::Write,
                GuestAccess {
                    address: addr,
                    size: 2,
                    value: u32::from(value),
                },
            );
        }
        result
    }
    fn write32(&mut self, addr: u32, value: u32) -> Result<(), BusError> {
        let result = self.write_inner(addr, 4, value).map_err(|e| {
            self.last_error = Some(e);
            Self::bus_error(addr, true)
        });
        if self.capture_guest_accesses && result.is_ok() {
            self.record_guest_access(
                GuestAccessKind::Write,
                GuestAccess {
                    address: addr,
                    size: 4,
                    value,
                },
            );
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coldfire::Bus;
    use emmc_card::{DEFAULT_CAPACITY_BLOCKS, SMALL_CAPACITY_BLOCKS};
    const RAM: u32 = 0x4000_0000;
    const NEXT: u32 = RAM + PAGE_SIZE as u32;
    const DS: u32 = RAM + 0x100;
    const AS: u32 = RAM + 0x104;
    const CS: u32 = RAM + 0x108;
    const ST: u32 = RAM + 0x10c;

    #[test]
    fn dtim_host_ref_byte_updates_backing_without_guest_mmio_dispatch() {
        let mut board = Board::new(
            Card::default(),
            SemaphoreAddresses::default(),
            CompletionPolicy::Oracle,
        );
        let ref_addr = DTIM_BASES[3] + 3;
        assert_eq!(board.ram_read(ref_addr, 1), None);
        board.apply_timer_host_write(ref_addr, 0x02).unwrap();
        assert_eq!(board.ram_read(ref_addr, 1), Some(0x02));
        assert!(board.apply_timer_host_write(0x4000_0000, 0x02).is_err());
    }

    #[test]
    fn guest_access_capture_retains_mixed_read_write_order() {
        let mut board = Board::new(
            Card::default(),
            SemaphoreAddresses::default(),
            CompletionPolicy::Oracle,
        );
        board.map_ram_page(RAM).unwrap();
        board.set_guest_access_capture(true);
        board.write8(RAM, 0x12).unwrap();
        assert_eq!(board.read8(RAM).unwrap(), 0x12);
        board.write8(RAM + 1, 0x34).unwrap();
        let captured = board.take_guest_accesses();
        assert_eq!(
            captured
                .iter()
                .map(|access| access.kind)
                .collect::<Vec<_>>(),
            [
                GuestAccessKind::Write,
                GuestAccessKind::Read,
                GuestAccessKind::Write
            ]
        );
        assert!(board.take_guest_accesses().is_empty());
    }

    #[test]
    fn oracle_sdram_faults_are_opt_in_and_restricted_to_sdram() {
        let mut board = Board::new(
            Card::default(),
            SemaphoreAddresses::default(),
            CompletionPolicy::Oracle,
        );
        let missing = 0x4660_0000;
        assert!(board.write32(missing, 0x1234_5678).is_err());
        board.enable_oracle_sdram_faults();
        assert_eq!(board.read32(missing).unwrap(), 0);
        board.write32(missing, 0x1234_5678).unwrap();
        assert_eq!(board.read32(missing).unwrap(), 0x1234_5678);
        assert!(board.read32(0x3000_0000).is_err());
        assert!(board.write32(0x8000_0000, 0).is_err());
        for index in 0..15 {
            assert_eq!(
                board.read8(0x4400_0000 + index * PAGE_SIZE as u32).unwrap(),
                0
            );
        }
        assert!(board.read8(0x44f0_0000).is_err());
    }

    fn board(policy: CompletionPolicy) -> Board {
        let mut b = Board::new(
            Card::new(DEFAULT_CAPACITY_BLOCKS).unwrap(),
            SemaphoreAddresses {
                dma_sem: Some(DS),
                data_sem: Some(AS),
                cmd_sem: Some(CS),
                status: Some(ST),
            },
            policy,
        );
        b.map_ram_page(RAM).unwrap();
        b.write32(ST, u32::MAX).unwrap();
        b
    }
    fn tcd(b: &mut Board, s: u32, d: u32, n: u32, c: u16) {
        let x = edma::TCD_BASE + 59 * 0x20;
        b.write32(x + edma::SADDR as u32, s).unwrap();
        b.write32(x + edma::NBYTES as u32, n).unwrap();
        b.write32(x + edma::DADDR as u32, d).unwrap();
        b.write16(x + edma::CITER as u32, c).unwrap();
        b.write16(x + edma::BITER as u32, c).unwrap();
        b.write16(x + edma::CSR as u32, 0).unwrap()
    }
    fn command(b: &mut Board, c: u8, read: bool) -> Result<(), BusError> {
        b.write32(
            esdhc::BASE + esdhc::XFERTYP,
            (c as u32) << 24 | 1 << 21 | if read { 1 << 4 } else { 0 },
        )
    }
    fn issue(b: &mut Board, c: u8, read: bool) -> Result<(), BusError> {
        b.write8(edma::SERQ, 59)?;
        command(b, c, read)
    }
    #[test]
    fn fetch_is_not_a_guest_data_read() {
        let mut b = board(CompletionPolicy::Oracle);
        let address = edma::EDMA_BASE;
        b.set_guest_access_capture(true);
        let fetched = b.fetch16(address).unwrap();
        assert!(b.take_guest_reads().is_empty());
        assert_eq!(b.read16(address).unwrap(), fetched);
        assert_eq!(
            b.take_guest_reads(),
            vec![GuestAccess {
                address,
                size: 2,
                value: u32::from(fetched),
            }]
        );
    }
    #[test]
    fn guest_access_capture_is_opt_in_and_disabling_discards_pending_entries() {
        let mut b = board(CompletionPolicy::Oracle);
        b.write16(RAM, 0x1234).unwrap();
        assert_eq!(b.read16(RAM).unwrap(), 0x1234);
        assert!(b.take_guest_reads().is_empty());
        assert!(b.take_guest_writes().is_empty());

        b.set_guest_access_capture(true);
        b.write16(RAM, 0xabcd).unwrap();
        assert_eq!(b.read16(RAM).unwrap(), 0xabcd);
        b.set_guest_access_capture(false);
        assert!(b.take_guest_reads().is_empty());
        assert!(b.take_guest_writes().is_empty());
    }
    #[test]
    fn cmd8_payload_tcd_and_semaphores() {
        let mut b = board(CompletionPolicy::Oracle);
        let d = RAM + 0x200;
        tcd(&mut b, 0, d, 512, 1);
        issue(&mut b, 8, true).unwrap();
        let x = edma::TCD_BASE + 59 * 0x20;
        assert_eq!(b.read8(d + 0xaf).unwrap(), 1);
        assert_eq!(b.read32(x + edma::DADDR as u32).unwrap(), d + 512);
        assert_eq!(b.read16(x + edma::CITER as u32).unwrap(), 1);
        assert_eq!(b.read16(x + edma::CSR as u32).unwrap(), edma::CSR_DONE);
        assert_eq!(
            [
                b.read32(DS).unwrap(),
                b.read32(AS).unwrap(),
                b.read32(CS).unwrap(),
                b.read32(ST).unwrap()
            ],
            [1, 1, 1, 0]
        );
        assert_eq!(b.completion_events().len(), 3)
    }
    #[test]
    fn cmd25_and_wrong_direction() {
        let mut b = board(CompletionPolicy::Oracle);
        let s = RAM + 0x400;
        for (i, v) in [1, 2, 3, 4].into_iter().enumerate() {
            b.write8(s + i as u32, v).unwrap()
        }
        tcd(&mut b, s, 0, 4, 1);
        issue(&mut b, 25, false).unwrap();
        assert_eq!(
            b.esdhc.card_mut().data_for(18, 0, 4).unwrap().unwrap(),
            vec![1, 2, 3, 4]
        );
        let mut b = Board::new(
            Card::new(SMALL_CAPACITY_BLOCKS).unwrap(),
            SemaphoreAddresses::default(),
            CompletionPolicy::Oracle,
        );
        b.map_ram_page(RAM).unwrap();
        tcd(&mut b, 0, RAM + 0x200, 512, 1);
        issue(&mut b, 8, false).unwrap();
        assert_eq!(b.read8(RAM + 0x2af).unwrap(), 0);
        assert!(b.dma59_armed())
    }
    #[test]
    fn cross_page_nonzero_and_wrong_channel() {
        let mut b = board(CompletionPolicy::Oracle);
        b.map_ram_page(NEXT).unwrap();
        let d = NEXT - 0xaf;
        tcd(&mut b, 0, d, 512, 1);
        issue(&mut b, 8, true).unwrap();
        assert_eq!(b.read8(NEXT).unwrap(), 1);
        let mut b = board(CompletionPolicy::Oracle);
        let d = RAM + 0x200;
        tcd(&mut b, 0, d, 4, 1);
        b.write8(edma::SERQ, 58).unwrap();
        b.write32(esdhc::BASE + esdhc::XFERTYP, 8 << 24 | 1 << 21 | 1 << 4)
            .unwrap();
        assert_eq!(b.read32(d).unwrap(), 0);
        assert_eq!(b.read32(DS).unwrap(), 0)
    }
    #[test]
    fn failed_dma_is_fallible_atomic_and_has_no_events() {
        let mut b = board(CompletionPolicy::Oracle);
        let d = RAM + 0x200;
        tcd(&mut b, 0, d, PAGE_SIZE as u32 + 1, 1);
        assert!(issue(&mut b, 8, true).is_err());
        let x = edma::TCD_BASE + 59 * 0x20;
        assert!(matches!(
            b.last_write_error(),
            Some(BoardWriteError::Dma(_))
        ));
        assert_eq!(b.read32(d).unwrap(), 0);
        assert_eq!(b.read32(x + edma::DADDR as u32).unwrap(), d);
        assert_eq!(b.read16(x + edma::CSR as u32).unwrap(), 0);
        assert!(b.completion_events().is_empty());
        let mut b = board(CompletionPolicy::Oracle);
        tcd(&mut b, 0, NEXT, 4, 1);
        issue(&mut b, 8, true).unwrap();
        assert_eq!(b.read32(NEXT).unwrap(), 0);
        assert_eq!(
            b.read16(edma::TCD_BASE + 59 * 0x20 + edma::CSR as u32)
                .unwrap(),
            edma::CSR_DONE
        );
        let mut device = board(CompletionPolicy::Device);
        tcd(&mut device, 0, NEXT, 4, 1);
        assert!(issue(&mut device, 8, true).is_err());
        assert!(device.completion_events().is_empty());
    }
    #[test]
    fn device_queues_completions_without_oracle_sem_posts() {
        let mut b = Board::new(
            Card::default(),
            SemaphoreAddresses {
                dma_sem: Some(NEXT),
                data_sem: None,
                cmd_sem: None,
                status: None,
            },
            CompletionPolicy::Device,
        );
        b.write_guest(esdhc::BASE + esdhc::XFERTYP, 4, 8 << 24 | 1 << 21 | 1 << 4)
            .unwrap();
        assert_eq!(b.completion_events().len(), 2);
        assert!(b.read32(NEXT).is_err());
        let mut oracle = Board::new(
            Card::default(),
            SemaphoreAddresses {
                dma_sem: Some(NEXT),
                ..SemaphoreAddresses::default()
            },
            CompletionPolicy::Oracle,
        );
        oracle.map_ram_page(RAM).unwrap();
        tcd(&mut oracle, 0, RAM + 0x200, 4, 1);
        issue(&mut oracle, 8, true).unwrap();
        assert_eq!(oracle.read32(NEXT).unwrap(), 1);
        for _ in 0..2000 {
            b.write32(esdhc::BASE + esdhc::XFERTYP, 0).unwrap();
        }
        assert!(b.esdhc.take_host_write().is_none())
    }

    #[test]
    fn zero_citer_read_posts_oracle_dma_sem_without_tcd_done() {
        let mut b = board(CompletionPolicy::Oracle);
        tcd(&mut b, 0, RAM + 0x200, 512, 0);
        issue(&mut b, 8, true).unwrap();
        assert_eq!(b.read32(DS).unwrap(), 1);
        assert_eq!(
            b.read16(edma::TCD_BASE + 59 * 0x20 + edma::CSR as u32)
                .unwrap(),
            0
        );
        assert!(!b.dma59_armed());
        assert!(
            matches!(b.completion_events()[0], CompletionEvent::Dma59 { completion, .. } if !completion.done)
        );
    }

    #[test]
    fn device_cerq59_disables_cmd18_dma_but_cerq58_does_not() {
        let mut b = board(CompletionPolicy::Device);
        let d = RAM + 0x200;
        tcd(&mut b, 0, d, 4, 1);
        b.write8(edma::SERQ, 59).unwrap();
        b.write8(edma::CERQ, 59).unwrap();
        command(&mut b, 18, true).unwrap();
        assert_eq!(b.read32(d).unwrap(), 0);
        assert!(
            !b.completion_events()
                .iter()
                .any(|e| matches!(e, CompletionEvent::Dma59 { .. }))
        );

        b.write8(edma::SERQ, 59).unwrap();
        b.write8(edma::CERQ, 58).unwrap();
        command(&mut b, 18, true).unwrap();
        assert_eq!(
            b.read16(edma::TCD_BASE + 59 * 0x20 + edma::CSR as u32)
                .unwrap(),
            edma::CSR_DONE
        );
        assert!(
            b.completion_events()
                .iter()
                .any(|e| matches!(e, CompletionEvent::Dma59 { .. }))
        );
    }

    #[test]
    fn device_request_bit7_is_nop() {
        let mut b = board(CompletionPolicy::Device);
        let d = RAM + 0x200;
        tcd(&mut b, 0, d, 4, 1);
        b.write8(edma::SERQ, 59).unwrap();
        b.write8(edma::CERQ, 0x80 | 59).unwrap();
        command(&mut b, 18, true).unwrap();
        assert!(b.dma59_armed());
        assert_eq!(
            b.read16(edma::TCD_BASE + 59 * 0x20 + edma::CSR as u32)
                .unwrap(),
            edma::CSR_DONE
        );

        b.write8(edma::CERQ, 59).unwrap();
        b.write8(edma::SERQ, 0x80 | 59).unwrap();
        command(&mut b, 18, true).unwrap();
        assert!(!b.dma59_armed());
    }

    #[test]
    fn device_global_request_enable_and_disable_control_dma59() {
        let mut b = board(CompletionPolicy::Device);
        let d = RAM + 0x200;
        tcd(&mut b, 0, d, 4, 1);
        b.write8(edma::SERQ, 0x40).unwrap();
        command(&mut b, 18, true).unwrap();
        assert!(b.dma59_armed());
        assert_eq!(
            b.read16(edma::TCD_BASE + 59 * 0x20 + edma::CSR as u32)
                .unwrap(),
            edma::CSR_DONE
        );

        b.write8(edma::CERQ, 0x40).unwrap();
        assert!(!b.dma59_armed());
        command(&mut b, 18, true).unwrap();
        assert_eq!(
            b.completion_events()
                .iter()
                .filter(|e| matches!(e, CompletionEvent::Dma59 { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn device_major_completion_keeps_or_disables_request_from_dreq() {
        let mut b = board(CompletionPolicy::Device);
        let d = RAM + 0x200;
        tcd(&mut b, 0, d, 4, 1);
        b.write8(edma::SERQ, 59).unwrap();
        command(&mut b, 18, true).unwrap();
        command(&mut b, 18, true).unwrap();
        assert!(b.dma59_armed());
        assert_eq!(
            b.completion_events()
                .iter()
                .filter(|e| matches!(e, CompletionEvent::Dma59 { .. }))
                .count(),
            2
        );

        let mut b = board(CompletionPolicy::Device);
        tcd(&mut b, 0, d, 4, 1);
        b.write16(
            edma::TCD_BASE + 59 * 0x20 + edma::CSR as u32,
            edma::CSR_D_REQ,
        )
        .unwrap();
        b.write8(edma::SERQ, 59).unwrap();
        command(&mut b, 18, true).unwrap();
        command(&mut b, 18, true).unwrap();
        assert!(!b.dma59_armed());
        assert_eq!(
            b.completion_events()
                .iter()
                .filter(|e| matches!(e, CompletionEvent::Dma59 { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn oracle_cerq59_preserves_one_shot_arm() {
        let mut b = board(CompletionPolicy::Oracle);
        let d = RAM + 0x200;
        tcd(&mut b, 0, d, 4, 1);
        b.write8(edma::SERQ, 0x40).unwrap();
        command(&mut b, 18, true).unwrap();
        assert!(!b.dma59_armed());
        assert!(
            !b.completion_events()
                .iter()
                .any(|e| matches!(e, CompletionEvent::Dma59 { .. }))
        );

        b.write8(edma::SERQ, 59).unwrap();
        b.write8(edma::CERQ, 59).unwrap();
        command(&mut b, 18, true).unwrap();
        assert_eq!(
            b.read16(edma::TCD_BASE + 59 * 0x20 + edma::CSR as u32)
                .unwrap(),
            edma::CSR_DONE
        );
        assert!(!b.dma59_armed());
    }
}
