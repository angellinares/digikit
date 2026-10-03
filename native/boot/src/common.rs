//! Internal Oracle compatibility helpers shared by the CLI and future runtime.
//!
//! These helpers are diagnostic services, not a public hardware interface.

use std::collections::{BTreeMap, VecDeque};

use coldfire::{Bus, BusError, Cpu, InterruptPolicy};
use machine::{Board, SemaphoreAddresses, Time};
use serde::Serialize;
use sha2::{Digest, Sha256};

pub(crate) const MAIN_LOAD: u32 = 0x4000_0400;
pub(crate) const ENTRY: u32 = 0x4000_04e8;
pub(crate) const STACK: u32 = 0x4080_0000;
pub(crate) const PAGE: usize = 1024 * 1024;
pub(crate) const UART8_USR: u32 = 0xec07_0004;
pub(crate) const UART8_UDR: u32 = 0xec07_000c;
pub(crate) const TX35_VECTOR: u8 = 155;
pub(crate) const INTC1_BASE: u32 = 0xfc04_c000;
pub(crate) const DSPI0_SR: u32 = 0xfc05_c02c;
pub(crate) const DSPI2_SR: u32 = 0xec03_802c;
pub(crate) const ESDHC_CMDARG: u32 = 0xfc0c_c008;
pub(crate) const ESDHC_XFERTYP: u32 = 0xfc0c_c00c;
pub(crate) const GPIO_PPDSDR_C: u32 = 0xec09_401a;
pub(crate) const GPIO_PPDSDR_D: u32 = 0xec09_401b;
pub(crate) const FLASH_SLOT: usize = 0x80_000;
pub(crate) const FLASH_SIZE: usize = 0x100_0000;
pub(crate) const FLASH_READ_SIG: &[u8] = &[
    0x4f, 0xef, 0xff, 0xf4, 0x48, 0xd7, 0x04, 0x0c, 0x24, 0x2f, 0x00, 0x10, 0x26, 0x2f, 0x00, 0x14,
    0x24, 0x6f, 0x00, 0x18, 0x4e, 0xba, 0xf7, 0xd6,
];
pub(crate) const MAINLOOP_SIG: &[u8] = &[
    0x48, 0x79, 0x40, 0x94, 0xef, 0x3c, 0x4e, 0xb9, 0x40, 0x00, 0x19, 0x28, 0x58, 0x8f, 0x72, 0x28,
    0x24, 0x40, 0x71, 0x92, 0xb2, 0x80, 0x65, 0xe8,
];
pub(crate) const JOB_PUMP_SIG: &[u8] = &[
    0x4f, 0xef, 0xff, 0xcc, 0x48, 0xd7, 0x7c, 0x3c, 0x24, 0x6f, 0x00, 0x38, 0x24, 0x0f, 0x2a, 0x0a,
    0x26, 0x0a, 0x06, 0x85, 0x00, 0x00, 0x00, 0x14,
];
pub(crate) const PANEL_DIFF_SIG: &[u8] = &[
    0x4f, 0xef, 0xff, 0xd8, 0x48, 0xd7, 0x1c, 0x7c, 0x24, 0x79, 0x40, 0x29, 0xf6, 0x50, 0x42, 0x83,
];
pub(crate) const PANEL_BYTES: usize = 128 * 8;
pub(crate) const SD_BRINGUP_SIG: &[u8] = &[
    0x4f, 0xef, 0xff, 0xe8, 0x48, 0xd7, 0x0c, 0x3c, 0x42, 0xa7, 0x76, 0xf3, 0x4e, 0xb9, 0x40, 0x12,
    0x6c, 0x24, 0x48, 0x79, 0x47, 0xdc, 0xaa, 0xfc, 0x78, 0x04,
];

#[cfg(test)]
pub(crate) fn panel_pixel(raw: &[u8], x: usize, y: usize) -> bool {
    x < 128
        && y < 64
        && raw
            .get((7 - y / 8) + 8 * x)
            .is_some_and(|byte| byte & (1 << (y % 8)) != 0)
}

pub(crate) fn unique(image: &[u8], sig: &[u8], mask: &[bool]) -> Result<u32, String> {
    if sig.len() != mask.len() {
        return Err("invalid resolver mask".into());
    }
    let hits: Vec<_> = image
        .windows(sig.len())
        .enumerate()
        .filter(|(_, b)| b.iter().enumerate().all(|(i, x)| mask[i] || *x == sig[i]))
        .map(|(i, _)| MAIN_LOAD + i as u32)
        .collect();
    if hits.len() == 1 {
        Ok(hits[0])
    } else {
        Err(format!("signature matched {} locations", hits.len()))
    }
}

pub(crate) fn hex(bytes: &str) -> Vec<u8> {
    (0..bytes.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&bytes[i..i + 2], 16).expect("literal hex"))
        .collect()
}

