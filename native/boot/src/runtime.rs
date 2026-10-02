use std::collections::{BTreeMap, BTreeSet, VecDeque};

use coldfire::{Bus, Cpu, InterruptPolicy};
use dt2_firmware_loader::{decode_syx, parse};
use emmc_card::{Card, DEFAULT_CAPACITY_BLOCKS};
use machine::{Board, CompletionEvent, CompletionPolicy, Time, TimerPolicy};
use plusdrive_format::build_sample_image;
use serde::Serialize;

use crate::capture::Dspi2Capture;
use crate::common::*;
use crate::ram_clear::RamClear;
use crate::softfloat::{ExecutionPolicy, SoftfloatAbi, SoftfloatCounts};
#[cfg(feature = "diagnostic-events")]
use crate::telemetry::EventKind;
use crate::telemetry::{DiagnosticReport, Mark, Position, Recorder};

const CHUNK_MAX: u32 = 250_000;
const CAPTURE_BUS_TRACE: bool = cfg!(feature = "diagnostic-trace");
const ACCELERATE_RAM_CLEAR: bool = !CAPTURE_BUS_TRACE && !cfg!(feature = "reference-ram-clear");
const SET_PIXEL_SIG: &str = "2f032f02206f000c222f0010202f00144a816d4c4a806d48b2a800046c42b0a800086c3c43e8000c761f4c1118002400ea82c680202f001820680010d282e589";
const TCD34_BASE: u32 = 0xfc04_5440;
const TCD34_DADDR: u32 = TCD34_BASE + 0x10;
const RX_VECTOR: u8 = 154;

#[derive(Clone, Debug, Serialize)]
pub struct Status {
    pub device: String,
    pub version: String,
    pub icount: u64,
    pub pc: u32,
    pub ready: bool,
    pub phase: String,
    pub error: Option<String>,
    pub frame_revision: u64,
    pub frame_source: Option<String>,
    pub main_ui_reached: bool,
    pub filesystem_verified: Option<bool>,
    pub input_ready: bool,
    pub input_pending: usize,
    pub input_irqs: u64,
    /// Actual CPU steps, excluding analytic idle and reset-clear batches.
    pub interpreted_instructions: u64,
    pub idle_fast_forwarded_instructions: u64,
    pub ram_clear_fast_forwarded_instructions: u64,
    pub flash_hle_calls: u64,
    pub softfloat_hle_calls: u64,
    pub softfloat: SoftfloatCounts,
    pub softfloat_available: bool,
    pub softfloat_reason: Option<String>,
    pub oracle_ticks: u64,
    pub execution_policy: ExecutionPolicy,
    pub clock_description: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub status: Status,
    pub frame: Option<Vec<u8>>,
}

/// Persistent, bounded Oracle diagnostic state. It has no host I/O and does
/// not claim a hardware or interrupt-device model.
pub struct Emulator {
    main: Vec<u8>,
    cpu: Cpu,
    bus: LoggingBus<CAPTURE_BUS_TRACE>,
    telemetry: Recorder,
    #[cfg(feature = "diagnostic-events")]
    dspi_frames_observed: u64,
    device: String,
    version: String,
    contract: device_profile::ReadinessContract,
    task_create: u32,
    mainloop: u32,
    job_pump: u32,
    panel_diff: u32,
    fb_front: u32,
    flash_read: u32,
    flash: Vec<u8>,
    fs_worker: Option<(u32, u32, u32, u32)>,
    intro_done: u32,
    display_start: u32,
    set_pixel: u32,
    uart_wait: u32,
    uart_handler: u32,
    current_tcb_addr: u32,
    idle_spins: BTreeSet<u32>,
    idle_passes: u64,
    task_create_hits: u64,
    mainloop_hits: u64,
    job_pump_hits: u64,
    intro_done_hits: u64,
    display_start_hits: u64,
    mainloop_tcb: u32,
    fs_starts: u64,
    fs_completions: u64,
    fs_last_complete: Option<u64>,
    fs_success_result: Option<bool>,
    fs_clear_pending: Option<(u32, u32, u32)>,
    fs_active: bool,
    frames: FrameTracker,
    intro_frames: IntroFrameTracker,
    main_frame_latched: bool,
    current_frame: Option<Vec<u8>>,
    frame_source: Option<String>,
    frame_revision: u64,
    emitted_revision: Option<u64>,
    delivery_counts: [u64; 256],
    deliveries: Vec<(u16, u8)>,
    delivery_dropped: u64,
    completion_events: u64,
    completion_kind_counts: [u64; 3],
    dma_ranges: u64,
    dma_bytes: u64,
    uart_bytes: u64,
    error: Option<String>,
    panel: device_profile::PanelProfile,
    held_masks: [u8; 16],
    input_packets: VecDeque<u8>,
    input_irqs: u64,
    rx_pending: bool,
    uart_ring_ptr: u32,
    uart_consume: u32,
    uart_callback: u32,
    uart_rx_isr: u32,
    input_ready: bool,
    input_attempted_in_chunk: bool,
    policy: ExecutionPolicy,
    softfloat: SoftfloatAbi,
    ram_clear: Option<RamClear>,
    interpreted_instructions: u64,
    idle_fast_forwarded_instructions: u64,
    ram_clear_fast_forwarded_instructions: u64,
    flash_hle_calls: u64,
    dspi2_capture: Option<Dspi2Capture>,
}

impl Emulator {
    pub fn new(syx: &[u8], card: Option<Card>) -> Result<Self, String> {
        Self::new_with_policy(syx, card, ExecutionPolicy::Reference)
    }

    /// Opt-in bounded diagnostic source for the recovered SSI/eDMA chain.
    /// It is intentionally not enabled by normal frontend construction.
    /// Record every ColdFire->DSP DSPI2 frame (replies stay zeros). Call
    /// before stepping; `dspi2_capture_bytes` returns the `.dt2cap` file.
    pub fn record_dspi2(&mut self, source_sha256: &str) {
        let capture = Dspi2Capture::new(&self.device, source_sha256);
        capture.set_icount(self.cpu.icount);
        self.bus.board.dma.peer = capture.peer();
        self.dspi2_capture = Some(capture);
    }

    /// Install the DSP side of the DSPI2 link (opt-in coupling, e.g.
    /// `sharc_peer::SharcPeer`). Replaces any recorder.
    pub fn set_dspi2_peer(&mut self, peer: Box<dyn periph::dspi::Peer>) {
        self.bus.board.dma.peer = peer;
    }

    pub fn dspi2_capture_bytes(&self) -> Option<Vec<u8>> {
        self.dspi2_capture.as_ref().map(Dspi2Capture::bytes)
    }

    pub fn enable_ssi_diagnostic(&mut self, request_hz: u64) -> Result<(), String> {
        if request_hz == 0 || request_hz > 132_000_000 {
            return Err("SSI request clock must be between 1 and 132000000 Hz".into());
        }
        if self.oracle_ticks()? != 0 {
            return Err("SSI diagnostic must be enabled before guest execution".into());
        }
        self.bus
            .board
            .enable_ssi_diagnostic(request_hz, 132_000_000)
            .then_some(())
            .ok_or_else(|| "SSI diagnostic is already enabled".into())
    }

    fn service_ssi(&mut self, done: u64) -> Result<bool, String> {
        if self.bus.board.dma.ssi.is_none() {
            return Ok(false);
        }
        let delivered = service_ssi(&mut self.bus, &mut self.cpu, done)?;
        if let Some(delivery) = delivered {
            record_deliveries(
                &[delivery],
                &mut self.deliveries,
                &mut self.delivery_counts,
                &mut self.delivery_dropped,
            );
        }
        Ok(delivered.is_some())
    }

    /// `(requests, rx_major_loops, tx_major_loops, rejected_requests)` for
    /// the opt-in SSI diagnostic lane; `None` means it was not enabled.
    pub fn ssi_diagnostic_counters(&self) -> Option<(u64, u64, u64, u64)> {
        let dma = self.bus.board.dma.ssi.as_ref()?;
        Some((
            dma.requests,
            dma.major_loops_rx,
            dma.major_loops_tx,
            dma.rejected_requests,
        ))
    }

