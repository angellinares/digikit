//! Firmware-agnostic sparse RAM board and synchronous eMMC DMA59 seam.
use std::collections::BTreeMap;

use coldfire::{Bus, BusError};
use emmc_card::{
    Card, CardError,
    dma::{StorageDmaError, service_dma59},
};
use periph::{
    DmaLink, dspi,
    dtim::{BASES as DTIM_BASES, DtimBank},
    edma,
    esdhc::{self, DmaCompletion, Esdhc, RegisterPolicy},
    gpio::{GPIO_BASE, PPDSDR_C, SdGate},
    intc::BASES as INTC_BASES,
    pit::BASES as PIT_BASES,
    regfile::SLOT_SIZE,
    spilink::SerqEffect,
    ssi::{self, RxHandoverPeer},
};

use crate::{host_state::HostState, state::OverlaySectorRecord, time::Time};

/// SDRAM and internal SRAM: [0x4000_0000, 0x8C00_0000). No device register
/// (every owner sits at 0x8C00_0000 or above) and no forced word may lie in it.
const SDRAM_BASE: u32 = 0x4000_0000;
const SDRAM_SIZE: u32 = 0x4C00_0000;
const PAGE_SIZE: usize = 1024 * 1024;
const PAGE_SHIFT: u32 = 20;
const PAGE_COUNT: usize = 4096;
const PAGE_MASK: u32 = PAGE_SIZE as u32 - 1;
/// The console TX descriptor is a 4 KiB modulo ring. Keep host collection
/// bounded even when a malformed descriptor requests a larger transfer.
const TX35_MAX_BYTES: usize = 4096;
/// eDMA channel the DT2 firmware uses for DSPI1 RX (finding 04, transfer inventory).
const DSPI1_RX_CHAN: usize = 14;
type RamPage = Box<[u8]>;
/// UART9 (MIDI DIN) data register: the firmware sends MIDI by byte stores here (finding 25).
pub const MIDI_UDR: u32 = 0xec07_400c;

// `owned_mmio`'s fast reject: no owned device slot starts below the end of
// SDRAM.
const _: () = {
    let end = SDRAM_BASE + SDRAM_SIZE;
    let mut k = 0;
    while k < 4 {
        assert!(PIT_BASES[k] >= end && DTIM_BASES[k] >= end);
        k += 1;
    }
    let mut k = 0;
    while k < 3 {
        assert!(INTC_BASES[k] >= end);
        k += 1;
    }
    assert!(edma::EDMA_BASE >= end);
    assert!(periph::spilink::DSPI1_SLOT >= end);
    assert!(periph::spilink::DSPI2_SLOT >= end);
    assert!(periph::spilink::DSP_SLOT >= end);
    assert!(esdhc::BASE >= end);
};

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
    DspiDma { address: u32, bytes: usize },
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

/// RCM.RCR, the reset control register: bit 7 is SOFTRST (docs/findings/36-boot-load-spike.md section 3).
pub const RCM_RCR: u32 = 0xEC09_0000;

impl Board {
    /// Has the guest written SOFTRST to RCM.RCR? (The machine does not reset itself: the host that owns it restarts the boot.)
    pub fn soft_reset_requested(&self) -> bool {
        self.soft_reset
    }
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
    esdhc_dma_bytes: u64,
    /// A store of 0x80 into RCM.RCR (0xEC090000) was seen: the firmware asked for a soft reset (see `soft_reset_requested`).
    soft_reset: bool,
    scratch_guest: RamPage,
    scratch_card: RamPage,
    events: Vec<CompletionEvent>,
    dma_written: Vec<(u32, usize)>,
    /// The engine of each `dma_written` entry (instruments only; drained with it).
    dma_sources: Vec<&'static str>,
    last_error: Option<BoardWriteError>,
    /// Optional PIT/INTC facade in a stable allocation; ownership transfers by pointer.
    /// Its owned MMIO is never backed by sparse RAM.
    time: Option<Box<Time>>,
    /// Oracle read hooks: each value overlays four big-endian bytes before a
    /// guest read, without changing the backing page or later guest writes.
    forced_mmio: BTreeMap<u32, u32>,
    /// False when a forced read word overlaps SDRAM; otherwise SDRAM accesses
    /// skip every MMIO ownership check (no device owns that range).
    sdram_fast: bool,
    /// Counts guest accesses that reached device-owned registers (eDMA, DSPI,
    /// SSI/DSP slots, eSDHC, PIT/DTIM/INTC writes, GPIO sense). A host loop may
    /// batch plain RAM instructions until this changes.
    mmio_epoch: u64,
    last_epoch_addr: u32,
    /// Opt-in only: ordinary long-running execution must not retain every Bus access.
    capture_guest_accesses: bool,
    guest_reads: Vec<GuestAccess>,
    guest_writes: Vec<GuestAccess>,
    guest_ordered: Vec<GuestBusAccess>,
    /// Python's unmapped-memory callback zero-maps pages on first touch.
    /// Opt-in and SDRAM-only here; never treat missing MMIO as RAM.
    oracle_sdram_faults: bool,
    oracle_fault_pages: u8,
    /// Optional board-only SD continuity wiring. GPIO remains sparse RAM;
    /// this only mirrors the three Python hook side effects into it.
    sd_gate: Option<SdGate>,
    /// Bytes drained by the eDMA35 UART8-TX model. This is host observation
    /// only; UART RX and peripheral responses remain outside this Board seam.
    uart_tx: Vec<u8>,
    /// Bytes the guest stored to the MIDI DIN UART (UART9) data register. Host observation
    /// only: not part of a checkpoint.
    midi_tx: Vec<u8>,
    /// DSPI1 RX completion (eDMA channel 14) waiting for the host to raise
    /// its interrupt. Not part of the snapshot: `snap_save` refuses while set.
    dspi1_irq: bool,
}
impl Board {
    /// Enable the recovered SSI/eDMA clock only for bounded diagnostics.
    /// Normal product construction leaves the lane absent.
    pub fn enable_ssi_diagnostic(&mut self, request_hz: u64, ips: u64) -> bool {
        self.dma.enable_ssi_diagnostic(request_hz, ips)
    }

    /// Device-register accesses so far; see the field docs.
    #[inline(always)]
    fn in_dsp_port(addr: u32) -> bool {
        addr.wrapping_sub(periph::dsp::BASE) < SLOT_SIZE as u32
    }
    #[inline(always)]
    fn bump_epoch(&mut self, addr: u32) {
        self.mmio_epoch = self.mmio_epoch.wrapping_add(1);
        self.last_epoch_addr = addr;
    }
    /// Address of the access that last advanced `mmio_epoch` (diagnostics).
    pub fn last_epoch_addr(&self) -> u32 {
        self.last_epoch_addr
    }
    pub fn mmio_epoch(&self) -> u64 {
        self.mmio_epoch
    }

