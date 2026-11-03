use std::collections::{BTreeMap, BTreeSet, VecDeque};

use coldfire::{Bus, Cpu, InterruptPolicy};
use dt2_firmware_loader::{decode_syx, parse};
use emmc_card::{Card, CardIdentity};
use machine::{Board, CompletionEvent, CompletionPolicy, Time, TimerPolicy};
use plusdrive_format::build_sample_image;
use serde::Serialize;

use crate::capture::Dspi2Capture;
use crate::common::*;
use crate::guard::{GuardHit, GuardRange, GuardReport};
use crate::ram_clear::RamClear;
use crate::softfloat::{ExecutionPolicy, SoftfloatAbi, SoftfloatCounts};
#[cfg(feature = "diagnostic-events")]
use crate::telemetry::EventKind;
use crate::telemetry::{DiagnosticReport, Mark, Position, Recorder};
use coldfire::fused::Loop;

mod call;
pub use call::{CallResult, CallSpec};

/// The interrupt level at which USB MIDI input is handed to the firmware (`service_usb`), and so the mask that holds it back.
const USB_LEVEL: u8 = 5;
const CHUNK_MAX: u32 = 250_000;
const CAPTURE_BUS_TRACE: bool = false;
const ACCELERATE_RAM_CLEAR: bool = true;
const SET_PIXEL_SIG: &str = "2f032f02206f000c222f0010202f00144a816d4c4a806d48b2a800046c42b0a800086c3c43e8000c761f4c1118002400ea82c680202f001820680010d282e589";
const TCD34_BASE: u32 = 0xfc04_5440;
const TCD34_DADDR: u32 = TCD34_BASE + 0x10;
const RX_VECTOR: u8 = 154;
/// Board RAM window the core may fetch from outside MAIN: SDRAM (DDR, ACR0) and internal SRAM.
const RAM_EXEC_START: u32 = 0x4000_0000;
const RAM_EXEC_END: u32 = 0x8c00_0000;

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
    /// Front-panel LED revision (see `panel_leds`); `Snapshot::leds` follows it.
    pub led_revision: u32,
    pub main_ui_reached: bool,
    pub filesystem_verified: Option<bool>,
    pub input_ready: bool,
    pub input_pending: usize,
    pub input_irqs: u64,
    /// Passes through the resolved main-loop PC since boot: the guest's heartbeat.
    pub mainloop_hits: u64,
    /// The resolved main-loop PC (the address counted by `mainloop_hits`).
    pub mainloop_pc: u32,
    /// `running`, `stopped` (waiting in STOP for an interrupt) or `halted`.
    pub cpu_state: &'static str,
    /// Actual CPU steps, excluding analytic idle and reset-clear batches.
    pub interpreted_instructions: u64,
    pub idle_fast_forwarded_instructions: u64,
    pub ram_clear_fast_forwarded_instructions: u64,
    /// Write-guard hits so far (`set_write_guards`); 0 when no guards are armed.
    pub guard_hits: u64,
    pub flash_hle_calls: u64,
    pub softfloat_hle_calls: u64,
    pub softfloat: SoftfloatCounts,
    pub softfloat_available: bool,
    pub softfloat_reason: Option<String>,
    pub oracle_ticks: u64,
    pub execution_policy: ExecutionPolicy,
    pub clock_description: String,
    /// The guest wrote SOFTRST to RCM.RCR: a restart of the boot is due (the controller does it for a patched build).
    pub soft_reset: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub status: Status,
    pub frame: Option<Vec<u8>>,
    /// The 64 panel LED values as 128 hex digits, only when `status.led_revision` changed since the
    /// last snapshot that carried them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub leds: Option<String>,
    /// The 64 RGB LED values (r, g, b each) as 384 hex digits, sent with `leds` once any is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rgb: Option<String>,
    /// Set when `emu_step` only reported because something else steps the
    /// guest (free running, paused, a job); the webview then polls once per
    /// display frame instead of at once.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub polled: bool,
}

/// The registers at one execution of a PC `record_regs_at` names.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct RegHit {
    pub pc: u32,
    pub icount: u64,
    pub d: [u32; 8],
    pub a: [u32; 8],
    /// The four longwords at A7: on a function's first instruction, the
    /// first is the return address, so it names the caller.
    pub stack: [u32; 4],
}

/// How many executions `record_regs_at` keeps; later ones are counted only.
pub const REG_LOG_MAX: usize = 4096;

/// A routine the host asks the guest to run: when the guest itself next reaches
/// `at`, the registers are saved, each `data` block is written, `args` are pushed
/// (C order, the first nearest the return address) with `at` as the return
/// address, and the CPU jumps to `func`. When it returns to `at` with the stack
/// back where the call left it, every register is restored and the guest carries
/// on as if nothing ran. Choose `at` in the task whose context the routine needs
/// (`call_function` runs a routine from the host's own stack instead).
///
/// A data block at `GuestCall::STACK` goes on the guest's own stack, below the
/// arguments, which is mapped wherever the guest runs; any argument equal to
/// `STACK` is then replaced by the block's address.
#[derive(Clone, Debug)]
pub struct GuestCall {
    pub at: u32,
    pub func: u32,
    pub args: Vec<u32>,
    pub data: Vec<(u32, Vec<u8>)>,
}

impl GuestCall {
    pub const STACK: u32 = 0xFFFF_FFF0;
}

/// The bytes a guest routine was handed, captured on its first instruction: the
/// first four stack arguments, and `len` bytes from the pointer in argument `buf`.
#[derive(Clone, Debug, Serialize)]
pub struct Capture {
    pub pc: u32,
    pub icount: u64,
    pub args: [u32; 4],
    pub bytes: Vec<u8>,
}

/// The most bytes one capture copies; a longer length is cut.
pub const CAPTURE_MAX: usize = 1 << 16;

