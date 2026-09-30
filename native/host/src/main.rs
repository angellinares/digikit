//! Bounded native ColdFire -> DSPI2 -> native SHARC offline host.
//!
//! This executable is deliberately Oracle-host-event replay only.  Its
//! command line requires every local input and rejects Device scheduling.

use std::{
    cell::RefCell,
    collections::HashSet,
    env,
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    rc::Rc,
    sync::Mutex,
};

use coldfire::{Bus, Cpu, InterruptPolicy};
use emmc_card::{Card, DEFAULT_CAPACITY_BLOCKS, RandomAccessRead};
use live_audio::{LivePlayer, StereoSample, sharc_lib::live_source};
use machine::{Board, CompletionPolicy, Machine, SemaphoreAddresses, Time, TimerPolicy};
use periph::dspi::Peer;
use serde_json::{Value, json};
use sharc_native::sha256::Sha256;

const MAX_FRAMES: usize = 68;
const MAX_INSTRUCTIONS: u64 = 20_000_000;
const MAX_RENDER_FRAMES: usize = 4096;
const MAX_RENDERED_INPUTS: usize = 4096;

struct Image(Mutex<File>, u64);
impl RandomAccessRead for Image {
    fn len(&self) -> u64 {
        self.1
    }
    fn read_at(&self, offset: u64, dest: &mut [u8]) -> usize {
        let Ok(mut file) = self.0.lock() else {
            return 0;
        };
        if file.seek(SeekFrom::Start(offset)).is_err() {
            return 0;
        }
        file.read(dest).unwrap_or(0)
    }
}

struct Wire(Rc<RefCell<Vec<Vec<u8>>>>, usize);
impl Peer for Wire {
    fn exchange(&mut self, tx: &[u8]) -> Vec<u8> {
        let mut frames = self.0.borrow_mut();
        assert!(frames.len() < self.1, "wire-frame bound exceeded");
        frames.push(tx.to_vec());
        vec![0; tx.len()]
    }
}

#[derive(Debug)]
struct Args {
    events: PathBuf,
    profile: PathBuf,
    mstate: PathBuf,
    card: PathBuf,
    firmware: FirmwareInput,
    pack: PathBuf,
    sharc_lib: PathBuf,
    expected_wire: PathBuf,
    out: PathBuf,
    frames: usize,
    limit: u64,
    render_frames: usize,
    rendered_input_log: Option<PathBuf>,
}

#[derive(Debug)]
enum FirmwareInput {
    Main(PathBuf),
    Syx(PathBuf),
}