pub(crate) fn data_mask(sig: &[u8], wild: Option<usize>) -> Vec<bool> {
    let mut mask = vec![false; sig.len()];
    let mut at = 0;
    while at + 4 <= sig.len() {
        let value = u32::from_be_bytes(sig[at..at + 4].try_into().unwrap());
        if (MAIN_LOAD..0x4800_0000).contains(&value) {
            mask[at..at + 4].fill(true);
            at += 4;
        } else {
            at += 1;
        }
    }
    if let Some(i) = wild {
        mask[i] = true;
    }
    mask
}

pub(crate) fn resolve_sd_semaphores(image: &[u8]) -> Result<(u32, SemaphoreAddresses), String> {
    let bringup = unique(image, SD_BRINGUP_SIG, &data_mask(SD_BRINGUP_SIG, None))?;
    let offset = usize::try_from(bringup - MAIN_LOAD).map_err(|_| "sd bringup below MAIN")?;
    let operand = image
        .get(offset + 28..offset + 32)
        .ok_or("sd bringup operand outside MAIN")?;
    let base = u32::from_be_bytes(operand.try_into().map_err(|_| "sd bringup operand width")?);
    if !(0x4000_0000..0x4800_0000).contains(&base) || base & 3 != 0 {
        return Err(format!("sd bringup semaphore base invalid: {base:#010x}"));
    }
    let add = |delta| {
        base.checked_add(delta)
            .ok_or("sd semaphore offset overflow".to_owned())
    };
    Ok((
        base,
        SemaphoreAddresses {
            status: Some(add(0x30)?),
            dma_sem: Some(add(0x3c)?),
            data_sem: Some(add(0x44)?),
            cmd_sem: Some(add(0x4c)?),
        },
    ))
}
pub(crate) fn resolve_fs_worker(image: &[u8]) -> Result<(u32, u32, u32, u32), String> {
    let entry_sig = hex("4e56ffb048d73cfc246e00084ebaffa64a00660c71ee000ee0884a00");
    let mut entry_mask = vec![false; entry_sig.len()];
    entry_mask[14..16].fill(true);
    let entry = unique(image, &entry_sig, &entry_mask)?;
    let completion_sig = hex("7001b58013c000000000700113c0000000001002600a");
    let mut completion_mask = vec![false; completion_sig.len()];
    completion_mask[6..10].fill(true);
    completion_mask[14..18].fill(true);
    let completion = unique(image, &completion_sig, &completion_mask)?;
    let off = (completion - MAIN_LOAD) as usize;
    let success = u32::from_be_bytes(image[off + 6..off + 10].try_into().unwrap());
    let done = u32::from_be_bytes(image[off + 14..off + 18].try_into().unwrap());
    if done
        != success
            .checked_add(1)
            .ok_or("fs completion address overflow")?
        || !(0x4000_0000..0x4800_0000).contains(&success)
        || completion <= entry
        || completion - entry > 4096
    {
        return Err("fs completion byte operands invalid".into());
    }
    Ok((entry, completion, success, done))
}

pub(crate) fn digest(bytes: &[u8]) -> String {
    let mut sha = Sha256::new();
    sha.update(bytes);
    sha.finalize().iter().map(|b| format!("{b:02x}")).collect()
}
pub(crate) fn map_image(board: &mut Board, image: &[u8]) {
    let first = MAIN_LOAD & !((PAGE as u32) - 1);
    let last = (MAIN_LOAD + image.len() as u32 - 1) & !((PAGE as u32) - 1);
    for page in (first..=last).step_by(PAGE) {
        board.map_zeroed_ram_page(page).expect("map MAIN_OS RAM");
    }
    for (off, byte) in image.iter().copied().enumerate() {
        board
            .write_guest(MAIN_LOAD + off as u32, 1, u32::from(byte))
            .expect("stage MAIN_OS byte");
    }
}

pub(crate) fn unique_signature(
    image: &[u8],
    signature: &[u8],
    wildcard: Option<usize>,
) -> Option<u32> {
    let matches: Vec<_> = image
        .windows(signature.len())
        .enumerate()
        .filter(|(_, bytes)| {
            bytes
                .iter()
                .enumerate()
                .all(|(i, byte)| wildcard == Some(i) || *byte == signature[i])
        })
        .map(|(offset, _)| MAIN_LOAD + offset as u32)
        .collect();
    if matches.len() == 1 {
        Some(matches[0])
    } else {
        None
    }
}