    pub fn new_with_policy(
        syx: &[u8],
        card: Option<Card>,
        policy: ExecutionPolicy,
    ) -> Result<Self, String> {
        let firmware = parse(syx).map_err(|e| format!("parse SYX/ELE3: {e}"))?;
        let decoded = decode_syx(syx).map_err(|e| format!("decode SYX transport: {e}"))?;
        let container_start = decoded
            .windows(4)
            .position(|bytes| bytes == b"ELE3")
            .ok_or("ELE3 container missing")?;
        if container_start != firmware.container_offset {
            return Err("loader container offset mismatch".into());
        }
        let container = &decoded[container_start..];
        let mains: Vec<_> = firmware
            .sections
            .iter()
            .filter(|section| section.id == 3)
            .collect();
        if mains.len() != 1 || mains[0].destination != MAIN_LOAD {
            return Err("require exactly one MAIN_OS section 3 at 0x40000400".into());
        }
        let main = mains[0].bytes.clone();
        let entry_offset = usize::try_from(ENTRY - MAIN_LOAD).map_err(|_| "entry below MAIN")?;
        if main.get(entry_offset..entry_offset + 6) != Some(&[0x41, 0xef, 0, 4, 0x23, 0xd0]) {
            return Err("entry prefix verification failed".into());
        }
        let registry = device_profile::Registry::embedded().map_err(|e| e.to_string())?;
        let syx_sha = digest(syx);
        let (device, profile, _) = registry
            .boot_for_main(&syx_sha, &main)
            .map_err(|e| e.to_string())?;
        let contract = profile
            .readiness_contract
            .ok_or("firmware has no readiness contract")?;
        let panel = device
            .panel
            .clone()
            .ok_or("known firmware has no panel profile")?;
        let task_create = verified_task_create(&main)?;
        let mainloop = unique_signature_data(&main, MAINLOOP_SIG, Some(15))
            .ok_or("mainloop signature ambiguous")?;
        let panel_diff = unique(&main, PANEL_DIFF_SIG, &data_mask(PANEL_DIFF_SIG, None))?;
        let (fb_front, _) = operand_pair(&main, panel_diff)?;
        let job_pump =
            unique_signature(&main, JOB_PUMP_SIG, None).ok_or("job_pump signature ambiguous")?;
        let flash_read = unique(&main, FLASH_READ_SIG, &vec![false; FLASH_READ_SIG.len()])?;
        let fs_worker = match contract {
            device_profile::ReadinessContract::MainPanelFsCheckV1 => {
                Some(resolve_fs_worker(&main)?)
            }
            device_profile::ReadinessContract::MainPanelV1 => None,
        };
        let (_, semaphores) = resolve_sd_semaphores(&main)?;
        let (intro_done, _) = resolve_intro_marks(&main)?;
        let display_start = unique(
            &main,
            &hex("701041f9fc08c000245f13c1fc050050722313c0fc05001d"),
            &data_mask(
                &hex("701041f9fc08c000245f13c1fc050050722313c0fc05001d"),
                None,
            ),
        )?;
        let (uart_wait, uart_handler, uart_ring_ptr, uart_consume, uart_callback, uart_rx_isr) =
            resolve_uart(&main)?;
        let set_pixel = unique(
            &main,
            &hex(SET_PIXEL_SIG),
            &vec![false; SET_PIXEL_SIG.len() / 2],
        )?;
        let (current_tcb_addr, _) = resolve_context_switch(&main)?;

        let card = match card {
            Some(card) => card,
            None => {
                let image = build_sample_image(Vec::new())
                    .map_err(|e| format!("build empty +Drive image: {e:?}"))?
                    .image;
                Card::with_backing(DEFAULT_CAPACITY_BLOCKS, Some(Box::new(image)))
                    .map_err(|e| format!("empty +Drive card identity: {e:?}"))?
            }
        };
        let mut board = Board::new(card, semaphores, CompletionPolicy::Oracle);
        board
            .enable_sd_gate(false)
            .map_err(|e| format!("SdGateEnable({e:?})"))?;
        board.attach_time(Time::with_dtims(
            TimerPolicy::Oracle,
            vec![3, 2, 0],
            vec![3, 1],
            132_000_000.0,
        ));
        for base in [0xfc04_8000, 0xfc04_c000, 0xfc05_0000] {
            board
                .write32(base + 0x08, 0)
                .expect("zero Oracle INTC IMRH");
            board
                .write32(base + 0x0c, 0)
                .expect("zero Oracle INTC IMRL");
        }
        let mut forced = BTreeMap::new();
        forced.insert(UART8_USR, 0x0d00_0000);
        forced.insert(DSPI0_SR, 0x1000_00f0);
        forced.insert(DSPI2_SR, 0x9000_0000);
        board
            .install_forced_mmio(forced)
            .expect("install forced status hooks");
        board.enable_oracle_sdram_faults();
        map_image(&mut board, &main);
        board
            .map_zeroed_ram_page(STACK & !((PAGE as u32) - 1))
            .expect("map stack RAM");
        let flash = flash_image(&main, flash_read, container)?;
        let idle_spins = main
            .windows(2)
            .enumerate()
            .filter(|(offset, bytes)| offset % 2 == 0 && *bytes == [0x60, 0xfe])
            .map(|(offset, _)| MAIN_LOAD + offset as u32)
            .collect();
        let supported_softfloat = matches!(
            (device.short.as_str(), profile.version.as_str()),
            ("dt2", "1.16") | ("dn2", "1.11")
        );
        let softfloat = match policy {
            ExecutionPolicy::Reference => SoftfloatAbi::disabled("reference policy"),
            ExecutionPolicy::SoftfloatAbiV1 => {
                SoftfloatAbi::resolve(&main, supported_softfloat, &board)
            }
        };
        let bus = LoggingBus::new(board, true, true, 160, ENTRY);
        let ram_clear = RamClear::resolve(&main);
        let mut cpu = Cpu::new();
        cpu.pc = ENTRY;
        cpu.sr = 0x2700;
        cpu.a[7] = STACK;
        let mut emulator = Self {
            main,
            cpu,
            bus,
            telemetry: Recorder::default(),
            #[cfg(feature = "diagnostic-events")]
            dspi_frames_observed: 0,
            device: device.short.clone(),
            version: profile.version.clone(),
            contract,
            task_create,
            mainloop,
            job_pump,
            panel_diff,
            fb_front,
            flash_read,
            flash,
            fs_worker,
            intro_done,
            display_start,
            set_pixel,
            uart_wait,
            uart_handler,
            current_tcb_addr,
            idle_spins,
            idle_passes: 0,
            task_create_hits: 0,
            mainloop_hits: 0,
            job_pump_hits: 0,
            intro_done_hits: 0,
            display_start_hits: 0,
            mainloop_tcb: 0,
            fs_starts: 0,
            fs_completions: 0,
            fs_last_complete: None,
            fs_success_result: None,
            fs_clear_pending: None,
            fs_active: false,
            frames: FrameTracker::default(),
            intro_frames: IntroFrameTracker::default(),
            main_frame_latched: false,
            current_frame: None,
            frame_source: None,
            frame_revision: 0,
            emitted_revision: None,
            delivery_counts: [0; 256],
            deliveries: Vec::new(),
            delivery_dropped: 0,
            completion_events: 0,
            completion_kind_counts: [0; 3],
            dma_ranges: 0,
            dma_bytes: 0,
            uart_bytes: 0,
            error: None,
            panel,
            held_masks: [0; 16],
            input_packets: VecDeque::new(),
            input_irqs: 0,
            rx_pending: false,
            uart_ring_ptr,
            uart_consume,
            uart_callback,
            uart_rx_isr,
            input_ready: false,
            input_attempted_in_chunk: false,
            policy,
            softfloat,
            ram_clear,
            interpreted_instructions: 0,
            idle_fast_forwarded_instructions: 0,
            ram_clear_fast_forwarded_instructions: 0,
            flash_hle_calls: 0,
            dspi2_capture: None,
        };
        emulator.mark(Mark::Entry, "before_instruction");
        Ok(emulator)
    }

    pub fn step_chunk(&mut self, budget: u32) -> Snapshot {
        let budget = budget.min(CHUNK_MAX);
        self.input_attempted_in_chunk = false;
        let mut iterations = 0;
        while iterations < budget && self.error.is_none() {
            if ACCELERATE_RAM_CLEAR
                && self
                    .ram_clear
                    .as_ref()
                    .is_some_and(|clear| self.cpu.pc == clear.loop_pc)
            {
                let count = self.advance_ram_clear(budget - iterations);
                if count != 0 {
                    iterations += count;
                    continue;
                }
            }
            let previous_pc = self.cpu.pc;
            self.step_once();
            iterations += 1;
            iterations += self.advance_idle(previous_pc, budget - iterations);
        }
        if budget != 0 {
            self.refresh_frame();
        }
        if self.bus.board.dma.dspi2.frames != 0 {
            self.mark(Mark::DspiExchange, "chunk_end");
        }
        #[cfg(feature = "diagnostic-events")]
        {
            let frames = self.bus.board.dma.dspi2.frames;
            if frames != self.dspi_frames_observed {
                self.event(EventKind::DspiExchange, frames - self.dspi_frames_observed);
                self.dspi_frames_observed = frames;
            }
        }
        self.snapshot()
    }

    pub fn snapshot(&mut self) -> Snapshot {
        let status = self.status();
        let frame = (self.emitted_revision != Some(self.frame_revision))
            .then(|| self.current_frame.clone())
            .flatten();
        if frame.is_some() {
            self.emitted_revision = Some(self.frame_revision);
        }
        Snapshot { status, frame }
    }