struct ActiveCall {
    at: u32,
    sp_after: u32,
    d: [u32; 8],
    a: [u32; 8],
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
    /// MAIN OS's sector erase and page program (`None` where the signature is not found): the OS upgrade's writes go to `flash`.
    flash_erase: Option<u32>,
    flash_program: Option<u32>,
    flash_writes: FlashWrites,
    flash: Vec<u8>,
    fs_worker: Option<(u32, u32, u32, u32)>,
    intro_done: u32,
    display_start: u32,
    set_pixel: u32,
    uart_wait: u32,
    uart_handler: u32,
    current_tcb_addr: u32,
    idle_spins: BTreeSet<u32>,
    /// Counting idle loops found in the running guest: `addq.l #1,(counter).l`
    /// then `bra.s` back to it (`52 b9 <counter> 60 f8`), keyed by the `addq`
    /// address, with the counter address. Learned from the PC at chunk starts
    /// (the code may be placed at run time), never from a fixed address.
    count_spins: BTreeMap<u32, u32>,
    /// Over-approximate bitset (one bit per halfword of the main image) of
    /// every PC the per-instruction observers compare against; a miss lets
    /// `step_once` skip all of those exact comparisons.
    watch: Vec<u64>,
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
    panel_leds: crate::panel_leds::PanelLeds,
    emitted_led_revision: u32,
    // A restore has a new host observer. This is intentionally not saved:
    // serializing it would change a load/save state digest.
    replay_restored_frame: bool,
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
    midi: crate::midi::MidiPort,
    policy: ExecutionPolicy,
    softfloat: SoftfloatAbi,
    ram_clear: Option<RamClear>,
    /// Heads of the loops `Cpu::run_fused` runs, with their code bytes
    /// (watched: `run_fast` stops there and tries a fused batch).
    fused_loops: Vec<(u32, Loop, Vec<u8>)>,
    /// Per fused loop: no observed PC inside its body (set by `rebuild_watch`,
    /// the only place `observed_pcs` changes).
    fused_clean: Vec<bool>,
    /// The watched PCs other than fused-loop heads, sorted.
    observed_pcs: Vec<u32>,
    /// Whether `observed_pcs` holds a PC outside the MAIN image (RAM code).
    observed_outside_main: bool,
    /// Host-requested PC hit counters (`probe_pcs`), for headless diagnosis.
    probe_hits: BTreeMap<u32, u64>,
    /// The DSPI1 peer's IRQ1 (EPORT, INTC0 source 1) after a transfer; not saved.
    irq1_pending: bool,
    /// A CPU exception runs the guest's handler instead of stopping the run with an error (the digikit runtime's crash capture,
    /// docs/findings/46-digikit-debug.md). Off by default: an exception in the firmware is a bug the harness must see.
    deliver_exceptions: bool,
    /// (probed pc, word at the stack top = return address for a `jsr`) -> (hits, last icount)
    probe_rets: BTreeMap<(u32, u32, u32), (u64, u64)>,
    /// PCs whose registers a host asked to record (`record_regs_at`), and the
    /// record: every execution, up to `REG_LOG_MAX`, then the count only.
    reg_watch: Vec<u32>,
    reg_log: Vec<RegHit>,
    reg_log_dropped: u64,
    /// Host calls not yet started, oldest first, the one running, and how many returned.
    calls: VecDeque<GuestCall>,
    call_active: Option<ActiveCall>,
    calls_done: u64,
    /// (pc, argument holding the buffer, argument holding the length).
    capture_at: Vec<(u32, usize, usize)>,
    captures: Vec<Capture>,
    interpreted_instructions: u64,
    idle_fast_forwarded_instructions: u64,
    ram_clear_fast_forwarded_instructions: u64,
    flash_hle_calls: u64,
    dspi2_capture: Option<Dspi2Capture>,
    #[cfg(feature = "cf-profile")]
    prof: crate::cfprof::CfProf,
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
            .enable_ssi_diagnostic(request_hz, periph::clock::cf_ips())
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
        Self::build(syx, card, policy, None)
    }

    /// Boot a patched build of a known firmware. `base_sha256` names the
    /// unpatched .syx whose profile (device, version, readiness contract,
    /// panel) applies; neither the whole-file digest nor the MAIN digest is
    /// checked, so any MAIN bytes boot if every signature lookup still
    /// resolves (each one fails loudly otherwise).
    /// Ready when the main panel is up, without the filesystem check the firmware runs on a fresh card (a card that an earlier
    /// boot already used can skip it, as after a soft reset).
    pub fn relax_fs_check(&mut self) {
        self.contract = device_profile::ReadinessContract::MainPanelV1;
    }

    pub fn new_patched(
        syx: &[u8],
        card: Option<Card>,
        base_sha256: &str,
    ) -> Result<Self, String> {
        Self::build(syx, card, ExecutionPolicy::Reference, Some(base_sha256))
    }

    fn build(
        syx: &[u8],
        card: Option<Card>,
        policy: ExecutionPolicy,
        base_sha256: Option<&str>,
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
        let registry = device_profile::Registry::load().map_err(|e| e.to_string())?;
        let (device, profile, _) = match base_sha256 {
            Some(base) => registry.boot_for_base(base),
            None => registry
                .boot_for_main(&digest(syx), &main)
                // A .syx of a known MAIN with another DSP stream has no known file digest.
                .or_else(|e| registry.boot_by_main(&main).ok_or(e))
                // A digikit-patched image names its stock base in its patch record.
                .or_else(|e| match patchkit::verify::patch_record(syx) {
                    Ok(Some(record)) => registry.boot_for_base(&record.base_sha256),
                    _ => Err(e),
                }),
        }
        .map_err(|e| match e {
            device_profile::RegistryError::UnknownFirmware { sha256 } => format!(
                "this file is not a supported firmware: use a stock {}, or a digikit patch of one (SHA-256 {sha256})",
                supported_firmware(&registry)
            ),
            device_profile::RegistryError::UnsupportedBoot { version, .. } => format!(
                "OS {version} does not run in the emulator: use {}",
                supported_firmware(&registry)
            ),
            e => e.to_string(),
        })?;
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
        let flash_erase = unique(&main, FLASH_ERASE_SIG, &data_mask(FLASH_ERASE_SIG, None)).ok();
        let flash_program = unique(&main, FLASH_PROGRAM_SIG, &data_mask(FLASH_PROGRAM_SIG, None)).ok();
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
                Card::with_backing(CardIdentity::from_env()?, Some(Box::new(image)))
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
            periph::clock::cf_ips() as f64,
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
        let supported_softfloat = profile.softfloat_abi;
        let softfloat = match policy {
            ExecutionPolicy::Reference => SoftfloatAbi::disabled("reference policy"),
            ExecutionPolicy::SoftfloatAbiV1 => {
                SoftfloatAbi::resolve(&main, supported_softfloat, &board)
            }
        };
        let bus = LoggingBus::new(board, true, true, 160, ENTRY);
        let ram_clear = RamClear::resolve(&main);
        let fused_loops = resolve_fused_loops(&main);
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
            flash_erase,
            flash_program,
            flash_writes: FlashWrites::default(),
            flash,
            fs_worker,
            intro_done,
            display_start,
            set_pixel,
            uart_wait,
            uart_handler,
            current_tcb_addr,
            idle_spins,
            count_spins: BTreeMap::new(),
            watch: Vec::new(),
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
            panel_leds: Default::default(),
            emitted_led_revision: 0,
            replay_restored_frame: false,
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
            midi: Default::default(),
            policy,
            softfloat,
            ram_clear,
            fused_loops,
            fused_clean: Vec::new(),
            observed_pcs: Vec::new(),
            observed_outside_main: false,
            probe_hits: BTreeMap::new(),
            irq1_pending: false,
            deliver_exceptions: false,
            probe_rets: BTreeMap::new(),
            reg_watch: Vec::new(),
            reg_log: Vec::new(),
            reg_log_dropped: 0,
            calls: VecDeque::new(),
            call_active: None,
            calls_done: 0,
            capture_at: Vec::new(),
            captures: Vec::new(),
            interpreted_instructions: 0,
            idle_fast_forwarded_instructions: 0,
            ram_clear_fast_forwarded_instructions: 0,
            flash_hle_calls: 0,
            dspi2_capture: None,
            #[cfg(feature = "cf-profile")]
            prof: Default::default(),
        };
        emulator.rebuild_watch();
        // DIGI_WRITE_WATCH / DIGI_FLUSH_CHECK: off unless set (see watch.rs).
        match crate::watch::WatchConfig::from_env() {
            Ok(cfg) => emulator.bus.guards.set_watch(cfg),
            Err(e) => eprintln!("write-watch: {e}; not armed"),
        }
        emulator.mark(Mark::Entry, "before_instruction");
        Ok(emulator)
    }

    pub fn step_chunk(&mut self, budget: u32) -> Snapshot {
        let budget = budget.min(CHUNK_MAX);
        self.input_attempted_in_chunk = false;
        self.learn_count_spin();
        let mut iterations = 0;
        while iterations < budget && self.error.is_none() {
            if ACCELERATE_RAM_CLEAR
                && self
                    .ram_clear
                    .as_ref()
                    .is_some_and(|clear| self.cpu.pc == clear.loop_pc)
            {
                #[cfg(feature = "cf-profile")]
                let t = crate::cputime::thread_cpu_ns();
                let count = self.advance_ram_clear(budget - iterations);
                #[cfg(feature = "cf-profile")]
                if count != 0 {
                    self.prof.add(0, crate::cputime::thread_cpu_ns() - t, u64::from(count));
                }
                if count != 0 {
                    iterations += count;
                    continue;
                }
            }
            #[cfg(feature = "cf-profile")]
            let t0 = crate::cputime::thread_cpu_ns();
            let fast = self.run_fast(budget - iterations);
            if fast != 0 {
                #[cfg(feature = "cf-profile")]
                self.prof.add(1, crate::cputime::thread_cpu_ns() - t0, u64::from(fast));
                iterations += fast;
                continue;
            }
            let previous_pc = self.cpu.pc;
            #[cfg(feature = "cf-profile")]
            let t1 = crate::cputime::thread_cpu_ns();
            self.step_once();
            #[cfg(feature = "cf-profile")]
            {
                self.prof.add(2, crate::cputime::thread_cpu_ns() - t1, 1);
                self.prof.sample(previous_pc, 1);
            }
            iterations += 1;
            #[cfg(feature = "cf-profile")]
            let t2 = crate::cputime::thread_cpu_ns();
            let idle = self.advance_idle(previous_pc, budget - iterations);
            #[cfg(feature = "cf-profile")]
            if idle != 0 {
                self.prof.add(3, crate::cputime::thread_cpu_ns() - t2, u64::from(idle));
                self.prof.sample_idle(previous_pc, u64::from(idle));
            }
            iterations += idle;
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
        let frame = (self.replay_restored_frame
            || self.emitted_revision != Some(self.frame_revision))
        .then(|| self.current_frame.clone())
        .flatten();
        if frame.is_some() {
            self.emitted_revision = Some(self.frame_revision);
            self.replay_restored_frame = false;
        }
        let led_changed = self.emitted_led_revision != self.panel_leds.revision;
        if led_changed {
            self.emitted_led_revision = self.panel_leds.revision;
        }
        Snapshot {
            status,
            frame,
            leds: led_changed.then(|| self.panel_leds.leds_hex()),
            rgb: led_changed.then(|| self.panel_leds.rgb_hex()).flatten(),
            polled: false,
        }
    }

    /// The latest OLED frame and its revision, without consuming it: unlike
    /// `snapshot`, this does not mark the frame as delivered, so a control
    /// client can read it without taking it from the UI.
    pub fn current_frame(&self) -> Option<(u64, &[u8])> {
        self.current_frame
            .as_deref()
            .map(|frame| (self.frame_revision, frame))
    }

    /// The panel LED revision and values (`Snapshot::leds` and `rgb` hex),
    /// without marking them as delivered, like `current_frame`.
    pub fn current_leds(&self) -> (u32, String, Option<String>) {
        (
            self.panel_leds.revision,
            self.panel_leds.leds_hex(),
            self.panel_leds.rgb_hex(),
        )
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

    /// ColdFire instructions executed so far (the clock scenarios and
    /// recorded inputs are timed in).
    /// Count how often each of `pcs` is executed from now on (diagnostics;
    /// reads no guest state and changes no behaviour).
    pub fn probe_pcs(&mut self, pcs: &[u32]) {
        for &pc in pcs {
            self.probe_hits.entry(pc).or_insert(0);
        }
        self.rebuild_watch();
    }

    /// ((pc, word at the stack top), (hits, last icount)).
    pub fn probe_rets(&self) -> Vec<((u32, u32, u32), (u64, u64))> {
        self.probe_rets.iter().map(|(k, v)| (*k, *v)).collect()
    }

    /// Every RAM page the guest touched that nothing had mapped beforehand, with
    /// its first touch: (page base, PC, "read" or "write", address). A page is
    /// 1 MiB. What the image load and the flash reads mapped is not listed.
    pub fn touched_pages(&self) -> Vec<(u32, u32, &'static str, u32)> {
        self.bus
            .unknown_touches
            .iter()
            .map(|(page, touch)| (*page, touch.pc, touch.kind, touch.addr))
            .collect()
    }

    /// Reads `len` bytes of guest memory for a host tool, as a debugger's peek:
    /// straight from the board at any address (SRAM included), so no
    /// instruction runs and no bus trace, timer or peripheral observer sees it.
    /// A peripheral register's read side effects are the board's.
    pub fn peek(&mut self, addr: u32, len: usize) -> Result<Vec<u8>, String> {
        (0..len)
            .map(|i| {
                let at = addr.wrapping_add(i as u32);
                self.bus
                    .board
                    .read8(at)
                    .map_err(|e| format!("peek {at:#010x}: {e:?}"))
            })
            .collect()
    }

    /// Writes guest memory for a host tool, as a debugger's poke: straight to
    /// the board at any address, then drops any decoded instructions over the
    /// written bytes, so a poke over code runs as written.
    pub fn poke(&mut self, addr: u32, bytes: &[u8]) -> Result<(), String> {
        for (i, &value) in bytes.iter().enumerate() {
            let at = addr.wrapping_add(i as u32);
            self.bus
                .board
                .write8(at, value)
                .map_err(|e| format!("poke {at:#010x}: {e:?}"))?;
        }
        self.cpu.invalidate_external_write(addr, bytes.len());
        Ok(())
    }

    /// Records D0-D7 and A0-A7 each time one of PCS is about to execute (replacing
    /// any PCs asked for before, and the record): a debugger's breakpoint that
    /// logs and goes on. See `reg_log`.
    pub fn record_regs_at(&mut self, pcs: &[u32]) {
        self.reg_watch = pcs.to_vec();
        self.reg_log.clear();
        self.reg_log_dropped = 0;
        self.rebuild_watch();
    }

    /// What `record_regs_at` recorded, oldest first, and how many executions past
    /// `REG_LOG_MAX` were only counted.
    pub fn reg_log(&self) -> (&[RegHit], u64) {
        (&self.reg_log, self.reg_log_dropped)
    }

    /// Queues a routine for the guest to run from a PC it reaches itself. See
    /// `GuestCall`.
    pub fn queue_call(&mut self, call: GuestCall) {
        self.calls.push_back(call);
        self.rebuild_watch();
    }

    /// Calls queued and not yet returned (running included), and calls returned.
    pub fn calls_pending(&self) -> (usize, u64) {
        (
            self.calls.len() + usize::from(self.call_active.is_some()),
            self.calls_done,
        )
    }

    /// Captures bytes at the pointer in stack argument `buf` each time PC runs;
    /// `buf` and `len` count arguments from 0, and a `len` above 3 is itself the
    /// byte count. Replaces any asked for before.
    pub fn capture_at(&mut self, at: &[(u32, usize, usize)]) {
        self.capture_at = at.to_vec();
        self.captures.clear();
        self.rebuild_watch();
    }

    /// The captures so far, oldest first; taking them empties the list.
    pub fn take_captures(&mut self) -> Vec<Capture> {
        std::mem::take(&mut self.captures)
    }

    fn stack_long(&mut self, at: u32) -> u32 {
        (0..4).fold(0u32, |word, j| {
            let byte = self.bus.board.read8(at.wrapping_add(j)).unwrap_or(0);
            (word << 8) | u32::from(byte)
        })
    }

    /// The host's register log, captures and calls, on a watched PC before it
    /// executes: -> true when the PC was redirected, so this step executes nothing.
    fn host_observers(&mut self, pc: u32) -> bool {
        if self.reg_watch.contains(&pc) {
            if self.reg_log.len() < REG_LOG_MAX {
                let sp = self.cpu.a[7];
                let mut stack = [0u32; 4];
                for (i, word) in stack.iter_mut().enumerate() {
                    *word = self.stack_long(sp.wrapping_add(4 * i as u32));
                }
                self.reg_log.push(RegHit {
                    pc,
                    icount: self.cpu.icount,
                    d: self.cpu.d,
                    a: self.cpu.a,
                    stack,
                });
            } else {
                self.reg_log_dropped += 1;
            }
        }
        for k in 0..self.capture_at.len() {
            let (at, buf, len) = self.capture_at[k];
            if at != pc {
                continue;
            }
            let sp = self.cpu.a[7];
            let mut args = [0u32; 4];
            for (i, arg) in args.iter_mut().enumerate() {
                *arg = self.stack_long(sp.wrapping_add(4 + 4 * i as u32));
            }
            let (ptr, n) = (
                args.get(buf).copied().unwrap_or(0),
                args.get(len).map_or(len, |&n| n as usize),
            );
            let bytes = (0..n.min(CAPTURE_MAX))
                .map(|i| self.bus.board.read8(ptr.wrapping_add(i as u32)).unwrap_or(0))
                .collect();
            self.captures.push(Capture {
                pc,
                icount: self.cpu.icount,
                args,
                bytes,
            });
        }
        if let Some(active) = &self.call_active {
            if pc == active.at && self.cpu.a[7] == active.sp_after {
                self.cpu.d = active.d;
                self.cpu.a = active.a;
                self.cpu.pc = active.at;
                self.call_active = None;
                self.calls_done += 1;
                self.rebuild_watch();
                return true;
            }
            return false;
        }
        if self.calls.front().is_none_or(|c| c.at != pc) {
            return false;
        }
        let mut call = self.calls.pop_front().expect("checked");
        let mut sp0 = self.cpu.a[7];
        let saved_a = self.cpu.a;
        for block in &mut call.data {
            if block.0 == GuestCall::STACK {
                let at = (sp0.wrapping_sub(block.1.len() as u32 + 64)) & !3;
                block.0 = at;
                for arg in &mut call.args {
                    if *arg == GuestCall::STACK {
                        *arg = at;
                    }
                }
                sp0 = at;
            }
        }
        for (at, bytes) in &call.data {
            if let Err(e) = self.poke(*at, bytes) {
                self.set_error(format!("guest call data: {e}"));
                return true;
            }
        }
        let n = call.args.len() as u32;
        let sp = sp0.wrapping_sub(4 * (n + 1));
        let words: Vec<u8> = std::iter::once(pc)
            .chain(call.args.iter().copied())
            .flat_map(u32::to_be_bytes)
            .collect();
        if let Err(e) = self.poke(sp, &words) {
            self.set_error(format!("guest call stack: {e}"));
            return true;
        }
        self.call_active = Some(ActiveCall {
            at: pc,
            sp_after: sp0.wrapping_sub(4 * n),
            d: self.cpu.d,
            a: saved_a,
        });
        self.cpu.a[7] = sp;
        self.cpu.pc = call.func;
        self.rebuild_watch();
        true
    }

    /// How many write commands the guest has sent to the eMMC card since this runtime started.
    pub fn card_write_count(&self) -> u64 {
        self.bus.board.card_write_count()
    }

    /// The guest's writes to the eMMC card, per touched sector (for `card.overlay`).
    pub fn card_overlay(&self) -> Vec<emmc_card::CardOverlaySector> {
        self.bus.board.card_overlay_sectors()
    }

    /// (d0-d7, a0-a7, pc, sr): for headless diagnosis (a fault report).
    pub fn cpu_regs(&self) -> ([u32; 8], [u32; 8], u32, u16) {
        (self.cpu.d, self.cpu.a, self.cpu.pc, self.cpu.sr)
    }

    /// Last execution/bus PC recorded by the run loop, including accelerated
    /// paths. This is not a full instruction trace or necessarily the CPU's
    /// current PC; an unsupported-PC fault leaves the previous value intact.
    pub fn last_bus_pc(&self) -> u32 {
        self.bus.current_pc
    }

    /// What the guest wrote to the container area of the flash (the OS upgrade): erases, page programs and bytes. All zero until it does.
    pub fn flash_writes(&self) -> FlashWrites {
        self.flash_writes
    }

    /// `len` bytes of the model's flash from `offset` (the container is at 0x80000); `None` outside the 16 MiB.
    pub fn flash_bytes(&self, offset: usize, len: usize) -> Option<&[u8]> {
        self.flash.get(offset..offset.checked_add(len)?)
    }

    /// Read a guest longword without side effects (None when unmapped or
    /// device-owned). For headless diagnosis only.
    pub fn peek32(&mut self, addr: u32) -> Option<u32> {
        self.bus.board.read32(addr).ok()
    }

    /// Arm write-range guards: a ColdFire store overlapping a `protect` range
    /// while the PC is outside every `allow_pc` range `[lo, hi)` is recorded
    /// (see `guard_report`). An empty `protect` disarms them. Armed guards
    /// disable the fused-loop and reset-clear shortcuts so each store is
    /// seen with its own PC. Only CPU stores are checked, not DMA or
    /// host writes (`poke32`, `poke_bytes`).
    pub fn set_write_guards(&mut self, protect: Vec<GuardRange>, allow_pc: Vec<(u32, u32)>) {
        self.bus.guards.set(protect, allow_pc);
    }

    /// Arm (or, with `None`, disarm) the write-watch and flush check; the same
    /// as the `DIGI_WRITE_WATCH*` and `DIGI_FLUSH_CHECK` variables read at
    /// construction. The CACR the instruments assume is the CPU's current one.
    pub fn set_write_watch(&mut self, cfg: Option<crate::watch::WatchConfig>) {
        self.bus.guards.set_watch(cfg);
        let cacr = self.cpu.ctrl.cacr;
        if let Some(w) = self.bus.guards.watch_mut() {
            w.set_cacr(cacr);
        }
    }

    /// Lines fetched while their last write was unflushed (flush check).
    pub fn flush_hits(&self) -> Vec<crate::watch::FlushHit> {
        self.bus.guards.flush_hits()
    }

    /// Reported write-watch events so far and, most frequent first, the PCs that made them.
    pub fn write_watch_totals(&mut self) -> (u64, Vec<(u32, u64)>) {
        self.bus
            .guards
            .watch_mut()
            .map_or((0, Vec::new()), |w| (w.totals().0, w.per_pc()))
    }

    /// Flush the write-watch log and write its summary (also done when the
    /// emulator is dropped).
    pub fn finish_write_watch(&mut self) {
        if let Some(w) = self.bus.guards.watch_mut() {
            w.finish();
        }
    }

    /// The first `GUARD_HIT_LIMIT` guard hits and the total count.
    pub fn guard_report(&self) -> GuardReport {
        self.bus.guards.report()
    }

    /// Forget recorded guard hits; the guards stay armed.
    pub fn clear_guard_hits(&mut self) {
        self.bus.guards.clear();
    }

    /// Hits that were the first for their protected range since the last call.
    pub fn take_guard_first_hits(&mut self) -> Vec<GuardHit> {
        self.bus.guards.take_first_hits()
    }

    /// Let a CPU exception run the guest's handler (true) or stop the run with an error (false, the default).
    pub fn set_deliver_exceptions(&mut self, deliver: bool) {
        self.deliver_exceptions = deliver;
    }

    /// `len` bytes of guest RAM from `addr` (a multiple of 4), read without side effects; unmapped words read 0.
    pub fn peek_ram(&mut self, addr: u32, len: u32) -> Vec<u8> {
        let mut out = Vec::with_capacity(len as usize);
        for a in (addr..addr + len).step_by(4) {
            out.extend_from_slice(&self.peek32(a).unwrap_or(0).to_be_bytes());
        }
        out
    }

    /// Write `bytes` (a multiple of 4 long) of guest RAM at `addr` without bus side effects.
    pub fn poke_ram(&mut self, addr: u32, bytes: &[u8]) -> Result<(), String> {
        for (i, w) in bytes.chunks_exact(4).enumerate() {
            self.poke32(addr + 4 * i as u32, u32::from_be_bytes([w[0], w[1], w[2], w[3]]))?;
        }
        Ok(())
    }

    /// Write `bytes` at any byte address of SDRAM without bus side effects, as one range: the words around the ends keep their
    /// other bytes, unmapped pages are mapped zeroed, and the decode cache is invalidated once for the whole range, so code written
    /// here runs as new code.
    pub fn write_ram(&mut self, addr: u32, bytes: &[u8]) -> Result<(), String> {
        if bytes.is_empty() {
            return Ok(());
        }
        let end = addr
            .checked_add(u32::try_from(bytes.len()).map_err(|_| "write_ram: range too long".to_string())?)
            .ok_or("write_ram: range wraps")?;
        if addr < 0x4000_0000 || end > 0x5000_0000 {
            return Err(format!("write_ram {addr:#x}+{}: not in SDRAM or its +128 MiB alias", bytes.len()));
        }
        let first = addr & !3;
        let last = (end + 3) & !3;
        for page in (first & !0xf_ffff..last).step_by(0x10_0000) {
            self.bus
                .board
                .map_zeroed_ram_page(page)
                .map_err(|e| format!("write_ram map {page:#x}: {e:?}"))?;
        }
        for word in (first..last).step_by(4) {
            let mut b = self.peek32(word).unwrap_or(0).to_be_bytes();
            for (i, byte) in b.iter_mut().enumerate() {
                let a = word + i as u32;
                if a >= addr && a < end {
                    *byte = bytes[(a - addr) as usize];
                }
            }
            self.bus
                .board
                .write_guest(word, 4, u32::from_be_bytes(b))
                .map_err(|e| format!("write_ram {word:#x}: {e:?}"))?;
        }
        self.cpu.invalidate_external_write(first, (last - first) as usize);
        Ok(())
    }

    /// Write a guest longword without bus side effects (headless diagnosis:
    /// canary fills). Unmapped SDRAM pages are mapped zeroed first.
    pub fn poke32(&mut self, addr: u32, value: u32) -> Result<(), String> {
        if addr & 3 != 0 || !(0x4000_0000..0x5000_0000).contains(&addr) {
            return Err(format!("poke32 {addr:#x}: not an aligned word in SDRAM or its +128 MiB alias"));
        }
        self.bus
            .board
            .map_zeroed_ram_page(addr & !0xf_ffff)
            .map_err(|e| format!("poke32 map {addr:#x}: {e:?}"))?;
        self.bus
            .board
            .write_guest(addr, 4, value)
            .map_err(|e| format!("poke32 {addr:#x}: {e:?}"))?;
        self.cpu.invalidate_external_write(addr, 4);
        Ok(())
    }

    /// Step (reset-clear batches included) until the program counter equals
    /// `target`; false when `max` instructions pass first or the run errors.
    pub fn run_until_pc(&mut self, target: u32, max: u64) -> bool {
        let start = self.cpu.icount;
        while self.error.is_none() && self.cpu.icount - start < max {
            if self.cpu.pc == target {
                return true;
            }
            if ACCELERATE_RAM_CLEAR
                && self
                    .ram_clear
                    .as_ref()
                    .is_some_and(|clear| self.cpu.pc == clear.loop_pc)
                && self.advance_ram_clear(250_000) != 0
            {
                continue;
            }
            self.step_once();
        }
        self.cpu.pc == target
    }

    pub fn probe_hits(&self) -> Vec<(u32, u64)> {
        self.probe_hits.iter().map(|(&pc, &n)| (pc, n)).collect()
    }

    /// The ColdFire path/PC profile (feature `cf-profile`).
    #[cfg(feature = "cf-profile")]
    pub fn cf_profile(&self) -> serde_json::Value {
        self.prof.report()
    }

    pub fn icount(&self) -> u64 {
        self.cpu.icount
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
        panel_tx_write(
            self.cpu.icount,
            &format!("@button {code} {}", if down { 'd' } else { 'u' }),
        );
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

    pub fn status(&mut self) -> Status {
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
            led_revision: self.panel_leds.revision,
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
            mainloop_hits: self.mainloop_hits,
            mainloop_pc: self.mainloop,
            cpu_state: match self.cpu.state {
                coldfire::RunState::Running => "running",
                coldfire::RunState::Stopped => "stopped",
                coldfire::RunState::Halted => "halted",
            },
            interpreted_instructions: self.interpreted_instructions,
            idle_fast_forwarded_instructions: self.idle_fast_forwarded_instructions,
            ram_clear_fast_forwarded_instructions: self.ram_clear_fast_forwarded_instructions,
            guard_hits: self.bus.guards.total(),
            flash_hle_calls: self.flash_hle_calls,
            softfloat_hle_calls: self.softfloat.counts.calls(),
            softfloat: self.softfloat.counts,
            softfloat_available: self.softfloat.enabled,
            softfloat_reason: self.softfloat.reason.clone(),
            oracle_ticks: self.oracle_ticks().unwrap_or(u64::MAX),
            execution_policy: self.policy,
            clock_description: "oracle_ticks = legacy cpu.icount + accepted softfloat ABI calls; atomic calls consume one scheduling tick, not a CPU instruction".into(),
            soft_reset: self.bus.board.soft_reset_requested(),
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
            || !self.bus.guards.is_empty()
            || self.cpu.pc != clear.loop_pc
            || self.cpu.state != coldfire::RunState::Running
            || self.cpu.sr & 0xf700 != 0x2700
            || self.cpu.last_exception.is_some()
            || self.cpu.last_unimplemented.is_some()
            || self.cpu.d[4..8] != [0; 4]
            || !self.input_packets.is_empty()
            || self.rx_pending
            || self.bus.board.dspi1_irq()
            || self.irq1_pending
            || self.input_attempted_in_chunk
            || self.midi.busy()
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
        #[cfg(feature = "cf-mix")]
        for off in [0, 4, 8, 10] {
            // MOVEM, LEA, SUBQ, BNE (taken every batched iteration).
            let taken = if off == 10 { u64::from(count) } else { 0 };
            self.mix_add(clear.loop_pc + off, 3, u64::from(count), None, taken);
        }
        instructions
    }

    /// Learn a counting idle loop (`addq.l #1,(counter).l; bra.s` back, bytes
    /// `52 b9 <counter> 60 f8`) the guest is in at a chunk start. Detected by
    /// the instruction pattern at the PC, in RAM wherever the code was placed.
    fn learn_count_spin(&mut self) {
        let pc = self.cpu.pc;
        for head in [pc, pc.wrapping_sub(6)] {
            if self.count_spins.contains_key(&head) {
                continue;
            }
            let mut b = [0u8; 8];
            if self.bus.board.ram_peek(head, &mut b)
                && b[..2] == [0x52, 0xb9]
                && b[6..] == [0x60, 0xf8]
            {
                let counter = u32::from_be_bytes([b[2], b[3], b[4], b[5]]);
                self.count_spins.insert(head, counter);
                self.rebuild_watch();
            }
        }
    }

    /// Evaluate repeated, verified idle passes analytically, stopping BEFORE
    /// either a timer deadline or the software rescheduling pass. Two shapes:
    /// `bra.b self` (one instruction per pass), and the counting loop
    /// `addq.l #1,(counter).l; bra.s` (two per pass; the counter in memory and
    /// the flags of the last `addq` get the skipped passes). The first pass
    /// has already run normally, including every observation and board drain.
    /// No guest or host event changes inside this interval.
    fn advance_idle(&mut self, previous_pc: u32, remaining: u32) -> u32 {
        // The counting loop: the `addq` just ran, the PC is on its `bra.s`.
        let counter = if self.cpu.pc == previous_pc.wrapping_add(6) {
            self.count_spins.get(&previous_pc).copied()
        } else {
            None
        };
        if remaining == 0
            || self.error.is_some()
            || self.cpu.state != coldfire::RunState::Running
            || self.cpu.sr & 0xc000 != 0
            || self.cpu.last_exception.is_some()
            || self.cpu.last_unimplemented.is_some()
            || !self.input_packets.is_empty()
            || self.rx_pending
            || self.bus.board.dspi1_irq()
            || self.irq1_pending
            || self.input_attempted_in_chunk
            || self.midi.din_busy()
            || self.midi.usb.active.is_some()
        {
            return 0;
        }
        // USB MIDI input waiting for its next call (the guest drained the last one inside the period, 125 us of model time) is idle time as
        // well, up to the instruction that makes the call due; `step_once` hands the bytes over at that boundary as it always did.
        let usb_wait = match self.midi.usb_due_in(self.cpu.icount) {
            Some(0) => return 0,
            Some(due_in) => due_in,
            None => u64::MAX,
        };
        if let Some(counter) = counter {
            let mut code = [0x52, 0xb9, 0, 0, 0, 0, 0x60, 0xf8];
            code[2..6].copy_from_slice(&counter.to_be_bytes());
            if !self.bus.board.ram_matches(previous_pc, &code) {
                return 0;
            }
        } else if self.cpu.pc != previous_pc
            || !self.idle_spins.contains(&previous_pc)
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
        if time.policy() != TimerPolicy::Oracle || time.forced_deliverable() {
            return 0;
        }
        let timer_deadline = time.deadline(now);
        let deadline = match (timer_deadline, ssi_deadline) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        // Instructions to skip. The counting loop has no yield pass (it never
        // had one while interpreted) and skips whole passes of two.
        let count = if counter.is_some() {
            let before_timer =
                deadline.map_or(u64::MAX, |due| due.saturating_sub(now).saturating_sub(1));
            (u64::from(remaining).min(before_timer).min(usb_wait) as u32) & !1
        } else {
            idle_advance_limit(remaining, self.idle_passes, now, deadline).min(usb_wait.min(u64::from(u32::MAX)) as u32)
        };
        if count == 0 {
            return 0;
        }
        let n = u64::from(count);
        // Reserve every counter before mutation; no fabricated interpreted work.
        let (Some(cpu_count), Some(passes), Some(skipped), Some(done)) = (
            self.cpu.icount.checked_add(n),
            self.idle_passes.checked_add(if counter.is_some() { 0 } else { n }),
            self.idle_fast_forwarded_instructions.checked_add(n),
            now.checked_add(n),
        ) else {
            self.set_error("idle advance counter overflow");
            return 0;
        };
        if let Some(counter) = counter {
            // The skipped passes' memory effect and the flags of the last addq.
            let Some((_, new)) = self.bus.board.sdram_add32(counter, count / 2) else {
                return 0;
            };
            self.cpu.set_add_flags_l(1, new.wrapping_sub(1), new);
        }
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
        #[cfg(feature = "cf-mix")]
        if counter.is_some() {
            self.mix_add(previous_pc, 2, n / 2, None, n / 2);
            self.mix_add(previous_pc.wrapping_add(6), 2, n / 2, None, n / 2);
        } else {
            self.mix_add(previous_pc, 2, n, None, n);
        }
        count
    }

    /// Rebuild the observed-PC bitset. Every PC that `step_once` compares
    /// against a stored address must be set; extra bits only cost the exact
    /// comparisons, never change behaviour.
    fn rebuild_watch(&mut self) {
        let mut watch = vec![0u64; (self.main.len() / 2 + 1).div_ceil(64)];
        let mut pcs: Vec<u32> = self.idle_spins.iter().copied().collect();
        pcs.extend(self.count_spins.keys().copied());
        pcs.extend([
            self.task_create,
            self.mainloop,
            self.job_pump,
            self.panel_diff,
            self.flash_read,
            self.flash_erase.unwrap_or(0),
            self.flash_program.unwrap_or(0),
            self.intro_done,
            self.display_start,
            self.set_pixel,
            self.uart_wait,
        ]);
        pcs.extend(self.softfloat.entries());
        pcs.extend(self.probe_hits.keys().copied());
        if self.midi.usb.enabled {
            pcs.extend([self.midi.usb.tx_fn, self.midi.usb.ret_pc].into_iter().filter(|&pc| pc != 0));
        }
        pcs.extend(self.reg_watch.iter().copied());
        pcs.extend(self.capture_at.iter().map(|c| c.0));
        pcs.extend(self.calls.front().map(|c| c.at));
        pcs.extend(self.call_active.as_ref().map(|c| c.at));
        if let Some(clear) = &self.ram_clear {
            pcs.push(clear.loop_pc);
        }
        pcs.extend(self.frames.pending.iter().map(|p| p.return_pc));
        pcs.extend(self.intro_frames.pending.map(|(_, return_pc, _)| return_pc));
        if let Some((entry, completion, _, _)) = self.fs_worker {
            pcs.extend([entry, completion.wrapping_add(12)]);
        }
        let mut observed = pcs.clone();
        observed.sort_unstable();
        observed.dedup();
        self.fused_clean = self
            .fused_loops
            .iter()
            .map(|(head, lp, _)| {
                lp.offsets()[1..]
                    .iter()
                    .all(|&o| observed.binary_search(&(head + o)).is_err())
            })
            .collect();
        self.observed_outside_main = observed.iter().any(|&pc| !self.in_main(pc));
        self.observed_pcs = observed;
        pcs.extend(self.fused_loops.iter().map(|(head, _, _)| *head));
        for pc in pcs {
            if let Some(off) = pc.checked_sub(MAIN_LOAD) {
                let i = (off >> 1) as usize;
                if let Some(word) = watch.get_mut(i / 64) {
                    *word |= 1 << (i % 64);
                }
            }
        }
        self.watch = watch;
    }

    /// A whole instruction (6 bytes) lies inside the MAIN image.
    #[inline]
    fn in_main(&self, pc: u32) -> bool {
        (pc.wrapping_sub(MAIN_LOAD) as usize) <= self.main.len().saturating_sub(6)
    }

    /// Whether the core may fetch at `pc` outside the MAIN image: anywhere in
    /// SDRAM or internal SRAM (the board's RAM window). The hardware executes
    /// from DDR (ACR0 covers it) and SRAM; the core's decode cache is evicted
    /// by every write, so code written at run time is seen.
    #[inline]
    fn ram_exec(pc: u32) -> bool {
        (RAM_EXEC_START..RAM_EXEC_END).contains(&pc)
    }

    #[inline]
    fn watched(&self, pc: u32) -> bool {
        let i = (pc.wrapping_sub(MAIN_LOAD) >> 1) as usize;
        self.watch
            .get(i / 64)
            .is_none_or(|word| word >> (i % 64) & 1 != 0)
    }

    /// Run plain instructions without the per-instruction observer, SSI and
    /// timer bookkeeping while none of it can do anything: the PC is not one
    /// the observers compare against, no device register was touched, the
    /// guest tick is below the SSI/timer deadlines, and the timer IRQ state is
    /// unchanged. The boundary where any of that stops holding runs through
    /// `finish_step`, so the machine is bit-identical to the one-instruction
    /// loop. Returns the instructions run (0: use `step_once`).
    fn run_fast(&mut self, budget: u32) -> u32 {
        if budget == 0
            || self.error.is_some()
            || self.rx_pending
            || self.irq1_pending
            || !self.input_packets.is_empty()
            || self.midi.din_busy()
            || self.fs_clear_pending.is_some()
            || self.cpu.state != coldfire::RunState::Running
            || self.bus.board.has_host_events()
        {
            return 0;
        }
        let Some((timer_limit, mut level)) = self
            .bus
            .board
            .time_mut()
            .and_then(|time| time.idle_window())
        else {
            return 0;
        };
        let Some(ssi_limit) = self.bus.board.ssi_batch_limit() else {
            return 0;
        };
        let mut limit = timer_limit.min(ssi_limit);
        let calls = self.softfloat.counts.calls();
        // USB MIDI input: while a callback call is in progress `service_usb` does nothing (the return to the interrupted code is seen at
        // `ret_pc`, an observed PC), and while the next call is not due it does nothing either, so the fast loop runs up to the
        // instruction that makes it due (the guest tick moves one for one with icount) and `step_once` hands the bytes over there, at
        // the boundary it always did. A call that is due while the guest runs with the interrupt mask at the call's level or above (the
        // audio interrupt) waits for the mask to drop, which the loop's own IPL check ends the run for, as it does for a timer.
        if let Some(due_in) = self.midi.usb_due_in(self.cpu.icount) {
            if due_in == 0 {
                if ((self.cpu.sr >> 8) & 7) < u16::from(USB_LEVEL) {
                    return 0;
                }
                level = level.max(USB_LEVEL);
            } else {
                limit = limit.min(self.cpu.icount.wrapping_add(calls).saturating_add(due_in));
            }
        }
        let epoch = self.bus.board.mmio_epoch();
        let last_pc_offset = self.main.len().saturating_sub(6);
        // `step` only adds to icount (one per instruction, none when it
        // stops), so the per-step deltas sum to the difference at exit.
        let start_icount = self.cpu.icount;
        let mut n = 0;
        #[cfg(feature = "cf-profile")]
        let (mut next_sample, mut last_n) = (0u32, 0u32);
        while n < budget {
            let pc = self.cpu.pc;
            let outside = pc.wrapping_sub(MAIN_LOAD) as usize > last_pc_offset;
            if outside
                && (!Self::ram_exec(pc)
                    || (self.observed_outside_main && self.observed_pcs.binary_search(&pc).is_ok()))
            {
                #[cfg(feature = "cf-profile")]
                self.prof.exit(1, pc);
                break;
            }
            #[cfg(feature = "cf-profile")]
            if n >= next_sample {
                self.prof.sample(pc, u64::from((n - last_n).max(1)));
                last_n = n;
                next_sample = n + 64;
            }
            if !outside && self.watched(pc) {
                // Only fused-loop heads are handled here (0: step the head
                // like any other instruction); any other observed PC goes
                // to `step_once`.
                match self.at_watched(pc, budget - n, limit, calls) {
                    None => {
                        #[cfg(feature = "cf-profile")]
                        self.prof.exit(2, pc);
                        break;
                    }
                    Some(0) => {}
                    Some(fused) => {
                        n += fused;
                        continue;
                    }
                }
            }
            let Some(before) = self.cpu.icount.checked_add(calls) else {
                break;
            };
            if let Some(capture) = &self.dspi2_capture {
                capture.set_icount(self.cpu.icount);
            }
            self.bus.current_icount = before;
            self.bus.clear();
            self.bus.current_pc = pc;
            let result = self.cpu.step_inline(&mut self.bus);
            #[cfg(feature = "cf-mix")]
            {
                let next = self.cpu.pc;
                self.mix_add(pc, 0, 1, Some(next), 0);
            }
            n += 1;
            let done = self.cpu.icount.wrapping_add(calls);
            if result.is_err()
                || done >= limit
                || self.cpu.last_exception.is_some()
                || self.cpu.state != coldfire::RunState::Running
                || self.bus.board.mmio_epoch() != epoch
                || ((self.cpu.sr >> 8) & 7) < u16::from(level)
            {
                #[cfg(feature = "cf-profile")]
                {
                    let reason = if result.is_err() {
                        4
                    } else if done >= limit {
                        5
                    } else if self.cpu.last_exception.is_some()
                        || self.cpu.state != coldfire::RunState::Running
                    {
                        6
                    } else if self.bus.board.mmio_epoch() != epoch {
                        7
                    } else {
                        8
                    };
                    self.prof.exit(
                        reason,
                        if reason == 7 { self.bus.board.last_epoch_addr() } else { pc },
                    );
                }
                self.interpreted_instructions += self.cpu.icount.saturating_sub(start_icount);
                self.finish_step(result.map_err(|error| format!("{error:?}")));
                return n;
            }
        }
        self.interpreted_instructions += self.cpu.icount.saturating_sub(start_icount);
        // The skipped services would have seeded the IPL tracker each time.
        let sr = self.cpu.sr;
        if let Some(time) = self.bus.board.time_mut() {
            time.seed_sr(sr);
        }
        n
    }

    /// A watched PC inside `run_fast`: `None` for an observed PC (stop for
    /// `step_once`); at a fused-loop head, the instructions it ran (0: step
    /// the head like any other instruction).
    #[inline(never)]
    fn at_watched(&mut self, pc: u32, budget: u32, limit: u64, calls: u64) -> Option<u32> {
        if self.observed_pcs.binary_search(&pc).is_ok()
            || !self.fused_loops.iter().any(|(head, _, _)| *head == pc)
        {
            return None;
        }
        Some(self.run_fused_loop(pc, budget, limit, calls))
    }

    /// At a fused-loop head inside `run_fast`: run whole iterations with
    /// `Cpu::run_fused` while stepping them would not have stopped
    /// `run_fast` (the batch ends before the guest tick, icount + `calls`,
    /// reaches `limit`, and within `budget`). Same machine state as
    /// stepping: the instructions are counted as interpreted (`run_fast`
    /// adds the icount difference), and the bus/capture bookkeeping is left
    /// as the batch's last instruction set it. Returns the instructions run
    /// (0: step normally).
    fn run_fused_loop(&mut self, pc: u32, budget: u32, limit: u64, calls: u64) -> u32 {
        let Some(index) = self.fused_loops.iter().position(|(head, _, _)| *head == pc) else {
            return 0;
        };
        let (head, lp, ref code) = self.fused_loops[index];
        let offsets = lp.offsets();
        // The code is still what was recognised, every instruction is
        // decoded already (stepping would only hit the decode cache), and
        // no other observer watches a PC inside the body.
        if !self.bus.board.ram_matches(head, code)
            || offsets.iter().any(|&o| !self.cpu.decoded_at(head + o))
            || !self.fused_clean[index]
        {
            return 0;
        }
        let per = lp.instructions();
        // `run_fast` stops after an instruction once the tick reaches the
        // limit; the batch's last instruction must stay below it.
        let room = limit
            .saturating_sub(self.cpu.icount.saturating_add(calls))
            .saturating_sub(1);
        let max = (budget / per).min(u32::try_from(room / u64::from(per)).unwrap_or(u32::MAX));
        if max == 0 {
            return 0;
        }
        #[cfg(feature = "cf-profile")]
        let tf = crate::cputime::thread_cpu_ns();
        let iterations = self.cpu.run_fused(&mut self.bus, lp, max);
        #[cfg(feature = "cf-profile")]
        self.prof.fused_add(
            head,
            u64::from(iterations) * u64::from(per),
            crate::cputime::thread_cpu_ns() - tf,
        );
        if iterations == 0 {
            return 0;
        }
        #[cfg(feature = "cf-mix")]
        for (k, &o) in offsets.iter().enumerate() {
            // The loop's conditional branch is the last instruction and is taken every iteration.
            let taken = if k + 1 == offsets.len() {
                u64::from(iterations)
            } else {
                0
            };
            self.mix_add(head + o, 1, u64::from(iterations), None, taken);
        }
        let last = self.cpu.icount - 1;
        if let Some(capture) = &self.dspi2_capture {
            capture.set_icount(last);
        }
        self.bus.current_icount = last + calls;
        self.bus.current_pc = head + offsets[offsets.len() - 1];
        iterations * per
    }

    #[cfg(feature = "cf-mix")]
    fn mix_add(&mut self, pc: u32, path: usize, n: u64, next: Option<u32>, taken: u64) {
        let board = &mut self.bus.board;
        crate::cfmix::add(pc, path, n, next, taken, || {
            let mut w = |a: u32| board.fetch16(a).unwrap_or(0);
            coldfire::decode(pc, [w(pc), w(pc.wrapping_add(2)), w(pc.wrapping_add(4))])
        });
    }

    /// The exact instruction mix (feature `cf-mix`).
    #[cfg(feature = "cf-mix")]
    pub fn cf_mix(&self) -> serde_json::Value {
        let mut v = crate::cfmix::report();
        let c = &self.cpu.ctrl;
        v["ctrl"] = serde_json::json!({
            "cacr": c.cacr, "acr": c.acr, "rambar": c.rambar, "vbr": c.vbr,
        });
        v
    }

    #[cfg(feature = "cf-mix")]
    pub fn cf_mix_reset(&self) {
        crate::cfmix::reset();
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
        if !self.in_main(pc) && !Self::ram_exec(pc) {
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
                if self.intro_frames.pending.is_some() {
                    // The completed raster returns to a new PC: observe it.
                    self.rebuild_watch();
                }
            }
        }
        let watched = self.watched(pc);
        if watched {
            self.observe_marks(pc);
            if self.host_observers(pc) {
                return;
            }
        }
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
            // A new pending frame completes at its return PC: observe it.
            self.rebuild_watch();
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
        if self.bus.board.dspi1_irq() || self.irq1_pending {
            match self.service_dspi1() {
                Ok(true) => return,
                Ok(false) => {}
                Err(error) => {
                    self.set_error(error);
                    return;
                }
            }
        }
        // `service_input` is a no-op unless a packet is queued or acknowledged.
        if self.rx_pending || !self.input_packets.is_empty() {
            match self.service_input() {
                Ok(true) => return,
                Ok(false) => {}
                Err(error) => {
                    self.set_error(error);
                    return;
                }
            }
        }
        if self.midi.busy() {
            let delivered = match self.service_midi() {
                Ok(true) => Ok(true),
                Ok(false) => self.service_usb(),
                Err(error) => Err(error),
            };
            match delivered {
                Ok(true) => return,
                Ok(false) => {}
                Err(error) => {
                    self.set_error(error);
                    return;
                }
            }
        }
        if watched && self.idle_spins.contains(&pc) {
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
        let result = if watched && pc == self.flash_read {
            let result = hle_flash_read(&mut self.bus, &mut self.cpu, &self.flash, &mut Vec::new());
            if result.is_ok() {
                self.cpu.icount += 1;
                self.flash_hle_calls += 1;
            }
            result
        } else if watched
            && (Some(pc) == self.flash_erase || Some(pc) == self.flash_program)
            && matches!(
                hle_flash_write(
                    &mut self.bus,
                    &mut self.cpu,
                    &mut self.flash,
                    Some(pc) == self.flash_erase,
                    &mut self.flash_writes,
                ),
                Ok(true)
            )
        {
            self.cpu.icount += 1;
            Ok(())
        } else if watched
            && self.softfloat.try_call(
                &mut self.cpu,
                &mut self.bus.board,
                MAIN_LOAD + self.main.len() as u32,
            )
        {
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
        #[cfg(feature = "cf-mix")]
        {
            let next = self.cpu.pc;
            self.mix_add(pc, 0, 1, Some(next), 0);
        }
        self.finish_step(result);
    }

    /// Everything after the CPU step of one instruction: host-event drain, the
    /// SSI and timer boundary services, and the exception check.
    fn finish_step(&mut self, result: Result<(), String>) {
        if self.fs_clear_pending.is_some() {
            self.observe_fs_completion(result.is_ok());
        }
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
            .is_some_and(|vector| vector != 32 && !timer_delivered && !self.deliver_exceptions)
        {
            self.set_error(format!(
                "exception vector {}",
                self.cpu.last_exception.unwrap()
            ));
        }
    }

    fn observe_marks(&mut self, pc: u32) {
        if self.midi.usb.enabled {
            self.observe_usb(pc);
        }
        if let Some(hits) = self.probe_hits.get_mut(&pc) {
            *hits += 1;
            let ret = self.bus.board.read32(self.cpu.a[7]).unwrap_or(0);
            let arg = self.bus.board.read32(self.cpu.a[7].wrapping_add(4)).unwrap_or(0);
            let entry = self.probe_rets.entry((pc, ret, arg)).or_insert((0, 0));
            entry.0 += 1;
            entry.1 = self.cpu.icount;
        }
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
        if !self.bus.board.has_host_events() {
            return;
        }
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
        let sources = self.bus.board.take_dma_sources();
        if !self.bus.guards.is_empty() {
            for (&(address, bytes), src) in ranges.iter().zip(sources) {
                self.bus
                    .guards
                    .note_dma(src, address, bytes, self.bus.current_icount);
            }
        }
        // DMA into a page with decoded instructions (code run from RAM).
        for &(address, bytes) in &ranges {
            self.cpu.invalidate_external_write(address, bytes);
        }
        self.dma_ranges += ranges.len() as u64;
        self.dma_bytes += ranges.iter().map(|(_, bytes)| *bytes as u64).sum::<u64>();
        #[cfg(feature = "diagnostic-events")]
        for &(address, bytes) in &ranges {
            self.event(
                EventKind::DmaWrite,
                (u64::from(address) << 32) | bytes as u64,
            );
        }
        let uart = self.bus.board.take_uart_tx();
        let uart_bytes = uart.len() as u64;
        if uart_bytes != 0 {
            panel_tx_log(self.cpu.icount, &uart);
            self.panel_leds.feed(&uart);
        }
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
        if let Some(frame) = self.frames.latest_for(self.mainloop_tcb)
            && (self.main_frame_latched || frame.lit_bytes > 0)
        {
            self.main_frame_latched = true;
            if self.current_frame.as_ref() != Some(&frame.raw) {
                self.publish_frame("main", frame.raw.clone());
            }
            return;
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

    /// Deliver the DSPI1 peer's interrupts once the guest's mask and priority
    /// allow them: first the RX completion (eDMA channel 14, INTC0 source 22,
    /// vector 86), then the peer's IRQ1 (EPORT, source 1, vector 65) that it
    /// raises after every transfer -- the command phase waits for it before
    /// reading the response back (`FUN_400124a0`/`FUN_40012500`).
    fn service_dspi1(&mut self) -> Result<bool, String> {
        let (source, vector) = if self.bus.board.dspi1_irq() {
            (22u32, 86u8)
        } else {
            (1, 65)
        };
        const INTC0_BASE: u32 = 0xfc04_8000;
        let level = self
            .bus
            .board
            .read8(INTC0_BASE + 0x40 + source)
            .map_err(|_| "DSPI1 ICR")?
            & 7;
        let imrl = self
            .bus
            .board
            .read32(INTC0_BASE + 0x0c)
            .map_err(|_| "DSPI1 IMRL")?;
        if level == 0 || (imrl >> source) & 1 != 0 || ((self.cpu.sr >> 8) & 7) >= u16::from(level)
        {
            if vector == 65 {
                // The pin pulsed while the guest had it masked: edge lost.
                self.irq1_pending = false;
            }
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
            vector,
            Some(level),
            InterruptPolicy::Oracle,
        ) {
            Ok(true) => {
                self.delivery_counts[usize::from(vector)] += 1;
                if vector == 86 {
                    self.bus.board.clear_dspi1_irq();
                    self.irq1_pending = true;
                } else {
                    self.irq1_pending = false;
                }
                Ok(true)
            }
            Ok(false) => Err("DSPI1 handler rejected".into()),
            Err(error) => Err(format!("DSPI1 delivery poisoned({error:?})")),
        }
    }

    /// Queue MIDI DIN bytes for the guest. `period` is the instruction count between two bytes
    /// (`midi::WIRE_PERIOD` is the real wire rate; 0 delivers as fast as the guest takes its interrupt).
    pub fn midi_in(&mut self, bytes: &[u8], period: u64) -> Result<usize, String> {
        if self.midi.queue.len() + bytes.len() > crate::midi::QUEUE_MAX {
            return Err("midi input queue full".into());
        }
        if !self.midi.busy() {
            self.midi.next_ic = self.cpu.icount;
        }
        self.midi.period = period;
        self.midi.queue.extend(bytes.iter().copied());
        Ok(self.midi.queue.len())
    }

    /// Queue bytes for the firmware's USB MIDI receive callback (cable 0). USB itself is not modelled: the bytes are
    /// handed to the callback in an interrupt taken at the next instruction boundary the guest allows, in chunks
    /// of at most 64 bytes one USB frame apart. Errors when this image has no resolvable USB MIDI callback.
    pub fn usb_midi_in(&mut self, bytes: &[u8]) -> Result<usize, String> {
        self.usb_midi_in_paced(bytes, None, None)
    }

    /// `usb_midi_in` with the delivery model chosen: `chunk` bytes per callback call (at most 384, the most one 512-byte packet of
    /// 4-byte events carries; default `USB_CHUNK`) and `period` instructions between two calls (default `USB_PERIOD`, one full-speed
    /// frame; a high-speed microframe is 16,500). The period stays for the later calls until another is given.
    pub fn usb_midi_in_paced(
        &mut self,
        bytes: &[u8],
        chunk: Option<usize>,
        period: Option<u64>,
    ) -> Result<usize, String> {
        use crate::midi::{resolve_usb, USB_CHUNK, USB_PERIOD};
        let chunk = chunk.unwrap_or(USB_CHUNK);
        if chunk == 0 || chunk > 384 {
            return Err("usb chunk is 1 to 384 bytes".into());
        }
        if !self.midi.usb.enabled {
            let (rx, tx) = resolve_usb(&self.main, MAIN_LOAD)
                .ok_or("this image has no USB MIDI callback this emulator knows")?;
            self.midi.usb.rx_cb = rx;
            self.midi.usb.tx_fn = tx;
            self.midi.usb.period = USB_PERIOD;
            self.midi.usb.enabled = true;
            self.rebuild_watch();
        }
        if let Some(p) = period {
            self.midi.usb.period = p;
        }
        if !self.midi.busy() {
            self.midi.usb.next_ic = self.cpu.icount;
        }
        for piece in bytes.chunks(chunk) {
            self.midi.usb.queue.push_back(piece.to_vec());
        }
        Ok(self.midi.usb.queue.len())
    }

    /// Start recording what the firmware sends to the USB MIDI transmit function without queueing input.
    pub fn usb_midi_watch(&mut self) -> Result<(), String> {
        self.usb_midi_in(&[]).map(|_| ())
    }

    /// The MIDI bytes the firmware handed to the USB transmit function since the last call.
    pub fn take_usb_out(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.midi.usb.out)
    }

    /// Hand the next chunk to the USB receive callback: an interrupt frame is pushed as for a hardware interrupt,
    /// the chunk and the callback's arguments are built below it, and the PC goes to the callback. The callback returns
    /// to an `rte` of the firmware; `observe_usb` sees it, pops the arguments and restores d0, d1, a0, a1.
    fn service_usb(&mut self) -> Result<bool, String> {
        const LEVEL: u8 = USB_LEVEL;
        if !self.midi.usb.enabled
            || self.midi.usb.active.is_some()
            || self.midi.usb.queue.is_empty()
            || self.cpu.icount < self.midi.usb.next_ic
            || ((self.cpu.sr >> 8) & 7) >= u16::from(LEVEL)
            || self.cpu.state != coldfire::RunState::Running
        {
            return Ok(false);
        }
        if self.midi.usb.ret_pc == 0 {
            // An `rte` at the end of the MIDI receive ISR: only this ISR returns here with these arguments.
            let handler = self
                .bus
                .board
                .read32(self.cpu.ctrl.vbr.wrapping_add(4 * u32::from(crate::midi::RX_VECTOR)))
                .map_err(|_| "usb: vector")?;
            let at = handler.checked_sub(MAIN_LOAD).ok_or("usb: handler")? as usize;
            let scan = self.main.get(at..at + 0x100).ok_or("usb: handler range")?;
            let rte = (0..scan.len() - 1)
                .step_by(2)
                .find(|&k| scan[k..k + 2] == [0x4e, 0x73])
                .ok_or("usb: no rte after the MIDI receive ISR")?;
            self.midi.usb.ret_pc = handler + rte as u32;
            self.rebuild_watch();
        }
        let chunk = self.midi.usb.queue.front().cloned().expect("queue checked");
        let stack = if self.cpu.sr & 0x2000 == 0 && self.cpu.ctrl.cacr & 0x20 != 0 {
            self.cpu.other_a7
        } else {
            self.cpu.a[7]
        };
        if !self.bus.board.can_write_ram_range((stack & !3).wrapping_sub(512 + chunk.len() as u32), 512 + chunk.len()) {
            return Ok(false);
        }
        let saved = [self.cpu.d[0], self.cpu.d[1], self.cpu.a[0], self.cpu.a[1]];
        match self.cpu.take_interrupt(
            &mut self.bus.board,
            crate::midi::RX_VECTOR,
            Some(LEVEL),
            InterruptPolicy::Oracle,
        ) {
            Ok(true) => {}
            Ok(false) => return Ok(false),
            Err(error) => return Err(format!("USB MIDI delivery poisoned({error:?})")),
        }
        let frame_sp = self.cpu.a[7];
        let data = frame_sp.wrapping_sub(chunk.len() as u32) & !3;
        self.poke_bytes(data, &chunk)?;
        let mut sp = data;
        for word in [chunk.len() as u32, data, self.midi.usb.ret_pc] {
            sp -= 4;
            self.bus.board.write32(sp, word).map_err(|_| "usb: stack write")?;
        }
        self.cpu.a[7] = sp;
        self.cpu.pc = self.midi.usb.rx_cb;
        self.midi.usb.queue.pop_front();
        self.midi.usb.in_bytes += chunk.len() as u64;
        self.midi.usb.calls += 1;
        self.midi.usb.next_ic = self.cpu.icount + self.midi.usb.period;
        self.midi.usb.active = Some(crate::midi::UsbCall {
            sp_after_rts: sp + 4,
            frame_sp,
            saved,
        });
        Ok(true)
    }

    /// At the transmit function: record its (length, buffer) arguments. At the callback's return: unwind.
    fn observe_usb(&mut self, pc: u32) {
        if pc == self.midi.usb.tx_fn {
            let sp = self.cpu.a[7];
            let len = self.bus.board.read32(sp.wrapping_add(4)).unwrap_or(0);
            let buf = self.bus.board.read32(sp.wrapping_add(8)).unwrap_or(0);
            if (1..=0x10000).contains(&len)
                && let Ok(bytes) = self.peek_bytes(buf, len as usize)
            {
                self.midi.usb.out.extend_from_slice(&bytes);
            }
        }
        if pc == self.midi.usb.ret_pc
            && let Some(call) = &self.midi.usb.active
            && self.cpu.a[7] == call.sp_after_rts
        {
            self.cpu.a[7] = call.frame_sp;
            [self.cpu.d[0], self.cpu.d[1], self.cpu.a[0], self.cpu.a[1]] = call.saved;
            self.midi.usb.active = None;
        }
    }

    /// The bytes the guest sent on the MIDI DIN output since the last call.
    pub fn take_midi_out(&mut self) -> Vec<u8> {
        let bytes = self.bus.board.take_midi_tx();
        self.midi.tx_bytes += bytes.len() as u64;
        bytes
    }

    pub fn midi_status(&self) -> crate::midi::MidiStatus {
        crate::midi::MidiStatus {
            queued: self.midi.queue.len(),
            rx_bytes: self.midi.rx_bytes,
            rx_irqs: self.midi.rx_irqs,
            tx_bytes: self.midi.tx_bytes,
            usb_queued: self.midi.usb.queue.iter().map(Vec::len).sum(),
            usb_in_bytes: self.midi.usb.in_bytes,
            usb_calls: self.midi.usb.calls,
            usb_out_bytes: self.midi.usb.out.len(),
        }
    }

    /// One MIDI byte at a time: store it where the receive DMA channel would (ring, DADDR), then offer the
    /// channel's interrupt when the guest's mask, priority level and stack allow it.
    fn service_midi(&mut self) -> Result<bool, String> {
        use crate::midi::{RX_SOURCE, RX_VECTOR as MIDI_VECTOR, TCD36_BASE, TCD36_DADDR};
        if !self.midi.pending {
            if self.cpu.icount < self.midi.next_ic || self.midi.queue.is_empty() {
                return Ok(false);
            }
            let board = &mut self.bus.board;
            let (Ok(saddr), Ok(attr), Ok(nbytes), Ok(daddr)) = (
                board.read32(TCD36_BASE),
                board.read16(TCD36_BASE + 4),
                board.read32(TCD36_BASE + 8),
                board.read32(TCD36_DADDR),
            ) else {
                return Ok(false);
            };
            // Set up by the driver: UART9 data register, one byte per minor loop, 8-bit, destination modulo 2^15.
            if saddr != machine::MIDI_UDR || attr != 0x0078 || nbytes != 1 {
                return Ok(false);
            }
            let base = daddr & !0x7fff;
            if !board.can_write_ram_range(base, 0x8000) {
                return Ok(false);
            }
            let byte = self.midi.queue.pop_front().expect("queue checked");
            board
                .write_guest(daddr, 1, u32::from(byte))
                .map_err(|_| "midi ring write")?;
            board
                .write32(TCD36_DADDR, base + ((daddr - base + 1) & 0x7fff))
                .map_err(|_| "midi DADDR write")?;
            self.midi.pending = true;
            self.midi.rx_bytes += 1;
            self.midi.next_ic = self.cpu.icount + self.midi.period;
        }
        let level = self
            .bus
            .board
            .read8(INTC1_BASE + 0x40 + RX_SOURCE)
            .map_err(|_| "midi ICR")?
            & 7;
        let imrl = self
            .bus
            .board
            .read32(INTC1_BASE + 0x0c)
            .map_err(|_| "midi IMRL")?;
        if level == 0
            || (imrl >> RX_SOURCE) & 1 != 0
            || ((self.cpu.sr >> 8) & 7) >= u16::from(level)
        {
            return Ok(false);
        }
        let handler = self
            .bus
            .board
            .read32(self.cpu.ctrl.vbr.wrapping_add(4 * u32::from(MIDI_VECTOR)))
            .map_err(|_| "midi vector")?;
        if !(MAIN_LOAD..MAIN_LOAD + self.main.len() as u32).contains(&handler) {
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
            MIDI_VECTOR,
            Some(level),
            InterruptPolicy::Oracle,
        ) {
            Ok(true) => {
                self.midi.rx_irqs += 1;
                self.midi.pending = false;
                Ok(true)
            }
            Ok(false) => Err("MIDI RX handler rejected".into()),
            Err(error) => Err(format!("MIDI RX delivery poisoned({error:?})")),
        }
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

/// The firmware releases the emulator boots, for messages: "Digitakt II 1.16 or 1.17, Digitone II 1.11 or 1.12".
fn supported_firmware(registry: &device_profile::Registry) -> String {
    registry
        .devices()
        .iter()
        .filter_map(|device| {
            let versions: Vec<&str> = device
                .firmwares
                .iter()
                .filter(|firmware| firmware.boot.is_some())
                .map(|firmware| firmware.version.as_str())
                .collect();
            (!versions.is_empty()).then(|| format!("{} {}", device.name, versions.join(" or ")))
        })
        .collect::<Vec<_>>()
        .join(", ")
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

/// Opt-in machine snapshot ("DT2SNP01"). It holds the mutable run state at a
/// `step_chunk` boundary: CPU, board (RAM, DMA/DSPI/SSI link, eSDHC and card,
/// timers), input queue, frame trackers and the counters that gate
/// readiness. Construction-time facts (firmware signatures, panel profile,
/// peer, card backing) come from building the same `Emulator` again, so a
/// snapshot only restores into a fresh emulator for the same firmware with
/// the same optional lanes enabled (SSI diagnostic), before any execution.
/// Telemetry marks, bus trace vectors and the DSPI capture recorder are not
/// state and start empty after a restore.
const SNAPSHOT_MAGIC: &[u8; 8] = b"DT2SNP01";

fn snap_frame(w: &mut periph::snap::Writer, f: &Frame) {
    w.u32(f.owner_tcb);
    w.u32(f.ptr);
    w.u64(f.icount);
    w.bytes(f.hash.as_bytes());
    w.u64(f.lit_bytes as u64);
    w.bytes(&f.raw);
}

fn load_frame(r: &mut periph::snap::Reader) -> Result<Frame, String> {
    Ok(Frame {
        owner_tcb: r.u32()?,
        ptr: r.u32()?,
        icount: r.u64()?,
        hash: String::from_utf8(r.bytes()?.to_vec()).map_err(|_| "snapshot frame hash")?,
        lit_bytes: r.u64()? as usize,
        raw: r.bytes()?.to_vec(),
    })
}

fn snap_opt3(w: &mut periph::snap::Writer, v: Option<(u32, u32, u32)>) {
    w.bool(v.is_some());
    let (a, b, c) = v.unwrap_or((0, 0, 0));
    w.u32(a);
    w.u32(b);
    w.u32(c);
}

fn load_opt3(r: &mut periph::snap::Reader) -> Result<Option<(u32, u32, u32)>, String> {
    let some = r.bool()?;
    let v = (r.u32()?, r.u32()?, r.u32()?);
    Ok(some.then_some(v))
}

fn snap_opt_bytes(w: &mut periph::snap::Writer, v: &Option<Vec<u8>>) {
    w.bool(v.is_some());
    w.bytes(v.as_deref().unwrap_or(&[]));
}

fn load_opt_bytes(r: &mut periph::snap::Reader) -> Result<Option<Vec<u8>>, String> {
    let some = r.bool()?;
    let v = r.bytes()?.to_vec();
    Ok(some.then_some(v))
}

impl Emulator {
    /// DT2SNP01 carries register backing, not storage/timer policy identity.
    /// Until a Device snapshot contract exists, both directions are restricted
    /// to the Oracle construction that produced the legacy format.
    fn validate_legacy_snapshot_policy(&self) -> Result<(), String> {
        if self.bus.board.completion_policy() != CompletionPolicy::Oracle {
            return Err("DT2SNP01 snapshots require Oracle completion policy; Device snapshots are unsupported".into());
        }
        let time = self
            .bus
            .board
            .time()
            .ok_or("DT2SNP01 snapshots require attached Oracle Time")?;
        if time.policy() != TimerPolicy::Oracle {
            return Err(
                "DT2SNP01 snapshots require Oracle timer policy; Device snapshots are unsupported"
                    .into(),
            );
        }
        Ok(())
    }

    /// Serialize the machine state (see `SNAPSHOT_MAGIC`). Call between
    /// `step_chunk` calls. Fails if the machine is faulted or holds state
    /// the format does not carry (FPU, unimplemented-form marker).
    pub fn save_state(&mut self) -> Result<Vec<u8>, String> {
        use periph::snap::Writer;
        self.validate_legacy_snapshot_policy()?;
        if self.error.is_some() {
            return Err("cannot snapshot a faulted emulator".into());
        }
        if periph::clock::cf_ips() != periph::clock::BASE_IPS {
            // Snapshots hold deadlines at the base rate; load_state rescales from it.
            return Err("save_state needs DIGI_CF_IPS unset (snapshots are taken at 132 MHz)".into());
        }
        if self.irq1_pending {
            return Err("a DSPI1 peer interrupt is pending".into());
        }
        self.cpu.resolve_nzv();
        if self.cpu.fpu.is_some() || self.cpu.last_unimplemented.is_some() {
            return Err("CPU holds state the snapshot does not carry".into());
        }
        let mut w = Writer::new();
        w.raw(SNAPSHOT_MAGIC);
        w.bytes(digest(&self.main).as_bytes());
        w.bytes(self.device.as_bytes());
        w.bytes(self.version.as_bytes());
        w.tag("CPU_");
        let c = &self.cpu;
        for v in c.d.iter().chain(&c.a) {
            w.u32(*v);
        }
        w.u32(c.other_a7);
        w.u32(c.pc);
        w.u16(c.sr);
        w.u32(c.ctrl.vbr);
        w.u32(c.ctrl.cacr);
        w.u32(c.ctrl.asid);
        for v in &c.ctrl.acr {
            w.u32(*v);
        }
        w.u32(c.ctrl.mmubar);
        w.u32(c.ctrl.rgpiobar);
        w.u32(c.ctrl.rambar);
        w.u32(c.emac.macsr);
        for v in &c.emac.acc {
            w.u32(*v);
        }
        w.u32(c.emac.accext01);
        w.u32(c.emac.accext23);
        w.u32(c.emac.mask);
        w.u8(match c.state {
            coldfire::RunState::Running => 0,
            coldfire::RunState::Stopped => 1,
            coldfire::RunState::Halted => 2,
        });
        w.u64(c.icount);
        w.u8(c.last_exception.unwrap_or(0));
        w.bool(c.last_exception.is_some());
        self.bus.board.snap_save(&mut w)?;
        w.tag("BUS_");
        w.u32(self.bus.current_pc);
        w.u64(self.bus.current_icount);
        w.u64(self.bus.access_dropped);
        w.u64(self.bus.cmdarg_writes);
        w.u64(self.bus.xfertyp_writes);
        w.u64(self.bus.gpio_reads);
        w.u64(self.bus.gpio_writes);
        w.u32(self.bus.last_cmdarg);
        w.u64(self.bus.unknown_touches.len() as u64);
        for page in self.bus.unknown_touches.keys() {
            w.u32(*page);
        }
        w.tag("EMU_");
        for v in [
            self.idle_passes,
            self.task_create_hits,
            self.mainloop_hits,
            self.job_pump_hits,
            self.intro_done_hits,
            self.display_start_hits,
            self.fs_starts,
            self.fs_completions,
            self.delivery_dropped,
            self.completion_events,
            self.dma_ranges,
            self.dma_bytes,
            self.uart_bytes,
            self.input_irqs,
            self.interpreted_instructions,
            self.idle_fast_forwarded_instructions,
            self.ram_clear_fast_forwarded_instructions,
            self.flash_hle_calls,
            self.frame_revision,
        ] {
            w.u64(v);
        }
        w.u32(self.mainloop_tcb);
        w.opt_u64(self.fs_last_complete);
        w.u8(match self.fs_success_result {
            None => 0,
            Some(false) => 1,
            Some(true) => 2,
        });
        snap_opt3(&mut w, self.fs_clear_pending);
        w.bool(self.fs_active);
        w.bool(self.main_frame_latched);
        snap_opt_bytes(&mut w, &self.current_frame);
        w.bool(self.frame_source.is_some());
        w.bytes(self.frame_source.as_deref().unwrap_or("").as_bytes());
        w.opt_u64(self.emitted_revision);
        for v in &self.delivery_counts {
            w.u64(*v);
        }
        w.u64(self.deliveries.len() as u64);
        for (vector, level) in &self.deliveries {
            w.u16(*vector);
            w.u8(*level);
        }
        for v in &self.completion_kind_counts {
            w.u64(*v);
        }
        w.raw(&self.held_masks);
        w.bytes(&self.input_packets.iter().copied().collect::<Vec<_>>());
        w.bool(self.rx_pending);
        w.bool(self.input_ready);
        w.bool(self.input_attempted_in_chunk);
        let k = &self.softfloat.counts;
        for v in [
            k.add_hits,
            k.mul_hits,
            k.div_hits,
            k.add_defers,
            k.mul_defers,
            k.div_defers,
        ] {
            w.u64(v);
        }
        w.tag("FRMS");
        w.u64(self.frames.pending.len() as u64);
        for p in &self.frames.pending {
            w.u32(p.owner_tcb);
            w.u32(p.return_pc);
            w.u32(p.expected_a7);
            snap_frame(&mut w, &p.frame);
        }
        w.u64(self.frames.completed.len() as u64);
        for (owner, queue) in &self.frames.completed {
            w.u32(*owner);
            w.u64(queue.len() as u64);
            for f in queue {
                snap_frame(&mut w, f);
            }
        }
        w.u64(self.frames.pending_dropped);
        w.u64(self.frames.completed_dropped);
        let t = &self.intro_frames;
        w.opt_u32(t.bitmap);
        for v in &t.seen {
            w.u64(*v);
        }
        w.u32(t.seen_pixels);
        snap_opt3(&mut w, t.pending);
        snap_opt_bytes(&mut w, &t.latest);
        w.tag("END_");
        Ok(w.buf)
    }

    /// After `load_state`: start the write watch when the restored state is past its `cacr` trigger.
    pub fn resume_watch(&mut self) {
        if let Some(w) = self.bus.guards.watch_mut() {
            w.resume();
        }
    }

    /// Restore a `save_state` blob into this freshly built emulator.
    pub fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        use periph::snap::Reader;
        self.validate_legacy_snapshot_policy()?;
        if self.cpu.icount != 0 || self.interpreted_instructions != 0 {
            return Err("load_state needs an emulator that has not executed".into());
        }
        let mut r = Reader::new(data);
        if r.raw(8)? != SNAPSHOT_MAGIC {
            return Err("not a DT2SNP01 snapshot".into());
        }
        self.snapshot_time_base();
        let main_sha = r.bytes()?;
        let device = r.bytes()?;
        let version = r.bytes()?;
        if main_sha != digest(&self.main).as_bytes()
            || device != self.device.as_bytes()
            || version != self.version.as_bytes()
        {
            return Err("snapshot was taken from different firmware".into());
        }
        r.tag("CPU_")?;
        let c = &mut self.cpu;
        for v in c.d.iter_mut().chain(c.a.iter_mut()) {
            *v = r.u32()?;
        }
        c.other_a7 = r.u32()?;
        c.pc = r.u32()?;
        c.sr = r.u16()?;
        c.ctrl.vbr = r.u32()?;
        c.ctrl.cacr = r.u32()?;
        c.ctrl.asid = r.u32()?;
        for v in &mut c.ctrl.acr {
            *v = r.u32()?;
        }
        c.ctrl.mmubar = r.u32()?;
        c.ctrl.rgpiobar = r.u32()?;
        c.ctrl.rambar = r.u32()?;
        c.emac.macsr = r.u32()?;
        for v in &mut c.emac.acc {
            *v = r.u32()?;
        }
        c.emac.accext01 = r.u32()?;
        c.emac.accext23 = r.u32()?;
        c.emac.mask = r.u32()?;
        c.state = match r.u8()? {
            0 => coldfire::RunState::Running,
            1 => coldfire::RunState::Stopped,
            2 => coldfire::RunState::Halted,
            _ => return Err("snapshot run state".into()),
        };
        c.icount = r.u64()?;
        let exc = r.u8()?;
        c.last_exception = r.bool()?.then_some(exc);
        c.last_unimplemented = None;
        self.bus.board.snap_load(&mut r)?;
        r.tag("BUS_")?;
        self.bus.current_pc = r.u32()?;
        self.bus.current_icount = r.u64()?;
        self.bus.access_dropped = r.u64()?;
        let cacr = self.cpu.ctrl.cacr;
        if let Some(w) = self.bus.guards.watch_mut() {
            w.set_cacr(cacr);
        }
        self.bus.cmdarg_writes = r.u64()?;
        self.bus.xfertyp_writes = r.u64()?;
        self.bus.gpio_reads = r.u64()?;
        self.bus.gpio_writes = r.u64()?;
        self.bus.last_cmdarg = r.u32()?;
        let n = r.len(1 << 16)?;
        self.bus.unknown_touches.clear();
        for _ in 0..n {
            let page = r.u32()?;
            self.bus.unknown_touches.insert(
                page,
                UnknownTouch {
                    pc: 0,
                    kind: "snapshot",
                    addr: page,
                    size: 0,
                    value: 0,
                },
            );
        }
        r.tag("EMU_")?;
        for slot in [
            &mut self.idle_passes,
            &mut self.task_create_hits,
            &mut self.mainloop_hits,
            &mut self.job_pump_hits,
            &mut self.intro_done_hits,
            &mut self.display_start_hits,
            &mut self.fs_starts,
            &mut self.fs_completions,
            &mut self.delivery_dropped,
            &mut self.completion_events,
            &mut self.dma_ranges,
            &mut self.dma_bytes,
            &mut self.uart_bytes,
            &mut self.input_irqs,
            &mut self.interpreted_instructions,
            &mut self.idle_fast_forwarded_instructions,
            &mut self.ram_clear_fast_forwarded_instructions,
            &mut self.flash_hle_calls,
            &mut self.frame_revision,
        ] {
            *slot = r.u64()?;
        }
        self.mainloop_tcb = r.u32()?;
        self.fs_last_complete = r.opt_u64()?;
        self.fs_success_result = match r.u8()? {
            0 => None,
            1 => Some(false),
            2 => Some(true),
            _ => return Err("snapshot fs result".into()),
        };
        self.fs_clear_pending = load_opt3(&mut r)?;
        self.fs_active = r.bool()?;
        self.main_frame_latched = r.bool()?;
        self.current_frame = load_opt_bytes(&mut r)?;
        let has_source = r.bool()?;
        let source = String::from_utf8(r.bytes()?.to_vec()).map_err(|_| "snapshot frame source")?;
        self.frame_source = has_source.then_some(source);
        self.emitted_revision = r.opt_u64()?;
        for v in &mut self.delivery_counts {
            *v = r.u64()?;
        }
        let n = r.len(1 << 24)?;
        self.deliveries.clear();
        for _ in 0..n {
            let vector = r.u16()?;
            let level = r.u8()?;
            self.deliveries.push((vector, level));
        }
        for v in &mut self.completion_kind_counts {
            *v = r.u64()?;
        }
        self.held_masks.copy_from_slice(r.raw(16)?);
        self.input_packets = r.bytes()?.iter().copied().collect();
        self.rx_pending = r.bool()?;
        self.input_ready = r.bool()?;
        self.input_attempted_in_chunk = r.bool()?;
        let k = &mut self.softfloat.counts;
        for slot in [
            &mut k.add_hits,
            &mut k.mul_hits,
            &mut k.div_hits,
            &mut k.add_defers,
            &mut k.mul_defers,
            &mut k.div_defers,
        ] {
            *slot = r.u64()?;
        }
        r.tag("FRMS")?;
        let n = r.len(1 << 16)?;
        self.frames.pending.clear();
        for _ in 0..n {
            let owner_tcb = r.u32()?;
            let return_pc = r.u32()?;
            let expected_a7 = r.u32()?;
            let frame = load_frame(&mut r)?;
            self.frames.pending.push_back(PendingFrame {
                owner_tcb,
                return_pc,
                expected_a7,
                frame,
            });
        }
        let owners = r.len(1 << 16)?;
        self.frames.completed.clear();
        for _ in 0..owners {
            let owner = r.u32()?;
            let n = r.len(1 << 16)?;
            let mut queue = VecDeque::new();
            for _ in 0..n {
                queue.push_back(load_frame(&mut r)?);
            }
            self.frames.completed.insert(owner, queue);
        }
        self.frames.pending_dropped = r.u64()?;
        self.frames.completed_dropped = r.u64()?;
        let t = &mut self.intro_frames;
        t.bitmap = r.opt_u32()?;
        for v in &mut t.seen {
            *v = r.u64()?;
        }
        t.seen_pixels = r.u32()?;
        t.pending = load_opt3(&mut r)?;
        t.latest = load_opt_bytes(&mut r)?;
        r.tag("END_")?;
        if !r.is_empty() {
            return Err("snapshot has trailing bytes".into());
        }
        #[cfg(feature = "diagnostic-events")]
        {
            self.dspi_frames_observed = self.bus.board.dma.dspi2.frames;
        }
        self.rebuild_watch();
        self.replay_restored_frame = true;
        self.rescale_to_cf_ips();
        Ok(())
    }

    /// Snapshots hold timer and SSI deadlines at [`periph::clock::BASE_IPS`]. While one loads,
    /// the clocks run at that base so the SSI check and the deadlines agree.
    fn snapshot_time_base(&mut self) {
        let base = periph::clock::BASE_IPS;
        if periph::clock::cf_ips() == base {
            return;
        }
        if let Some(time) = self.bus.board.time_mut() {
            time.set_ips(base as f64);
        }
        if let Some(ssi) = self.bus.board.dma.ssi.as_mut() {
            ssi.ips = base;
        }
    }

    /// After a load: move every deadline from the snapshot's base rate to `DIGI_CF_IPS`,
    /// keeping its emulated time.
    fn rescale_to_cf_ips(&mut self) {
        let ips = periph::clock::cf_ips();
        if ips == periph::clock::BASE_IPS {
            return;
        }
        let now = self.cpu.icount.saturating_add(self.softfloat.counts.calls());
        if let Some(time) = self.bus.board.time_mut() {
            time.rescale(now as f64, ips as f64);
        }
        if let Some(ssi) = self.bus.board.dma.ssi.as_mut() {
            ssi.rescale_at(now, ips);
        }
    }

    /// SHA-256 of the serialized machine state: a deterministic digest of
    /// CPU, RAM, peripherals and counters for determinism checks.
    pub fn state_digest(&mut self) -> Result<String, String> {
        Ok(digest(&self.save_state()?))
    }

    /// `state_digest` with the three counters of how the instructions were run (interpreted, idle skipped, RAM clear skipped) left out:
    /// two runs that reach the same guest state by different paths (stepping one instruction at a time, the batched loop, the idle skip)
    /// have the same digest. The oracle for a change to those paths (`state.digest {"arch": true}`).
    pub fn state_digest_arch(&mut self) -> Result<String, String> {
        let saved = (
            self.interpreted_instructions,
            self.idle_fast_forwarded_instructions,
            self.ram_clear_fast_forwarded_instructions,
        );
        (
            self.interpreted_instructions,
            self.idle_fast_forwarded_instructions,
            self.ram_clear_fast_forwarded_instructions,
        ) = (0, 0, 0);
        let digest = self.state_digest();
        (
            self.interpreted_instructions,
            self.idle_fast_forwarded_instructions,
            self.ram_clear_fast_forwarded_instructions,
        ) = saved;
        digest
    }
}

/// Every fused loop in MAIN (`Loop::recognise` at each halfword).
fn resolve_fused_loops(main: &[u8]) -> Vec<(u32, Loop, Vec<u8>)> {
    (0..main.len())
        .step_by(2)
        .filter_map(|at| {
            let lp = Loop::recognise(&main[at..])?;
            Some((MAIN_LOAD + at as u32, lp, lp.code()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_ram_keeps_the_bytes_around_an_unaligned_range() {
        let Ok(syx) = std::fs::read("../../../firmware/Digitone_II_OS1.12.syx") else {
            return;
        };
        let mut runtime = Emulator::new(&syx, None).unwrap();
        let base = 0x4400_0000; // an unmapped page: mapped zeroed on first write
        runtime.write_ram(base, &[0xaa; 12]).unwrap();
        runtime.write_ram(base + 3, &[1, 2, 3, 4, 5]).unwrap();
        assert_eq!(
            runtime.peek_ram(base, 12),
            [0xaa, 0xaa, 0xaa, 1, 2, 3, 4, 5, 0xaa, 0xaa, 0xaa, 0xaa]
        );
        assert!(runtime.write_ram(0x1000, &[0]).is_err(), "outside SDRAM");
        assert!(runtime.write_ram(0x4fff_fffe, &[0; 4]).is_err(), "past the end");
        runtime.write_ram(base, &[]).unwrap();
    }

    #[test]
    fn ssi_delivery_precedes_same_boundary_timer_and_is_recorded_without_fault() {
        let Ok(syx) = std::fs::read("../../../firmware/Digitone_II_OS1.12.syx") else {
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

    /// Every signature-resolved anchor of the boot path resolves on the
    /// current firmware of both products, relinked builds included.
    #[test]
    fn boot_anchors_resolve_on_every_supported_firmware() {
        for file in ["Digitakt_II_OS1.17.syx", "Digitone_II_OS1.12.syx"] {
            let Ok(syx) = std::fs::read(format!("../../../firmware/{file}")) else {
                continue;
            };
            let emu = Emulator::new_with_policy(&syx, None, ExecutionPolicy::SoftfloatAbiV1)
                .unwrap_or_else(|e| panic!("{file}: {e}"));
            assert!(emu.softfloat.enabled, "{file}: softfloat {:?}", emu.softfloat.reason);
            assert!(emu.ram_clear.is_some(), "{file}: RAM clear loop");
            assert!(!emu.fused_loops.is_empty(), "{file}: fused loops");
            assert!(!emu.idle_spins.is_empty(), "{file}: idle spins");
        }
    }

    #[test]
    fn ram_clear_batches_match_interpreter_across_chunks_pages_and_final_flags() {
        for file in ["Digitakt_II_OS1.17.syx", "Digitone_II_OS1.12.syx"] {
            let Ok(syx) = std::fs::read(format!("../../../firmware/{file}")) else {
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

    #[test]
    fn ram_clear_declines_modified_code_inputs_active_timers_and_invalid_state() {
        let Ok(syx) = std::fs::read("../../../firmware/Digitakt_II_OS1.17.syx") else {
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
        let Ok(syx) = std::fs::read("../../../firmware/Digitakt_II_OS1.17.syx") else {
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
    fn restored_frame_is_published_once_to_a_fresh_observer() {
        let Ok(syx) = std::fs::read("../../../firmware/Digitakt_II_OS1.17.syx") else {
            return;
        };
        let mut saved = Emulator::new(&syx, None).unwrap();
        saved.current_frame = Some(vec![0xa5; PANEL_BYTES]);
        saved.frame_revision = 42;
        saved.frame_source = Some("main".into());
        assert_eq!(saved.snapshot().frame, Some(vec![0xa5; PANEL_BYTES]));
        let state = saved.save_state().unwrap();

        let mut restored = Emulator::new(&syx, None).unwrap();
        restored.load_state(&state).unwrap();
        assert_eq!(restored.frame_revision, 42);
        assert_eq!(restored.frame_source.as_deref(), Some("main"));
        assert_eq!(restored.save_state().unwrap(), state);
        assert_eq!(restored.snapshot().frame, Some(vec![0xa5; PANEL_BYTES]));
        assert!(restored.snapshot().frame.is_none());
        assert_eq!(restored.frame_revision, 42);
        assert_eq!(restored.frame_source.as_deref(), Some("main"));
    }

    fn with_legacy_snapshot_firmware(mut check: impl FnMut(&[u8])) {
        for (env, sha) in [
            (
                "DIGI_LEGACY_SNAPSHOT_DT2_SYX",
                "26c22f6652625ac2cfd47f7ee970d388ed8b6427dae3563c0a6a2f2d334350d5",
            ),
            (
                "DIGI_LEGACY_SNAPSHOT_DN2_SYX",
                "7e5b8a733d4ab1fdf7e5d22757f7d8eddaa3cbf71d0858874a791844c02ceb35",
            ),
        ] {
            let Some(path) = std::env::var_os(env) else {
                eprintln!(
                    "SKIP legacy snapshot policy proof: set {env} to the exact current stock image"
                );
                continue;
            };
            let syx = std::fs::read(path)
                .unwrap_or_else(|error| panic!("configured {env} must be readable: {error}"));
            assert_eq!(
                digest(&syx),
                sha,
                "{env} requires exact current stock firmware"
            );
            check(&syx);
        }
    }

    #[test]
    fn legacy_snapshot_oracle_roundtrip_keeps_format_and_bytes() {
        with_legacy_snapshot_firmware(|syx| {
            let mut saved = Emulator::new(syx, None).unwrap();
            saved.cpu.d[7] = 0x51a7_0017;
            saved
                .write_ram(0x4400_0000, &0xaabb_ccddu32.to_be_bytes())
                .unwrap();
            saved.bus.board.write8(0xfc05_004d, 3).unwrap();
            let state = saved.save_state().unwrap();
            assert_eq!(&state[..8], b"DT2SNP01");
            let mut restored = Emulator::new(syx, None).unwrap();
            restored.load_state(&state).unwrap();
            assert_eq!(restored.cpu, saved.cpu);
            assert_eq!(
                restored.peek_ram(0x4400_0000, 4),
                0xaabb_ccddu32.to_be_bytes()
            );
            assert_eq!(restored.bus.board.read8(0xfc05_004d).unwrap(), 3);
            assert_eq!(restored.save_state().unwrap(), state);
        });
    }

    fn legacy_snapshot_board_bytes(runtime: &Emulator) -> Vec<u8> {
        let mut writer = periph::snap::Writer::new();
        runtime.bus.board.snap_save(&mut writer).unwrap();
        writer.buf
    }

    #[test]
    fn legacy_snapshot_unsupported_policies_reject_before_mutation() {
        with_legacy_snapshot_firmware(|syx| {
            let mut source = Emulator::new(syx, None).unwrap();
            source.cpu.d[7] = 0x7777_7777;
            source.write_ram(0x4400_0000, &[0x55; 8]).unwrap();
            let state = source.save_state().unwrap();
            for mode in 0..3 {
                let mut runtime = Emulator::new(syx, None).unwrap();
                let expected_error = match mode {
                    0 => {
                        runtime.bus.board = Board::new(
                            Card::default(),
                            Default::default(),
                            CompletionPolicy::Device,
                        );
                        runtime.bus.board.attach_time(Time::new(
                            TimerPolicy::Oracle,
                            vec![0],
                            66_000_000.0,
                        ));
                        "Oracle completion policy"
                    }
                    1 => {
                        runtime.bus.board.attach_time(Time::new(
                            TimerPolicy::Device,
                            vec![0],
                            66_000_000.0,
                        ));
                        "Oracle timer policy"
                    }
                    2 => {
                        runtime.bus.board.take_time();
                        "attached Oracle Time"
                    }
                    _ => unreachable!(),
                };
                // A deferred flag update makes an accidental resolve_nzv on
                // failed save observable through the complete CPU comparison.
                runtime.cpu = Cpu::new();
                runtime.cpu.pc = 0x4400_0000;
                runtime.cpu.sr = 0x2010;
                runtime
                    .write_ram(0x4400_0000, &[0x70, 0xff, 0xa5, 0x5a])
                    .unwrap(); // MOVEQ #-1,D0
                runtime.cpu.step(&mut runtime.bus.board).unwrap();
                runtime.cpu.icount = 0; // fresh-state load prerequisite, with flags still deferred
                runtime.cpu.d[7] = 0x1234_5678;
                runtime.bus.current_icount = 1234;
                runtime.bus.current_pc = runtime.cpu.pc;
                runtime.idle_passes = 37;
                if let Some(time) = runtime.bus.board.time_mut() {
                    time.write(0xfc05_004d, 1, 5);
                    time.write(0xfc08_0002, 2, 9); // PIT0 PMR
                    time.write(0xfc08_0000, 2, 0x000b);
                    assert!(time.deadline(1234).is_some());
                }
                let before_cpu = runtime.cpu.clone();
                let mut resolved_cpu = before_cpu.clone();
                resolved_cpu.resolve_nzv();
                assert_ne!(before_cpu, resolved_cpu, "fixture must hold deferred flags");
                let before_board = legacy_snapshot_board_bytes(&runtime);
                let before_policy = (
                    runtime.bus.board.completion_policy(),
                    runtime.bus.board.time().map(Time::policy),
                );
                let before_counters = (
                    runtime.bus.current_icount,
                    runtime.bus.current_pc,
                    runtime.idle_passes,
                    runtime.interpreted_instructions,
                );
                let error = runtime.save_state().unwrap_err();
                assert!(error.contains(expected_error), "{error}");
                assert_eq!(
                    runtime.cpu, before_cpu,
                    "save rejection leaves deferred flags intact"
                );
                assert_eq!(legacy_snapshot_board_bytes(&runtime), before_board);
                // A valid Oracle blob must not deserialize into this unsupported
                // construction, even though its CPU execution count is zero.
                let error = runtime.load_state(&state).unwrap_err();
                assert!(error.contains(expected_error), "{error}");
                assert_eq!(
                    runtime.cpu, before_cpu,
                    "restore rejection preserves the constructed CPU"
                );
                assert_eq!(
                    legacy_snapshot_board_bytes(&runtime),
                    before_board,
                    "restore rejection preserves RAM, registers and timer deadlines"
                );
                assert_eq!(
                    (
                        runtime.bus.board.completion_policy(),
                        runtime.bus.board.time().map(Time::policy)
                    ),
                    before_policy
                );
                assert_eq!(
                    (
                        runtime.bus.current_icount,
                        runtime.bus.current_pc,
                        runtime.idle_passes,
                        runtime.interpreted_instructions
                    ),
                    before_counters
                );
                // Policy rejection also precedes parsing a malformed input.
                assert!(
                    runtime
                        .load_state(b"invalid")
                        .unwrap_err()
                        .contains(expected_error)
                );
                assert_eq!(runtime.cpu, before_cpu);
            }
        });
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

    /// Code in DDR (outside MAIN), written at run time and rewritten by the
    /// guest itself, runs under `step_once` and under the batched `run_fast`
    /// to the same machine; a PC outside RAM still stops with
    /// `UnsupportedGuestPc`.
    #[test]
    fn code_in_ddr_runs_and_sees_its_own_writes() {
        let Ok(syx) = std::fs::read("../../../firmware/Digitakt_II_OS1.17.syx") else {
            return;
        };
        let base = 0x47e8_b000u32;
        let routine = base + 0x40;
        let mut code: Vec<u8> = Vec::new();
        let w16 = |c: &mut Vec<u8>, v: u16| c.extend_from_slice(&v.to_be_bytes());
        let w32 = |c: &mut Vec<u8>, v: u32| c.extend_from_slice(&v.to_be_bytes());
        w16(&mut code, 0x207c); // movea.l #routine,A0
        w32(&mut code, routine);
        w16(&mut code, 0x4eb9); // jsr routine
        w32(&mut code, routine);
        w16(&mut code, 0x2601); // move.l D1,D3
        w16(&mut code, 0x30bc); // move.w #moveq #2,D1,(A0)
        w16(&mut code, 0x7202);
        w16(&mut code, 0x4eb9); // jsr routine (patched by the guest)
        w32(&mut code, routine);
        w16(&mut code, 0x60fe); // bra.s *
        let mut emus = Vec::new();
        for fast in [false, true] {
            let mut emu = Emulator::new(&syx, None).unwrap();
            emu.bus.board.map_zeroed_ram_page(base & !0xf_ffff).unwrap();
            for (i, b) in code.iter().enumerate() {
                emu.bus.board.write_guest(base + i as u32, 1, u32::from(*b)).unwrap();
            }
            for (i, b) in [0x72u8, 0x01, 0x4e, 0x75].iter().enumerate() {
                emu.bus.board.write_guest(routine + i as u32, 1, u32::from(*b)).unwrap();
            }
            emu.cpu.pc = base;
            emu.cpu.sr = 0x2700;
            emu.cpu.a[7] = base + 0x800;
            emu.rebuild_watch();
            if fast {
                emu.step_chunk(60);
            } else {
                for _ in 0..60 {
                    emu.step_once();
                }
            }
            assert_eq!(emu.error, None);
            assert_eq!(emu.cpu.d[3], 1, "first call ran the original routine");
            assert_eq!(emu.cpu.d[1], 2, "second call ran the rewritten routine");
            assert_eq!(emu.cpu.pc, base + 0x18);
            emus.push(emu);
        }
        assert_eq!(emus[0].cpu, emus[1].cpu);
        let mut off = Emulator::new(&syx, None).unwrap();
        off.cpu.pc = 0xfc00_0000;
        off.step_once();
        assert_eq!(off.error.as_deref(), Some("UnsupportedGuestPc(0xfc000000)"));
    }

    #[test]
    fn idle_advance_matches_reference_cpu_and_guest_clock() {
        let Ok(syx) = std::fs::read("../../../firmware/Digitakt_II_OS1.17.syx") else {
            return;
        };
        let mut slow = Emulator::new(&syx, None).unwrap();
        let mut fast = Emulator::new(&syx, None).unwrap();
        let pc = MAIN_LOAD + 0x100;
        for emu in [&mut slow, &mut fast] {
            emu.cpu.pc = pc;
            emu.bus.board.write16(pc, 0x60fe).unwrap();
            emu.idle_spins.insert(pc);
            emu.rebuild_watch();
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

    /// The counting idle loop (`addq.l #1,(counter).l; bra.s`) placed in DDR at
    /// run time is learned from the PC and fast-forwarded to the machine the
    /// one-instruction loop reaches: CPU (flags included), counter, guest clock.
    #[test]
    fn counting_idle_loop_matches_reference() {
        let Ok(syx) = std::fs::read("../../../firmware/Digitakt_II_OS1.17.syx") else {
            return;
        };
        let base = 0x47e8_b000u32;
        let counter = base + 0x100;
        let mut code = vec![0x52, 0xb9];
        code.extend_from_slice(&counter.to_be_bytes());
        code.extend_from_slice(&[0x60, 0xf8]);
        for start in [base, base + 6] {
            for init in [0u32, 0xffff_fff0, 0x7fff_ffe0] {
                let mut emus = Vec::new();
                for fast in [false, true] {
                    let mut emu = Emulator::new(&syx, None).unwrap();
                    emu.bus.board.map_zeroed_ram_page(base & !0xf_ffff).unwrap();
                    for (i, b) in code.iter().enumerate() {
                        emu.bus.board.write_guest(base + i as u32, 1, u32::from(*b)).unwrap();
                    }
                    emu.bus.board.write_guest(counter, 4, init).unwrap();
                    emu.cpu.pc = start;
                    emu.cpu.sr = 0x2700;
                    emu.cpu.a[7] = base + 0x800;
                    emu.rebuild_watch();
                    if fast {
                        emu.step_chunk(401);
                    } else {
                        for _ in 0..401 {
                            emu.step_once();
                        }
                    }
                    assert_eq!(emu.error, None);
                    emus.push(emu);
                }
                let (slow_part, fast_part) = emus.split_at_mut(1);
                let (slow, fast) = (&mut slow_part[0], &mut fast_part[0]);
                assert!(fast.idle_fast_forwarded_instructions >= 390);
                assert_eq!(fast.cpu, slow.cpu);
                let (mut a, mut b) = ([0u8; 4], [0u8; 4]);
                assert!(fast.bus.board.ram_peek(counter, &mut a));
                assert!(slow.bus.board.ram_peek(counter, &mut b));
                assert_eq!(a, b);
                assert_ne!(u32::from_be_bytes(a), init);
                assert_eq!(fast.oracle_ticks(), slow.oracle_ticks());
                assert_eq!(fast.bus.current_icount, slow.bus.current_icount);
                assert_eq!(fast.idle_passes, slow.idle_passes);
            }
        }
    }

    /// `step_chunk` (batched `run_fast`) must leave exactly the machine the
    /// one-instruction `step_once` loop leaves, across cold-boot code with
    /// reset RAM clears, timers and MMIO.
    #[test]
    fn batched_chunks_match_single_steps_from_cold_boot() {
        let Ok(syx) = std::fs::read("../../../firmware/Digitakt_II_OS1.17.syx") else {
            return;
        };
        let mut slow = Emulator::new(&syx, None).unwrap();
        let mut fast = Emulator::new(&syx, None).unwrap();
        let mut done = 0u64;
        let chunks = [1u32, 7, 1000].into_iter().chain([250_000; 24]);
        for chunk in chunks {
            let before = slow.cpu.icount;
            fast.step_chunk(chunk);
            // Reference: single steps with the same analytic idle/RAM-clear
            // steps `step_chunk` applies, but never the batched loop.
            let mut iterations = 0;
            while iterations < chunk && slow.error.is_none() {
                if ACCELERATE_RAM_CLEAR
                    && slow
                        .ram_clear
                        .as_ref()
                        .is_some_and(|clear| slow.cpu.pc == clear.loop_pc)
                {
                    let count = slow.advance_ram_clear(chunk - iterations);
                    if count != 0 {
                        iterations += count;
                        continue;
                    }
                }
                let previous_pc = slow.cpu.pc;
                slow.step_once();
                iterations += 1;
                iterations += slow.advance_idle(previous_pc, chunk - iterations);
            }
            slow.refresh_frame();
            done += slow.cpu.icount - before;
            assert_eq!(fast.error, slow.error);
            assert_eq!(fast.cpu, slow.cpu, "after {done} instructions");
            assert_eq!(fast.oracle_ticks(), slow.oracle_ticks());
            assert_eq!(fast.interpreted_instructions, slow.interpreted_instructions);
            assert_eq!(
                fast.state_digest().unwrap(),
                slow.state_digest().unwrap(),
                "after {done} instructions"
            );
        }
        assert!(done > 5_000_000);
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
        let Ok(syx) = std::fs::read("../../../firmware/Digitakt_II_OS1.17.syx") else {
            return;
        };
        let runtime = Emulator::new(&syx, None).unwrap();
        assert_eq!(runtime.cpu.icount, 0);
        assert_eq!(runtime.input_packets.len(), 0);
    }

    #[test]
    fn panel_packets_keep_chords_split_signed_turns_and_reserve_capacity() {
        let Ok(syx) = std::fs::read("../../../firmware/Digitakt_II_OS1.17.syx") else {
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

/// `DIGI_PANEL_TX_LOG=<path>`: append one line `<icount> <hex bytes>` per UART8 drain
/// (front-panel LED stream), and `<icount> @button <code> <d|u>` when a panel button is injected.
/// The path is read once; unset costs one atomic load per drain.
fn panel_tx_write(icount: u64, text: &str) {
    use std::io::Write;
    use std::sync::{Mutex, OnceLock};
    static SINK: OnceLock<Option<Mutex<std::io::BufWriter<std::fs::File>>>> = OnceLock::new();
    let sink = SINK.get_or_init(|| {
        let path = std::env::var_os("DIGI_PANEL_TX_LOG")?;
        std::fs::File::create(path)
            .ok()
            .map(|f| Mutex::new(std::io::BufWriter::new(f)))
    });
    if let Some(sink) = sink {
        let mut w = sink.lock().unwrap();
        let _ = writeln!(w, "{icount} {text}");
        let _ = w.flush();
    }
}

fn panel_tx_log(icount: u64, bytes: &[u8]) {
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    panel_tx_write(icount, &hex);
}