// emu.symbols.Sig(..., hi=DATA_HI): mask only abs32 operands already present
// in the signature's [MAIN_LOAD, DATA_HI) window, plus an explicit wildcard.
pub(crate) fn unique_signature_data(
    image: &[u8],
    signature: &[u8],
    wildcard: Option<usize>,
) -> Option<u32> {
    let mut masked = vec![false; signature.len()];
    let mut at = 0;
    while at + 4 <= signature.len() {
        let value = u32::from_be_bytes(signature[at..at + 4].try_into().expect("abs32"));
        if (MAIN_LOAD..0x4800_0000).contains(&value) {
            masked[at..at + 4].fill(true);
            at += 4;
        } else {
            at += 1;
        }
    }
    if let Some(index) = wildcard {
        masked[index] = true;
    }
    let matches: Vec<_> = image
        .windows(signature.len())
        .enumerate()
        .filter(|(_, bytes)| {
            bytes
                .iter()
                .enumerate()
                .all(|(i, byte)| masked[i] || *byte == signature[i])
        })
        .map(|(offset, _)| MAIN_LOAD + offset as u32)
        .collect();
    if matches.len() == 1 {
        Some(matches[0])
    } else {
        None
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Access {
    pub(crate) kind: &'static str,
    pub(crate) addr: u32,
    pub(crate) size: u8,
    pub(crate) value: Option<u32>,
    pub(crate) fault: Option<(u32, bool)>,
}

pub(crate) fn operand_pair(image: &[u8], base: u32) -> Result<(u32, u32), String> {
    let offset = base
        .checked_sub(MAIN_LOAD)
        .ok_or("panel diff before image")? as usize;
    let window = image
        .get(offset..offset + 0x120)
        .ok_or("panel diff operand window outside image")?;
    let mut found = Vec::new();
    let mut at = 0;
    while at + 4 <= window.len() && found.len() < 2 {
        let value = u32::from_be_bytes(window[at..at + 4].try_into().expect("four bytes"));
        if (0x4020_0000..0x4040_0000).contains(&value) {
            if !found.contains(&value) {
                found.push(value);
            }
            at += 4;
        } else {
            at += 1;
        }
    }
    if found.len() == 2 {
        Ok((found[0], found[1]))
    } else {
        Err("panel diff did not contain two distinct framebuffer globals".into())
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Frame {
    pub(crate) owner_tcb: u32,
    #[allow(dead_code, reason = "frame address is reported by the diagnostic CLI")]
    pub(crate) ptr: u32,
    pub(crate) icount: u64,
    #[allow(dead_code, reason = "frame hash is reported by the diagnostic CLI")]
    pub(crate) hash: String,
    pub(crate) lit_bytes: usize,
    pub(crate) raw: Vec<u8>,
}
#[derive(Clone, Debug)]
pub(crate) struct PendingFrame {
    pub(crate) owner_tcb: u32,
    pub(crate) return_pc: u32,
    pub(crate) expected_a7: u32,
    pub(crate) frame: Frame,
}
#[derive(Default)]
pub(crate) struct FrameTracker {
    pub(crate) pending: VecDeque<PendingFrame>,
    pub(crate) completed: BTreeMap<u32, VecDeque<Frame>>,
    pub(crate) pending_dropped: u64,
    pub(crate) completed_dropped: u64,
}
impl FrameTracker {
    pub(crate) fn capture(
        &mut self,
        board: &mut Board,
        current_tcb_addr: u32,
        front_global: u32,
        icount: u64,
        a7: u32,
    ) -> Result<(), String> {
        if !board.can_write_ram_range(current_tcb_addr, 4)
            || !board.can_write_ram_range(front_global, 4)
        {
            return Err("PanelPointerGlobalUnmapped".into());
        }
        let owner_tcb = board
            .read32(current_tcb_addr)
            .map_err(|_| "CurrentTcbUnreadable")?;
        let ptr = board
            .read32(front_global)
            .map_err(|_| "PanelPointerUnreadable")?;
        if !board.can_write_ram_range(ptr, PANEL_BYTES) {
            return Err(format!("PanelBufferUnmapped({ptr:#010x})"));
        }
        let mut raw = Vec::with_capacity(PANEL_BYTES);
        for offset in 0..PANEL_BYTES {
            raw.push(
                board
                    .read8(ptr + offset as u32)
                    .map_err(|_| "PanelBufferUnreadable")?,
            );
        }
        let frame = Frame {
            owner_tcb,
            ptr,
            icount,
            hash: digest(&raw),
            lit_bytes: raw.iter().filter(|&&byte| byte != 0).count(),
            raw,
        };
        let return_pc = board.read32(a7).map_err(|_| "PanelReturnUnreadable")?;
        if self.pending.len() == 16 {
            self.pending.pop_front();
            self.pending_dropped += 1;
        }
        self.pending.push_back(PendingFrame {
            owner_tcb,
            return_pc,
            expected_a7: a7.wrapping_add(4),
            frame,
        });
        Ok(())
    }

    pub(crate) fn complete_at_return(
        &mut self,
        board: &mut Board,
        current_tcb_addr: u32,
        pc: u32,
        a7: u32,
    ) {
        // Most guest instructions cannot complete a pending display call.
        // Check its return boundary before reading the current task in RAM.
        if !self
            .pending
            .iter()
            .any(|pending| pending.return_pc == pc && pending.expected_a7 == a7)
        {
            return;
        }
        let Ok(owner_tcb) = board.read32(current_tcb_addr) else {
            return;
        };
        let Some(index) = self.pending.iter().position(|pending| {
            pending.owner_tcb == owner_tcb && pending.return_pc == pc && pending.expected_a7 == a7
        }) else {
            return;
        };
        let frame = self.pending.remove(index).expect("pending index").frame;
        if !self.completed.contains_key(&owner_tcb) && self.completed.len() == 16 {
            self.completed_dropped += 1;
            return;
        }
        let frames = self.completed.entry(owner_tcb).or_default();
        if frames.len() == 8 {
            frames.pop_front();
            self.completed_dropped += 1;
        }
        frames.push_back(frame);
    }

    pub(crate) fn latest_for(&self, owner_tcb: u32) -> Option<&Frame> {
        self.completed.get(&owner_tcb).and_then(VecDeque::back)
    }
}

pub(crate) fn is_ready(
    intro: u64,
    mainloop: u64,
    jobs: u64,
    dtim3: u64,
    mainloop_tcb: u32,
    frame: Option<&Frame>,
) -> bool {
    intro > 0
        && mainloop > 0
        && jobs > 0
        && dtim3 > 0
        && mainloop_tcb != 0
        && frame.is_some_and(|frame| frame.owner_tcb == mainloop_tcb && frame.lit_bytes > 0)
}
pub(crate) fn readiness_contract_ready(
    contract: device_profile::ReadinessContract,
    starts: u64,
    completions: u64,
    last_complete: Option<u64>,
    frame: Option<&Frame>,
    success: Option<bool>,
) -> bool {
    match contract {
        device_profile::ReadinessContract::MainPanelV1 => true,
        device_profile::ReadinessContract::MainPanelFsCheckV1 => {
            starts > 0
                && starts == completions
                && completions > 0
                && success == Some(true)
                && last_complete.is_some_and(|done| frame.is_some_and(|frame| frame.icount >= done))
        }
    }
}

pub(crate) fn record_deliveries(
    raised: &[(u16, u8)],
    ring: &mut Vec<(u16, u8)>,
    counts: &mut [u64; 256],
    dropped: &mut u64,
) -> u64 {
    for &(vector, level) in raised {
        counts[usize::from(vector)] += 1;
        if ring.len() < 512 {
            ring.push((vector, level));
        } else {
            *dropped += 1;
        }
    }
    raised.len() as u64
}
pub(crate) trait TraceValue {
    fn trace_value(&self) -> Option<u32>;
}
impl TraceValue for u8 {
    fn trace_value(&self) -> Option<u32> {
        Some(u32::from(*self))
    }
}
impl TraceValue for u16 {
    fn trace_value(&self) -> Option<u32> {
        Some(u32::from(*self))
    }
}
impl TraceValue for u32 {
    fn trace_value(&self) -> Option<u32> {
        Some(*self)
    }
}
impl TraceValue for () {
    fn trace_value(&self) -> Option<u32> {
        None
    }
}
#[derive(Debug)]
#[allow(
    dead_code,
    reason = "retained telemetry is printed by the diagnostic CLI"
)]
pub(crate) struct UnknownTouch {
    pub(crate) pc: u32,
    pub(crate) kind: &'static str,
    pub(crate) addr: u32,
    pub(crate) size: u8,
    pub(crate) value: u32,
}
#[derive(Debug)]
#[allow(
    dead_code,
    reason = "retained telemetry is printed by the diagnostic CLI"
)]
pub(crate) struct FlashRead {
    pub(crate) offset: u32,
    pub(crate) length: u32,
    pub(crate) dest: u32,
}