    /// Export observations without stepping, reading guest memory, or consuming a frame.
    pub fn diagnostics(&self) -> DiagnosticReport {
        DiagnosticReport {
            schema_version: 1,
            device: self.device.clone(),
            version: self.version.clone(),
            execution_policy: self.policy,
            main_sha256: digest(&self.main),
            at: self.position(),
            milestones: self.telemetry.milestones(),
            interrupt_deliveries: self.delivery_counts.to_vec(),
            uart_tx_bytes: self.uart_bytes,
            dma_written_ranges: self.dma_ranges,
            dma_written_bytes: self.dma_bytes,
            storage_completions: self.completion_events,
            dspi_exchanges: self.bus.board.dma.dspi2.frames,
            dspi_tx_bytes: self.bus.board.dma.dspi2.tx_bytes,
            sharc_execution_connected: false,
            pcm_output_connected: false,
            bus_trace_enabled: CAPTURE_BUS_TRACE,
            ram_clear_acceleration_enabled: ACCELERATE_RAM_CLEAR && self.ram_clear.is_some(),
            fault: self.error.clone(),
            profile: self.telemetry.profile(),
            events: self.telemetry.events(),
        }
    }

    fn position(&self) -> Position {
        Position {
            icount: self.cpu.icount,
            oracle_ticks: self.oracle_ticks().ok(),
            interpreted_instructions: self.interpreted_instructions,
            idle_fast_forwarded_instructions: self.idle_fast_forwarded_instructions,
            ram_clear_fast_forwarded_instructions: self.ram_clear_fast_forwarded_instructions,
            pc: self.cpu.pc,
        }
    }
    fn mark(&mut self, kind: Mark, boundary: &'static str) {
        if !self.telemetry.has(kind) {
            self.telemetry.mark(kind, self.position(), boundary);
        }
    }
    #[cfg(feature = "diagnostic-events")]
    fn event(&mut self, kind: EventKind, value: u64) {
        self.telemetry.event(kind, value, self.position());
    }

    pub fn button(&mut self, code: u8, down: bool) -> Result<(), String> {
        let (channel, bit) = self.panel.button(code).ok_or("unknown panel button code")?;
        let old = self.held_masks[channel as usize];
        let next = if down {
            old | (1 << bit)
        } else {
            old & !(1 << bit)
        };
        if next == old {
            return Ok(());
        }
        self.queue_packet(0x20 | channel, next)?;
        self.held_masks[channel as usize] = next;
        #[cfg(feature = "diagnostic-events")]
        self.event(EventKind::Input, (u64::from(code) << 1) | u64::from(down));
        Ok(())
    }

    pub fn turn(&mut self, encoder: u8, detents: i32) -> Result<(), String> {
        if encoder == 0 || encoder > self.panel.encoders {
            return Err("unknown panel encoder".into());
        }
        let packets = detents.unsigned_abs().div_ceil(128) as usize;
        if self
            .input_packets
            .len()
            .saturating_add(packets.saturating_mul(2))
            > 1024
        {
            return Err("panel input queue full".into());
        }
        #[cfg(feature = "diagnostic-events")]
        self.event(
            EventKind::Encoder,
            (u64::from(encoder) << 32) | u64::from(detents as u32),
        );
        let mut remaining = i64::from(detents);
        while remaining != 0 {
            let part = remaining.clamp(-128, 127) as i8;
            self.queue_packet(0x30 | (encoder - 1), part as u8)?;
            remaining -= i64::from(part);
        }
        Ok(())
    }

    fn status(&mut self) -> Status {
        let main_frame = self.frames.latest_for(self.mainloop_tcb);
        let main_ui_reached = is_ready(
            self.intro_done_hits,
            self.mainloop_hits,
            self.job_pump_hits,
            self.delivery_counts[99],
            self.mainloop_tcb,
            main_frame,
        );
        let ready = main_ui_reached
            && readiness_contract_ready(
                self.contract,
                self.fs_starts,
                self.fs_completions,
                self.fs_last_complete,
                main_frame,
                self.fs_success_result,
            );
        if ready {
            self.mark(Mark::Ready, "snapshot");
        }
        let phase = if self.error.is_some() {
            "fault"
        } else if ready {
            "ready"
        } else if main_ui_reached {
            "main-panel"
        } else {
            "booting"
        };
        Status {
            device: self.device.clone(),
            version: self.version.clone(),
            icount: self.cpu.icount,
            pc: self.cpu.pc,
            ready,
            phase: phase.into(),
            error: self.error.clone(),
            frame_revision: self.frame_revision,
            frame_source: self.frame_source.clone(),
            main_ui_reached,
            filesystem_verified: self.fs_worker.map(|_| self.fs_success_result).flatten(),
            input_ready: self.input_ready,
            input_pending: self.input_packets.len() + usize::from(self.rx_pending),
            input_irqs: self.input_irqs,
            interpreted_instructions: self.interpreted_instructions,
            idle_fast_forwarded_instructions: self.idle_fast_forwarded_instructions,
            ram_clear_fast_forwarded_instructions: self.ram_clear_fast_forwarded_instructions,
            flash_hle_calls: self.flash_hle_calls,
            softfloat_hle_calls: self.softfloat.counts.calls(),
            softfloat: self.softfloat.counts,
            softfloat_available: self.softfloat.enabled,
            softfloat_reason: self.softfloat.reason.clone(),
            oracle_ticks: self.oracle_ticks().unwrap_or(u64::MAX),
            execution_policy: self.policy,
            clock_description: "oracle_ticks = legacy cpu.icount + accepted softfloat ABI calls; atomic calls consume one scheduling tick, not a CPU instruction".into(),
        }
    }

    fn set_error(&mut self, error: impl Into<String>) {
        self.mark(Mark::Fault, "observed_instruction_boundary");
        self.error.get_or_insert_with(|| error.into());
    }

    fn oracle_ticks(&self) -> Result<u64, String> {
        self.cpu
            .icount
            .checked_add(self.softfloat.counts.calls())
            .ok_or_else(|| "oracle tick overflow".into())
    }

    /// Replace complete reset-clear iterations with the same RAM and CPU
    /// effects. Page faults, the final iteration and any active timers retain
    /// the interpreter path. No guest clock ticks are removed.
    fn advance_ram_clear(&mut self, remaining: u32) -> u32 {
        let Some(clear) = self.ram_clear else {
            return 0;
        };
        if !ACCELERATE_RAM_CLEAR
            || self.cpu.pc != clear.loop_pc
            || self.cpu.state != coldfire::RunState::Running
            || self.cpu.sr & 0xf700 != 0x2700
            || self.cpu.last_exception.is_some()
            || self.cpu.last_unimplemented.is_some()
            || self.cpu.d[4..8] != [0; 4]
            || !self.input_packets.is_empty()
            || self.rx_pending
            || self.input_attempted_in_chunk
            || !self.bus.board.ram_matches(clear.entry, &clear.code)
        {
            return 0;
        }
        let count = clear.iterations(self.cpu.a[0], self.cpu.d[1], remaining);
        if count == 0 {
            return 0;
        }
        let Ok(now) = self.oracle_ticks() else {
            return 0;
        };
        let ssi_active = self.bus.board.ssi_deadline(now).is_some();
        let Some(time) = self.bus.board.time_mut() else {
            return 0;
        };
        // This shortcut is intentionally limited to the timer-inactive reset
        // phase. Enabled or pending timers keep every original boundary.
        if time.policy() != TimerPolicy::Oracle
            || time.has_pending_interrupts()
            || time.deadline(now).is_some()
            || ssi_active
        {
            return 0;
        }
        let instructions = count * 4;
        let n = u64::from(instructions);
        let (Some(cpu_count), Some(skipped), Some(done)) = (
            self.cpu.icount.checked_add(n),
            self.ram_clear_fast_forwarded_instructions.checked_add(n),
            now.checked_add(n),
        ) else {
            return 0;
        };
        let addr = self.cpu.a[0];
        let bytes = (count * 16) as usize;
        if !self.bus.board.zero_mapped_sdram(addr, bytes) {
            return 0;
        }
        self.cpu.invalidate_external_write(addr, bytes);
        self.cpu.a[0] += bytes as u32;
        self.cpu.d[1] -= count;
        // The last batched SUBQ is positive without overflow/borrow, and BNE
        // resolves its lazy flags. X/N/Z/V/C are therefore all clear.
        self.cpu.resolve_nzv();
        self.cpu.sr &= !0x1f;
        self.cpu.icount = cpu_count;
        self.ram_clear_fast_forwarded_instructions = skipped;
        self.bus.current_pc = clear.loop_pc + 10;
        self.bus.current_icount = done - 1;
        if let Err(error) = self.service_ssi(done) {
            self.set_error(error);
        }
        match service_timers(
            &mut self.bus,
            &mut self.cpu,
            done,
            &mut self.deliveries,
            &mut self.delivery_counts,
            &mut self.delivery_dropped,
        ) {
            Ok(0) => {}
            Ok(_) => self.set_error("unexpected interrupt during reset RAM-clear batch"),
            Err(error) => self.set_error(error),
        }
        instructions
    }

