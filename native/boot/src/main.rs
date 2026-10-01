//! Bounded diagnostic for the documented Python MAIN_OS entry contract.
//!
//! This explicitly enables bounded Oracle compatibility services only when
//! requested by the diagnostic CLI; it is not a hardware boot contract.

mod common;

use common::*;

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    env, fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use coldfire::{Bus, Cpu, InterruptPolicy, decode_at};
use dt2_firmware_loader::{decode_syx, parse};
use emmc_card::{
    Card, DEFAULT_CAPACITY_BLOCKS, RandomAccessRead, SECTOR_SIZE, SMALL_CAPACITY_BLOCKS,
};
use machine::{Board, CompletionEvent, CompletionPolicy, SemaphoreAddresses, Time, TimerPolicy};
use serde_json::json;
use sha2::{Digest, Sha256};

#[derive(Debug)]
struct Cli {
    syx: PathBuf,
    out: PathBuf,
    limit: u64,
    stop_at: String,
    progress_every: u64,
    diagnostic_services: bool,
    card_image: Option<PathBuf>,
}

fn usage() -> &'static str {
    "usage: elektron-native-boot --syx ABS --out ABS_DIR --mode oracle-diagnostic --limit N [--card-image ABS_FILE] [--stop-at ready|limit] [--progress-every N] [--diagnostic-services]"
}

fn cli() -> Result<Cli, String> {
    let mut syx = None;
    let mut out = None;
    let mut mode = None;
    let mut limit = None;
    let mut stop_at = "ready".to_string();
    let mut progress_every = 10_000_000;
    let mut diagnostic_services = false;
    let mut card_image = None;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!("{}", usage());
                std::process::exit(0)
            }
            "--syx" => {
                if syx.is_some() {
                    return Err("duplicate --syx".into());
                }
                syx = args.next().map(PathBuf::from)
            }
            "--out" => {
                if out.is_some() {
                    return Err("duplicate --out".into());
                }
                out = args.next().map(PathBuf::from)
            }
            "--card-image" => {
                if card_image.is_some() {
                    return Err("duplicate --card-image".into());
                }
                card_image = args.next().map(PathBuf::from)
            }
            "--mode" => mode = args.next(),
            "--limit" => limit = args.next().and_then(|v| v.parse().ok()),
            "--stop-at" => stop_at = args.next().ok_or("missing --stop-at value")?,
            "--progress-every" => {
                progress_every = args
                    .next()
                    .ok_or("missing --progress-every value")?
                    .parse()
                    .map_err(|_| "invalid --progress-every")?
            }
            "--diagnostic-services" => diagnostic_services = true,
            _ => return Err(format!("unknown or incomplete argument {arg}")),
        }
    }
    let syx = syx.ok_or("--syx is required")?;
    let out = out.ok_or("--out is required")?;
    let limit = limit.ok_or("--limit is required")?;
    if !syx.is_absolute() || !out.is_absolute() {
        return Err("--syx and --out must be absolute".into());
    }
    if mode.as_deref() != Some("oracle-diagnostic") {
        return Err("--mode must be oracle-diagnostic".into());
    }
    if !(1..=LIMIT).contains(&limit)
        || progress_every == 0
        || !matches!(stop_at.as_str(), "ready" | "limit")
    {
        return Err("invalid limit, progress interval, or stop target".into());
    }
    Ok(Cli {
        syx,
        out,
        limit,
        stop_at,
        progress_every,
        diagnostic_services,
        card_image,
    })
}

struct Image {
    file: Mutex<fs::File>,
    len: u64,
    error: Arc<Mutex<Option<String>>>,
}
impl RandomAccessRead for Image {
    fn len(&self) -> u64 {
        self.len
    }
    fn read_at(&self, offset: u64, dest: &mut [u8]) -> usize {
        if offset
            .checked_add(dest.len() as u64)
            .is_none_or(|end| end > self.len)
        {
            if let Ok(mut error) = self.error.lock() {
                error.get_or_insert(
                    "UnexpectedEof(card image request outside preflight length)".into(),
                );
            }
            return 0;
        }
        let Ok(mut file) = self.file.lock() else {
            if let Ok(mut error) = self.error.lock() {
                error.get_or_insert("card image lock poisoned".into());
            }
            return 0;
        };
        let result = file
            .seek(SeekFrom::Start(offset))
            .and_then(|_| file.read_exact(dest));
        if let Err(error) = result {
            if let Ok(mut first) = self.error.lock() {
                first.get_or_insert_with(|| format!("card image read at {offset:#x}: {error}"));
            }
            return 0;
        }
        dest.len()
    }
}

struct CardImage {
    file: fs::File,
    len: u64,
    blocks: u32,
    sha256: String,
    error: Arc<Mutex<Option<String>>>,
}

fn open_card_image(path: &Path) -> Result<CardImage, String> {
    let metadata = fs::metadata(path).map_err(|e| format!("card image metadata: {e}"))?;
    if !metadata.file_type().is_file() {
        return Err("--card-image must be a regular readable file".into());
    }
    let len = metadata.len();
    let blocks = card_blocks(len)?;
    let mut file = fs::File::open(path).map_err(|e| format!("open card image: {e}"))?;
    let mut hash = Sha256::new();
    let mut read_total = 0u64;
    let mut buffer = [0u8; 1024 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|e| format!("read card image: {e}"))?;
        if read == 0 {
            break;
        }
        read_total = read_total
            .checked_add(read as u64)
            .ok_or("card image length overflow")?;
        hash.update(&buffer[..read]);
    }
    if read_total != len
        || fs::metadata(path)
            .map_err(|e| format!("card image restat: {e}"))?
            .len()
            != len
    {
        return Err("card image changed while hashing".into());
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|e| format!("rewind card image: {e}"))?;
    Ok(CardImage {
        file,
        len,
        blocks,
        sha256: hash.finalize().iter().map(|b| format!("{b:02x}")).collect(),
        error: Arc::new(Mutex::new(None)),
    })
}