pub(crate) struct LoggingBus<const TRACE: bool = true> {
    pub(crate) board: Board,
    pub(crate) accesses: Vec<Access>,
    pub(crate) access_dropped: u64,
    pub(crate) ppmcr_contract: bool,
    pub(crate) zero_page_mmio: bool,
    pub(crate) zero_page_limit: usize,
    pub(crate) current_pc: u32,
    pub(crate) current_icount: u64,
    pub(crate) unknown_touches: BTreeMap<u32, UnknownTouch>,
    pub(crate) timer_accesses: Vec<(u32, Access)>,
    pub(crate) timer_writes: Vec<(u32, u32, u8, u32)>,
    pub(crate) uart8_tx: Vec<(u32, u8, u32)>,
    pub(crate) dspi2_status_reads: Vec<(u32, u32)>,
    pub(crate) dspi2_dma_writes: Vec<(u32, u32, u8, u32)>,
    pub(crate) cmdarg_writes: u64,
    pub(crate) xfertyp_writes: u64,
    pub(crate) gpio_reads: u64,
    pub(crate) gpio_writes: u64,
    pub(crate) last_cmdarg: u32,
    pub(crate) command_trace: Vec<(u64, u32, u32, u32)>,
    pub(crate) gpio_trace: Vec<(u64, u32, &'static str, u32)>,
}
impl<const TRACE: bool> LoggingBus<TRACE> {
    pub(crate) fn new(
        board: Board,
        ppmcr_contract: bool,
        zero_page_mmio: bool,
        zero_page_limit: usize,
        current_pc: u32,
    ) -> Self {
        Self {
            board,
            accesses: Vec::new(),
            access_dropped: 0,
            ppmcr_contract,
            zero_page_mmio,
            zero_page_limit,
            current_pc,
            current_icount: 0,
            unknown_touches: BTreeMap::new(),
            timer_accesses: Vec::new(),
            timer_writes: Vec::new(),
            uart8_tx: Vec::new(),
            dspi2_status_reads: Vec::new(),
            dspi2_dma_writes: Vec::new(),
            cmdarg_writes: 0,
            xfertyp_writes: 0,
            gpio_reads: 0,
            gpio_writes: 0,
            last_cmdarg: 0,
            command_trace: Vec::new(),
            gpio_trace: Vec::new(),
        }
    }