fn usage() -> ! {
    eprintln!(
        "usage: dt2-native-host --timer oracle --events ABS --profile ABS --mstate ABS --card ABS (--main ABS | --syx ABS) --pack ABS --sharc-lib ABS --expected-wire ABS --out ABS --frames 1..=68 --limit 1..=20000000 --render-frames 1..=4096 [--rendered-input-log ABS]"
    );
    std::process::exit(2)
}
fn absolute(value: String, name: &str) -> PathBuf {
    let path = PathBuf::from(value);
    assert!(
        path.is_absolute() && path.is_file(),
        "{name} must be an existing absolute file"
    );
    path
}
fn absolute_output(value: String, name: &str) -> PathBuf {
    let path = PathBuf::from(value);
    assert!(path.is_absolute(), "{name} must be an absolute path");
    path
}
fn parse_args() -> Args {
    let mut it = env::args().skip(1);
    let mut v = std::collections::BTreeMap::new();
    while let Some(key) = it.next() {
        if key == "--help" || key == "-h" {
            usage();
        }
        let value = it.next().unwrap_or_else(|| usage());
        assert!(
            key.starts_with("--") && v.insert(key, value).is_none(),
            "duplicate or invalid argument"
        );
    }
    assert_eq!(
        v.remove("--timer").as_deref(),
        Some("oracle"),
        "only --timer oracle is validated"
    );
    let events = absolute(v.remove("--events").unwrap_or_else(|| usage()), "--events");
    let profile = absolute(
        v.remove("--profile").unwrap_or_else(|| usage()),
        "--profile",
    );
    let mstate = absolute(v.remove("--mstate").unwrap_or_else(|| usage()), "--mstate");
    let card = absolute(v.remove("--card").unwrap_or_else(|| usage()), "--card");
    let firmware = match (v.remove("--main"), v.remove("--syx")) {
        (Some(main), None) => FirmwareInput::Main(absolute(main, "--main")),
        (None, Some(syx)) => FirmwareInput::Syx(absolute(syx, "--syx")),
        _ => {
            eprintln!("provide exactly one of --main or --syx");
            usage();
        }
    };
    let pack = absolute(v.remove("--pack").unwrap_or_else(|| usage()), "--pack");
    let sharc_lib = absolute(
        v.remove("--sharc-lib").unwrap_or_else(|| usage()),
        "--sharc-lib",
    );
    let expected_wire = absolute(
        v.remove("--expected-wire").unwrap_or_else(|| usage()),
        "--expected-wire",
    );
    let out = absolute_output(v.remove("--out").unwrap_or_else(|| usage()), "--out");
    assert!(out.parent().is_some(), "--out must be an absolute path");
    let frames = v
        .remove("--frames")
        .unwrap_or_else(|| usage())
        .parse()
        .unwrap();
    let limit = v
        .remove("--limit")
        .unwrap_or_else(|| usage())
        .parse()
        .unwrap();
    let render_frames = v
        .remove("--render-frames")
        .unwrap_or_else(|| usage())
        .parse()
        .unwrap();
    let rendered_input_log = v
        .remove("--rendered-input-log")
        .map(|value| absolute_output(value, "--rendered-input-log"));
    assert!(v.is_empty(), "unknown argument");
    assert!((1..=MAX_FRAMES).contains(&frames));
    assert!((1..=MAX_INSTRUCTIONS).contains(&limit));
    assert!((1..=MAX_RENDER_FRAMES).contains(&render_frames));
    assert!(
        rendered_input_log.is_none() || frames * render_frames <= MAX_RENDERED_INPUTS,
        "--rendered-input-log requires frames * render-frames <= {MAX_RENDERED_INPUTS}"
    );
    Args {
        events,
        profile,
        mstate,
        card,
        firmware,
        pack,
        sharc_lib,
        expected_wire,
        out,
        frames,
        limit,
        render_frames,
        rendered_input_log,
    }
}

fn main_image(input: &FirmwareInput) -> Vec<u8> {
    match input {
        FirmwareInput::Main(path) => bytes(path),
        FirmwareInput::Syx(path) => {
            let syx = bytes(path);
            let firmware = dt2_firmware_loader::parse(&syx)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            let mut mains = firmware
                .sections
                .into_iter()
                .filter(|section| section.id == 3);
            let main = mains
                .next()
                .unwrap_or_else(|| panic!("{}: no ELE3 section 3 MAIN_OS", path.display()));
            assert!(
                mains.next().is_none(),
                "{}: multiple ELE3 section 3 MAIN_OS entries",
                path.display()
            );
            assert_eq!(
                main.destination,
                0x4000_0400,
                "{}: section 3 is not the ColdFire MAIN_OS image",
                path.display()
            );
            main.bytes
        }
    }
}