    /// Evaluate repeated, verified BRA-to-self passes analytically, stopping
    /// BEFORE either a timer deadline or the software rescheduling pass.
    /// The first pass has already run normally, including every observation
    /// and board drain. No guest or host event changes inside this interval.
    fn advance_idle(&mut self, previous_pc: u32, remaining: u32) -> u32 {
        if remaining == 0
            || self.error.is_some()
            || self.cpu.pc != previous_pc
            || !self.idle_spins.contains(&previous_pc)
            || self.cpu.state != coldfire::RunState::Running
            || self.cpu.sr & 0xc000 != 0
            || self.cpu.last_exception.is_some()
            || self.cpu.last_unimplemented.is_some()
            || !self.input_packets.is_empty()
            || self.rx_pending
            || self.input_attempted_in_chunk
            || !self.bus.board.ram_matches(previous_pc, &[0x60, 0xfe])
        {
            return 0;
        }
        let Ok(now) = self.oracle_ticks() else {
            return 0;
        };
        let ssi_deadline = self.bus.board.ssi_deadline(now);
        let Some(time) = self.bus.board.time_mut() else {
            return 0;
        };
        if time.policy() != TimerPolicy::Oracle {
            return 0;
        }
        let timer_deadline = time.deadline(now);
        let deadline = match (timer_deadline, ssi_deadline) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let count = idle_advance_limit(remaining, self.idle_passes, now, deadline);
        if count == 0 {
            return 0;
        }
        let n = u64::from(count);
        // Reserve every counter before mutation; no fabricated interpreted work.
        let (Some(cpu_count), Some(passes), Some(skipped), Some(done)) = (
            self.cpu.icount.checked_add(n),
            self.idle_passes.checked_add(n),
            self.idle_fast_forwarded_instructions.checked_add(n),
            now.checked_add(n),
        ) else {
            self.set_error("idle advance counter overflow");
            return 0;
        };
        self.cpu.icount = cpu_count;
        self.idle_passes = passes;
        self.idle_fast_forwarded_instructions = skipped;
        self.bus.current_icount = done - 1;
        if let Err(error) = self.service_ssi(done) {
            self.set_error(error);
        }
        match service_timers(
            &mut self.bus,
            &mut self.cpu,
            done,
            &mut self.deliveries,
            &mut self.delivery_counts,
            &mut self.delivery_dropped,
        ) {
            Ok(0) => {}
            Ok(_) => self.set_error("unexpected interrupt before idle advance deadline"),
            Err(error) => self.set_error(error),
        }
        count
    }

    fn step_once(&mut self) {
        let pc = self.cpu.pc;
        if let Some(capture) = &self.dspi2_capture {
            capture.set_icount(self.cpu.icount);
        }
        match self.oracle_ticks() {
            Ok(ticks) => self.bus.current_icount = ticks,
            Err(error) => {
                self.set_error(error);
                return;
            }
        }
        let Some(offset) = pc.checked_sub(MAIN_LOAD).map(|value| value as usize) else {
            self.set_error(format!("UnsupportedGuestPc({pc:#010x})"));
            return;
        };
        if self.main.get(offset..offset + 6).is_none() {
            self.set_error(format!("UnsupportedGuestPc({pc:#010x})"));
            return;
        }
        if !self.main_frame_latched {
            if self
                .intro_frames
                .complete_at_return(&mut self.bus.board, pc, self.cpu.a[7])
            {
                self.mark(Mark::IntroRaster, "before_instruction");
            }
            if pc == self.set_pixel {
                self.mark(Mark::IntroPixel, "before_instruction");
                self.intro_frames
                    .observe_pixel(&mut self.bus.board, self.cpu.a[7]);
            }
        }
        self.observe_marks(pc);
        self.frames.complete_at_return(
            &mut self.bus.board,
            self.current_tcb_addr,
            pc,
            self.cpu.a[7],
        );
        if pc == self.panel_diff && self.intro_done_hits > 0 {
            let ticks = match self.oracle_ticks() {
                Ok(ticks) => ticks,
                Err(error) => {
                    self.set_error(error);
                    return;
                }
            };
            if let Err(error) = self.frames.capture(
                &mut self.bus.board,
                self.current_tcb_addr,
                self.fb_front,
                ticks,
                self.cpu.a[7],
            ) {
                self.set_error(error);
                return;
            }
        }
        match service_tx35_wait(
            &mut self.bus,
            &mut self.cpu,
            self.uart_wait,
            self.uart_handler,
        ) {
            Ok(true) => {
                self.drain_board();
                return;
            }
            Ok(false) => {}
            Err(error) => {
                self.set_error(error);
                self.drain_board();
                return;
            }
        }
        match self.service_input() {
            Ok(true) => return,
            Ok(false) => {}
            Err(error) => {
                self.set_error(error);
                return;
            }
        }
        if self.idle_spins.contains(&pc) {
            self.idle_passes += 1;
            if self.idle_passes.is_multiple_of(20_000)
                && let Err(error) =
                    self.cpu
                        .take_interrupt(&mut self.bus.board, 32, None, InterruptPolicy::Oracle)
            {
                self.set_error(format!("IdleYieldPoisoned({error:?})"));
                return;
            }
        }
        self.bus.clear();
        self.bus.current_pc = pc;
        let result = if pc == self.flash_read {
            let result = hle_flash_read(&mut self.bus, &mut self.cpu, &self.flash, &mut Vec::new());
            if result.is_ok() {
                self.cpu.icount += 1;
                self.flash_hle_calls += 1;
            }
            result
        } else if self.softfloat.try_call(
            &mut self.cpu,
            &mut self.bus.board,
            MAIN_LOAD + self.main.len() as u32,
        ) {
            Ok(())
        } else {
            #[cfg(feature = "diagnostic-profile")]
            self.telemetry.sample(
                self.interpreted_instructions,
                pc,
                if self.main_frame_latched {
                    2
                } else if self.intro_frames.latest.is_some() {
                    1
                } else {
                    0
                },
            );
            let before = self.cpu.icount;
            let result = self
                .cpu
                .step(&mut self.bus)
                .map_err(|error| format!("{error:?}"));
            self.interpreted_instructions += self.cpu.icount.saturating_sub(before);
            result
        };
        self.observe_fs_completion(result.is_ok());
        self.drain_board();
        let mut timer_delivered = false;
        if result.is_ok() {
            let done = match self.oracle_ticks() {
                Ok(ticks) => ticks,
                Err(error) => {
                    self.set_error(error);
                    return;
                }
            };
            match self.service_ssi(done) {
                Ok(delivered) => timer_delivered |= delivered,
                Err(error) => self.set_error(error),
            }
            match service_timers(
                &mut self.bus,
                &mut self.cpu,
                done,
                &mut self.deliveries,
                &mut self.delivery_counts,
                &mut self.delivery_dropped,
            ) {
                Ok(count) => {
                    timer_delivered |= count != 0;
                    #[cfg(feature = "diagnostic-events")]
                    if count != 0 {
                        self.event(EventKind::TimerDeliveries, count);
                    }
                }
                Err(error) => self.set_error(error),
            }
        }
        if let Err(error) = result {
            self.set_error(error);
        } else if self
            .cpu
            .last_exception
            .is_some_and(|vector| vector != 32 && !timer_delivered)
        {
            self.set_error(format!(
                "exception vector {}",
                self.cpu.last_exception.unwrap()
            ));
        }
    }

    fn observe_marks(&mut self, pc: u32) {
        if pc == self.task_create {
            self.task_create_hits += 1;
            if self.task_create_hits == 1 {
                self.mark(Mark::TaskCreated, "before_instruction");
            }
        }
        if pc == self.mainloop {
            self.mainloop_hits += 1;
            if self.mainloop_hits == 1 {
                self.mainloop_tcb = self.bus.board.read32(self.current_tcb_addr).unwrap_or(0);
            }
        }
        if pc == self.job_pump {
            self.job_pump_hits += 1;
        }
        if pc == self.intro_done {
            self.intro_done_hits += 1;
            if self.intro_done_hits == 1 {
                self.mark(Mark::IntroDone, "before_instruction");
            }
        }
        if pc == self.display_start {
            self.display_start_hits += 1;
            if self.display_start_hits == 1 {
                self.mark(Mark::DisplayStart, "before_instruction");
            }
        }
        if let Some((entry, completion, success, done)) = self.fs_worker {
            if pc == entry {
                self.fs_starts += 1;
                if self.fs_starts == 1 {
                    self.mark(Mark::FilesystemStart, "before_instruction");
                }
                self.fs_active = true;
                self.fs_last_complete = None;
                self.fs_success_result = None;
            }
            if pc == completion + 12 && self.fs_active {
                self.fs_clear_pending = Some((success, done, completion + 18));
            }
        }
    }