    /// First guest tick at which [`Self::service_ssi`] can do any work, for a
    /// host that skips the per-instruction call. `u64::MAX` when SSI is off or
    /// idle; `None` when it must be called every instruction (unarmed clock,
    /// or an interrupt it still owes the CPU).
    pub fn ssi_batch_limit(&self) -> Option<u64> {
        match self.dma.ssi.as_ref() {
            None => Some(u64::MAX),
            Some(ssi) => ssi.batch_limit(),
        }
    }

    pub fn ssi_deadline(&mut self, done: u64) -> Option<u64> {
        self.dma.ssi.as_mut()?.deadline(done)
    }

    /// Service one due SSI request using actual mapped guest RAM. Invalid
    /// descriptors and ranges are rejected before any TCD or RAM mutation.
    /// Returned ranges require CPU decode-cache invalidation by the owner.
    /// Device mode also queues RX ranges and latches qualifying TX50 major
    /// completions in INTH until guest acknowledgement; no IRQ is delivered here.
    pub fn service_ssi(&mut self, done: u64) -> Vec<(u32, usize)> {
        // Called after every interpreted instruction: decide "not due"
        // without moving the (large) SSI state out of and back into the Option.
        match self.dma.ssi.as_mut() {
            None => return Vec::new(),
            Some(ssi) => {
                if !ssi.due(done) {
                    return Vec::new();
                }
            }
        }
        let mut ssi = self.dma.ssi.take().expect("checked above");
        struct Planned {
            channel: usize,
            tcd: edma::TcdSnapshot,
            payload: Vec<u8>,
            descriptor: Option<Vec<u8>>,
        }
        let mut plans = Vec::new();
        for channel in [ssi::RX_CHAN, ssi::TX_CHAN] {
            if !ssi.enabled[channel] {
                continue;
            }
            let view = edma::TcdView::new(&mut self.dma.edma_regs, channel);
            let linked = view.citer_raw() & 0x8000 != 0 || view.biter_raw() & 0x8000 != 0;
            let smod = view.smod_mask();
            let dmod = view.dmod_mask();
            let tcd = view.snapshot();
            let n = tcd.nbytes as usize;
            let valid = tcd.citer != 0
                && !linked
                && smod == 0
                && dmod == 0
                && tcd.nbytes & 0xc000_0000 == 0
                && n != 0
                && n <= 4096
                && n % 4 == 0
                && (tcd.attr & 7) == 2
                && ((tcd.attr >> 8) & 7) == 2
                && if channel == ssi::RX_CHAN {
                    tcd.soff == 0 && tcd.doff == 4
                } else {
                    tcd.soff == 4 && tcd.doff == 0
                };
            let addr = if channel == ssi::RX_CHAN {
                tcd.daddr
            } else {
                tcd.saddr
            };
            let range_ok = valid
                && (0..n).all(|offset| {
                    let Some(at) = addr.checked_add(offset as u32) else {
                        return false;
                    };
                    !Self::owned_mmio(at) && self.page(at).is_some()
                });
            if !range_ok {
                ssi.rejected_requests += 1;
                ssi.requests += 1;
                ssi.advance_due();
                self.dma.ssi = Some(ssi);
                return Vec::new();
            }
            let major_start = tcd.citer == tcd.biter;
            let payload = if channel == ssi::RX_CHAN {
                let mut peer = RxHandoverPeer;
                peer.rx_major(n, major_start)
            } else {
                let mut bytes = Vec::with_capacity(n);
                for offset in 0..n {
                    bytes.push(self.ram_read(addr + offset as u32, 1).unwrap() as u8);
                }
                bytes
            };
            let descriptor = if tcd.csr & edma::CSR_E_SG != 0 && tcd.citer == 1 {
                let descriptor = tcd.dlast as u32;
                let descriptor_ok = descriptor & 0x1f == 0
                    && (0..0x20u32).all(|offset| {
                        let Some(at) = descriptor.checked_add(offset) else {
                            return false;
                        };
                        !Self::owned_mmio(at) && self.page(at).is_some()
                    });
                if !descriptor_ok {
                    ssi.rejected_requests += 1;
                    ssi.requests += 1;
                    ssi.advance_due();
                    self.dma.ssi = Some(ssi);
                    return Vec::new();
                }
                Some(
                    (0..0x20u32)
                        .map(|offset| self.ram_read(descriptor + offset, 1).unwrap() as u8)
                        .collect(),
                )
            } else {
                None
            };
            plans.push(Planned {
                channel,
                tcd,
                payload,
                descriptor,
            });
        }
        let mut writes = Vec::new();
        for plan in plans {
            let channel = plan.channel;
            let n = plan.payload.len();
            let result = ssi
                .run_minor(
                    &mut self.dma.edma_regs,
                    channel,
                    (channel == ssi::RX_CHAN).then_some(plan.payload.as_slice()),
                )
                .expect("preflighted SSI descriptor");
            if channel == ssi::RX_CHAN {
                for (offset, byte) in plan.payload.into_iter().enumerate() {
                    let _ = self.ram_write(result.dest_addr + offset as u32, 1, u32::from(byte));
                }
                writes.push((result.dest_addr, n));
                if self.policy == CompletionPolicy::Device {
                    self.dma_written.push((result.dest_addr, n));
                    self.dma_sources.push("ssi0-rx");
                }
            } else {
                ssi.tx_bytes += n as u64;
            }
            if result.major_complete && plan.tcd.csr & edma::CSR_E_SG != 0 {
                if let Some(descriptor) = plan.descriptor {
                    for (offset, byte) in descriptor.into_iter().enumerate() {
                        self.dma
                            .edma_regs
                            .set_u8_at(edma::tcd_offset(channel) + offset, byte);
                    }
                }
            }
            if self.policy == CompletionPolicy::Device
                && channel == ssi::TX_CHAN
                && result.major_complete
                && plan.tcd.csr & edma::CSR_INT_MAJOR != 0
            {
                // MCF5441XRM p0380: one sticky request bit, not a completion queue.
                // All payload and TCD/SG writeback above has completed.
                let pending = self.dma.edma_regs.read(edma::INTH, 4);
                self.dma.edma_regs.write(edma::INTH, 4, pending | (1 << 18));
            }
        }
        ssi.requests += 1;
        ssi.now = done;
        // Advance exactly one period; no coalescing across guest boundaries.
        ssi.advance_due();
        self.dma.ssi = Some(ssi);
        writes
    }
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
            esdhc_dma_bytes: 0,
            soft_reset: false,
            scratch_guest: vec![0; PAGE_SIZE].into_boxed_slice(),
            scratch_card: vec![0; PAGE_SIZE].into_boxed_slice(),
            events: vec![],
            dma_written: vec![],
            dma_sources: vec![],
            last_error: None,
            time: None,
            forced_mmio: BTreeMap::new(),
            sdram_fast: true,
            mmio_epoch: 0,
            last_epoch_addr: 0,
            capture_guest_accesses: false,
            guest_reads: vec![],
            guest_writes: vec![],
            guest_ordered: vec![],
            oracle_sdram_faults: false,
            oracle_fault_pages: 0,
            sd_gate: None,
            uart_tx: vec![],
            midi_tx: vec![],
            dspi1_irq: false,
        }
    }

    /// Opt-in machine snapshot: RAM pages (zero 4 KiB blocks elided), DMA/DSPI/
    /// SSI link, eSDHC and card, timers, SD gate and counters. Restore into a
    /// board built by the same construction code (same pages mapped by the
    /// loader, same optional lanes enabled); host-owned peers are untouched.
    /// Fails if completion events or DMA ranges are still undrained.
    pub fn snap_save(&self, w: &mut periph::snap::Writer) -> Result<(), String> {
        if !self.events.is_empty() || !self.dma_written.is_empty() || self.dspi1_irq {
            return Err(
                "board has undrained completion events, DMA ranges or a DSPI1 interrupt".into(),
            );
        }
        const BLOCK: usize = 4096;
        w.tag("BORD");
        let mapped: Vec<usize> = (0..PAGE_COUNT)
            .filter(|&i| self.pages[i].is_some())
            .collect();
        w.u64(mapped.len() as u64);
        for i in mapped {
            let page = self.pages[i].as_ref().expect("mapped");
            w.u32(i as u32);
            for block in page.chunks(BLOCK) {
                let zero = block.iter().all(|&b| b == 0);
                w.bool(!zero);
                if !zero {
                    w.raw(block);
                }
            }
        }
        self.dma.snap_save(w);
        w.tag("ESDH");
        self.esdhc.snap_save(w);
        self.esdhc_card().snap_save(w);
        w.bool(self.armed_dma59);
        w.u64(self.esdhc_dma_bytes);
        w.u8(self.oracle_fault_pages);
        w.bool(self.time.is_some());
        if let Some(time) = &self.time {
            time.snap_save(w);
        }
        w.bool(self.sd_gate.is_some());
        if let Some(gate) = &self.sd_gate {
            gate.snap_save(w);
        }
        w.bytes(&self.uart_tx);
        Ok(())
    }

    pub fn snap_load(&mut self, r: &mut periph::snap::Reader) -> Result<(), String> {
        const BLOCK: usize = 4096;
        r.tag("BORD")?;
        let n = r.len(PAGE_COUNT)?;
        let mut saved = vec![false; PAGE_COUNT];
        for _ in 0..n {
            let i = r.u32()? as usize;
            if i >= PAGE_COUNT {
                return Err("snapshot page index out of range".into());
            }
            saved[i] = true;
            let page = self.pages[i].get_or_insert_with(|| vec![0; PAGE_SIZE].into_boxed_slice());
            for block in page.chunks_mut(BLOCK) {
                if r.bool()? {
                    block.copy_from_slice(r.raw(BLOCK)?);
                } else {
                    block.fill(0);
                }
            }
        }
        if (0..PAGE_COUNT).any(|i| self.pages[i].is_some() && !saved[i]) {
            return Err("board maps a page the snapshot does not hold".into());
        }
        self.events.clear();
        self.dma_written.clear();
        self.dma_sources.clear();
        self.dma.snap_load(r)?;
        r.tag("ESDH")?;
        self.esdhc.snap_load(r)?;
        self.esdhc.card_mut().snap_load(r)?;
        self.armed_dma59 = r.bool()?;
        self.esdhc_dma_bytes = r.u64()?;
        self.oracle_fault_pages = r.u8()?;
        match (r.bool()?, self.time.as_mut()) {
            (true, Some(time)) => time.snap_load(r)?,
            (false, None) => {}
            _ => return Err("snapshot timer facade presence differs".into()),
        }
        match (r.bool()?, self.sd_gate.as_mut()) {
            (true, Some(gate)) => gate.snap_load(r)?,
            (false, None) => {}
            _ => return Err("snapshot SD gate presence differs".into()),
        }
        self.uart_tx = r.bytes()?.to_vec();
        Ok(())
    }

    fn esdhc_card(&self) -> &Card {
        self.esdhc.card_ref()
    }

    /// How many write commands the guest has sent to the eMMC card (`Card::write_count`).
    pub fn card_write_count(&self) -> u64 {
        self.esdhc.card_ref().write_count()
    }

    /// The guest's writes to the eMMC card (see `Card::overlay_sectors`).
    pub fn card_overlay_sectors(&self) -> Vec<emmc_card::CardOverlaySector> {
        self.esdhc.card_ref().overlay_sectors()
    }

    /// Enable the board's port-D4-to-port-C3 SD continuity gate. The caller
    /// supplies the hook state separately from checkpoint GPIO bytes, as the
    /// Python oracle does. GPIO itself remains backed by sparse RAM.
    pub fn enable_sd_gate(&mut self, driven: bool) -> Result<(), BusError> {
        let page_base = GPIO_BASE & !PAGE_MASK;
        self.map_zeroed_ram_page(page_base)?;
        let offset = (GPIO_BASE & PAGE_MASK) as usize;
        let mut gate = SdGate::default();
        gate.load_page(
            GPIO_BASE,
            &self.page(page_base).expect("page just mapped")[offset..offset + SLOT_SIZE],
        );
        gate.load_state(driven);
        self.sd_gate = Some(gate);
        Ok(())
    }

    /// Route PIT and INTC MMIO through the supplied timer facade.
    pub fn attach_time(&mut self, time: Time) {
        self.time = Some(Box::new(time));
    }

    /// Read the timer facade without changing its registers or scheduling state.
    pub fn time(&self) -> Option<&Time> {
        self.time.as_deref()
    }

    /// Borrow the timer facade while retaining its stable allocation.
    pub fn time_mut(&mut self) -> Option<&mut Time> {
        self.time.as_deref_mut()
    }

    /// Temporarily transfer ownership of the timer facade's stable allocation.
    pub fn take_time(&mut self) -> Option<Box<Time>> {
        self.time.take()
    }

    /// Restore a timer facade allocation transferred by [`Self::take_time`].
    pub fn restore_time(&mut self, time: Box<Time>) {
        debug_assert!(self.time.is_none());
        self.time = Some(time);
    }

    /// Restore validated host-only storage state after checkpoint pages load.
    /// Guest register files and eDMA TCDs come from the mapped MSTATE pages.
    pub(crate) fn restore_storage_state(
        &mut self,
        state: &HostState,
        overlay_sectors: Option<&[OverlaySectorRecord]>,
    ) -> Result<(), CardError> {
        if let Some(sectors) = overlay_sectors {
            self.esdhc
                .card_mut()
                .restore_checkpoint_sectors(&state.card, sectors)?;
        } else {
            self.esdhc.card_mut().restore_checkpoint(&state.card)?;
        }
        self.esdhc.restore_pattern(state.pattern);
        self.armed_dma59 = state.armed_dma59;
        self.esdhc_dma_bytes = state.esdhc_dma_bytes;
        self.dma.tx35.bytes = state.tx_bytes;
        self.dma.tx35.transfers = state.tx_transfers;
        Ok(())
    }

    #[inline]
    fn owned_mmio(addr: u32) -> bool {
        // Every owner sits at or above the end of SDRAM (checked below), so
        // SDRAM/SRAM addresses skip the per-owner range scans.
        addr >= SDRAM_BASE + SDRAM_SIZE
            && (Time::owns(addr) || DmaLink::owns(addr) || Esdhc::<Card>::owns(addr))
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
        self.sdram_fast = !words
            .keys()
            .any(|&base| base.wrapping_sub(SDRAM_BASE.wrapping_sub(3)) < SDRAM_SIZE + 3);
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
        // DmaLink's register files live inside MSTATE's 1 MiB pages, just
        // like Time's PIT/DTIM/INTC slots. Guest TCD writes after import must
        // see the imported starting values rather than zeroed register files.
        for slot in [
            edma::EDMA_BASE,
            dspi::DSPI2_BASE,
            dspi::DSPI1_BASE,
            periph::dsp::BASE,
        ] {
            if slot & !PAGE_MASK == addr {
                let offset = (slot & PAGE_MASK) as usize;
                let _ = self.dma.load_page(slot, &data[offset..offset + SLOT_SIZE]);
            }
        }
        if let Some(gate) = self.sd_gate.as_mut()
            && GPIO_BASE & !PAGE_MASK == addr
        {
            let offset = (GPIO_BASE & PAGE_MASK) as usize;
            gate.load_page(GPIO_BASE, &data[offset..offset + SLOT_SIZE]);
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

    /// Copy `out.len()` bytes of mapped RAM at `addr` without MMIO dispatch or
    /// guest-access records. `false` if any byte is unmapped.
    pub fn ram_peek(&self, addr: u32, out: &mut [u8]) -> bool {
        for (i, byte) in out.iter_mut().enumerate() {
            let a = addr.wrapping_add(i as u32);
            let Some(page) = self.page(a) else {
                return false;
            };
            *byte = page[Self::page_offset(a)];
        }
        true
    }

    /// Host acceleration for a guest counting loop: add `delta` (wrapping) to the
    /// aligned 32-bit word at `addr` in mapped SDRAM and return `(old, new)`.
    /// Touches nothing else (no MMIO, no guest-access record); `None` when the
    /// word is not entirely in one mapped SDRAM page.
    pub fn sdram_add32(&mut self, addr: u32, delta: u32) -> Option<(u32, u32)> {
        if addr & 3 != 0 || addr.wrapping_sub(SDRAM_BASE) >= SDRAM_SIZE {
            return None;
        }
        let old = self.ram_read(addr, 4)?;
        let new = old.wrapping_add(delta);
        self.ram_write(addr, 4, new).then_some((old, new))
    }

    /// Compare backing RAM without copying pages, dispatching MMIO, or recording
    /// guest accesses. Like `read_ram_page`, this is host-side observation only.
    pub fn ram_matches(&self, mut addr: u32, mut expected: &[u8]) -> bool {
        let Ok(len) = u32::try_from(expected.len()) else {
            return false;
        };
        if addr.checked_add(len).is_none() {
            return false;
        }
        while !expected.is_empty() {
            let Some(page) = self.page(addr) else {
                return false;
            };
            let offset = Self::page_offset(addr);
            let count = expected.len().min(PAGE_SIZE - offset);
            if page[offset..offset + count] != expected[..count] {
                return false;
            }
            addr += count as u32;
            expected = &expected[count..];
        }
        true
    }

    /// Host acceleration of stores within one already mapped SDRAM page.
    /// Never allocates, dispatches MMIO, or emits DMA/guest-access events.
    /// The caller must invalidate the CPU decode cache for the written range.
    pub fn zero_mapped_sdram(&mut self, addr: u32, len: usize) -> bool {
        let Ok(len) = u32::try_from(len) else {
            return false;
        };
        let Some(end) = addr.checked_add(len) else {
            return false;
        };
        if len == 0
            || self.capture_guest_accesses
            || addr < 0x4000_0000
            || end > 0x4800_0000
            || (addr & !PAGE_MASK) != ((end - 1) & !PAGE_MASK)
        {
            return false;
        }
        let offset = Self::page_offset(addr);
        let Some(page) = self.page_mut(addr) else {
            return false;
        };
        page[offset..offset + len as usize].fill(0);
        true
    }
    pub fn completion_events(&self) -> &[CompletionEvent] {
        &self.events
    }
    /// True when `take_completion_events`, `take_dma_written_ranges` or
    /// `take_uart_tx` would return anything.
    #[inline]
    pub fn has_host_events(&self) -> bool {
        !self.events.is_empty()
            || !self.dma_written.is_empty()
            || !self.uart_tx.is_empty()
            || self.dspi1_irq
    }
    /// A DSPI1 RX completion interrupt (eDMA channel 14) is waiting for the
    /// host to deliver it; the host calls `clear_dspi1_irq` once it has.
    #[inline]
    pub fn dspi1_irq(&self) -> bool {
        self.dspi1_irq
    }
    pub fn clear_dspi1_irq(&mut self) {
        self.dspi1_irq = false;
    }
    /// DSPI1 (`0xFC03_C000`) has no modelled peer on the DT2 board: its
    /// command/response protocol (`FUN_400124a0`) sends a command and reads
    /// back a block, then waits for the RX channel's completion. Model the
    /// peer as answering with zeros: when the guest arms channel 14 with the
    /// DSPI1 POPR source, fill the destination, retire the major loop and
    /// raise the completion. Without this the sample loader blocked forever
    /// on the first sample (every pend that waited on DSPI1 never woke).
    fn dspi1_rx_request(&mut self) -> Result<(), BoardWriteError> {
        const DSPI1_POPR_LO: u32 = 0xFC03_C03A;
        let mut tcd = edma::TcdView::new(&mut self.dma.edma_regs, DSPI1_RX_CHAN);
        if tcd.saddr() != DSPI1_POPR_LO || tcd.citer() == 0 || tcd.nbytes() == 0 {
            return Ok(());
        }
        let total = u32::from(tcd.citer()) * tcd.nbytes();
        let dest = tcd.daddr();
        let step = tcd.doff();
        let end = (i64::from(dest) + step * i64::from(tcd.citer()) + tcd.dlast()) as u32;
        let biter = tcd.biter();
        tcd.set_daddr(end);
        tcd.set_citer(biter);
        // Contiguous destination (DOFF == element size) is all the firmware uses.
        if step != i64::from(tcd.nbytes()) {
            return Ok(());
        }
        for offset in 0..total {
            let at = dest.wrapping_add(offset);
            if Self::owned_mmio(at) || self.page(at).is_none() {
                return Err(BoardWriteError::DspiDma {
                    address: at,
                    bytes: total as usize,
                });
            }
        }
        for offset in 0..total {
            assert!(self.ram_write(dest.wrapping_add(offset), 1, 0));
        }
        self.dma_written.push((dest, total as usize));
        self.dma_sources.push("dspi1-rx");
        self.dspi1_irq = true;
        Ok(())
    }
    pub fn take_completion_events(&mut self) -> Vec<CompletionEvent> {
        std::mem::take(&mut self.events)
    }
    pub fn dma59_armed(&self) -> bool {
        self.armed_dma59
    }
    /// Python Esdhc v1's cumulative host transfer counter, not pending DMA.
    pub fn esdhc_dma_bytes(&self) -> u64 {
        self.esdhc_dma_bytes
    }
    /// Pending channels in INTC2 source 0's eDMA56..63 group.
    pub fn storage_dma_pending(&self) -> u32 {
        self.dma.edma_regs.read(edma::INTH, 4) & 0xff00_0000
    }

    /// Raw Device eDMA channel-50 request, independent of SSI Oracle delivery
    /// diagnostics and INTC/CPU eligibility. Entry does not clear this latch.
    pub fn device_ssi50_pending(&self) -> bool {
        self.policy == CompletionPolicy::Device
            && self.dma.edma_regs.read(edma::INTH, 4) & (1 << 18) != 0
    }

    pub fn completion_policy(&self) -> CompletionPolicy {
        self.policy
    }
    /// External DMA invalidates these CPU decode pages before the next
    /// instruction fetch; successful DMA writes, including Device SSI RX, appear.
    pub fn take_dma_written_ranges(&mut self) -> Vec<(u32, usize)> {
        std::mem::take(&mut self.dma_written)
    }
    /// The engine of each range the last `take_dma_written_ranges` returned,
    /// in order (take it second; for the write-watch). Machine consumes these
    /// paired tags when it owns invalidation; they are not retained after stepping.
    pub fn take_dma_sources(&mut self) -> Vec<&'static str> {
        std::mem::take(&mut self.dma_sources)
    }
    /// Return the bytes the guest wrote to the MIDI DIN UART9 data register since the prior call.
    pub fn take_midi_tx(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.midi_tx)
    }
    /// Return UART8 TX bytes drained by eDMA channel 35 since the prior call.
    pub fn take_uart_tx(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.uart_tx)
    }
    /// Consume one eDMA35 completion after a host has successfully offered
    /// its verified vector to the CPU.
    pub fn consume_tx35_pending(&mut self) -> bool {
        self.dma.tx35.consume_pending()
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
    #[cold]
    #[inline(never)]
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
    #[inline]
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
    #[inline]
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
            if self.policy == CompletionPolicy::Device && completion.done {
                // The data copy and descriptor writeback have already succeeded.
                self.esdhc.device_event(1 << 1).expect("validated TC event");
                if completion.major_interrupt {
                    let pending = self.dma.edma_regs.read(edma::INTH, 4);
                    self.dma.edma_regs.write(edma::INTH, 4, pending | (1 << 27));
                }
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
        self.esdhc_dma_bytes = self.esdhc_dma_bytes.saturating_add(effect.bytes as u64);
        if effect.direction == esdhc::DmaDirection::CardToGuest && effect.completion.done {
            for i in 0..len {
                let value = self.scratch_guest[i];
                let address = base + i as u32;
                self.page_mut(address).unwrap()[Self::page_offset(address)] = value;
            }
            self.dma_written.push((base, effect.bytes));
            self.dma_sources.push("esdhc-dma59");
        }
        Ok(effect.completion)
    }
    fn update_dma59_request(&mut self, addr: u32, size: u8, value: u32) {
        if size != 1 {
            return;
        }
        let byte = value as u8;
        match self.policy {
            // `unicorn-reference-final:emu/edma.py` only recognizes the channel form of SERQ. Its CERQ
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

    /// Read channel 29's raw PUSHR source, respecting the TCD's SOFF/SMOD
    /// before DmaLink strips its 16-bit tags. This is a host DMA read, not
    /// a ColdFire Bus read and therefore not a guest access recorder entry.
    fn dspi2_source(&mut self) -> Result<Vec<u8>, BoardWriteError> {
        let view = edma::TcdView::new(&mut self.dma.edma_regs, dspi::TX_CHAN);
        let mask = view.smod_mask();
        let tcd = view.snapshot();
        let elem = 1usize << (tcd.attr & 7);
        let nbytes = tcd.nbytes as usize;
        let len = (tcd.citer as usize).checked_mul(nbytes);
        let Some(len) = len else {
            return Err(BoardWriteError::DspiDma {
                address: tcd.saddr,
                bytes: usize::MAX,
            });
        };
        if nbytes == 0 || !nbytes.is_multiple_of(elem) || len > dspi::FRAME_BYTES * 4 {
            return Err(BoardWriteError::DspiDma {
                address: tcd.saddr,
                bytes: len,
            });
        }
        let base = tcd.saddr & !mask;
        let mut source = Vec::with_capacity(len);
        let mut addr = tcd.saddr;
        for _ in 0..tcd.citer as usize {
            for _ in 0..nbytes / elem {
                for offset in 0..elem {
                    let at = addr
                        .checked_add(offset as u32)
                        .ok_or(BoardWriteError::DspiDma {
                            address: addr,
                            bytes: len,
                        })?;
                    if Self::owned_mmio(at) {
                        return Err(BoardWriteError::DspiDma {
                            address: at,
                            bytes: len,
                        });
                    }
                    source.push(self.ram_read(at, 1).ok_or(BoardWriteError::DspiDma {
                        address: at,
                        bytes: len,
                    })? as u8);
                }
                addr = addr.wrapping_add_signed(tcd.soff as i32);
                if mask != 0 {
                    addr = base | (addr & mask);
                }
            }
        }
        Ok(source)
    }

    /// Read eDMA35's UART8 source exactly as `unicorn-reference-final:emu/edma.py`'s `TxChannel.run` does:
    /// every byte advances by SOFF and source modulo is applied per byte.
    /// This preflights all guest reads before the SERQ write mutates the TCD.
    fn tx35_source(&mut self) -> Result<Vec<u8>, BoardWriteError> {
        let (mask, tcd) = {
            let view = edma::TcdView::new(&mut self.dma.edma_regs, self.dma.tx35.chan);
            (view.smod_mask(), view.snapshot())
        };
        let nbytes = tcd.nbytes as usize;
        let len = (tcd.citer as usize)
            .checked_mul(nbytes)
            .ok_or(BoardWriteError::DspiDma {
                address: tcd.saddr,
                bytes: usize::MAX,
            })?;
        if len > TX35_MAX_BYTES {
            return Err(BoardWriteError::DspiDma {
                address: tcd.saddr,
                bytes: len,
            });
        }
        let base = tcd.saddr & !mask;
        let mut source = Vec::with_capacity(len);
        let mut addr = tcd.saddr;
        for _ in 0..len {
            if Self::owned_mmio(addr) {
                return Err(BoardWriteError::DspiDma {
                    address: addr,
                    bytes: len,
                });
            }
            source.push(self.ram_read(addr, 1).ok_or(BoardWriteError::DspiDma {
                address: addr,
                bytes: len,
            })? as u8);
            let soff = i32::try_from(tcd.soff).expect("eDMA SOFF is signed 16-bit");
            addr = addr
                .checked_add_signed(soff)
                .ok_or(BoardWriteError::DspiDma {
                    address: addr,
                    bytes: len,
                })?;
            if mask != 0 {
                addr = base | (addr & mask);
            }
        }
        Ok(source)
    }

    /// `DeliverWrite` holds contiguous bytes. Refuse a non-contiguous RX
    /// descriptor before the peripheral exchanges the frame and advances it.
    fn preflight_dspi2_rx(&mut self, tx_len: Option<usize>) -> Result<(), BoardWriteError> {
        let view = edma::TcdView::new(&mut self.dma.edma_regs, dspi::RX_CHAN);
        let mask = view.dmod_mask();
        let tcd = view.snapshot();
        if tcd.citer == 0 {
            return Ok(());
        }
        let elem = 1usize << ((tcd.attr >> 8) & 7);
        let nbytes = tcd.nbytes as usize;
        let len = (tcd.citer as usize).checked_mul(nbytes);
        let Some(len) = len else {
            return Err(BoardWriteError::DspiDma {
                address: tcd.daddr,
                bytes: usize::MAX,
            });
        };
        if nbytes == 0
            || !nbytes.is_multiple_of(elem)
            || tcd.doff != elem as i16
            || mask != 0
            || len > dspi::FRAME_BYTES * 2
            || tx_len.is_some_and(|tx| tx != len)
        {
            return Err(BoardWriteError::DspiDma {
                address: tcd.daddr,
                bytes: len,
            });
        }
        for offset in 0..len {
            let at = tcd
                .daddr
                .checked_add(offset as u32)
                .ok_or(BoardWriteError::DspiDma {
                    address: tcd.daddr,
                    bytes: len,
                })?;
            if Self::owned_mmio(at) || self.page(at).is_none() {
                return Err(BoardWriteError::DspiDma {
                    address: at,
                    bytes: len,
                });
            }
        }
        Ok(())
    }

    fn write_dspi2_rx(&mut self, addr: u32, data: &[u8]) -> Result<(), BoardWriteError> {
        if data.len() > dspi::FRAME_BYTES * 2 {
            return Err(BoardWriteError::DspiDma {
                address: addr,
                bytes: data.len(),
            });
        }
        for offset in 0..data.len() {
            let at = addr
                .checked_add(offset as u32)
                .ok_or(BoardWriteError::DspiDma {
                    address: addr,
                    bytes: data.len(),
                })?;
            if Self::owned_mmio(at) || self.page(at).is_none() {
                return Err(BoardWriteError::DspiDma {
                    address: at,
                    bytes: data.len(),
                });
            }
        }
        for (offset, &value) in data.iter().enumerate() {
            assert!(self.ram_write(addr + offset as u32, 1, u32::from(value)));
        }
        if !data.is_empty() {
            self.dma_written.push((addr, data.len()));
            self.dma_sources.push("dspi2");
        }
        Ok(())
    }
    /// `ram_read` for an address in SDRAM, where `addr + size` cannot
    /// overflow: the same result with a cheaper in-page test.
    #[inline(always)]
    fn sdram_read(&self, addr: u32, size: u8) -> Option<u32> {
        let o = Self::page_offset(addr);
        if o + usize::from(size) > PAGE_SIZE {
            return self.ram_read(addr, size);
        }
        let p = self.page(addr)?;
        Some(match size {
            1 => p[o] as u32,
            2 => u16::from_be_bytes([p[o], p[o + 1]]) as u32,
            4 => u32::from_be_bytes([p[o], p[o + 1], p[o + 2], p[o + 3]]),
            _ => return None,
        })
    }
    /// `ram_write` for an address in SDRAM (see `sdram_read`).
    #[inline(always)]
    fn sdram_write(&mut self, addr: u32, size: u8, value: u32) -> bool {
        let o = Self::page_offset(addr);
        if o + usize::from(size) > PAGE_SIZE {
            return self.ram_write(addr, size, value);
        }
        let Some(p) = self.page_mut(addr) else {
            return false;
        };
        match size {
            1 => p[o] = value as u8,
            2 => p[o..o + 2].copy_from_slice(&(value as u16).to_be_bytes()),
            4 => p[o..o + 4].copy_from_slice(&value.to_be_bytes()),
            _ => return false,
        };
        true
    }
    #[inline(always)]
    fn read_inner(&mut self, addr: u32, size: u8) -> Result<u32, BusError> {
        if self.sdram_fast
            && addr.wrapping_sub(SDRAM_BASE) < SDRAM_SIZE
            && let Some(value) = self.sdram_read(addr, size)
        {
            // Mapped SDRAM: exactly what the cascade below ends in.
            return Ok(value);
        }
        self.read_inner_slow(addr, size)
    }

    #[inline(never)]
    fn read_inner_slow(&mut self, addr: u32, size: u8) -> Result<u32, BusError> {
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
            // The FlexBus coprocessor port (`periph::dsp::Fifo`) keeps only a
            // poll counter and an echo register: no timer, DMA or interrupt
            // state can change, so a poll loop on it need not leave the
            // fast path (its state still changes exactly as before).
            if !Self::in_dsp_port(addr) {
                self.bump_epoch(addr);
            }
            return Ok(value);
        }
        if let Some(value) = self.esdhc.read(addr, size) {
            self.bump_epoch(addr);
            self.drain_esdhc_host_writes();
            return Ok(value);
        }
        if addr == PPDSDR_C
            && size == 1
            && let Some(gate) = self.sd_gate.as_mut()
        {
            self.mmio_epoch = self.mmio_epoch.wrapping_add(1);
            self.last_epoch_addr = addr;
            let sensed = gate.sense();
            let wrote = self.ram_write(addr, 1, u32::from(sensed));
            debug_assert!(wrote);
            return Ok(u32::from(sensed));
        }
        if !self.ensure_oracle_sdram(addr, size) {
            return Err(Self::bus_error(addr, false));
        }
        self.ram_read(addr, size)
            .ok_or(Self::bus_error(addr, false))
    }
    #[inline(always)]
    fn write_inner(&mut self, addr: u32, size: u8, value: u32) -> Result<(), BoardWriteError> {
        if self.last_error.is_some() {
            self.last_error = None;
        }
        if self.sdram_fast
            && addr.wrapping_sub(SDRAM_BASE) < SDRAM_SIZE
            && self.sdram_write(addr, size, value)
        {
            // Mapped SDRAM: exactly what the cascade below ends in.
            return Ok(());
        }
        self.write_inner_slow(addr, size, value)
    }

    #[inline(never)]
    fn write_inner_slow(&mut self, addr: u32, size: u8, value: u32) -> Result<(), BoardWriteError> {
        if addr == MIDI_UDR && size == 1 {
            self.midi_tx.push(value as u8);
        }
        if Time::owns(addr) {
            self.bump_epoch(addr);
            if let Some(time) = self.time.as_mut()
                && time.write(addr, size, value)
            {
                if addr == ssi::INTFRCH1
                    && size == 4
                    && let Some(ssi) = self.dma.ssi.as_mut()
                    && let Some(current) = time.read(addr, size)
                {
                    // Observe the register's actual post-write value; do not
                    // infer bit transitions from an arbitrary-sized write.
                    ssi.on_intfrch1(current);
                }
                return Ok(());
            }
            return Err(BoardWriteError::Bus(Self::bus_error(addr, true)));
        }
        if DmaLink::owns(addr) {
            // Device interrupt latches share the already checkpointed eDMA
            // register file. Reject crossing accesses before changing anything.
            let end = addr.checked_add(u32::from(size));
            if self.policy == CompletionPolicy::Device
                && addr < edma::INTL + 4
                && end.is_some_and(|end| end > edma::INTH)
            {
                let base = if addr < edma::INTL {
                    edma::INTH
                } else {
                    edma::INTL
                };
                if !matches!(size, 1 | 2 | 4) || addr < base || end.is_none_or(|end| end > base + 4)
                {
                    return Err(BoardWriteError::Bus(Self::bus_error(addr, true)));
                }
                self.bump_epoch(addr);
                let ssi_pending = self.device_ssi50_pending();
                let pending = self.dma.edma_regs.read(addr, size);
                self.dma.edma_regs.write(addr, size, pending & !value);
                if ssi_pending && !self.device_ssi50_pending()
                    && let Some(ssi) = self.dma.ssi.as_mut()
                {
                    ssi.on_cint(ssi::TX_CHAN as u8);
                }
                return Ok(());
            }
            if !Self::in_dsp_port(addr) {
                self.bump_epoch(addr);
            }
            // Device CINT NOP precedes all legacy callbacks (including DSPI's
            // callback, which intentionally retains its Oracle behavior).
            if self.policy == CompletionPolicy::Device
                && addr == edma::CINT
                && size == 1
                && value as u8 & 0x80 != 0
            {
                return Ok(());
            }
            let tx35_citer =
                edma::TcdView::new(&mut self.dma.edma_regs, self.dma.tx35.chan).citer();
            let is_tx35_serq = addr == edma::SERQ
                && (value as u8 & 0x40 != 0 || (value as u8 & 0x3f) as usize == self.dma.tx35.chan)
                && tx35_citer != 0;
            let tx35_source = is_tx35_serq.then(|| self.tx35_source()).transpose()?;
            let is_tx_serq = addr == edma::SERQ
                && size == 1
                && value as u8 & 0x80 == 0
                && self.dma.dspi2.arms_tx(value as u8);
            let dspi_source = if is_tx_serq
                && edma::TcdView::new(&mut self.dma.edma_regs, dspi::TX_CHAN).citer() != 0
            {
                Some(self.dspi2_source()?)
            } else {
                None
            };
            let is_rx_serq = addr == edma::SERQ
                && size == 1
                && value as u8 & 0x80 == 0
                && self.dma.dspi2.arms_rx(value as u8);
            if is_rx_serq || (is_tx_serq && self.dma.dspi2.rx_armed()) {
                let tx_len = is_tx_serq.then(|| {
                    dspi_source.as_ref().map_or(0, |source| {
                        let attr =
                            edma::TcdView::new(&mut self.dma.edma_regs, dspi::TX_CHAN).attr();
                        if attr & 7 == 2 {
                            source.len() / 2
                        } else {
                            source.len()
                        }
                    })
                });
                self.preflight_dspi2_rx(tx_len.or_else(|| self.dma.dspi2.pending_tx_len()))?;
            }
            let (owned, effect, mut host_write) = self.dma.write(addr, size, value);
            debug_assert!(owned);
            // SAER (SERQ bit 6) reaches Python's independent UART35 and
            // DSPI2 hooks. DmaLink reports only one capture effect, so the
            // preflighted TX35 lane must complete independently when DSPI2
            // also claims that write.
            if let Some(source) = tx35_source.as_deref() {
                self.dma.finish_tx35_capture(source);
                self.uart_tx.extend_from_slice(source);
            }
            if effect == SerqEffect::Dspi2Capture {
                self.dma.finish_dspi2_capture(
                    dspi_source
                        .as_deref()
                        .expect("preflighted channel 29 source"),
                );
            }
            if is_tx_serq && host_write.is_none() {
                host_write = self.dma.finish_dspi2_exchange();
            }
            if let Some((dest, bytes)) = host_write {
                self.write_dspi2_rx(dest, &bytes)?;
            }
            if self.policy == CompletionPolicy::Device && addr == edma::CINT && size == 1 {
                let command = value as u8;
                if command & 0x80 == 0 {
                    if command & 0x40 != 0 {
                        self.dma.edma_regs.write(edma::INTH, 4, 0);
                        self.dma.edma_regs.write(edma::INTL, 4, 0);
                    } else {
                        let channel = command & 0x3f;
                        let register = if channel < 32 { edma::INTL } else { edma::INTH };
                        let pending = self.dma.edma_regs.read(register, 4);
                        self.dma
                            .edma_regs
                            .write(register, 4, pending & !(1 << (channel % 32)));
                    }
                }
            }
            self.update_dma59_request(addr, size, value);
            if addr == edma::SERQ
                && size == 1
                && value as u8 & 0xC0 == 0
                && (value as u8 & 0x3F) as usize == DSPI1_RX_CHAN
            {
                self.dspi1_rx_request()?;
            }
            return Ok(());
        }
        if !Esdhc::<Card>::owns(addr) {
            if !self.ensure_oracle_sdram(addr, size) {
                return Err(BoardWriteError::Bus(Self::bus_error(addr, true)));
            }
            let wrote = self
                .ram_write(addr, size, value)
                .then_some(())
                .ok_or(BoardWriteError::Bus(Self::bus_error(addr, true)));
            if wrote.is_ok() && addr == RCM_RCR && size == 1 && value & 0x80 != 0 {
                self.soft_reset = true;
            }
            if wrote.is_ok()
                && addr >= GPIO_BASE
                && addr
                    .checked_add(u32::from(size))
                    .is_some_and(|end| end <= GPIO_BASE + SLOT_SIZE as u32)
                && let Some(gate) = self.sd_gate.as_mut()
            {
                gate.write(addr, size, value);
            }
            return wrote;
        }
        self.bump_epoch(addr);
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
    /// Mapped SDRAM/SRAM on the fast path, with access recording off: the
    /// access cannot fault and touches nothing but the bytes.
    #[inline]
    fn plain_ram(&self, addr: u32, len: u32) -> bool {
        let Some(last) = len.checked_sub(1).and_then(|l| addr.checked_add(l)) else {
            return false;
        };
        self.sdram_fast
            && !self.capture_guest_accesses
            && addr.wrapping_sub(SDRAM_BASE) < SDRAM_SIZE
            && last.wrapping_sub(SDRAM_BASE) < SDRAM_SIZE
            && self.page(addr).is_some()
            && self.page(last).is_some()
    }
    #[inline(always)]
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
    #[inline(always)]
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
    #[inline]
    fn fetch16(&mut self, addr: u32) -> Result<u16, BusError> {
        self.read_inner(addr, 2).map(|value| value as u16)
    }
    #[inline(always)]
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
    #[inline(always)]
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
    #[inline(always)]
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
    #[inline(always)]
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
    use emmc_card::CardIdentity;
    const RAM: u32 = 0x4000_0000;
    const NEXT: u32 = RAM + PAGE_SIZE as u32;
    const DS: u32 = RAM + 0x100;
    const AS: u32 = RAM + 0x104;
    const CS: u32 = RAM + 0x108;
    const ST: u32 = RAM + 0x10c;

    #[test]
    fn bulk_zero_only_changes_a_mapped_sdram_slice_without_events() {
        let mut b = board(CompletionPolicy::Oracle);
        b.write32(RAM + 0x100, 0x1234_5678).unwrap();
        b.write32(RAM + 0x104, 0x9abc_def0).unwrap();
        assert!(b.zero_mapped_sdram(RAM + 0x102, 4));
        assert!(b.ram_matches(RAM + 0x100, &[0x12, 0x34, 0, 0, 0, 0, 0xde, 0xf0]));
        assert!(!b.zero_mapped_sdram(NEXT, 4));
        assert!(b.page(NEXT).is_none());
        assert!(!b.zero_mapped_sdram(NEXT - 4, 8));
        assert!(!b.zero_mapped_sdram(esdhc::BASE, 4));
        assert!(!b.zero_mapped_sdram(u32::MAX - 1, 4));
        assert!(!b.zero_mapped_sdram(RAM, usize::MAX));
        assert!(!b.zero_mapped_sdram(RAM, 0));
        b.set_guest_access_capture(true);
        assert!(!b.zero_mapped_sdram(RAM + 0x100, 4));
        assert!(b.take_guest_accesses().is_empty());
        assert!(b.completion_events().is_empty());
        assert!(b.take_dma_written_ranges().is_empty());
        assert!(b.ram_matches(RAM + 0x100, &[0x12, 0x34, 0, 0]));
    }

    #[test]
    fn backing_ram_comparison_is_bounded_and_side_effect_free() {
        let mut b = board(CompletionPolicy::Oracle);
        b.map_zeroed_ram_page(NEXT).unwrap();
        b.write32(NEXT - 2, 0x1234_5678).unwrap();
        b.set_guest_access_capture(true);
        assert!(b.ram_matches(NEXT - 2, &[0x12, 0x34, 0x56, 0x78]));
        assert!(!b.ram_matches(NEXT - 2, &[0x12, 0x34, 0x56, 0x79]));
        assert!(!b.ram_matches(NEXT + PAGE_SIZE as u32, &[0]));
        assert!(!b.ram_matches(u32::MAX, &[0]));
        assert!(b.ram_matches(u32::MAX, &[]));
        assert!(b.take_guest_accesses().is_empty());
        assert!(b.completion_events().is_empty());
    }

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
            Card::new(CardIdentity::Mlc4G).unwrap(),
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

    #[test]
    fn sd_gate_is_opt_in_and_keeps_gpio_bytes_in_board_ram() {
        let mut b = board(CompletionPolicy::Oracle);
        let gpio_page = GPIO_BASE & !PAGE_MASK;
        b.map_ram_page(gpio_page).unwrap();
        b.write8(PPDSDR_C, 0xa5).unwrap();
        b.write8(periph::gpio::PPDSDR_D, 0x10).unwrap();
        assert_eq!(b.read8(PPDSDR_C).unwrap(), 0xa5);

        b.enable_sd_gate(false).unwrap();
        b.write8(periph::gpio::PPDSDR_D, 0x10).unwrap();
        assert_eq!(b.read8(PPDSDR_C).unwrap(), 0xad);
        b.write8(periph::gpio::PCLRR_D, 0xef).unwrap();
        assert_eq!(b.read8(PPDSDR_C).unwrap(), 0xa5);
    }

    fn snapshot_of(b: &Board) -> Vec<u8> {
        let mut w = periph::snap::Writer::new();
        b.snap_save(&mut w).unwrap();
        w.buf
    }

    #[test]
    fn snapshot_round_trips_ram_gate_timers_and_ssi() {
        let build = || {
            let mut b = board(CompletionPolicy::Oracle);
            b.enable_sd_gate(true).unwrap();
            b.attach_time(Time::with_dtims(
                crate::time::TimerPolicy::Oracle,
                vec![3, 2, 0],
                vec![3, 1],
                132_000_000.0,
            ));
            b.enable_ssi_diagnostic(96_000, 132_000_000);
            b
        };
        let mut a = build();
        a.map_ram_page(0x4000_0000).unwrap();
        a.map_ram_page(0x4010_0000).unwrap();
        a.write32(0x4000_1234, 0xdead_beef).unwrap();
        a.write8(0x4010_ffff, 0x5a).unwrap();
        a.write8(PPDSDR_C, 0x40).unwrap();
        a.dma.dspi2.frames = 7;
        a.dma.ssi.as_mut().unwrap().requests = 11;
        a.time_mut().unwrap().deadline(5);
        let saved = snapshot_of(&a);

        // The target maps one page the snapshot also holds, plus nothing else.
        let mut b = build();
        b.map_ram_page(0x4000_0000).unwrap();
        b.map_ram_page(0x4010_0000).unwrap();
        b.write32(0x4000_0000, 1).unwrap(); // must be overwritten by the load
        b.snap_load(&mut periph::snap::Reader::new(&saved)).unwrap();
        assert_eq!(b.read32(0x4000_1234).unwrap(), 0xdead_beef);
        assert_eq!(b.read32(0x4000_0000).unwrap(), 0);
        assert_eq!(b.read8(0x4010_ffff).unwrap(), 0x5a);
        assert_eq!(b.dma.dspi2.frames, 7);
        assert_eq!(snapshot_of(&b), saved);

        // A board that maps a page the snapshot lacks is refused.
        let mut c = build();
        c.map_ram_page(0x4020_0000).unwrap();
        assert!(c.snap_load(&mut periph::snap::Reader::new(&saved)).is_err());
        // So is one built without the SSI lane.
        let mut d = board(CompletionPolicy::Oracle);
        assert!(d.snap_load(&mut periph::snap::Reader::new(&saved)).is_err());
    }

    #[test]
    fn sd_gate_seeds_checkpoint_gpio_bytes_and_explicit_state() {
        let mut b = board(CompletionPolicy::Oracle);
        let gpio_page = GPIO_BASE & !PAGE_MASK;
        let mut page = vec![0; PAGE_SIZE];
        page[(PPDSDR_C & PAGE_MASK) as usize] = 0x40;
        b.import_ram_page(gpio_page, &page).unwrap();

        b.enable_sd_gate(true).unwrap();
        assert_eq!(b.read8(PPDSDR_C).unwrap(), 0x48);

        let mut exported = vec![0; PAGE_SIZE];
        b.read_ram_page(gpio_page, &mut exported).unwrap();
        assert_eq!(exported[(PPDSDR_C & PAGE_MASK) as usize], 0x48);
    }

    #[test]
    fn sd_gate_ignores_slot_overflow_accesses_without_panicking() {
        let mut b = board(CompletionPolicy::Oracle);
        b.enable_sd_gate(false).unwrap();
        let overflow = GPIO_BASE + SLOT_SIZE as u32 - 1;
        b.write32(overflow, 0x1234_5678).unwrap();
        assert_eq!(b.read32(overflow).unwrap(), 0x1234_5678);
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
            Card::new(CardIdentity::Small).unwrap(),
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