    pub(crate) fn clear(&mut self) {
        if TRACE {
            self.accesses.clear();
        }
    }
    #[inline]
    fn log<T: TraceValue>(
        &mut self,
        kind: &'static str,
        addr: u32,
        size: u8,
        result: &Result<T, BusError>,
    ) {
        if !TRACE {
            return;
        }
        let (value, fault) = match result {
            Ok(v) => (v.trace_value(), None),
            Err(e) => (None, Some((e.addr, e.write))),
        };
        let access = Access {
            kind,
            addr,
            size,
            value,
            fault,
        };
        if self.accesses.len() < 256 {
            self.accesses.push(access.clone());
        } else {
            self.access_dropped += 1;
        }
        if Time::owns(addr) && self.timer_accesses.len() < 64 {
            self.timer_accesses.push((self.current_pc, access));
        }
        if matches!(addr, GPIO_PPDSDR_C | GPIO_PPDSDR_D) {
            if kind.starts_with("read") {
                self.gpio_reads += 1;
            } else if kind.contains("write") {
                self.gpio_writes += 1;
            }
            if self.gpio_trace.len() == 64 {
                self.gpio_trace.remove(0);
            }
            self.gpio_trace
                .push((self.current_icount, addr, kind, value.unwrap_or(0)));
        }
    }
    #[inline]
    fn log_write<T: TraceValue>(
        &mut self,
        kind: &'static str,
        addr: u32,
        size: u8,
        supplied: u32,
        result: &Result<T, BusError>,
    ) {
        if !TRACE {
            return;
        }
        self.log(kind, addr, size, result);
        if let Some(access) = self.accesses.last_mut() {
            if access.addr == addr && access.kind == kind {
                access.value = Some(supplied);
            }
        }
    }
    #[inline]
    fn zero_page(
        &mut self,
        kind: &'static str,
        addr: u32,
        size: u8,
        value: u32,
        failed: bool,
    ) -> bool {
        // emu.harness.Machine._fault maps a zeroed page for any unmapped guest
        // access.  This is a diagnostic compatibility policy only: it never
        // supplies a device value. Owned-device errors still fail their retry.
        if !failed || !self.zero_page_mmio {
            return false;
        }
        self.zero_page_map(kind, addr, size, value)
    }
    #[cold]
    #[inline(never)]
    fn zero_page_map(&mut self, kind: &'static str, addr: u32, size: u8, value: u32) -> bool {
        let page = addr & !0x000f_ffff;
        if !self.unknown_touches.contains_key(&page)
            && self.unknown_touches.len() == self.zero_page_limit
        {
            return false;
        }
        if self.board.map_zeroed_ram_page(page).is_err() {
            return false;
        }
        self.unknown_touches.entry(page).or_insert(UnknownTouch {
            pc: self.current_pc,
            kind,
            addr,
            size,
            value,
        });
        true
    }
    #[inline]
    fn note_timer_write(&mut self, addr: u32, size: u8, value: u32) {
        if TRACE && Time::owns(addr) && self.timer_writes.len() < 64 {
            self.timer_writes.push((self.current_pc, addr, size, value));
        }
    }
    #[inline]
    fn note_uart8_write(&mut self, addr: u32, size: u8, value: u32) {
        if TRACE && addr == UART8_UDR && self.uart8_tx.len() < 256 {
            self.uart8_tx.push((self.current_pc, size, value));
        }
    }
    #[inline]
    fn note_dspi2_dma_write(&mut self, addr: u32, size: u8, value: u32) {
        if TRACE && (0xfc04_4000..0xfc04_6000).contains(&addr) && self.dspi2_dma_writes.len() < 64 {
            self.dspi2_dma_writes
                .push((self.current_pc, addr, size, value));
        }
    }
    #[inline]
    fn note_esdhc_write(&mut self, addr: u32, value: u32) {
        if addr == ESDHC_CMDARG {
            self.cmdarg_writes += 1;
            self.last_cmdarg = value;
        }
        if addr == ESDHC_XFERTYP {
            self.xfertyp_writes += 1;
            if !TRACE {
                return;
            }
            if self.command_trace.len() == 64 {
                self.command_trace.remove(0);
            }
            self.command_trace.push((
                self.current_icount,
                self.current_pc,
                self.last_cmdarg,
                value,
            ));
        }
    }
}
impl<const TRACE: bool> Bus for LoggingBus<TRACE> {
    /// A traced bus records every access per instruction: never plain.
    #[inline]
    fn plain_ram(&self, addr: u32, len: u32) -> bool {
        !TRACE && self.board.plain_ram(addr, len)
    }
    #[inline(always)]
    fn read8(&mut self, addr: u32) -> Result<u8, BusError> {
        let r = self.board.read8(addr);
        if !TRACE && (r.is_ok() || !self.zero_page_mmio) {
            // zero_page() is false and log() a no-op: the board's result.
            return r;
        }
        self.read8_tail(addr, r)
    }
    #[inline(always)]
    fn read16(&mut self, addr: u32) -> Result<u16, BusError> {
        let r = self.board.read16(addr);
        if !TRACE && (r.is_ok() || !self.zero_page_mmio) {
            // zero_page() is false and log() a no-op: the board's result.
            return r;
        }
        self.read16_tail(addr, r)
    }
    #[inline(always)]
    fn read32(&mut self, addr: u32) -> Result<u32, BusError> {
        let r = self.board.read32(addr);
        if !TRACE && (r.is_ok() || !self.zero_page_mmio) {
            // zero_page() is false and log() a no-op: the board's result.
            return r;
        }
        self.read32_tail(addr, r)
    }
    #[inline]
    fn fetch16(&mut self, addr: u32) -> Result<u16, BusError> {
        let r = self.board.fetch16(addr);
        self.log("fetch", addr, 2, &r);
        r
    }
    #[inline(always)]
    fn write8(&mut self, addr: u32, value: u8) -> Result<(), BusError> {
        // docs/contracts/early-init-v1.json identifies these as write-only
        // SCM PPM clear registers; accepting writes is enough for this narrow
        // probe and intentionally models no other SCM register.
        if self.ppmcr_contract && matches!(addr, 0xfc04_002d | 0xfc04_002f) {
            let r = Ok(());
            self.log_write("ppmcr-contract-write", addr, 1, u32::from(value), &r);
            return r;
        }
        self.note_timer_write(addr, 1, u32::from(value));
        self.note_uart8_write(addr, 1, u32::from(value));
        self.note_dspi2_dma_write(addr, 1, u32::from(value));
        let r = self.board.write8(addr, value);
        if !TRACE && (r.is_ok() || !self.zero_page_mmio) {
            // zero_page() is false and log() a no-op: the board's result.
            return r;
        }
        self.write8_tail(addr, value, r)
    }
    #[inline(always)]
    fn write16(&mut self, addr: u32, value: u16) -> Result<(), BusError> {
        self.note_timer_write(addr, 2, u32::from(value));
        self.note_uart8_write(addr, 2, u32::from(value));
        self.note_dspi2_dma_write(addr, 2, u32::from(value));
        let r = self.board.write16(addr, value);
        if !TRACE && (r.is_ok() || !self.zero_page_mmio) {
            // zero_page() is false and log() a no-op: the board's result.
            return r;
        }
        self.write16_tail(addr, value, r)
    }
    #[inline(always)]
    fn write32(&mut self, addr: u32, value: u32) -> Result<(), BusError> {
        self.note_timer_write(addr, 4, value);
        self.note_uart8_write(addr, 4, value);
        self.note_dspi2_dma_write(addr, 4, value);
        self.note_esdhc_write(addr, value);
        let r = self.board.write32(addr, value);
        if !TRACE && (r.is_ok() || !self.zero_page_mmio) {
            // zero_page() is false and log() a no-op: the board's result.
            return r;
        }
        self.write32_tail(addr, value, r)
    }
}
impl<const TRACE: bool> LoggingBus<TRACE> {
    #[cold]
    #[inline(never)]
    fn read8_tail(&mut self, addr: u32, r: Result<u8, BusError>) -> Result<u8, BusError> {
        if self.zero_page("read", addr, 1, 0, r.is_err()) {
            let retry = self.board.read8(addr);
            self.log("zero-page-read-retry", addr, 1, &retry);
            retry
        } else {
            self.log("read", addr, 1, &r);
            r
        }
    }