    fn observe_fs_completion(&mut self, step_ok: bool) {
        if let Some((success, done, expected_pc)) = self.fs_clear_pending.take()
            && step_ok
            && self.cpu.pc == expected_pc
            && self.fs_active
            && self.bus.board.read8(done).ok() == Some(1)
        {
            self.fs_success_result = match self.bus.board.read8(success).ok() {
                Some(0) => Some(false),
                Some(1) => Some(true),
                _ => None,
            };
            self.fs_completions += 1;
            self.mark(Mark::FilesystemComplete, "after_instruction");
            self.fs_last_complete = self.oracle_ticks().ok();
            self.fs_active = false;
        }
    }

    fn drain_board(&mut self) {
        let completions = self.bus.board.take_completion_events();
        self.completion_events += completions.len() as u64;
        #[cfg(feature = "diagnostic-events")]
        if !completions.is_empty() {
            self.event(EventKind::StorageCompletion, completions.len() as u64);
        }
        for completion in completions {
            match completion {
                CompletionEvent::Dma59 { .. } => self.completion_kind_counts[0] += 1,
                CompletionEvent::Data { .. } => self.completion_kind_counts[1] += 1,
                CompletionEvent::Command { .. } => self.completion_kind_counts[2] += 1,
            }
        }
        let ranges = self.bus.board.take_dma_written_ranges();
        self.dma_ranges += ranges.len() as u64;
        self.dma_bytes += ranges.iter().map(|(_, bytes)| *bytes as u64).sum::<u64>();
        #[cfg(feature = "diagnostic-events")]
        for &(address, bytes) in &ranges {
            self.event(
                EventKind::DmaWrite,
                (u64::from(address) << 32) | bytes as u64,
            );
        }
        let uart_bytes = self.bus.board.take_uart_tx().len() as u64;
        self.uart_bytes += uart_bytes;
        if uart_bytes != 0 {
            self.mark(Mark::UartTx, "after_instruction");
        }
        #[cfg(feature = "diagnostic-events")]
        if uart_bytes != 0 {
            self.event(EventKind::UartTx, uart_bytes);
        }
    }

    fn refresh_frame(&mut self) {
        if let Some(frame) = self.frames.latest_for(self.mainloop_tcb) {
            if self.main_frame_latched || frame.lit_bytes > 0 {
                self.main_frame_latched = true;
                if self.current_frame.as_ref() != Some(&frame.raw) {
                    self.publish_frame("main", frame.raw.clone());
                }
                return;
            }
        }
        if !self.main_frame_latched
            && let Some(raw) = &self.intro_frames.latest
            && self.current_frame.as_ref() != Some(raw)
        {
            self.publish_frame("intro", raw.clone());
        }
    }

    fn publish_frame(&mut self, source: &str, raw: Vec<u8>) {
        self.mark(
            if source == "intro" {
                Mark::IntroPublished
            } else {
                Mark::MainPublished
            },
            "chunk_end",
        );
        self.current_frame = Some(raw);
        self.frame_source = Some(source.into());
        self.frame_revision = self.frame_revision.wrapping_add(1);
    }

    fn queue_packet(&mut self, header: u8, value: u8) -> Result<(), String> {
        if self.input_packets.len() > 1022 {
            return Err("panel input queue full".into());
        }
        self.input_packets.push_back(header);
        self.input_packets.push_back(value);
        Ok(())
    }

    fn input_state(&mut self) -> Option<(u32, u32, u32)> {
        let base = self.bus.board.read32(self.uart_ring_ptr).ok()?;
        let consumed = self.bus.board.read32(self.uart_consume).ok()?;
        let callback = self.bus.board.read32(self.uart_callback).ok()?;
        let daddr = self.bus.board.read32(TCD34_DADDR).ok()?;
        let saddr = self.bus.board.read32(TCD34_BASE).ok()?;
        let attr = self.bus.board.read16(TCD34_BASE + 4).ok()?;
        let nbytes = self.bus.board.read32(TCD34_BASE + 8).ok()?;
        let vector = self
            .bus
            .board
            .read32(self.cpu.ctrl.vbr.wrapping_add(4 * u32::from(RX_VECTOR)))
            .ok()?;
        if base & 1023 != 0
            || !self.bus.board.can_write_ram_range(base, 1024)
            || consumed >= 1024
            || !base
                .checked_add(1024)
                .is_some_and(|end| (base..end).contains(&daddr))
            || saddr != 0xec07_000c
            || attr != 0x0050
            || nbytes != 1
            || !(MAIN_LOAD..MAIN_LOAD + self.main.len() as u32).contains(&callback)
            || !self.bus.board.can_write_ram_range(callback, 2)
            || !self
                .bus
                .board
                .can_write_ram_range(self.cpu.ctrl.vbr.wrapping_add(4 * u32::from(RX_VECTOR)), 2)
            || vector != self.uart_rx_isr
        {
            return None;
        }
        Some((base, consumed, daddr))
    }

    fn service_input(&mut self) -> Result<bool, String> {
        if !self.rx_pending {
            if self.input_packets.is_empty() || self.input_attempted_in_chunk {
                return Ok(false);
            }
            self.input_attempted_in_chunk = true;
            let Some((base, consumed, mut daddr)) = self.input_state() else {
                return Ok(false);
            };
            self.input_ready = true;
            self.mark(Mark::InputReady, "before_input_delivery");
            let used = ((daddr - base).wrapping_sub(consumed)) & 1023;
            let free = 1023 - used;
            let packets = (free as usize / 2).min(self.input_packets.len() / 2);
            for _ in 0..packets {
                for _ in 0..2 {
                    let byte = self.input_packets.pop_front().expect("whole packet");
                    self.bus
                        .board
                        .write_guest(daddr, 1, u32::from(byte))
                        .map_err(|_| "panel input ring write")?;
                    daddr = base + ((daddr - base + 1) & 1023);
                }
            }
            if packets != 0 {
                self.bus
                    .board
                    .write32(TCD34_DADDR, daddr)
                    .map_err(|_| "panel input DADDR write")?;
                self.rx_pending = true;
            }
        }
        if !self.rx_pending {
            return Ok(false);
        }
        let level = self
            .bus
            .board
            .read8(INTC1_BASE + 0x40 + 26)
            .map_err(|_| "panel input ICR")?
            & 7;
        let imrl = self
            .bus
            .board
            .read32(INTC1_BASE + 0x0c)
            .map_err(|_| "panel input IMRL")?;
        if level == 0 || (imrl >> 26) & 1 != 0 || ((self.cpu.sr >> 8) & 7) >= u16::from(level) {
            return Ok(false);
        }
        let stack = if self.cpu.sr & 0x2000 == 0 && self.cpu.ctrl.cacr & 0x20 != 0 {
            self.cpu.other_a7
        } else {
            self.cpu.a[7]
        };
        let frame = (stack & !3).wrapping_sub(8);
        if !self.bus.board.can_write_ram_range(frame, 8) {
            return Ok(false);
        }
        match self.cpu.take_interrupt(
            &mut self.bus.board,
            RX_VECTOR,
            Some(level),
            InterruptPolicy::Oracle,
        ) {
            Ok(true) => {
                self.input_irqs += 1;
                #[cfg(feature = "diagnostic-events")]
                self.event(EventKind::InputIrq, u64::from(RX_VECTOR));
                self.rx_pending = false;
                Ok(true)
            }
            Ok(false) => Err("panel RX handler rejected".into()),
            Err(error) => Err(format!("panel RX delivery poisoned({error:?})")),
        }
    }
}

/// Offer SSI's guest-created eDMA interrupt requests at a real instruction
/// boundary. Eligibility comes from the guest's INTC mask/ICR and CPU IPL;
/// a declined request stays latched for the next boundary.
fn service_ssi<const TRACE: bool>(
    bus: &mut LoggingBus<TRACE>,
    cpu: &mut Cpu,
    done: u64,
) -> Result<Option<(u16, u8)>, String> {
    for (addr, len) in bus.board.service_ssi(done) {
        cpu.invalidate_external_write(addr, len);
    }
    let mut selected = None;
    for (vector, owed) in [(170u8, 0usize), (191u8, 1usize)] {
        let pending = bus.board.dma.ssi.as_ref().is_some_and(|dma| {
            if owed == 0 {
                dma.vector170_owed()
            } else {
                dma.vector191_owed()
            }
        });
        if !pending {
            continue;
        }
        let source = u32::from(vector) - 128;
        let level = bus
            .board
            .read8(0xfc04_c000 + 0x40 + source)
            .map_err(|_| "SSI ICR unavailable")?
            & 7;
        let imrh = bus
            .board
            .read32(0xfc04_c000 + 0x08)
            .map_err(|_| "SSI IMRH unavailable")?;
        if level == 0 || (imrh >> (source - 32)) & 1 != 0 || ((cpu.sr >> 8) & 7) >= u16::from(level)
        {
            continue;
        }
        let handler = bus
            .board
            .read32(cpu.ctrl.vbr.wrapping_add(4 * u32::from(vector)))
            .map_err(|_| "SSI vector unavailable")?;
        if handler == 0 || handler >= 0x4800_0000 {
            continue;
        }
        let stack = if cpu.sr & 0x2000 == 0 && cpu.ctrl.cacr & 0x20 != 0 {
            cpu.other_a7
        } else {
            cpu.a[7]
        };
        let frame = (stack & !3).wrapping_sub(8);
        if !bus.board.can_write_ram_range(frame, 8) {
            continue;
        }
        if selected
            .is_none_or(|(best_level, best_vector, _)| (level, vector) > (best_level, best_vector))
        {
            selected = Some((level, vector, owed));
        }
    }
    if let Some((level, vector, owed)) = selected {
        match cpu.take_interrupt(&mut bus.board, vector, Some(level), InterruptPolicy::Oracle) {
            Ok(true) => {
                if let Some(dma) = bus.board.dma.ssi.as_mut() {
                    if owed == 0 {
                        dma.mark_vector170_delivered();
                    } else {
                        dma.mark_vector191_delivered();
                    }
                }
                return Ok(Some((u16::from(vector), level)));
            }
            Ok(false) => {}
            Err(stop) => return Err(format!("SSI interrupt delivery poisoned({stop:?})")),
        }
    }
    Ok(None)
}