fn digest(data: &[u8]) -> String {
    let mut hash = Sha256::new();
    hash.update(data);
    hash.finish().iter().map(|b| format!("{b:02x}")).collect()
}
fn digest_file(path: &Path) -> String {
    let mut file = File::open(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut hash = Sha256::new();
    let mut buf = [0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    hash.finish().iter().map(|b| format!("{b:02x}")).collect()
}
fn bytes(path: &Path) -> Vec<u8> {
    fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}
fn input_json(path: &Path) -> Value {
    serde_json::from_slice(&bytes(path)).unwrap()
}
fn addr(profile: &Value, key: &str) -> u32 {
    profile[key].as_u64().unwrap().try_into().unwrap()
}
fn hex(encoded: &str) -> Vec<u8> {
    assert!(encoded.len() <= 128 && encoded.len().is_multiple_of(2) && !encoded.is_empty());
    encoded
        .as_bytes()
        .chunks_exact(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect()
}
fn dtfr(frames: &[Vec<u8>]) -> Vec<u8> {
    let mut out = b"DTFR".to_vec();
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&(frames.len() as u32).to_le_bytes());
    for frame in frames {
        out.extend_from_slice(&(frame.len() as u32).to_le_bytes());
        out.extend_from_slice(frame);
    }
    out
}
fn write_wav(path: &Path, samples: &[StereoSample]) -> Result<(), String> {
    let bytes = (samples.len() * 8) as u32;
    let mut out = File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut write = |data: &[u8]| {
        out.write_all(data)
            .map_err(|e| format!("{}: {e}", path.display()))
    };
    write(b"RIFF")?;
    write(&(36 + bytes).to_le_bytes())?;
    write(b"WAVEfmt ")?;
    write(&16u32.to_le_bytes())?;
    write(&3u16.to_le_bytes())?;
    write(&2u16.to_le_bytes())?;
    write(&48_000u32.to_le_bytes())?;
    write(&(48_000u32 * 8).to_le_bytes())?;
    write(&8u16.to_le_bytes())?;
    write(&32u16.to_le_bytes())?;
    write(b"data")?;
    write(&bytes.to_le_bytes())?;
    for s in samples {
        write(&s.l.to_le_bytes())?;
        write(&s.r.to_le_bytes())?;
    }
    Ok(())
}

fn output_dir(path: &Path) -> Result<(), String> {
    if path.exists() && !path.is_dir() {
        return Err(format!(
            "output path {} already exists and is not a directory",
            path.display()
        ));
    }
    fs::create_dir_all(path)
        .map_err(|e| format!("cannot create output directory {}: {e}", path.display()))?;
    Ok(())
}

fn main() {
    let a = parse_args();
    let reference = input_json(&a.events);
    let profile = input_json(&a.profile);
    assert_eq!(reference["format_version"], 1);
    assert!(matches!(
        reference["stepping"].as_str(),
        Some("counted") | Some("fast-observed")
    ));
    assert_eq!(
        reference["identity"]["main_sha256"],
        profile["image_sha256"]
    );
    let state = machine::state::parse(&bytes(&a.mstate)).unwrap();
    assert_eq!(state.manifest["main_sha256"], profile["image_sha256"]);
    let main = main_image(&a.firmware);
    assert_eq!(
        digest(&main),
        profile["image_sha256"].as_str().unwrap(),
        "selected firmware does not match the checkpoint profile"
    );
    assert_eq!(
        reference["dtfr_sha256"].as_str(),
        Some(digest(&bytes(&a.expected_wire)).as_str())
    );
    let expected = bytes(&a.expected_wire);
    assert!(expected.starts_with(b"DTFR\x01\0\0\0"));
    let card_bytes = fs::metadata(&a.card).unwrap().len();
    let card_hash = digest_file(&a.card);
    let card = Card::with_backing(
        DEFAULT_CAPACITY_BLOCKS,
        Some(Box::new(Image(
            Mutex::new(File::open(&a.card).unwrap()),
            card_bytes,
        ))),
    )
    .unwrap();
    let mut board = Board::new(
        card,
        SemaphoreAddresses::default(),
        CompletionPolicy::Oracle,
    );
    board.attach_time(Time::with_dtims(
        TimerPolicy::Oracle,
        vec![3, 2, 0],
        vec![3],
        132_000_000.0,
    ));
    let exchanged = Rc::new(RefCell::new(Vec::new()));
    board.dma.peer = Box::new(Wire(Rc::clone(&exchanged), a.frames));
    let mut machine = Machine::new(Cpu::new(), board);
    machine.apply_state(&state).unwrap();
    machine.board.enable_oracle_sdram_faults();
    machine.cpu.emac.mask = 0;
    let status = 0xec03_802c;
    let mut mmio = state.mmio_forced.clone();
    let saved = mmio
        .get(&status)
        .copied()
        .unwrap_or_else(|| machine.board.read32(status).unwrap());
    mmio.insert(status, saved | periph::dspi::SR_LINK_IDLE);
    machine.board.install_forced_mmio(mmio).unwrap();
    machine.board.write32(addr(&profile, "gate"), 0).unwrap();
    let spins: HashSet<u32> = (0..main.len().saturating_sub(1))
        .step_by(2)
        .filter(|&i| main[i..i + 2] == [0x60, 0xfe])
        .map(|i| 0x4000_0400 + i as u32)
        .collect();
    assert!(
        spins.contains(&machine.cpu.pc),
        "MSTATE is not at a validated Oracle idle site"
    );
    let host: Vec<&Value> = reference["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "force" || e["kind"] == "feed")
        .collect();
    assert!(
        !host.is_empty()
            && host
                .windows(2)
                .all(|w| w[0]["clock"].as_u64() <= w[1]["clock"].as_u64())
    );
    let vector: u8 = addr(&profile, "vector").try_into().unwrap();
    let level = periph::intc::VECTOR_BASE
        .iter()
        .enumerate()
        .find(|(_, first)| u16::from(vector) >= **first && u16::from(vector) < **first + 64)
        .and_then(|(i, first)| {
            let icr = machine
                .board
                .read8(periph::intc::BASES[i] + 0x40 + u32::from(vector) - u32::from(*first))
                .unwrap()
                & 7;
            (icr != 0).then_some(icr)
        });
    let (mut source, library_info) =
        live_source(&a.sharc_lib, &a.pack, 1.0, Some(&card_hash)).unwrap();
    let log = source.log();
    let rendered_inputs = a
        .rendered_input_log
        .as_ref()
        .map(|_| source.enable_rendered_input_log(a.frames * a.render_frames));
    let player = LivePlayer::offline(Box::new(source));
    let mut rendered = Vec::new();
    let mut delivered = Vec::new();
    let mut next = 0usize;
    let mut idle_passes = 0u64;
    let mut idle_yields = 0u64;
    let mut forces = 0u64;
    let mut stop = "instruction_limit";
    for done in 0..a.limit {
        assert_eq!(machine.clock, done);
        while next < host.len() && host[next]["clock"].as_u64() == Some(done) {
            let event = host[next];
            if event["kind"] == "feed" {
                let base = machine
                    .board
                    .read32(addr(&profile, "uart8_ring_ptr"))
                    .unwrap();
                let mut write = machine.board.read32(0xfc04_5450).unwrap();
                assert!(write.wrapping_sub(base) <= 0x3ff);
                for byte in hex(event["bytes_hex"].as_str().unwrap()) {
                    machine.board.write8(write, byte).unwrap();
                    write = base + ((write.wrapping_sub(base) + 1) & 0x3ff);
                }
                machine.board.write32(0xfc04_5450, write).unwrap();
                assert!(
                    machine
                        .cpu
                        .take_interrupt(&mut machine.board, 154, None, InterruptPolicy::Oracle)
                        .unwrap()
                );
            } else {
                machine.board.write32(addr(&profile, "counter"), 0).unwrap();
                assert!(
                    machine
                        .cpu
                        .take_interrupt(&mut machine.board, vector, level, InterruptPolicy::Oracle)
                        .unwrap()
                );
                forces += 1;
            }
            next += 1;
        }
        if spins.contains(&machine.cpu.pc) {
            idle_passes += 1;
            if idle_passes.is_multiple_of(20_000) {
                assert!(
                    machine
                        .cpu
                        .take_interrupt(&mut machine.board, 32, None, InterruptPolicy::Oracle)
                        .unwrap()
                );
                idle_yields += 1;
            }
        }
        if let Err(error) = machine.step_timed() {
            stop = "machine_stop";
            eprintln!("machine stopped at {done}: {error:?}");
            break;
        }
        let mut pending = exchanged.borrow_mut();
        for frame in pending.drain(..) {
            player.push_frame(&frame);
            delivered.push(frame);
            let mut pcm = vec![StereoSample::default(); a.render_frames * 32];
            assert_eq!(player.render_offline(&mut pcm).unwrap(), a.render_frames);
            rendered.extend(pcm);
        }
        drop(pending);
        if delivered.len() == a.frames {
            stop = "requested_wire_frames";
            break;
        }
    }
    let actual = dtfr(&delivered);
    let wire_ok = actual == expected;
    let delivered_wire_order: Vec<Value> = delivered
        .iter()
        .enumerate()
        .map(|(ordinal, frame)| {
            json!({"ordinal": ordinal, "byte_len": frame.len(), "sha256": digest(frame)})
        })
        .collect();
    let pcm_bytes = bytemuckless_pcm(&rendered);
    if let Err(error) = output_dir(&a.out) {
        eprintln!("output error: {error}");
        std::process::exit(1);
    }
    let wav = a.out.join("native-host.wav");
    if let Err(error) = write_wav(&wav, &rendered) {
        eprintln!("output error: {error}");
        std::process::exit(1);
    }
    let (rendered_input_diagnostic, rendered_input_write_ok) =
        if let (Some(path), Some(inputs)) = (&a.rendered_input_log, &rendered_inputs) {
            let inputs = inputs.lock().unwrap();
            let count = inputs.records().len();
            let dropped = inputs.dropped;
            let error = inputs.write_ndjson(path).err();
            let write_ok = error.is_none();
            (
                json!({"path": path, "records": count, "dropped": dropped, "error": error}),
                write_ok,
            )
        } else {
            (Value::Null, true)
        };
    let log = log.lock().unwrap();
    let success = stop == "requested_wire_frames"
        && delivered.len() == a.frames
        && wire_ok
        && !rendered.is_empty()
        && log.nonzero_frames > 0
        && log.stopped == 0
        && log.dma_failures == 0
        && rendered_input_write_ok;
    let report = json!({
        "format_version": 1, "policy": "oracle-recorded-force-and-feed/native-timer-and-idle-yield", "timer": "oracle",
        "timing": "wire queue is drained after each native step; each delivered TX renders render_frames immediate offline SHARC frames. This is offline queue timing, not captured device or callback timing.",
        "inputs": {"events": a.events, "profile": a.profile, "mstate": a.mstate, "card_sha256": card_hash, "main_sha256": digest(&main), "firmware": format!("{:?}", a.firmware), "pack": a.pack, "sharc_library": a.sharc_lib, "library_info": library_info},
        "limits": {"requested_wire_frames": a.frames, "instruction_limit": a.limit, "render_frames_per_tx": a.render_frames},
        "result": {"stop_cause": stop, "actual_native_instructions": machine.clock, "host_events_consumed": next, "forces": forces, "idle_yields": idle_yields, "delivered_wire_frames": delivered.len(), "delivered_wire_order": delivered_wire_order, "wire_sha256": digest(&actual), "expected_wire_sha256": digest(&expected), "wire_matches_expected": wire_ok, "pcm_samples": rendered.len(), "pcm_sha256": digest(&pcm_bytes), "wav": wav, "sharc": {"frames":log.frames,"clean":log.clean,"stopped":log.stopped,"dma_failures":log.dma_failures,"instructions":log.instructions,"first_stop":log.first_stop,"nonzero_frames":log.nonzero_frames,"first_nonzero":log.first_nonzero}, "rendered_input_log": rendered_input_diagnostic, "queue": {"pushed": player.frame_stats().pushed, "taken": player.frame_stats().taken, "repeats": player.frame_stats().repeats}},
        "success": success
    });
    let report_path = a.out.join("native-host-report.json");
    if let Err(error) = fs::write(&report_path, serde_json::to_vec_pretty(&report).unwrap()) {
        eprintln!("output error: {}: {error}", report_path.display());
        std::process::exit(1);
    }
    if !success {
        eprintln!("bounded host gate failed; see {}", report_path.display());
        std::process::exit(1);
    }
}

fn bytemuckless_pcm(samples: &[StereoSample]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(samples.len() * 8);
    for sample in samples {
        bytes.extend_from_slice(&sample.l.to_le_bytes());
        bytes.extend_from_slice(&sample.r.to_le_bytes());
    }
    bytes
}