    #[cold]
    #[inline(never)]
    fn read16_tail(&mut self, addr: u32, r: Result<u16, BusError>) -> Result<u16, BusError> {
        if self.zero_page("read", addr, 2, 0, r.is_err()) {
            let retry = self.board.read16(addr);
            self.log("zero-page-read-retry", addr, 2, &retry);
            retry
        } else {
            self.log("read", addr, 2, &r);
            r
        }
    }

    #[cold]
    #[inline(never)]
    fn read32_tail(&mut self, addr: u32, r: Result<u32, BusError>) -> Result<u32, BusError> {
        if self.zero_page("read", addr, 4, 0, r.is_err()) {
            let retry = self.board.read32(addr);
            if TRACE && addr == DSPI2_SR && self.dspi2_status_reads.len() < 64 {
                if let Ok(value) = retry {
                    self.dspi2_status_reads.push((self.current_pc, value));
                }
            }
            self.log("zero-page-read-retry", addr, 4, &retry);
            retry
        } else {
            if TRACE && addr == DSPI2_SR && self.dspi2_status_reads.len() < 64 {
                if let Ok(value) = r {
                    self.dspi2_status_reads.push((self.current_pc, value));
                }
            }
            self.log("read", addr, 4, &r);
            r
        }
    }

    #[cold]
    #[inline(never)]
    fn write8_tail(
        &mut self,
        addr: u32,
        value: u8,
        r: Result<(), BusError>,
    ) -> Result<(), BusError> {
        if self.zero_page("write", addr, 1, u32::from(value), r.is_err()) {
            let retry = self.board.write8(addr, value);
            self.log_write("zero-page-write-retry", addr, 1, u32::from(value), &retry);
            retry
        } else {
            self.log_write("write", addr, 1, u32::from(value), &r);
            r
        }
    }