fn verified_task_create(main: &[u8]) -> Result<u32, String> {
    let task_create = 0x4000_12c8;
    main.get((task_create - MAIN_LOAD) as usize..)
        .is_some_and(|bytes| bytes.starts_with(&[0x20, 0x2f, 0, 0x0c, 0x72, 0xfc, 0xc2, 0xaf]))
        .then_some(task_create)
        .ok_or("task_create prefix verification failed".into())
}

fn resolve_intro_marks(main: &[u8]) -> Result<(u32, u32), String> {
    let done_sig = hex(
        "424048794313120845f94000141a33c0fc08c000701013c0fc05001c4eb94000155c588f4879431312004e92588f60f4",
    );
    let intro_done = unique(main, &done_sig, &data_mask(&done_sig, None))?;
    let off = (intro_done - MAIN_LOAD) as usize;
    let frame_sem = u32::from_be_bytes(
        main[off + 4..off + 8]
            .try_into()
            .map_err(|_| "intro_done frame semaphore operand")?,
    )
    .wrapping_sub(8);
    let isr_sig = hex(
        "4feffff048d7030341f9fc08c00072043010487943131200808130804eb94000148c4cef030300044fef0014",
    );
    let mut mask = data_mask(&isr_sig, None);
    mask[20..24].fill(true);
    let hits: Vec<_> = main
        .windows(isr_sig.len())
        .enumerate()
        .filter(|(_, bytes)| {
            bytes
                .iter()
                .enumerate()
                .all(|(i, byte)| mask[i] || *byte == isr_sig[i])
        })
        .filter(|(i, _)| {
            u32::from_be_bytes(main[*i + 20..*i + 24].try_into().unwrap()) == frame_sem
        })
        .map(|(i, _)| MAIN_LOAD + i as u32)
        .collect();
    if hits.len() == 1 {
        Ok((intro_done, hits[0]))
    } else {
        Err(format!("intro PIT3 ISR matched {} locations", hits.len()))
    }
}

fn resolve_uart(main: &[u8]) -> Result<(u32, u32, u32, u32, u32, u32), String> {
    let wait_sig = hex("24394094cd90d48022794094cd8828394094cd9493c43239fc0454743639fc04");
    let uart_wait = unique(main, &wait_sig, &data_mask(&wait_sig, None))?;
    let init = 0x4000_243e;
    let at = (init - MAIN_LOAD) as usize;
    let prefix = hex("2f02740f41f9ec09404b1210202f000843f9ec07000042b9");
    if main.get(at..at + prefix.len()) != Some(prefix.as_slice())
        || main.get(at + 0x16..at + 0x18) != Some(&[0x42, 0xb9])
        || main
            .get(at + 0x19a..)
            .is_none_or(|bytes| !bytes.starts_with(&[0x20, 0x3c]))
        || main
            .get(at + 0x1b4..)
            .is_none_or(|bytes| !bytes.starts_with(&hex("23c04000026c")))
    {
        return Err("uart8 initializer verification failed".into());
    }
    let handler = u32::from_be_bytes(
        main[at + 0x19c..at + 0x1a0]
            .try_into()
            .map_err(|_| "uart handler operand")?,
    );
    let handler_prefix = hex("46fc27002f012f00702313c0fc04401c");
    if handler < MAIN_LOAD
        || main
            .get((handler - MAIN_LOAD) as usize..)
            .is_none_or(|bytes| !bytes.starts_with(&handler_prefix))
    {
        return Err("UART handler prefix verification failed".into());
    }
    let globals = u32::from_be_bytes(
        main[at + 0x18..at + 0x1c]
            .try_into()
            .map_err(|_| "uart globals operand")?,
    );
    if !(0x4000_0000..0x4800_0000).contains(&globals) {
        return Err("UART globals outside RAM".into());
    }
    let rx_prefix = hex("46fc27004feffff048d70303702213c0fc04401c701a13c0fc04c01c46fc2300");
    let rx_isr = unique(main, &rx_prefix, &vec![false; rx_prefix.len()])?;
    let vector_store = hex("243c40001f1a720323c240000268");
    if main.get(at + 0x136..at + 0x136 + vector_store.len()) != Some(vector_store.as_slice()) {
        return Err("UART RX vector store verification failed".into());
    }
    Ok((
        uart_wait,
        handler,
        globals + 0x10,
        globals + 0x30,
        globals + 0x40,
        rx_isr,
    ))
}

fn resolve_context_switch(main: &[u8]) -> Result<(u32, u32), String> {
    let at = (0x4000_0410 - MAIN_LOAD) as usize;
    if main
        .get(at..)
        .is_none_or(|bytes| !bytes.starts_with(&hex("46fc27002f48fffc2079")))
    {
        return Err("ctx_switch prefix verification failed".into());
    }
    Ok((
        u32::from_be_bytes(
            main[at + 10..at + 14]
                .try_into()
                .map_err(|_| "current TCB operand")?,
        ),
        u32::from_be_bytes(
            main[at + 22..at + 26]
                .try_into()
                .map_err(|_| "ready cursor operand")?,
        ),
    ))
}

fn flash_image(main: &[u8], flash_read: u32, container: &[u8]) -> Result<Vec<u8>, String> {
    let at = (flash_read - MAIN_LOAD) as usize;
    if main.get(at..at + FLASH_READ_SIG.len()) != Some(FLASH_READ_SIG) {
        return Err("flash_read signature changed after resolution".into());
    }
    let end = FLASH_SLOT
        .checked_add(container.len())
        .filter(|end| *end <= FLASH_SIZE)
        .ok_or("ELE3 container does not fit flash")?;
    let mut flash = vec![0; FLASH_SIZE];
    flash[FLASH_SLOT..end].copy_from_slice(container);
    Ok(flash)
}

// Supported intro routines start at (0, 0) and visit the entire bitmap. Capture
// after the final setPixel returns: the next draw clears this same bitmap before
// its first pixel, so neither chunk boundaries nor the next pixel entry are safe.
struct IntroFrameTracker {
    bitmap: Option<u32>,
    seen: [u64; 128],
    seen_pixels: u32,
    pending: Option<(u32, u32, u32)>, // bitmap, return PC, expected A7
    latest: Option<Vec<u8>>,
}

impl Default for IntroFrameTracker {
    fn default() -> Self {
        Self {
            bitmap: None,
            seen: [0; 128],
            seen_pixels: 0,
            pending: None,
            latest: None,
        }
    }
}

impl IntroFrameTracker {
    fn observe_pixel(&mut self, board: &mut Board, a7: u32) {
        let args = (|| {
            Some((
                board.read32(a7.wrapping_add(4)).ok()?,
                board.read32(a7.wrapping_add(8)).ok()?,
                board.read32(a7.wrapping_add(12)).ok()?,
            ))
        })();
        let Some((bitmap, x, y)) = args.filter(|(_, x, y)| *x < 128 && *y < 64) else {
            self.bitmap = None;
            return;
        };
        if (x, y) == (0, 0) {
            self.bitmap = Some(bitmap);
            self.seen.fill(0);
            self.seen_pixels = 0;
        }
        let bit = 1u64 << y;
        if self.bitmap != Some(bitmap) || self.seen[x as usize] & bit != 0 {
            self.bitmap = None;
            return;
        }
        self.seen[x as usize] |= bit;
        self.seen_pixels += 1;
        if self.seen_pixels == 128 * 64 {
            self.pending = board
                .read32(a7)
                .ok()
                .map(|pc| (bitmap, pc, a7.wrapping_add(4)));
            self.bitmap = None;
        }
    }

    fn complete_at_return(&mut self, board: &mut Board, pc: u32, a7: u32) -> bool {
        if let Some((bitmap, return_pc, expected_a7)) = self.pending
            && pc == return_pc
            && a7 == expected_a7
        {
            self.pending = None;
            // Validate the live header here; bitmap dimensions/storage can change.
            if let Some(raw) = decode_intro_bitmap(board, bitmap) {
                self.latest = Some(raw);
                return true;
            }
        }
        false
    }
}