fn card_blocks(len: u64) -> Result<u32, String> {
    if len == 0 || len % SECTOR_SIZE as u64 != 0 {
        return Err("--card-image length must be a nonzero multiple of 512".into());
    }
    let blocks = u32::try_from(len / SECTOR_SIZE as u64)
        .map_err(|_| "--card-image capacity exceeds u32 sectors")?;
    if matches!(blocks, DEFAULT_CAPACITY_BLOCKS | SMALL_CAPACITY_BLOCKS) {
        Ok(blocks)
    } else {
        Err(format!(
            "--card-image has unsupported capacity {blocks:#x} sectors"
        ))
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let cli = cli()?;
    let syx = fs::read(&cli.syx).map_err(|e| format!("read SYX: {e}"))?;
    let syx_sha = digest(&syx);
    let firmware = parse(&syx).map_err(|e| format!("parse SYX/ELE3: {e}"))?;
    let decoded = decode_syx(&syx).map_err(|e| format!("decode SYX transport: {e}"))?;
    let container_start = decoded
        .windows(4)
        .position(|bytes| bytes == b"ELE3")
        .ok_or("ELE3 container missing")?;
    if container_start != firmware.container_offset {
        return Err("loader container offset mismatch".into());
    }
    let container = &decoded[container_start..];
    let mains: Vec<_> = firmware.sections.iter().filter(|s| s.id == 3).collect();
    if mains.len() != 1 {
        return Err("require exactly one MAIN_OS section 3".into());
    }
    let main = mains[0];
    if main.destination != MAIN_LOAD {
        return Err("section 3 MAIN_OS destination is not 0x40000400".into());
    }
    let main_sha = digest(&main.bytes);
    let entry_offset = ENTRY
        .checked_sub(MAIN_LOAD)
        .map(|v| v as usize)
        .ok_or("entry is below MAIN load")?;
    if main.bytes.get(entry_offset..entry_offset + 6) != Some(&[0x41, 0xef, 0x00, 0x04, 0x23, 0xd0])
    {
        return Err("entry prefix verification failed".into());
    }
    let registry = device_profile::Registry::embedded().map_err(|e| e.to_string())?;
    let (device, firmware_profile, boot) = registry
        .boot_for_main(&syx_sha, &main.bytes)
        .map_err(|e| e.to_string())?;
    let readiness_contract = firmware_profile
        .readiness_contract
        .ok_or("firmware has no readiness contract")?;
    let task_create = if main
        .bytes
        .get((0x4000_12c8 - MAIN_LOAD) as usize..)
        .is_some_and(|b| b.starts_with(&[0x20, 0x2f, 0x00, 0x0c, 0x72, 0xfc, 0xc2, 0xaf]))
    {
        0x4000_12c8
    } else {
        return Err("task_create prefix verification failed".into());
    };
    let mainloop = unique_signature_data(&main.bytes, MAINLOOP_SIG, Some(15))
        .ok_or("mainloop signature ambiguous")?;
    let panel_diff = unique(
        &main.bytes,
        PANEL_DIFF_SIG,
        &data_mask(PANEL_DIFF_SIG, None),
    )?;
    let (fb_front, fb_back) = operand_pair(&main.bytes, panel_diff)?;
    let job_pump =
        unique_signature(&main.bytes, JOB_PUMP_SIG, None).ok_or("job_pump signature ambiguous")?;
    let flash_read = unique(
        &main.bytes,
        FLASH_READ_SIG,
        &vec![false; FLASH_READ_SIG.len()],
    )?;
    let fs_worker = match readiness_contract {
        device_profile::ReadinessContract::MainPanelFsCheckV1 => {
            Some(resolve_fs_worker(&main.bytes)?)
        }
        device_profile::ReadinessContract::MainPanelV1 => None,
    };
    let (sd_bringup, sd_semaphore_base, sd_semaphores) = if cli.diagnostic_services {
        let (base, addresses) = resolve_sd_semaphores(&main.bytes)?;
        (
            Some(unique(
                &main.bytes,
                SD_BRINGUP_SIG,
                &data_mask(SD_BRINGUP_SIG, None),
            )?),
            Some(base),
            addresses,
        )
    } else {
        (None, None, SemaphoreAddresses::default())
    };
    let intro_done_sig = hex(
        "424048794313120845f94000141a33c0fc08c000701013c0fc05001c4eb94000155c588f4879431312004e92588f60f4",
    );
    let intro_done = unique(
        &main.bytes,
        &intro_done_sig,
        &data_mask(&intro_done_sig, None),
    )?;
    let intro_off = (intro_done - MAIN_LOAD) as usize;
    let frame_sem = u32::from_be_bytes(
        main.bytes[intro_off + 4..intro_off + 8]
            .try_into()
            .map_err(|_| "intro_done frame semaphore operand out of range")?,
    )
    .wrapping_sub(8);
    let intro_isr_sig = hex(
        "4feffff048d7030341f9fc08c00072043010487943131200808130804eb94000148c4cef030300044fef0014",
    );
    let mut isr_mask = data_mask(&intro_isr_sig, None);
    isr_mask[20..24].fill(true);
    let isr_hits: Vec<_> = main
        .bytes
        .windows(intro_isr_sig.len())
        .enumerate()
        .filter(|(_, b)| {
            b.iter()
                .enumerate()
                .all(|(i, x)| isr_mask[i] || *x == intro_isr_sig[i])
        })
        .filter(|(i, _)| {
            u32::from_be_bytes(main.bytes[*i + 20..*i + 24].try_into().unwrap()) == frame_sem
        })
        .map(|(i, _)| MAIN_LOAD + i as u32)
        .collect();
    if isr_hits.len() != 1 {
        return Err(format!(
            "intro PIT3 ISR matched {} locations",
            isr_hits.len()
        ));
    }
    let intro_pit3_isr = isr_hits[0];
    let display_start_sig = hex("701041f9fc08c000245f13c1fc050050722313c0fc05001d");
    let display_start = unique(
        &main.bytes,
        &display_start_sig,
        &data_mask(&display_start_sig, None),
    )?;
    let uart_wait_sig = hex("24394094cd90d48022794094cd8828394094cd9493c43239fc0454743639fc04");
    let uart_wait = unique(
        &main.bytes,
        &uart_wait_sig,
        &data_mask(&uart_wait_sig, None),
    )?;
    let uart_init = 0x4000_243e;
    let uart_prefix = hex("2f02740f41f9ec09404b1210202f000843f9ec07000042b9");
    let uart_at = (uart_init - MAIN_LOAD) as usize;
    if main.bytes.get(uart_at..uart_at + uart_prefix.len()) != Some(uart_prefix.as_slice()) {
        return Err("uart8_init prefix verification failed".into());
    }
    if main
        .bytes
        .get(uart_at + 0x19a..)
        .is_none_or(|b| !b.starts_with(&[0x20, 0x3c]))
    {
        return Err("UART initializer opcode verification failed".into());
    }
    let uart_handler = u32::from_be_bytes(
        main.bytes[uart_at + 0x19c..uart_at + 0x1a0]
            .try_into()
            .map_err(|_| "uart handler operand out of range")?,
    );
    if main
        .bytes
        .get(uart_at + 0x1b4..)
        .is_none_or(|b| !b.starts_with(&hex("23c04000026c")))
    {
        return Err("UART vector store verification failed".into());
    }
    let handler_prefix = hex("46fc27002f012f00702313c0fc04401c");
    if uart_handler < MAIN_LOAD
        || main
            .bytes
            .get((uart_handler - MAIN_LOAD) as usize..)
            .is_none_or(|b| !b.starts_with(&handler_prefix))
    {
        return Err("UART handler prefix verification failed".into());
    }
    let card_image = cli.card_image.as_deref().map(open_card_image).transpose()?;
    let (card, card_provenance, card_error) = if let Some(image) = card_image {
        let error = Arc::clone(&image.error);
        let card = Card::with_backing(
            image.blocks,
            Some(Box::new(Image {
                file: Mutex::new(image.file),
                len: image.len,
                error,
            })),
        )
        .map_err(|error| format!("card image identity: {error:?}"))?;
        (
            card,
            json!({"blank_card":false,"sha256":image.sha256,"length":image.len,"capacity_blocks":image.blocks}),
            Some(Arc::clone(&image.error)),
        )
    } else {
        (Card::default(), json!({"blank_card":true}), None)
    };
    if !cli.out.exists() {
        fs::create_dir_all(&cli.out).map_err(|e| format!("create output directory: {e}"))?;
    }
    if !cli.out.is_dir()
        || fs::read_dir(&cli.out)
            .map_err(|e| e.to_string())?
            .next()
            .is_some()
    {
        return Err("--out must be a new or empty directory".into());
    }
    let sd_semaphore_json = sd_semaphore_base.map(|base| json!({"base":base,"status":sd_semaphores.status,"dma":sd_semaphores.dma_sem,"data":sd_semaphores.data_sem,"command":sd_semaphores.cmd_sem}));
    fs::write(cli.out.join("resolved-profile.json"), serde_json::to_vec_pretty(&json!({"device":device.short,"version":firmware_profile.version,"boot_contract":format!("{:?}",boot.contract),"symbol_profile":format!("{:?}",boot.symbol_profile),"entry":ENTRY,"image_end":MAIN_LOAD + main.bytes.len() as u32,"task_create":task_create,"mainloop":mainloop,"job_pump":job_pump,"flash_read":flash_read,"intro_done":intro_done,"frame_sem":frame_sem,"intro_pit3_isr":intro_pit3_isr,"display_start":display_start,"uart8_handler":uart_handler,"panel_diff":panel_diff,"fb_front":fb_front,"fb_back":fb_back,"sd_bringup":sd_bringup,"sd_semaphores":sd_semaphore_json})).unwrap()).map_err(|e| e.to_string())?;
    let mut progress =
        fs::File::create(cli.out.join("progress.ndjson")).map_err(|e| e.to_string())?;

    let dtim1_enabled = cli.diagnostic_services;
    let mut board = Board::new(card, sd_semaphores, CompletionPolicy::Oracle);
    let sd_gate_enabled = cli.diagnostic_services;
    if sd_gate_enabled {
        board
            .enable_sd_gate(false)
            .map_err(|e| format!("SdGateEnable({e:?})"))?;
    }
    // Diagnostic register facade only.  This direct Cpu::step loop never
    // calls Machine::step_timed, so it cannot service or deliver timer IRQs.
    board.attach_time(Time::with_dtims(
        TimerPolicy::Oracle,
        vec![3, 2, 0],
        if dtim1_enabled { vec![3, 1] } else { vec![3] },
        132_000_000.0,
    ));
    // Python Machine._fault zero-maps unknown peripheral pages. Its blank
    // INTC pages therefore start with clear masks, unlike IntcBank's explicit
    // hardware-reset all-ones masks. This is an opt-in Oracle diagnostic
    // setup only; it does not reinterpret firmware writes or model hardware.
    let oracle_zero_intc_masks = cli.diagnostic_services;
    if oracle_zero_intc_masks {
        for base in [0xfc04_8000, 0xfc04_c000, 0xfc05_0000] {
            board
                .write32(base + 0x08, 0)
                .expect("zero Oracle INTC IMRH");
            board
                .write32(base + 0x0c, 0)
                .expect("zero Oracle INTC IMRL");
        }
    }
    let uart8_status = cli.diagnostic_services;
    let dspi0_status = cli.diagnostic_services;
    let dspi2_peer_status = cli.diagnostic_services;
    let mut forced_mmio = BTreeMap::new();
    if uart8_status {
        forced_mmio.insert(UART8_USR, 0x0d00_0000);
    }
    if dspi0_status {
        forced_mmio.insert(DSPI0_SR, 0x1000_00f0);
    }
    if dspi2_peer_status {
        forced_mmio.insert(DSPI2_SR, 0x9000_0000);
    }
    if !forced_mmio.is_empty() {
        board
            .install_forced_mmio(forced_mmio)
            .expect("install documented forced status hooks");
    }
    // Python Machine._fault zero-maps SDRAM first touches.  The native board's
    // bounded equivalent is explicit; peripheral holes intentionally remain
    // faults so this probe reports the first missing service.
    board.enable_oracle_sdram_faults();
    map_image(&mut board, &main.bytes);
    board
        .map_zeroed_ram_page(STACK & !((PAGE as u32) - 1))
        .expect("map stack RAM");
    let zero_page_mmio = cli.diagnostic_services;
    let service_timers_enabled = cli.diagnostic_services;
    let flash_read_enabled = cli.diagnostic_services;
    let flash = if flash_read_enabled {
        let target = (flash_read - MAIN_LOAD) as usize;
        if main.bytes.get(target..target + FLASH_READ_SIG.len()) != Some(FLASH_READ_SIG) {
            return Err("flash_read signature changed after resolution".into());
        }
        assert!(
            FLASH_SLOT
                .checked_add(container.len())
                .is_some_and(|end| end <= FLASH_SIZE),
            "ELE3 container fits 16MiB flash"
        );
        let mut image = vec![0; FLASH_SIZE];
        image[FLASH_SLOT..FLASH_SLOT + container.len()].copy_from_slice(container);
        Some(image)
    } else {
        None
    };
    let idle_yield_enabled = cli.diagnostic_services;
    let tx35_service_enabled = cli.diagnostic_services;
    let idle_spins: BTreeSet<u32> = main
        .bytes
        .windows(2)
        .enumerate()
        .filter(|(offset, bytes)| offset % 2 == 0 && *bytes == [0x60, 0xfe])
        .map(|(offset, _)| MAIN_LOAD + offset as u32)
        .collect();
    let max_steps = cli.limit;
    let mut bus = LoggingBus {
        board,
        accesses: vec![],
        access_dropped: 0,
        ppmcr_contract: cli.diagnostic_services,
        zero_page_mmio,
        zero_page_limit: 160,
        current_pc: ENTRY,
        current_icount: 0,
        unknown_touches: BTreeMap::new(),
        timer_accesses: vec![],
        timer_writes: vec![],
        uart8_tx: vec![],
        dspi2_status_reads: vec![],
        dspi2_dma_writes: vec![],
        cmdarg_writes: 0,
        xfertyp_writes: 0,
        gpio_reads: 0,
        gpio_writes: 0,
        last_cmdarg: 0,
        command_trace: vec![],
        gpio_trace: vec![],
    };
    let mut cpu = Cpu::new();
    cpu.pc = ENTRY;
    cpu.sr = 0x2700;
    cpu.a[7] = STACK;
    let mut stop = String::from("instruction limit");
    println!(
        "policy=ppmcr_contract={} unknown_mmio_zero_page={} oracle_zero_intc_masks={} uart8_status={} dspi0_status={} dspi2_peer_status={} flash_read={} idle_yield={} tx35_service={} idle_sites={} dtim1={} max_steps={max_steps} time_facade=oracle(pit=3,2,0;dtim=3{};ips=132000000); timer_service={service_timers_enabled}",
        bus.ppmcr_contract,
        zero_page_mmio,
        oracle_zero_intc_masks,
        uart8_status,
        dspi0_status,
        dspi2_peer_status,
        flash_read_enabled,
        idle_yield_enabled,
        tx35_service_enabled,
        idle_spins.len(),
        dtim1_enabled,
        if dtim1_enabled { ",1" } else { "" }
    );
    println!(
        "resolved_marks=task_create={task_create:?} mainloop={mainloop:?} job_pump={job_pump:?}"
    );
    let mut deliveries = Vec::new();
    let mut delivery_events = Vec::new();
    let mut delivery_counts = [0u64; 256];
    let mut delivery_dropped = 0u64;
    let mut flash_reads = Vec::new();
    let mut idle_passes = 0u64;
    let mut idle_yields = 0u64;
    let mut task_create_hits = 0u64;
    let mut mainloop_hits = 0u64;
    let mut fs_starts = 0u64;
    let mut fs_completions = 0u64;
    let mut fs_last_complete = None;
    let mut fs_success_result = None;
    let mut fs_clear_pending = None;
    let mut fs_active = false;
    let mut job_pump_hits = 0u64;
    let mut intro_done_hits = 0u64;
    let mut display_start_hits = 0u64;
    let mut tx35_deliveries = 0u64;
    let mut tx35_bytes = 0u64;
    if main
        .bytes
        .get((0x4000_0410 - MAIN_LOAD) as usize..)
        .is_none_or(|bytes| !bytes.starts_with(&hex("46fc27002f48fffc2079")))
    {
        return Err("ctx_switch prefix verification failed".into());
    }
    let current_tcb_addr = u32::from_be_bytes(
        main.bytes[(0x4000_041a - MAIN_LOAD) as usize..][..4]
            .try_into()
            .expect("current TCB operand"),
    );
    let ready_cursor_addr = u32::from_be_bytes(
        main.bytes[(0x4000_0426 - MAIN_LOAD) as usize..][..4]
            .try_into()
            .expect("ready cursor operand"),
    );
    let mut task_creates = Vec::new();
    let mut pends = Vec::new();
    let mut contexts = Vec::new();
    let mut timer_deadline_first = None;
    let mut next_progress = cli.progress_every;
    let mut step = 0u64;
    let mut service_iterations = 0u64;
    let mut instruction_trace = VecDeque::with_capacity(256);
    let mut instruction_trace_dropped = 0u64;
    let mut vector208_transitions: Vec<(u64, Option<u32>, Option<u32>)> = Vec::new();
    let mut previous_vector208 = None;
    let mut mainloop_tcb = 0u32;
    let mut frames = FrameTracker::default();
    let mut completion_events_total = 0u64;
    let mut completion_kind_counts = [0u64; 3];
    let mut dma_ranges_total = 0u64;
    let mut dma_bytes_total = 0u64;
    while cpu.icount < max_steps {
        let pc = cpu.pc;
        bus.current_icount = cpu.icount;
        if instruction_trace.len() == 256 {
            instruction_trace.pop_front();
            instruction_trace_dropped += 1;
        }
        instruction_trace.push_back(InstructionObservation {
            icount: cpu.icount,
            pc,
            sr: cpu.sr,
        });
        let Some(offset) = pc.checked_sub(MAIN_LOAD).map(|v| v as usize) else {
            stop = format!("UnsupportedGuestPc({pc:#010x})");
            break;
        };
        let Some(bytes) = main.bytes.get(offset..offset + 6) else {
            stop = format!("UnsupportedGuestPc({pc:#010x})");
            break;
        };
        let verbose = step < 32 || step % 5_000_000 == 0;
        if verbose {
            println!(
                "pre step={step} pc={pc:#010x} bytes={:02x?} decode={:?} sr={:#06x} d0={:#010x} a0={:#010x} a1={:#010x} a5={:#010x} a7={:#010x}",
                bytes,
                decode_at(&main.bytes, MAIN_LOAD, offset),
                cpu.sr,
                cpu.d[0],
                cpu.a[0],
                cpu.a[1],
                cpu.a[5],
                cpu.a[7]
            );
        }
        if task_create == pc {
            task_create_hits += 1;
        }
        if let Some((entry, completion, success, done)) = fs_worker {
            if pc == entry {
                fs_starts += 1;
                fs_active = true;
                fs_last_complete = None;
                fs_success_result = None;
            }
            if pc == completion + 12 && fs_active {
                fs_clear_pending = Some((success, done, completion + 18));
            }
        }
        if mainloop == pc {
            mainloop_hits += 1;
            if mainloop_hits == 1 {
                mainloop_tcb = bus.board.read32(current_tcb_addr).unwrap_or(0);
            }
        }
        if job_pump == pc {
            job_pump_hits += 1;
        }
        if intro_done == pc {
            intro_done_hits += 1;
        }
        if display_start == pc {
            display_start_hits += 1;
        }
        // A completed frame is proved only at the diff JSR's exact return
        // boundary, with the same task still current.  This survives timer
        // preemption without attributing one task's frame to another.
        frames.complete_at_return(&mut bus.board, current_tcb_addr, pc, cpu.a[7]);
        if pc == panel_diff && intro_done_hits > 0 {
            if let Err(error) = frames.capture(
                &mut bus.board,
                current_tcb_addr,
                fb_front,
                cpu.icount,
                cpu.a[7],
            ) {
                stop = error;
                break;
            }
        }
        if tx35_service_enabled {
            match service_tx35_wait(&mut bus, &mut cpu, uart_wait, uart_handler) {
                Ok(true) => {
                    tx35_deliveries += 1;
                    let completions = bus.board.take_completion_events();
                    completion_events_total += completions.len() as u64;
                    for completion in &completions {
                        match completion {
                            CompletionEvent::Dma59 { .. } => completion_kind_counts[0] += 1,
                            CompletionEvent::Data { .. } => completion_kind_counts[1] += 1,
                            CompletionEvent::Command { .. } => completion_kind_counts[2] += 1,
                        }
                    }
                    let dma_ranges = bus.board.take_dma_written_ranges();
                    dma_ranges_total += dma_ranges.len() as u64;
                    dma_bytes_total += dma_ranges
                        .iter()
                        .map(|(_, bytes)| *bytes as u64)
                        .sum::<u64>();
                    service_iterations += 1;
                    if service_iterations > max_steps {
                        stop = "service iteration limit".into();
                        break;
                    }
                    step += 1;
                    continue;
                }
                Ok(false) => {}
                Err(error) => {
                    stop = error;
                    break;
                }
            }
        }
        let live_208 = bus.board.read32(cpu.ctrl.vbr.wrapping_add(4 * 208)).ok();
        if live_208 != previous_vector208 && vector208_transitions.len() < 16 {
            vector208_transitions.push((cpu.icount, previous_vector208, live_208));
        }
        previous_vector208 = live_208;
        let main_os_running = is_ready(
            intro_done_hits,
            mainloop_hits,
            job_pump_hits,
            delivery_counts[99],
            mainloop_tcb,
            frames.latest_for(mainloop_tcb),
        ) && readiness_contract_ready(
            readiness_contract,
            fs_starts,
            fs_completions,
            fs_last_complete,
            frames.latest_for(mainloop_tcb),
            fs_success_result,
        );
        if cli.stop_at == "ready" && main_os_running {
            stop = String::from("VERIFIED_READY");
            break;
        }
        if cpu.icount >= next_progress {
            println!(
                "checkpoint icount={} pc={pc:#010x} d0={:#010x} d1={:#010x} d2={:#010x} d3={:#010x} tasks={task_create_hits} pends={} flash_reads={} idle_passes={idle_passes} timer_deliveries={} mainloop={mainloop_hits} job_pump={job_pump_hits}",
                cpu.icount,
                cpu.d[0],
                cpu.d[1],
                cpu.d[2],
                cpu.d[3],
                pends.len(),
                flash_reads.len(),
                deliveries.len()
            );
            writeln!(progress, "{}", json!({"icount":cpu.icount,"pc":pc,"step":step,"mainloop":mainloop_hits,"job_pump":job_pump_hits,"deliveries_total":delivery_counts.iter().sum::<u64>(),"deliveries_retained":deliveries.len()})).map_err(|e| e.to_string())?;
            progress.flush().map_err(|e| e.to_string())?;
            next_progress = cpu.icount.saturating_add(cli.progress_every);
        }
        if task_create == pc && task_creates.len() < 64 {
            let sp = cpu.a[7];
            let mut read = |offset| bus.board.read32(sp.wrapping_add(offset)).unwrap_or(0);
            // The JSR return address is at A7; dspboot's task fields begin
            // with the following five longwords at the callee boundary.
            task_creates.push(TaskCreate {
                at: cpu.icount,
                tcb: read(4),
                entry: read(8),
                prio: read(12),
                stack: read(16),
                size: read(20),
            });
        }
        if pc == 0x4000_141a && pends.len() < 64 {
            let sp = cpu.a[7];
            let return_pc = bus.board.read32(sp).unwrap_or(0);
            let sem = bus.board.read32(sp.wrapping_add(4)).unwrap_or(0);
            let value = bus.board.read32(sem).ok();
            pends.push(Pend {
                at: cpu.icount,
                return_pc,
                sem,
                value,
            });
        }
        if pc == 0x4000_0410 && contexts.len() < 64 {
            contexts.push(Context {
                at: cpu.icount,
                current_tcb: bus.board.read32(current_tcb_addr).unwrap_or(0),
                ready_cursor: bus.board.read32(ready_cursor_addr).unwrap_or(0),
            });
        }
        if idle_yield_enabled && idle_spins.contains(&pc) {
            idle_passes += 1;
            if idle_passes.is_multiple_of(20_000) {
                if let Err(error) =
                    cpu.take_interrupt(&mut bus.board, 32, None, InterruptPolicy::Oracle)
                {
                    stop = format!("IdleYieldPoisoned({error:?})");
                    break;
                }
                idle_yields += 1;
            }
        }
        if service_timers_enabled {
            let mut time = bus.board.take_time().expect("attached diagnostic Time");
            if let Some(deadline) = time.deadline(cpu.icount) {
                timer_deadline_first.get_or_insert((cpu.icount, deadline));
            }
            bus.board.restore_time(time);
        }
        bus.clear();
        bus.current_pc = pc;
        let result: Result<(), String> = if flash_read_enabled && cpu.pc == flash_read {
            hle_flash_read(
                &mut bus,
                &mut cpu,
                flash.as_deref().expect("flash image"),
                &mut flash_reads,
            )
            .map(|()| {
                cpu.icount += 1;
            })
        } else {
            cpu.step(&mut bus).map_err(|error| format!("{error:?}"))
        };
        if let Some((success, done, expected_pc)) = fs_clear_pending.take()
            && result.is_ok()
            && cpu.pc == expected_pc
            && fs_active
            && bus.board.read8(done).ok() == Some(1)
        {
            fs_success_result = match bus.board.read8(success).ok() {
                Some(0) => Some(false),
                Some(1) => Some(true),
                _ => None,
            };
            fs_completions += 1;
            fs_last_complete = Some(cpu.icount);
            fs_active = false;
        }
        if let Some(error) = &card_error
            && let Ok(mut error) = error.lock()
            && let Some(error) = error.take()
        {
            stop = format!("CardImageRead({error})");
            break;
        }
        let completions = bus.board.take_completion_events();
        completion_events_total += completions.len() as u64;
        for completion in &completions {
            match completion {
                CompletionEvent::Dma59 { .. } => completion_kind_counts[0] += 1,
                CompletionEvent::Data { .. } => completion_kind_counts[1] += 1,
                CompletionEvent::Command { .. } => completion_kind_counts[2] += 1,
            }
        }
        let dma_ranges = bus.board.take_dma_written_ranges();
        dma_ranges_total += dma_ranges.len() as u64;
        dma_bytes_total += dma_ranges
            .iter()
            .map(|(_, bytes)| *bytes as u64)
            .sum::<u64>();
        tx35_bytes += bus.board.take_uart_tx().len() as u64;
        let deliveries_before = deliveries.len();
        let mut delivered_now = 0u64;
        if result.is_ok() && service_timers_enabled {
            let done = cpu.icount;
            match service_timers(
                &mut bus,
                &mut cpu,
                done,
                &mut deliveries,
                &mut delivery_counts,
                &mut delivery_dropped,
            ) {
                Ok(count) => delivered_now = count,
                Err(error) => {
                    stop = error;
                    println!("timer_service_stop step={step} pc={:#010x}", cpu.pc);
                    break;
                }
            }
        }
        if deliveries.len() > deliveries_before && delivery_events.len() < 128 {
            delivery_events.extend(
                deliveries[deliveries_before..]
                    .iter()
                    .map(|&(vector, level)| (cpu.icount, vector, level)),
            );
        }
        let timer_delivered = delivered_now != 0;
        if verbose || result.is_err() || cpu.last_exception.is_some() {
            println!(
                "post step={step} result={result:?} pc={:#010x} sr={:#06x} d0={:#010x} a0={:#010x} a1={:#010x} a5={:#010x} a7={:#010x} exception={:?} accesses={:?}",
                cpu.pc,
                cpu.sr,
                cpu.d[0],
                cpu.a[0],
                cpu.a[1],
                cpu.a[5],
                cpu.a[7],
                cpu.last_exception,
                bus.accesses
            );
        }
        // trap #0 is the firmware's normal RTOS yield path; Cpu::exception
        // has already placed PC at its installed vector handler, so keep
        // exploring it. Other synchronous exceptions remain boundaries.
        if result.is_err()
            || cpu
                .last_exception
                .is_some_and(|vector| vector != 32 && !timer_delivered)
        {
            stop = match (result, cpu.last_exception) {
                (Err(error), _) => error,
                (Ok(()), Some(vector)) => format!("exception vector {vector}"),
                (Ok(()), None) => unreachable!(),
            };
            break;
        }
        step += 1;
    }
    println!("source_syx_sha256={syx_sha}");
    println!("main_sha256={main_sha}");
    println!("section3_bytes={}", main.bytes.len());
    println!(
        "setup=MAIN_LOAD={MAIN_LOAD:#010x} ENTRY={ENTRY:#010x} VECTOR_RAM={VECTOR_RAM:#010x} SR=0x2700 A7={STACK:#010x} ctrl_vbr=Cpu::new() reset value"
    );
    println!("stop={stop}");
    println!(
        "cpu_icount={} pc={:#010x} sr={:#06x} a7={:#010x} vbr={:#010x} last_exception={:?} last_unimplemented={:?}",
        cpu.icount,
        cpu.pc,
        cpu.sr,
        cpu.a[7],
        cpu.ctrl.vbr,
        cpu.last_exception,
        cpu.last_unimplemented
    );
    let final_offset = cpu.pc.checked_sub(MAIN_LOAD).map(|v| v as usize);
    println!(
        "final_decode={:?} final_accesses={:?}",
        final_offset.and_then(|offset| decode_at(&main.bytes, MAIN_LOAD, offset)),
        bus.accesses
    );
    println!(
        "timer_deliveries={} first={:?} last={:?}",
        deliveries.len(),
        deliveries.first(),
        deliveries.last()
    );
    println!("timer_delivery_events={delivery_events:?}");
    let pit3 = delivery_counts[208];
    let dtim3 = delivery_counts[99];
    println!(
        "idle_passes={idle_passes} idle_yields={idle_yields} task_create_hits={task_create_hits} mainloop_hits={mainloop_hits} job_pump_hits={job_pump_hits} pit3_deliveries={pit3} dtim3_deliveries={dtim3} main_os_running={}",
        is_ready(
            intro_done_hits,
            mainloop_hits,
            job_pump_hits,
            dtim3,
            mainloop_tcb,
            frames.latest_for(mainloop_tcb)
        )
    );
    println!("timer_deadline_first={timer_deadline_first:?}");
    println!("timer_guest_accesses={:?}", bus.timer_accesses);
    println!("timer_guest_writes={:?}", bus.timer_writes);
    println!(
        "timer_registers=INTC2_IMRL={:?} INTC2_ICR13={:?} PIT0_PCSR={:?} PIT0_PMR={:?}",
        bus.board.read32(0xfc05_000c),
        bus.board.read8(0xfc05_004d),
        bus.board.read16(0xfc08_0000),
        bus.board.read16(0xfc08_0002)
    );
    println!(
        "dtim1_state=vector97={:?} dtmr1={:?} dtrr1={:?} intc0_imrh={:?} intc0_icr33={:?}",
        bus.board.read32(VECTOR_RAM + 4 * 97),
        bus.board.read16(0xfc07_4000),
        bus.board.read32(0xfc07_4004),
        bus.board.read32(0xfc04_8008),
        bus.board.read8(0xfc04_8000 + 0x40 + 33)
    );
    println!(
        "uart8_tx_count={} writes={:?} tx35_deliveries={tx35_deliveries} tx35_bytes={tx35_bytes} tx35_pending={}",
        bus.uart8_tx.len(),
        bus.uart8_tx,
        bus.board.dma.tx35.pending
    );
    println!(
        "dspi2_status_reads={:?} dspi2_dma_writes={:?}",
        bus.dspi2_status_reads, bus.dspi2_dma_writes
    );
    println!(
        "flash_container_sha256={} flash_container_bytes={} flash_reads={:?}",
        digest(container),
        container.len(),
        flash_reads
    );
    println!(
        "scheduler_symbols=current_tcb={current_tcb_addr:#010x} ready_cursor={ready_cursor_addr:#010x}"
    );
    println!("task_creates={task_creates:?}");
    println!("sem_pends={pends:?}");
    println!("contexts={contexts:?}");
    println!(
        "zero_page_first_touch_count={} first={:?} last={:?}",
        bus.unknown_touches.len(),
        bus.unknown_touches.first_key_value(),
        bus.unknown_touches.last_key_value()
    );
    let mut observed = cpu.clone();
    observed.resolve_nzv();
    let final_live_208 = bus
        .board
        .read32(observed.ctrl.vbr.wrapping_add(4 * 208))
        .ok();
    let image_end = MAIN_LOAD + main.bytes.len() as u32;
    let main_frame = frames.latest_for(mainloop_tcb);
    let structural_ready = is_ready(
        intro_done_hits,
        mainloop_hits,
        job_pump_hits,
        dtim3,
        mainloop_tcb,
        main_frame,
    );
    let fs_ready = readiness_contract_ready(
        readiness_contract,
        fs_starts,
        fs_completions,
        fs_last_complete,
        main_frame,
        fs_success_result,
    );
    let ready = structural_ready && fs_ready;
    let frame_metadata = main_frame.map(|frame| json!({"owner_tcb":frame.owner_tcb,"ptr":frame.ptr,"sha256":frame.hash,"nonzero_bytes":frame.lit_bytes,"icount":frame.icount,"completed":true}));
    let missing: Vec<&str> = [
        (!structural_ready, "structural_main_ui"),
        (!fs_ready, "readiness_contract"),
        (mainloop_hits == 0, "mainloop"),
        (job_pump_hits == 0, "job_pump"),
        (dtim3 == 0, "dtim3"),
        (mainloop_tcb == 0, "mainloop_owner_tcb"),
        (main_frame.is_none(), "completed_main_owner_frame"),
        (
            main_frame.is_some_and(|frame| frame.lit_bytes == 0),
            "nonzero_main_frame",
        ),
    ]
    .into_iter()
    .filter_map(|(missing, name)| missing.then_some(name))
    .collect();
    let readiness = json!({"intro_done":intro_done_hits > 0,"mainloop":mainloop_hits > 0,"job_pump":job_pump_hits > 0,"dtim3":dtim3 > 0,"mainloop_owner_tcb":mainloop_tcb,"completed_main_owner_frame":main_frame.is_some(),"nonzero_main_frame":main_frame.is_some_and(|frame| frame.lit_bytes > 0),"missing":missing,"maintenance_diagnostics":{"pit3":pit3,"live_vector208":final_live_208,"vector208_handoff":final_live_208.is_some_and(|p| p >= MAIN_LOAD && p < image_end && p != intro_pit3_isr),"intro_pit3_isr":intro_pit3_isr,"transitions":vector208_transitions}});
    let binary_sha256 = std::env::current_exe()
        .ok()
        .and_then(|path| fs::read(path).ok())
        .map(|bytes| digest(&bytes));
    let coverage = json!({"mode":"oracle-diagnostic","icount":cpu.icount,"pc":cpu.pc,"sr":cpu.sr,"marks":[intro_done_hits,mainloop_hits,job_pump_hits,pit3,dtim3],"delivery_counts":delivery_counts.to_vec(),"frame_sha256":main_frame.map(|frame| &frame.hash)});
    let report = json!({"mode":"oracle-diagnostic", "stop":stop, "ready":ready, "icount":observed.icount, "pc":observed.pc, "sr":observed.sr, "cpu":{"d":observed.d,"a":observed.a,"other_a7":observed.other_a7,"ctrl":{"vbr":observed.ctrl.vbr,"cacr":observed.ctrl.cacr,"asid":observed.ctrl.asid,"acr":observed.ctrl.acr,"mmubar":observed.ctrl.mmubar,"rgpiobar":observed.ctrl.rgpiobar,"rambar":observed.ctrl.rambar},"emac":{"macsr":observed.emac.macsr,"acc":observed.emac.acc,"accext01":observed.emac.accext01,"accext23":observed.emac.accext23,"mask":observed.emac.mask}}, "input_hashes":{"syx":syx_sha,"main":main_sha,"container":digest(container),"card":card_provenance.clone()}, "binary_sha256":binary_sha256, "source_fingerprint":{"coverage":"partial: runner source and canonical identity records","runner":digest(include_str!("main.rs").as_bytes()),"digitakt":digest(include_str!("../../../devices/digitakt-ii.toml").as_bytes()),"digitone":digest(include_str!("../../../devices/digitone-ii.toml").as_bytes())}, "services":{"diagnostic_services":cli.diagnostic_services,"sd_gate":sd_gate_enabled,"oracle_ips":132000000,"pit":[3,2,0],"dtim":if cli.diagnostic_services {json!([3,1])} else {json!([3])},"blank_card":card_provenance["blank_card"],"default_semaphores":true}, "marks":{"intro_done":intro_done_hits,"display_start":display_start_hits,"task_create":task_create_hits,"mainloop":mainloop_hits,"job_pump":job_pump_hits,"pit3":pit3,"dtim3":dtim3}, "frame":frame_metadata.clone(),"frame_retention":{"pending":frames.pending.len(),"owners":frames.completed.len(),"pending_dropped":frames.pending_dropped,"completed_dropped":frames.completed_dropped}, "telemetry":{"unknown_pages":bus.unknown_touches.len(),"accesses":bus.accesses.len(),"access_dropped":bus.access_dropped,"timer_accesses":bus.timer_accesses.len(),"delivery_retained":deliveries.len(),"delivery_dropped":delivery_dropped,"delivery_counts":delivery_counts.to_vec(),"instruction_trace_dropped":instruction_trace_dropped,"completion_events_drained":completion_events_total,"dma_written_ranges_drained":dma_ranges_total,"dma_written_bytes_drained":dma_bytes_total,"esdhc_dma_bytes":bus.board.esdhc_dma_bytes(),"cmdarg_writes":bus.cmdarg_writes,"xfertyp_writes":bus.xfertyp_writes,"command_trace":bus.command_trace,"gpio_reads":bus.gpio_reads,"gpio_writes":bus.gpio_writes,"gpio_trace":bus.gpio_trace},"observation_digest":{"coverage":"bounded diagnostic observation, not complete checkpoint","sha256":digest(serde_json::to_string(&coverage).unwrap().as_bytes())}});
    let mut report = report;
    report["readiness_conditions"] = readiness.clone();
    report["readiness_conditions"]["contract"] = json!(format!("{readiness_contract:?}"));
    report["readiness_conditions"]["structural_main_ui_reached"] = json!(structural_ready);
    report["readiness_conditions"]["fs_verification"] = json!({"entry":fs_worker.map(|value| value.0),"completion":fs_worker.map(|value| value.1),"success_addr":fs_worker.map(|value| value.2),"done_addr":fs_worker.map(|value| value.3),"success_result":fs_success_result,"starts":fs_starts,"completions":fs_completions,"last_complete_icount":fs_last_complete,"scope":if fs_worker.is_some() {"required by contract"} else {"not required by main-panel-v1; filesystem-verification completion is unresolved and not claimed"}});
    report["readiness_scope"] = json!(
        "Oracle diagnostic main panel; DT2 filesystem checker only. No full hardware, audio, or input claim."
    );
    report["services"]["mmc_semaphore_completion"] = json!(sd_semaphore_base.is_some());
    report["services"]["mmc_semaphore_addresses"] =
        sd_semaphore_json.clone().unwrap_or(serde_json::Value::Null);
    report["services"]["sd_gate_initial_driven"] = json!(false);
    report["services"]["resolved_semaphore_addresses"] =
        sd_semaphore_json.clone().unwrap_or(serde_json::Value::Null);
    report["services"]["completion_service_scope"] =
        json!("Oracle synchronous completion only; no Device ISR model");
    report["telemetry"]["gpio_bus_write_calls"] = json!(bus.gpio_writes);
    report["telemetry"]["gpio_write_count_scope"] = json!(
        "explicit LoggingBus write calls; CPU read-modify-write internals are not counted as writes"
    );
    report["telemetry"]["completion_kind_counts"] = json!({"dma59":completion_kind_counts[0],"data":completion_kind_counts[1],"command":completion_kind_counts[2]});
    fs::write(
        cli.out.join("report.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .map_err(|e| e.to_string())?;
    if let Some(frame) = main_frame {
        fs::write(cli.out.join("framebuffer.bin"), &frame.raw).map_err(|e| e.to_string())?;
    }
    if !stop_success(&stop, &cli.stop_at, ready, observed.icount, cli.limit) {
        fs::write(cli.out.join("failure.json"), serde_json::to_vec_pretty(&json!({"reason":stop,"frame":frame_metadata,"cpu":{"d":observed.d,"a":observed.a,"pc":observed.pc,"sr":observed.sr,"other_a7":observed.other_a7,"ctrl":{"vbr":observed.ctrl.vbr,"cacr":observed.ctrl.cacr,"asid":observed.ctrl.asid,"acr":observed.ctrl.acr,"mmubar":observed.ctrl.mmubar,"rgpiobar":observed.ctrl.rgpiobar,"rambar":observed.ctrl.rambar},"emac":{"macsr":observed.emac.macsr,"acc":observed.emac.acc,"accext01":observed.emac.accext01,"accext23":observed.emac.accext23,"mask":observed.emac.mask}},"last_instruction_trace":instruction_trace,"last_bus_accesses":bus.accesses,"timer_observations":bus.timer_accesses,"unknown_pages":bus.unknown_touches.len(),"delivery_counts":delivery_counts.to_vec(),"delivery_dropped":delivery_dropped})).unwrap()).map_err(|e| e.to_string())?;
        return Err(stop);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolver_requires_one_match() {
        let image = [1, 2, 3, 1, 2, 3];
        assert!(unique(&image, &[1, 2, 3], &[false; 3]).is_err());
        assert_eq!(
            unique(&[0, 1, 2, 3], &[1, 2, 3], &[false; 3]),
            Ok(MAIN_LOAD + 1)
        );
    }

    #[test]
    fn resolver_mask_allows_relocated_word() {
        let image = [0x48, 0x79, 0x40, 0x01, 0x02, 0x03];
        let signature = [0x48, 0x79, 0x40, 0x94, 0xef, 0x3c];
        assert_eq!(
            unique(&image, &signature, &data_mask(&signature, None)),
            Ok(MAIN_LOAD)
        );
    }

    #[test]
    fn resolver_rejects_bad_masks_and_short_images() {
        assert!(unique(&[1], &[1, 2], &[false]).is_err());
        assert!(unique(&[1], &[1, 2], &[false, false]).is_err());
    }

    #[test]
    fn sd_bringup_resolves_direct_completion_words() {
        let mut image = vec![0; 40];
        image[4..4 + SD_BRINGUP_SIG.len()].copy_from_slice(SD_BRINGUP_SIG);
        image[32..36].copy_from_slice(&0x44e3_fe7cu32.to_be_bytes());
        let (base, addresses) = resolve_sd_semaphores(&image).unwrap();
        assert_eq!(base, 0x44e3_fe7c);
        assert_eq!(addresses.status, Some(0x44e3_feac));
        assert_eq!(addresses.dma_sem, Some(0x44e3_feb8));
        assert_eq!(addresses.data_sem, Some(0x44e3_fec0));
        assert_eq!(addresses.cmd_sem, Some(0x44e3_fec8));
    }

    #[test]
    fn sd_bringup_rejects_collisions_and_invalid_operands() {
        let mut collision = vec![0; 80];
        collision[..SD_BRINGUP_SIG.len()].copy_from_slice(SD_BRINGUP_SIG);
        collision[28..32].copy_from_slice(&0x44e3_fe7cu32.to_be_bytes());
        collision[40..40 + SD_BRINGUP_SIG.len()].copy_from_slice(SD_BRINGUP_SIG);
        collision[68..72].copy_from_slice(&0x44e3_fe7cu32.to_be_bytes());
        assert!(resolve_sd_semaphores(&collision).is_err());
        let mut invalid = vec![0; 32];
        invalid[..SD_BRINGUP_SIG.len()].copy_from_slice(SD_BRINGUP_SIG);
        invalid[28..32].copy_from_slice(&0x4800_0000u32.to_be_bytes());
        assert!(resolve_sd_semaphores(&invalid).is_err());
    }

    #[test]
    fn fs_worker_resolver_rejects_mismatch_and_collision() {
        let mut image = vec![0; 128];
        let entry = hex("4e56ffb048d73cfc246e00084ebaffa64a00660c71ee000ee0884a00");
        let completion = hex("7001b58013c000000000700113c0000000001002600a");
        image[..entry.len()].copy_from_slice(&entry);
        image[64..64 + completion.len()].copy_from_slice(&completion);
        image[70..74].copy_from_slice(&0x4096_5b64u32.to_be_bytes());
        image[78..82].copy_from_slice(&0x4096_5b66u32.to_be_bytes());
        assert!(resolve_fs_worker(&image).is_err());
        image[78..82].copy_from_slice(&0x4096_5b65u32.to_be_bytes());
        image[96..96 + entry.len()].copy_from_slice(&entry);
        assert!(resolve_fs_worker(&image).is_err());
    }

    #[test]
    fn fs_contract_requires_completion_before_later_frame_but_dn2_does_not() {
        let frame = Frame {
            owner_tcb: 1,
            ptr: 2,
            icount: 20,
            hash: String::new(),
            lit_bytes: 1,
            raw: vec![],
        };
        use device_profile::ReadinessContract::*;
        assert!(!readiness_contract_ready(
            MainPanelFsCheckV1,
            0,
            1,
            Some(10),
            Some(&frame),
            Some(true)
        ));
        assert!(!readiness_contract_ready(
            MainPanelFsCheckV1,
            1,
            0,
            None,
            Some(&frame),
            Some(true)
        ));
        assert!(!readiness_contract_ready(
            MainPanelFsCheckV1,
            1,
            1,
            Some(21),
            Some(&frame),
            Some(false)
        ));
        assert!(!readiness_contract_ready(
            MainPanelFsCheckV1,
            2,
            1,
            None,
            Some(&frame),
            None
        ));
        assert!(!readiness_contract_ready(
            MainPanelFsCheckV1,
            2,
            1,
            Some(20),
            Some(&frame),
            Some(true)
        ));
        assert!(readiness_contract_ready(
            MainPanelFsCheckV1,
            1,
            1,
            Some(20),
            Some(&frame),
            Some(true)
        ));
        assert!(readiness_contract_ready(
            MainPanelV1,
            0,
            0,
            None,
            None,
            None
        ));
    }

    #[test]
    fn card_image_capacity_requires_exact_supported_sector_count() {
        assert_eq!(
            card_blocks(u64::from(DEFAULT_CAPACITY_BLOCKS) * SECTOR_SIZE as u64),
            Ok(DEFAULT_CAPACITY_BLOCKS)
        );
        assert_eq!(
            card_blocks(u64::from(SMALL_CAPACITY_BLOCKS) * SECTOR_SIZE as u64),
            Ok(SMALL_CAPACITY_BLOCKS)
        );
        assert!(card_blocks(513).is_err());
        assert!(card_blocks(SECTOR_SIZE as u64).is_err());
    }

    #[test]
    fn image_backing_reads_known_sector_and_overlay_leaves_file_unchanged() {
        let path = std::env::temp_dir().join(format!("boot-card-{}", std::process::id()));
        fs::write(&path, vec![0x5a; SECTOR_SIZE]).unwrap();
        let error = Arc::new(Mutex::new(None));
        let image = Image {
            file: Mutex::new(fs::File::open(&path).unwrap()),
            len: SECTOR_SIZE as u64,
            error,
        };
        let mut card = Card::with_backing(DEFAULT_CAPACITY_BLOCKS, Some(Box::new(image))).unwrap();
        let mut read = [0u8; SECTOR_SIZE];
        card.read_into(0, &mut read).unwrap();
        assert_eq!(read, [0x5a; SECTOR_SIZE]);
        card.write_data(25, 0, &[0xa5; SECTOR_SIZE]).unwrap();
        card.read_into(0, &mut read).unwrap();
        assert_eq!(read, [0xa5; SECTOR_SIZE]);
        assert_eq!(fs::read(&path).unwrap(), vec![0x5a; SECTOR_SIZE]);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn image_backing_reports_short_read_instead_of_zero_filling() {
        let path = std::env::temp_dir().join(format!("boot-card-short-{}", std::process::id()));
        fs::write(&path, vec![0; SECTOR_SIZE]).unwrap();
        let error = Arc::new(Mutex::new(None));
        let image = Image {
            file: Mutex::new(fs::File::open(&path).unwrap()),
            len: (SECTOR_SIZE * 2) as u64,
            error: Arc::clone(&error),
        };
        let mut bytes = [0u8; SECTOR_SIZE * 2];
        assert_eq!(image.read_at(0, &mut bytes), 0);
        assert!(
            error
                .lock()
                .unwrap()
                .as_deref()
                .is_some_and(|error| error.contains("card image read"))
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn readiness_requires_main_owned_nonzero_frame_not_maintenance_irq() {
        let frame = Frame {
            owner_tcb: 0x4020_1000,
            ptr: 0x4020_2000,
            icount: 1,
            hash: "frame".into(),
            lit_bytes: 1,
            raw: vec![1; PANEL_BYTES],
        };
        assert!(is_ready(1, 1, 1, 1, frame.owner_tcb, Some(&frame)));
        assert!(!is_ready(1, 1, 1, 0, frame.owner_tcb, Some(&frame)));
        assert!(!is_ready(1, 1, 1, 1, 0, Some(&frame)));
        assert!(!is_ready(1, 1, 1, 1, frame.owner_tcb + 4, Some(&frame)));
        assert!(!is_ready(1, 1, 1, 1, frame.owner_tcb, None));
        let zero = Frame {
            lit_bytes: 0,
            ..frame.clone()
        };
        assert!(!is_ready(1, 1, 1, 1, zero.owner_tcb, Some(&zero)));
    }

    #[test]
    fn panel_operand_pair_requires_two_distinct_in_range_values() {
        let mut image = vec![0; 0x120];
        image[0..4].copy_from_slice(&0x4020_0010u32.to_be_bytes());
        image[4..8].copy_from_slice(&0x4020_0010u32.to_be_bytes());
        image[8..12].copy_from_slice(&0x4020_0020u32.to_be_bytes());
        assert_eq!(
            operand_pair(&image, MAIN_LOAD),
            Ok((0x4020_0010, 0x4020_0020))
        );
        assert!(operand_pair(&image[..8], MAIN_LOAD).is_err());
    }

    #[test]
    fn panel_bytes_map_to_documented_corner_pixels() {
        let mut raw = vec![0; PANEL_BYTES];
        raw[7] = 1; // x=0, y=0
        raw[7 + 8 * 127] = 0x80; // x=127, y=7
        raw[0] = 1; // x=0, y=56
        raw[8 * 127] = 0x80; // x=127, y=63
        assert!(panel_pixel(&raw, 0, 0));
        assert!(panel_pixel(&raw, 127, 7));
        assert!(panel_pixel(&raw, 0, 56));
        assert!(panel_pixel(&raw, 127, 63));
        assert!(!panel_pixel(&raw, 128, 0));
    }

    #[test]
    fn frame_tracker_matches_return_owner_and_stack_and_stays_bounded() {
        let mut board = Board::new(
            Card::default(),
            SemaphoreAddresses::default(),
            CompletionPolicy::Oracle,
        );
        let page = 0x4020_0000;
        board.map_zeroed_ram_page(page).unwrap();
        let tcb_global = page;
        let front_global = page + 4;
        let frame_ptr = page + 0x100;
        board.write_guest(tcb_global, 4, 0x4020_4000).unwrap();
        board.write_guest(front_global, 4, frame_ptr).unwrap();
        board.write_guest(frame_ptr, 1, 1).unwrap();
        let mut tracker = FrameTracker::default();
        for n in 0..17u32 {
            let sp = page + 0x800 + n * 4;
            board.write_guest(sp, 4, 0x4000_1000 + n).unwrap();
            tracker
                .capture(&mut board, tcb_global, front_global, u64::from(n), sp)
                .unwrap();
        }
        assert_eq!(tracker.pending.len(), 16);
        assert_eq!(tracker.pending_dropped, 1);
        let pending = tracker.pending.front().unwrap().clone();
        tracker.complete_at_return(
            &mut board,
            tcb_global,
            pending.return_pc,
            pending.expected_a7 - 4,
        );
        assert!(tracker.latest_for(0x4020_4000).is_none());
        tracker.complete_at_return(
            &mut board,
            tcb_global,
            pending.return_pc,
            pending.expected_a7,
        );
        assert!(tracker.latest_for(0x4020_4000).is_some());
    }

    #[test]
    fn stop_success_only_accepts_requested_terminal_condition() {
        assert!(stop_success("VERIFIED_READY", "ready", true, 1, 10));
        assert!(!stop_success("instruction limit", "ready", true, 10, 10));
        assert!(stop_success("instruction limit", "limit", false, 10, 10));
        assert!(!stop_success("GuestFault", "ready", true, 10, 10));
        assert!(!stop_success("GuestFault", "limit", true, 10, 10));
    }

    #[test]
    fn retained_irq_ring_does_not_hide_late_vectors() {
        let mut ring = Vec::new();
        let mut counts = [0; 256];
        let mut dropped = 0;
        let mut batch = vec![(208, 3); 600];
        batch.push((99, 3));
        assert_eq!(
            record_deliveries(&batch, &mut ring, &mut counts, &mut dropped),
            601
        );
        assert_eq!(
            (ring.len(), dropped, counts[208], counts[99]),
            (512, 89, 600, 1)
        );
        assert_eq!(
            record_deliveries(&[(99, 3)], &mut ring, &mut counts, &mut dropped),
            1
        );
        assert_eq!((ring.len(), dropped, counts[99]), (512, 90, 2));
    }
}