    #[cold]
    #[inline(never)]
    fn write16_tail(
        &mut self,
        addr: u32,
        value: u16,
        r: Result<(), BusError>,
    ) -> Result<(), BusError> {
        if self.zero_page("write", addr, 2, u32::from(value), r.is_err()) {
            let retry = self.board.write16(addr, value);
            self.log_write("zero-page-write-retry", addr, 2, u32::from(value), &retry);
            retry
        } else {
            self.log_write("write", addr, 2, u32::from(value), &r);
            r
        }
    }

    #[cold]
    #[inline(never)]
    fn write32_tail(
        &mut self,
        addr: u32,
        value: u32,
        r: Result<(), BusError>,
    ) -> Result<(), BusError> {
        if self.zero_page("write", addr, 4, value, r.is_err()) {
            let retry = self.board.write32(addr, value);
            self.log_write("zero-page-write-retry", addr, 4, value, &retry);
            retry
        } else {
            self.log_write("write", addr, 4, value, &r);
            r
        }
    }
}

// Exact diagnostic adaptation of Machine::step_timed: the caller performs
// the guest step between the deadline arm and this boundary service.
pub(crate) fn service_timers<const TRACE: bool>(
    bus: &mut LoggingBus<TRACE>,
    cpu: &mut Cpu,
    done: u64,
    deliveries: &mut Vec<(u16, u8)>,
    delivery_counts: &mut [u64; 256],
    delivery_dropped: &mut u64,
) -> Result<u64, String> {
    let time = bus.board.time_mut().ok_or("TimerNotAttached")?;
    time.seed_sr(cpu.sr);
    if time.can_skip_service(done) {
        return Ok(0);
    }
    let mut time = bus.board.take_time().ok_or("TimerNotAttached")?;
    let mut delivery_error = None;
    let raised = time
        .service_with(done, |raw_vector, level| {
            let Ok(vector) = u8::try_from(raw_vector) else {
                delivery_error = Some(format!("VectorOutOfRange({raw_vector})"));
                return false;
            };
            match bus
                .board
                .read32(cpu.ctrl.vbr.wrapping_add(4 * u32::from(vector)))
            {
                Ok(handler) if handler != 0 && handler < 0x4800_0000 => {}
                _ => {
                    delivery_error = Some(format!("MissingOracleHandler(vector={vector})"));
                    return false;
                }
            }
            let stack = if cpu.sr & 0x2000 == 0 && cpu.ctrl.cacr & 0x20 != 0 {
                cpu.other_a7
            } else {
                cpu.a[7]
            };
            let frame = (stack & !3).wrapping_sub(8);
            if !bus.board.can_write_ram_range(frame, 4)
                || !bus.board.can_write_ram_range(frame.wrapping_add(4), 4)
            {
                delivery_error = Some(format!("InterruptFrameUnavailable(vector={vector})"));
                return false;
            }
            match cpu.take_interrupt(&mut bus.board, vector, Some(level), InterruptPolicy::Oracle) {
                Ok(true) => true,
                Ok(false) => {
                    delivery_error = Some(format!("MissingOracleHandler(vector={vector})"));
                    false
                }
                Err(stop) => {
                    delivery_error = Some(format!("InterruptDeliveryPoisoned({stop:?})"));
                    false
                }
            }
        })
        .map_err(|error| format!("TimeError({error:?})"))?;
    let writes = time.take_host_writes();
    bus.board.restore_time(time);
    for write in writes {
        bus.board
            .apply_timer_host_write(write.addr, write.byte)
            .map_err(|_| format!("TimerHostWriteUnavailable({:#010x})", write.addr))?;
    }
    if let Some(error) = delivery_error {
        return Err(error);
    }
    Ok(record_deliveries(
        &raised,
        deliveries,
        delivery_counts,
        delivery_dropped,
    ))
}

// Exact Python emu.edma.py completion boundary: channel 35 is offered only
// at its firmware queue-space wait loop. Unlike a generic IRQ scheduler, this
// checks the programmed INTC1 source-27 level/mask and the live CPU IPL.
pub(crate) fn service_tx35_wait<const TRACE: bool>(
    bus: &mut LoggingBus<TRACE>,
    cpu: &mut Cpu,
    wait_pc: u32,
    handler_expected: u32,
) -> Result<bool, String> {
    if bus.board.dma.tx35.pending == 0 || cpu.pc != wait_pc {
        return Ok(false);
    }
    let level = bus
        .board
        .read8(INTC1_BASE + 0x40 + 27)
        .map_err(|_| "Tx35IntcIcrUnavailable".to_string())?
        & 7;
    let imrl = bus
        .board
        .read32(INTC1_BASE + 0x0c)
        .map_err(|_| "Tx35IntcMaskUnavailable".to_string())?;
    if level == 0 || (imrl >> 27) & 1 != 0 || ((cpu.sr >> 8) & 7) >= u16::from(level) {
        return Ok(false);
    }
    let handler = bus
        .board
        .read32(cpu.ctrl.vbr.wrapping_add(4 * u32::from(TX35_VECTOR)))
        .map_err(|_| "Tx35VectorUnavailable".to_string())?;
    if handler != handler_expected {
        return Err(format!("Tx35HandlerMismatch({handler:#010x})"));
    }
    let stack = if cpu.sr & 0x2000 == 0 && cpu.ctrl.cacr & 0x20 != 0 {
        cpu.other_a7
    } else {
        cpu.a[7]
    };
    let frame = (stack & !3).wrapping_sub(8);
    if !bus.board.can_write_ram_range(frame, 4)
        || !bus.board.can_write_ram_range(frame.wrapping_add(4), 4)
    {
        return Err(format!("Tx35InterruptFrameUnavailable({frame:#010x})"));
    }
    match cpu.take_interrupt(
        &mut bus.board,
        TX35_VECTOR,
        Some(level),
        InterruptPolicy::Oracle,
    ) {
        Ok(true) if bus.board.consume_tx35_pending() => Ok(true),
        Ok(true) => Err("Tx35PendingLostBeforeDelivery".to_string()),
        Ok(false) => Err("Tx35HandlerRejected".to_string()),
        Err(stop) => Err(format!("Tx35DeliveryPoisoned({stop:?})")),
    }
}

// Exact emu.longrun.py flash_read ABI: [return, offset, length, destination]
// at A7; copy only an in-range request, return D0=0, pop return address.
pub(crate) fn hle_flash_read<const TRACE: bool>(
    bus: &mut LoggingBus<TRACE>,
    cpu: &mut Cpu,
    flash: &[u8],
    reads: &mut Vec<FlashRead>,
) -> Result<(), String> {
    let sp = cpu.a[7];
    let ret = bus
        .board
        .read32(sp)
        .map_err(|_| format!("FlashReadStack(ret={sp:#010x})"))?;
    let offset = bus
        .board
        .read32(sp.wrapping_add(4))
        .map_err(|_| "FlashReadStack(offset)".to_string())?;
    let length = bus
        .board
        .read32(sp.wrapping_add(8))
        .map_err(|_| "FlashReadStack(length)".to_string())?;
    let dest = bus
        .board
        .read32(sp.wrapping_add(12))
        .map_err(|_| "FlashReadStack(dest)".to_string())?;
    if length != 0 {
        if dest == 0 {
            return Err("FlashReadInvalidDestination(0)".into());
        }
        let end = usize::try_from(offset)
            .ok()
            .and_then(|off| off.checked_add(length as usize))
            .filter(|end| *end <= flash.len())
            .ok_or("FlashReadRange")?;
        let last_addr = dest
            .checked_add(length - 1)
            .ok_or("FlashReadDestinationOverflow")?;
        if !(0x4000_0000..0x8000_0000).contains(&dest)
            || !(0x4000_0000..0x8000_0000).contains(&last_addr)
        {
            return Err(format!("FlashReadInvalidDestination({dest:#010x})"));
        }
        if reads.len() < 128 {
            reads.push(FlashRead {
                offset,
                length,
                dest,
            });
        }
        {
            let start = offset as usize;
            let first = dest & !((PAGE as u32) - 1);
            let last = last_addr & !((PAGE as u32) - 1);
            for page in (first..=last).step_by(PAGE) {
                bus.board
                    .map_zeroed_ram_page(page)
                    .map_err(|_| format!("FlashReadMap({page:#010x})"))?;
            }
            for (at, byte) in flash[start..end].iter().copied().enumerate() {
                bus.board
                    .write_guest(dest.wrapping_add(at as u32), 1, u32::from(byte))
                    .map_err(|_| {
                        format!("FlashReadWrite({:#010x})", dest.wrapping_add(at as u32))
                    })?;
            }
        }
    }
    cpu.d[0] = 0;
    cpu.a[7] = sp.wrapping_add(4);
    cpu.pc = ret;
    Ok(())
}