fn decode_intro_bitmap(board: &mut Board, bmp: u32) -> Option<Vec<u8>> {
    if !board.can_write_ram_range(bmp.wrapping_add(4), 16) {
        return None;
    }
    let width = board.read32(bmp.wrapping_add(4)).ok()?;
    let height = board.read32(bmp.wrapping_add(8)).ok()?;
    let stride = board.read32(bmp.wrapping_add(12)).ok()?;
    let base = board.read32(bmp.wrapping_add(16)).ok()?;
    if (width, height, stride) != (128, 64, 2) || !board.can_write_ram_range(base, 1024) {
        return None;
    }
    let mut raw = vec![0; PANEL_BYTES];
    for x in 0..128u32 {
        for word_index in 0..2u32 {
            let word = board.read32(base + (x * stride + word_index) * 4).ok()?;
            for bit in 0..32u32 {
                let y = word_index * 32 + bit;
                if word & (0x8000_0000 >> bit) != 0 {
                    let index = (7 - y / 8) as usize + 8 * x as usize;
                    raw[index] |= 1 << (y % 8);
                }
            }
        }
    }
    Some(raw)
}

fn idle_advance_limit(remaining: u32, passes: u64, now: u64, deadline: Option<u64>) -> u32 {
    let before_yield = 19_999 - passes % 20_000;
    let before_timer = deadline.map_or(u64::MAX, |due| due.saturating_sub(now).saturating_sub(1));
    u64::from(remaining).min(before_yield).min(before_timer) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssi_delivery_precedes_same_boundary_timer_and_is_recorded_without_fault() {
        let Ok(syx) = std::fs::read("../../Digitone_II_OS1.11.syx") else {
            return;
        };
        let mut runtime = Emulator::new(&syx, None).unwrap();
        runtime.enable_ssi_diagnostic(96_000).unwrap();
        assert!(runtime.enable_ssi_diagnostic(96_000).is_err());
        runtime.cpu = Cpu::new();
        runtime.cpu.pc = MAIN_LOAD + 0x10000;
        runtime.cpu.sr = 0x2000;
        runtime.cpu.ctrl.vbr = MAIN_LOAD;
        runtime.cpu.a[7] = STACK + 0x100;
        let board = &mut runtime.bus.board;
        board.write16(runtime.cpu.pc, 0x4e71).unwrap(); // NOP
        board
            .write32(MAIN_LOAD + 191 * 4, MAIN_LOAD + 0x11000)
            .unwrap();
        let timer_vector = 192 + 13;
        board
            .write32(MAIN_LOAD + timer_vector * 4, MAIN_LOAD + 0x12000)
            .unwrap();
        board.attach_time(Time::new(TimerPolicy::Oracle, vec![0], 132_000_000.0));
        board.write8(0xfc04_c000 + 0x40 + 63, 4).unwrap();
        board.write8(0xfc05_0000 + 0x40 + 13, 3).unwrap();
        board.write16(0xfc08_0000 + 2, 0).unwrap(); // PIT0 due at tick 1
        board.write16(0xfc08_0000, 0x000b).unwrap();
        assert_eq!(board.time_mut().unwrap().deadline(0), Some(1));
        board.write32(0xfc04_c010, 0x8000_0000).unwrap();
        runtime.step_once();
        assert_eq!(runtime.error, None);
        assert_eq!(runtime.cpu.pc, MAIN_LOAD + 0x11000);
        assert_eq!(runtime.delivery_counts[191], 1);
        assert_eq!(runtime.delivery_counts[timer_vector as usize], 0);
        assert!(runtime.enable_ssi_diagnostic(96_000).is_err());
        runtime.cpu.sr = 0x2000;
        let count = service_timers(
            &mut runtime.bus,
            &mut runtime.cpu,
            1,
            &mut runtime.deliveries,
            &mut runtime.delivery_counts,
            &mut runtime.delivery_dropped,
        )
        .unwrap();
        assert_eq!(count, 1);
        assert_eq!(runtime.delivery_counts[timer_vector as usize], 1);
    }

    #[cfg(not(any(feature = "reference-ram-clear", feature = "diagnostic-trace")))]
    #[test]
    fn ram_clear_batches_match_interpreter_across_chunks_pages_and_final_flags() {
        for file in ["Digitakt_II_OS1.16.syx", "Digitone_II_OS1.11.syx"] {
            let Ok(syx) = std::fs::read(format!("../../{file}")) else {
                continue;
            };
            let mut slow = Emulator::new(&syx, None).unwrap();
            let mut fast = Emulator::new(&syx, None).unwrap();
            let clear = slow.ram_clear.unwrap();
            for a0 in [(clear.start | 0x0f_ffff) + 1 - 48, clear.end - 64] {
                for emu in [&mut slow, &mut fast] {
                    emu.cpu = Cpu::new();
                    emu.cpu.pc = clear.loop_pc;
                    emu.cpu.sr = 0x271f;
                    emu.cpu.icount = 100;
                    emu.cpu.d = [
                        0x7654_3210,
                        (clear.end - a0) / 16,
                        0x8765_4321,
                        0xfeed,
                        0,
                        0,
                        0,
                        0,
                    ];
                    emu.cpu.a[0] = a0;
                    emu.cpu.a[7] = STACK;
                    emu.interpreted_instructions = 100;
                    emu.ram_clear_fast_forwarded_instructions = 0;
                    for at in a0 - 4..a0 + 68 {
                        emu.bus.write8(at, 0xa5).unwrap();
                    }
                }
                let budgets: &[u32] = if a0 == clear.end - 64 {
                    &[16]
                } else {
                    &[0, 1, 2, 3, 4, 7, 13, 64]
                };
                for &budget in budgets {
                    for _ in 0..budget {
                        slow.step_once();
                    }
                    fast.step_chunk(budget);
                    assert_eq!(fast.error, None);
                    let mut actual = fast.cpu.clone();
                    let mut expected = slow.cpu.clone();
                    for cpu in [&mut actual, &mut expected] {
                        cpu.resolve_nzv();
                        cpu.invalidate_external_write(0, usize::MAX);
                    }
                    assert_eq!(actual, expected, "{file}, {a0:#x}, budget {budget}");
                    assert_eq!(fast.bus.current_icount, slow.bus.current_icount);
                    assert_eq!(fast.bus.current_pc, slow.bus.current_pc);
                    assert_eq!(fast.delivery_counts, slow.delivery_counts);
                    assert_eq!(fast.oracle_ticks(), slow.oracle_ticks());
                    assert_eq!(
                        fast.interpreted_instructions + fast.ram_clear_fast_forwarded_instructions,
                        slow.interpreted_instructions
                    );
                    let mut actual = vec![0; PAGE];
                    let mut expected = vec![0; PAGE];
                    for page in [a0 & !0x0f_ffff, fast.cpu.a[0] & !0x0f_ffff] {
                        fast.bus.board.read_ram_page(page, &mut actual).unwrap();
                        slow.bus.board.read_ram_page(page, &mut expected).unwrap();
                        assert_eq!(actual, expected);
                    }
                }
                assert!(fast.ram_clear_fast_forwarded_instructions > 0);
            }
        }
    }

    #[cfg(not(any(feature = "reference-ram-clear", feature = "diagnostic-trace")))]
    #[test]
    fn ram_clear_declines_modified_code_inputs_active_timers_and_invalid_state() {
        let Ok(syx) = std::fs::read("../../Digitakt_II_OS1.16.syx") else {
            return;
        };
        let mut emu = Emulator::new(&syx, None).unwrap();
        let clear = emu.ram_clear.unwrap();
        emu.cpu.pc = clear.loop_pc;
        emu.cpu.sr = 0x2700;
        emu.cpu.a[0] = clear.start;
        emu.cpu.d[1] = (clear.end - clear.start) / 16;
        let original = emu.cpu.clone();
        for mode in 0..6 {
            emu.cpu = original.clone();
            emu.input_packets.clear();
            emu.bus.board.attach_time(Time::with_dtims(
                TimerPolicy::Oracle,
                vec![3],
                vec![],
                132_000_000.0,
            ));
            emu.bus.board.write8(clear.loop_pc, clear.code[32]).unwrap();
            match mode {
                0 => {
                    emu.cpu.sr |= 0x8000;
                }
                1 => {
                    emu.cpu.d[4] = 1;
                }
                2 => {
                    emu.cpu.d[1] -= 1;
                }
                3 => {
                    emu.input_packets.push_back(0);
                }
                4 => {
                    emu.bus.board.write8(clear.loop_pc, 0).unwrap();
                }
                5 => {
                    let time = emu.bus.board.time_mut().unwrap();
                    time.write(0xfc08_c002, 2, 10); // PIT3 PMR
                    time.write(0xfc08_c000, 2, 0x000b);
                    assert!(time.deadline(0).is_some());
                }
                _ => unreachable!(),
            }
            let before = emu.cpu.clone();
            assert_eq!(emu.advance_ram_clear(100), 0, "guard {mode}");
            assert_eq!(emu.cpu, before);
        }
        assert_eq!(emu.ram_clear_fast_forwarded_instructions, 0);
    }

    #[test]
    fn diagnostics_preserves_cpu_and_pending_frame_delivery() {
        let Ok(syx) = std::fs::read("../../Digitakt_II_OS1.16.syx") else {
            return;
        };
        let mut runtime = Emulator::new(&syx, None).unwrap();
        runtime.current_frame = Some(vec![0xa5; PANEL_BYTES]);
        runtime.frame_revision = 1;
        let before = runtime.cpu.clone();
        let first = runtime.diagnostics();
        let second = runtime.diagnostics();
        assert_eq!(runtime.cpu, before);
        assert_eq!(first.at.icount, second.at.icount);
        assert_eq!(runtime.emitted_revision, None);
        assert!(!first.sharc_execution_connected && !first.pcm_output_connected);
        assert_eq!(runtime.snapshot().frame.unwrap(), vec![0xa5; PANEL_BYTES]);
        runtime.diagnostics();
        assert!(runtime.snapshot().frame.is_none());
    }

    #[test]
    fn tracing_choice_preserves_bus_results_and_compatibility_fault_mapping() {
        let board = || {
            Board::new(
                Card::default(),
                Default::default(),
                CompletionPolicy::Oracle,
            )
        };
        let mut traced = LoggingBus::<true>::new(board(), true, true, 160, ENTRY);
        let mut plain = LoggingBus::<false>::new(board(), true, true, 160, ENTRY);
        for addr in [0x4020_0100, 0x4030_0100, 0xfc04_002d] {
            assert_eq!(traced.write8(addr, 0x12), plain.write8(addr, 0x12));
        }
        assert_eq!(traced.read32(0x4020_0100), plain.read32(0x4020_0100));
        assert_eq!(traced.fetch16(0x4020_0100), plain.fetch16(0x4020_0100));
        assert_eq!(traced.unknown_touches.len(), plain.unknown_touches.len());
        assert!(!traced.accesses.is_empty());
        assert!(plain.accesses.is_empty());
    }

    #[test]
    fn idle_advance_stops_before_each_observable_boundary() {
        assert_eq!(idle_advance_limit(250_000, 1, 100, None), 19_998);
        assert_eq!(idle_advance_limit(20, 19_999, 100, None), 0);
        assert_eq!(idle_advance_limit(20, 1, 100, Some(110)), 9);
        assert_eq!(idle_advance_limit(20, 1, 100, Some(101)), 0);
        assert_eq!(idle_advance_limit(20, 1, 100, Some(99)), 0);
        assert_eq!(idle_advance_limit(3, 1, 100, None), 3);
    }

    #[test]
    fn idle_advance_matches_reference_cpu_and_guest_clock() {
        let Ok(syx) = std::fs::read("../../Digitakt_II_OS1.16.syx") else {
            return;
        };
        let mut slow = Emulator::new(&syx, None).unwrap();
        let mut fast = Emulator::new(&syx, None).unwrap();
        let pc = MAIN_LOAD + 0x100;
        for emu in [&mut slow, &mut fast] {
            emu.cpu.pc = pc;
            emu.bus.board.write16(pc, 0x60fe).unwrap();
            emu.idle_spins.insert(pc);
        }
        for _ in 0..400 {
            slow.step_once();
        }
        fast.step_chunk(400);
        assert_eq!(fast.error, None);
        assert_eq!(fast.cpu, slow.cpu);
        assert_eq!(fast.idle_passes, slow.idle_passes);
        assert_eq!(fast.oracle_ticks(), slow.oracle_ticks());
        assert_eq!(fast.interpreted_instructions, 1);
        assert_eq!(fast.idle_fast_forwarded_instructions, 399);
        assert_eq!(fast.bus.current_icount, slow.bus.current_icount);
    }

    #[test]
    fn intro_publishes_only_after_a_complete_raster_returns_including_black() {
        let mut board = Board::new(
            Card::default(),
            Default::default(),
            CompletionPolicy::Oracle,
        );
        let bmp = 0x4020_0000;
        let base = 0x4030_0000;
        let sp = 0x4020_0100;
        for page in [bmp, base] {
            board.map_zeroed_ram_page(page).unwrap();
        }
        for (offset, value) in [(4, 128), (8, 64), (12, 2), (16, base)] {
            board.write32(bmp + offset, value).unwrap();
        }
        board.write32(sp, MAIN_LOAD + 0x100).unwrap();
        board.write32(sp + 4, bmp).unwrap();
        let mut tracker = IntroFrameTracker::default();
        for black in [false, true] {
            for pixel in 0..8192 {
                let (x, y) = if black {
                    (pixel % 128, pixel / 128)
                } else {
                    (pixel / 64, pixel % 64)
                };
                board.write32(sp + 8, x).unwrap();
                board.write32(sp + 12, y).unwrap();
                tracker.observe_pixel(&mut board, sp);
                if pixel < 8191 {
                    assert!(tracker.pending.is_none());
                }
            }
            let before_return = tracker.latest.clone();
            tracker.complete_at_return(&mut board, MAIN_LOAD + 0x100, sp);
            assert_eq!(tracker.latest, before_return); // caller PC alone is insufficient
            board
                .write32(base, if black { 0 } else { 0x8000_0000 })
                .unwrap();
            tracker.complete_at_return(&mut board, MAIN_LOAD + 0x100, sp + 4);
            let frame = tracker.latest.as_ref().unwrap();
            assert_eq!(panel_pixel(frame, 0, 0), !black);
            assert!(tracker.pending.is_none());
            // A partial redraw must leave the last complete frame visible.
            board.write32(base, 0xffff_ffff).unwrap();
            board.write32(sp + 8, 0).unwrap();
            board.write32(sp + 12, 0).unwrap();
            tracker.observe_pixel(&mut board, sp);
            tracker.complete_at_return(&mut board, MAIN_LOAD + 0x100, sp + 4);
            assert_eq!(panel_pixel(tracker.latest.as_ref().unwrap(), 0, 0), !black);
        }
        // A duplicate before full coverage invalidates the partial raster.
        board.write32(sp + 12, 1).unwrap();
        tracker.observe_pixel(&mut board, sp);
        tracker.observe_pixel(&mut board, sp);
        assert_eq!(tracker.bitmap, None);
        assert!(tracker.pending.is_none());
    }

    #[test]
    fn intro_bitmap_layout_matches_panel_layout() {
        let card = Card::default();
        let mut board = Board::new(card, Default::default(), CompletionPolicy::Oracle);
        for page in [0x4020_0000, 0x4030_0000] {
            board.map_zeroed_ram_page(page).unwrap();
        }
        board.write32(0x4020_0004, 128).unwrap();
        board.write32(0x4020_0008, 64).unwrap();
        board.write32(0x4020_000c, 2).unwrap();
        board.write32(0x4020_0010, 0x4030_0000).unwrap();
        board.write32(0x4030_0000, 0x8000_0000).unwrap();
        board.write32(0x4030_0004, 1).unwrap();
        let raw = decode_intro_bitmap(&mut board, 0x4020_0000).unwrap();
        assert!(panel_pixel(&raw, 0, 0));
        assert!(panel_pixel(&raw, 0, 63));
        assert!(!panel_pixel(&raw, 1, 0));
    }

    #[test]
    fn known_fixture_constructs_without_running_guest_code() {
        let Ok(syx) = std::fs::read("../../Digitakt_II_OS1.16.syx") else {
            return;
        };
        let runtime = Emulator::new(&syx, None).unwrap();
        assert_eq!(runtime.cpu.icount, 0);
        assert_eq!(runtime.input_packets.len(), 0);
    }

    #[test]
    fn panel_packets_keep_chords_split_signed_turns_and_reserve_capacity() {
        let Ok(syx) = std::fs::read("../../Digitakt_II_OS1.16.syx") else {
            return;
        };
        let mut runtime = Emulator::new(&syx, None).unwrap();
        runtime.button(1, true).unwrap();
        runtime.button(1, true).unwrap();
        runtime.button(2, true).unwrap();
        runtime.turn(1, 130).unwrap();
        assert_eq!(
            runtime.input_packets,
            [0x20, 1, 0x20, 3, 0x30, 127, 0x30, 3]
        );
        runtime.input_packets = VecDeque::from(vec![0; 1024]);
        assert!(runtime.button(3, true).is_err());
        assert_eq!(runtime.held_masks[0], 3);

        let before = runtime.input_packets.clone();
        assert!(runtime.turn(1, i32::MIN).is_err());
        assert_eq!(runtime.input_packets, before);
        assert!(runtime.turn(1, i32::MAX).is_err());
        assert_eq!(runtime.input_packets, before);
    }
}
