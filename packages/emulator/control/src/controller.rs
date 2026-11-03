//! The emulator controller: one owner thread for the (`!Send`) coupled
//! runtime, driven by commands. The desktop app's webview commands and the
//! protocol's RPC requests are two front ends to the same loop; both are
//! plain messages on one channel, so any host (desktop, daemon, a browser
//! worker with its own dispatcher) uses the same controller.
//!
//! The loop serves a command whenever one is waiting and otherwise advances
//! the guest by one chunk when a job (`step`, `run_until`, a cold boot) or the
//! free-run mode asks for it, so the UI and status requests stay live while a
//! job runs. See `docs/design/emulator-control.md`.

use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use digi_coupled::{
    AudioOptions, CoupledRuntime as Runtime, STEP_CHUNK,
    scenario::{self, Action, Scenario},
};
use elektron_native_boot::{Snapshot, dsp_patch::DspPatch};
use emmc_card::{Card, CardIdentity, RandomAccessRead};
use serde_json::{Value, json};

use crate::{
    cardmirror::Mirror,
    cards::{self, CardPaths},
    devicefolders::{DeviceFolders, DeviceFoldersConfig, device_ok},
    events::Events,
    firmware::{self, Analysis},
    health::{self, Alert, Health},
    load,
    proto::{Params, RpcError, RpcResult, base64, code},
    runtime_patch,
};

pub const MAX_SYX: usize = 32 * 1024 * 1024;
/// Guest instructions NO is held when the controller closes the boot modal.
const NO_HOLD: u64 = 2_000_000;
/// Guest instructions per emulated second (the ColdFire oracle clock).
const CF_IPS: f64 = 132_000_000.0;
/// How far a paced runtime may lag the wall clock before its stretch starts again, in seconds.
const PACE_SLACK_S: f64 = 0.05;

/// Pacing of a free-running runtime without audio: after `wall_s` seconds of wall clock a runtime that executed `executed` instructions
/// (`None`: its count went back) is ahead of the clock (`Some(true)`: wait), on time or a little behind (`Some(false)`: step), or
/// lagging by more than the slack (`None`: start the stretch again).
fn pace_ahead(wall_s: f64, executed: Option<u64>) -> Option<bool> {
    let seconds = executed? as f64 / CF_IPS;
    if seconds + PACE_SLACK_S < wall_s {
        None
    } else {
        Some(seconds > wall_s)
    }
}

/// Most guest instructions a cold boot may take before it is called stuck.
const BOOT_LIMIT: u64 = 3_000_000_000;

struct FileBacking {
    file: Mutex<File>,
    len: u64,
    error: Arc<Mutex<Option<String>>>,
}
impl RandomAccessRead for FileBacking {
    fn len(&self) -> u64 {
        self.len
    }
    fn read_at(&self, offset: u64, destination: &mut [u8]) -> usize {
        let mut file = match self.file.lock() {
            Ok(file) => file,
            Err(_) => return 0,
        };
        if let Err(error) = file
            .seek(SeekFrom::Start(offset))
            .and_then(|_| file.read_exact(destination))
        {
            *self.error.lock().expect("backing error mutex") = Some(error.to_string());
            return 0;
        }
        destination.len()
    }
}
type ReadError = Arc<Mutex<Option<String>>>;

fn card(path: PathBuf) -> Result<(Card, ReadError), String> {
    let metadata = std::fs::metadata(&path).map_err(|e| format!("card image: {e}"))?;
    if !metadata.is_file() || metadata.len() % 512 != 0 {
        return Err("card image must be a regular 512-byte aligned file".into());
    }
    let identity = CardIdentity::from_env()?;
    let blocks = u32::try_from(metadata.len() / 512).map_err(|_| "card image is too large")?;
    if blocks > identity.blocks() {
        return Err(format!(
            "card image has {blocks:#x} sectors, more than the {} identity's {:#x}",
            identity.name(),
            identity.blocks()
        ));
    }
    let error = Arc::new(Mutex::new(None));
    let backing = FileBacking {
        file: Mutex::new(File::open(&path).map_err(|e| format!("card image: {e}"))?),
        len: metadata.len(),
        error: error.clone(),
    };
    let mut card = Card::with_backing(identity, Some(Box::new(backing)))
        .map_err(|e| format!("card: {e:?}"))?;
    apply_saved_overlay(&mut card, &path);
    Ok((card, error))
}

/// Where a card image keeps the firmware's writes between sessions.
pub fn overlay_path(image: &Path) -> PathBuf {
    image.with_file_name("overlay.bin")
}

/// Put the writes a synced session saved next to `image` (`overlay.bin`) back on a fresh card. A damaged or
/// foreign file is ignored with a message. A ready snapshot restored afterwards brings its own overlay.
fn apply_saved_overlay(card: &mut Card, image: &Path) {
    let path = overlay_path(image);
    let Ok(bytes) = std::fs::read(&path) else { return };
    let result = plusdrive_format::parse_overlay(&bytes)
        .map_err(|e| e.to_string())
        .and_then(|sectors| apply_overlay_sectors(card, &sectors));
    if let Err(error) = result {
        eprintln!("card: ignoring {}: {error}", path.display());
    }
}

/// Lay sectors the guest wrote over a fresh card.
fn apply_overlay_sectors(card: &mut Card, sectors: &[emmc_card::CardOverlaySector]) -> Result<(), String> {
    let state = emmc_card::CardCheckpoint {
        blocks: card.blocks(),
        rca: card.rca(),
        selected: card.selected(),
        overlay: Default::default(),
    };
    card.restore_checkpoint_sectors(&state, sectors).map_err(|e| format!("{e:?}"))
}

/// A digest of an overlay, for the key of a cached cold boot that started from it.
#[cfg(feature = "coupled-audio")]
fn overlay_digest(sectors: &[emmc_card::CardOverlaySector]) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    for s in sectors {
        hash.update(s.sector.to_le_bytes());
        hash.update(&s.written);
        hash.update(&s.data);
    }
    hash.finalize().to_vec()
}

fn card_for(image: Option<PathBuf>) -> Result<(Option<Card>, Option<ReadError>), String> {
    match image {
        Some(path) => card(path).map(|(card, error)| (Some(card), Some(error))),
        None => Ok((None, None)),
    }
}

fn runtime_from(
    bytes: &[u8],
    image: Option<PathBuf>,
    audio: Option<&AudioOptions>,
) -> Result<(Runtime, Option<ReadError>), String> {
    let (card, error) = card_for(image)?;
    Runtime::new(bytes, card, audio).map(|runtime| (runtime, error))
}

pub type Reply = mpsc::Sender<Result<Snapshot, String>>;
pub type JsonReply = mpsc::Sender<Result<Value, String>>;
pub type RpcReply = mpsc::Sender<RpcResult>;

/// What a host sends the controller. The first group is the desktop app's
/// webview protocol (session-checked, one `Snapshot` per answer); `Rpc` is the
/// control protocol.
pub enum Command {
    Diagnostics {
        session: u64,
        reply: JsonReply,
    },
    AudioStatus {
        session: u64,
        reply: JsonReply,
    },
    Startup {
        reply: Reply,
    },
    Load {
        session: u64,
        bytes: Vec<u8>,
        card: Option<PathBuf>,
        reply: Reply,
    },
    Restart {
        session: u64,
        next_session: u64,
        reply: Reply,
    },
    Step {
        session: u64,
        budget: u32,
        reply: Reply,
    },
    Button {
        session: u64,
        code: u8,
        down: bool,
        reply: Reply,
    },
    Turn {
        session: u64,
        encoder: u8,
        detents: i32,
        reply: Reply,
    },
    Tap {
        session: u64,
        code: u8,
        hold: u32,
        reply: Reply,
    },
    Stop {
        session: u64,
        reply: Reply,
    },
    Rpc {
        method: String,
        params: Value,
        reply: RpcReply,
    },
}

/// Who advances the guest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The webview pump's `emu_step` (desktop scenario runs).
    Ui,
    /// Nobody; `emu_step` returns the status without stepping.
    Paused,
    /// The controller, chunk after chunk, whenever it is idle, waiting while
    /// the output audio queue is at its target (the desktop default).
    Free,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::Ui => "ui",
            Mode::Paused => "paused",
            Mode::Free => "free",
        }
    }
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "ui" => Some(Mode::Ui),
            "paused" => Some(Mode::Paused),
            "free" => Some(Mode::Free),
            _ => None,
        }
    }
}

/// A scenario to run in the `Step` path (the desktop `--scenario` mode); the
/// controller raises `scenario.done` when it ends.
pub struct ScenarioSetup {
    pub scenario: Scenario,
}

pub fn load_scenario(spec: &str) -> Result<Scenario, String> {
    if Path::new(spec).is_file() {
        let text = std::fs::read_to_string(spec).map_err(|e| format!("scenario file: {e}"))?;
        let value: Value =
            serde_json::from_str(&text).map_err(|e| format!("scenario file: {e}"))?;
        let found = scenario::find_scenario(&value)
            .ok_or("scenario file has no scenario (need \"scenarios\" or a scenario object)")?;
        scenario::parse_scenario(found)
    } else {
        scenario::parse_scenarios(scenario::DEFAULT_SCENARIOS)?
            .into_iter()
            .find(|candidate| candidate.name == spec)
            .ok_or_else(|| format!("unknown scenario: {spec}"))
    }
}

pub struct Config {
    pub audio: Option<AudioOptions>,
    /// The ready inputs (DSP image, DSP state, ColdFire snapshot) of other devices than `audio`'s, for a build with
    /// the second generated core: a firmware loaded for one of them boots coupled (audio, DSP, restart) instead of
    /// ColdFire only. Matched by the firmware the inputs were made for (`SOURCE.json`, else `expected_syx`).
    pub audio_alt: Vec<AudioOptions>,
    pub scenario: Option<ScenarioSetup>,
    pub mode: Mode,
    /// Where patched-firmware snapshots are cached (`<run_dir>/patched`).
    pub run_dir: PathBuf,
    pub events: Events,
    /// Called on the controller thread when `quit` arrives, after the reply.
    pub on_quit: Option<Box<dyn Fn() + Send>>,
    /// Keep the last loaded card in `<run_dir>/cards/last.json` and mount it
    /// again when a DT2 firmware is loaded without a card (the desktop app;
    /// off for scenarios and tests, which must not depend on earlier runs).
    pub remember_card: bool,
    /// Free running without a DSP audio session (a ColdFire-only runtime, as a DT2 opened in a session that has the DN2's audio inputs)
    /// waits for the wall clock: one emulated second per second. Without it such a runtime runs as fast as the CPU allows (about 20 times
    /// real time), and a mouse click on the faceplate lasts seconds of emulated time: the firmware sees a long press, so SETTINGS opens
    /// nothing. The desktop app sets it; the daemon and the tests keep the full speed.
    pub pace_free: bool,
    /// The +Drive folder the startup card (`firmware.load`'s `card`) was built from: `card.export` writes
    /// there, and with `sync` a quit does so too.
    pub card_folder: Option<CardSource>,
    /// The folder the module store is mirrored to for the first load of the session (`--mirror-dir`); with `device_folders` it is only
    /// the default for the first device: a load of another device uses that device's folder. None: the device's own folder.
    pub mirror_dir: Option<PathBuf>,
    /// One folder per device (`plus_drive/<device>/` or the user's choice): the module store is mirrored to it, and the card of a load
    /// without a `card` is built from it (`devicefolders.rs`). None: only `mirror_dir`, if given.
    pub device_folders: Option<DeviceFoldersConfig>,
    /// The repository root that `firmware.load {runtime: true}` reads the module library and device files from
    /// (default: `$DIGI_REPO_ROOT`, else the working directory's repository; see `runtime_patch::find_root`).
    pub repo_root: Option<PathBuf>,
}

/// A +Drive folder and what to do with the firmware's changes to its card.
#[derive(Clone, Debug)]
pub struct CardSource {
    pub dir: PathBuf,
    /// Export the changes to `dir` when the session ends.
    pub sync: bool,
    /// Where exports and backups go (default `$DIGI_PLUSDRIVE_OUT` or `out/plusdrive`).
    pub out: Option<PathBuf>,
}

impl Config {
    pub fn new(audio: Option<AudioOptions>) -> Self {
        Self {
            audio,
            audio_alt: Vec::new(),
            scenario: None,
            mode: Mode::Ui,
            run_dir: PathBuf::from("out/native/run"),
            events: Events::default(),
            on_quit: None,
            remember_card: false,
            pace_free: false,
            card_folder: None,
            mirror_dir: None,
            device_folders: None,
            repo_root: None,
        }
    }
}

/// A cloneable way to talk to a running controller.
#[derive(Clone)]
pub struct Handle {
    sender: mpsc::Sender<Command>,
    events: Events,
}

impl Handle {
    pub fn sender(&self) -> &mpsc::Sender<Command> {
        &self.sender
    }
    pub fn events(&self) -> &Events {
        &self.events
    }
    /// Run one protocol method and wait for its result. A job method
    /// (`step`, `run_until`, a cold-boot `firmware.load`) answers when it ends.
    pub fn call(&self, method: &str, params: Value) -> RpcResult {
        let (reply, response) = mpsc::channel();
        self.sender
            .send(Command::Rpc {
                method: method.to_string(),
                params,
                reply,
            })
            .map_err(|_| RpcError::io("emulator controller stopped"))?;
        response
            .recv()
            .map_err(|_| RpcError::io("emulator controller dropped the reply"))?
    }
}

/// Start the controller thread.
pub fn spawn(config: Config) -> (Handle, thread::JoinHandle<()>) {
    let (sender, receiver) = mpsc::channel();
    let events = config.events.clone();
    let join = thread::Builder::new()
        .name("digi-controller".into())
        .spawn(move || Actor::new(receiver, config).run())
        .expect("spawn controller thread");
    (Handle { sender, events }, join)
}

/// What the controller knows about the firmware it is running.
/// What a swap leaves behind: the image now running is the baseline with module code written over it, and the boot cache holds the
/// baseline's ready state, not the running image's.
struct ReloadState {
    /// SHA-256 of the image running after the last `dev.reload`. The state is valid while `Loaded::current` still is that image.
    running_sha: String,
    /// The image whose ready pair is in the boot cache: the last image that was booted or restored from it.
    baseline: Vec<u8>,
}

/// A `dev.reload` reply with the build report of `build: true` added.
fn with_build(mut reply: Value, build: &Option<Value>) -> Value {
    if let Some(b) = build {
        reply["build"] = b.clone();
    }
    reply
}

/// The modules `dev.reload` swaps without a boot, besides every module that requires `core/ui`.
const SWAP_DEFAULT: &[&str] = &["core/ui", "core/digikit-menu"];

struct Loaded {
    /// The .syx the DSP audio profile and the device profile belong to, when
    /// known (a first load of an already patched .syx has none).
    base: Option<Vec<u8>>,
    base_sha: String,
    /// The firmware as running now (the base plus injected changes).
    current: Vec<u8>,
    card: Option<PathBuf>,
    base_analysis: Option<Analysis>,
    current_analysis: Option<Analysis>,
}

impl Loaded {
    fn stock(bytes: Vec<u8>, card: Option<PathBuf>) -> Self {
        Self {
            base_sha: firmware::sha256_hex(&bytes),
            base: Some(bytes.clone()),
            current: bytes,
            card,
            base_analysis: None,
            current_analysis: None,
        }
    }
    /// A patched image loaded as stock (the daemon's command line) is its own base here. Its patch record names the real base:
    /// take the stock image of that SHA-256 from `firmware/` (the ready inputs and the DSP at boot belong to the base).
    fn find_base(&mut self) -> Result<(), String> {
        if self.base.as_deref() != Some(self.current.as_slice()) {
            return Ok(());
        }
        let Some(record) = patchkit::verify::patch_record(&self.current).ok().flatten() else {
            return Ok(());
        };
        if record.base_sha256 == self.base_sha {
            return Ok(());
        }
        let registry =
            plusdrive_format::device_profile::Registry::load().map_err(|e| e.to_string())?;
        let name = registry
            .identify(&record.base_sha256)
            .map_err(|e| format!("the patched image's base is not a known firmware: {e}"))?
            .1
            .filename
            .clone()
            .ok_or("the base firmware has no file name in its device file")?;
        // `$DIGI_FIRMWARE_DIR`, else `firmware/` of the working directory or an ancestor of the executable (as `devices/`)
        let mut dirs: Vec<PathBuf> = Vec::new();
        if let Some(dir) = std::env::var_os("DIGI_FIRMWARE_DIR") {
            dirs.push(dir.into());
        } else {
            if let Ok(cwd) = std::env::current_dir() {
                dirs.extend(cwd.ancestors().map(|a| a.join("firmware")));
            }
            if let Ok(exe) = std::env::current_exe() {
                dirs.extend(exe.ancestors().skip(1).map(|a| a.join("firmware")));
            }
        }
        let bytes = dirs
            .iter()
            .find_map(|dir| std::fs::read(dir.join(&name)).ok())
            .ok_or_else(|| format!("the patched image's base firmware/{name} was not found"))?;
        if firmware::sha256_hex(&bytes) != record.base_sha256 {
            return Err(format!(
                "firmware/{name} is not the base this image was patched from"
            ));
        }
        self.base_sha = record.base_sha256;
        self.base = Some(bytes);
        self.base_analysis = None;
        Ok(())
    }
    fn ensure_analysis(&mut self) -> Result<(), String> {
        if self.current_analysis.is_none() {
            self.current_analysis = Some(Analysis::of(&self.current)?);
        }
        if self.base_analysis.is_none() {
            self.base_analysis = match &self.base {
                Some(base) if *base == self.current => self.current_analysis.clone(),
                Some(base) => Some(Analysis::of(base)?),
                None => None,
            };
        }
        Ok(())
    }
}

struct Scheduled {
    at: u64,
    action: Action,
}

enum Until {
    Elapsed(u64),
    Icount(u64),
    Frames(u64),
}

enum JobKind {
    Steps {
        end: u64,
    },
    Until(Until),
    /// A cold boot to the ready state; saves the snapshot cache when given.
    Boot {
        cache: Option<PathBuf>,
        limit: u64,
    },
}

struct Job {
    kind: JobKind,
    reply: RpcReply,
    started: Instant,
    wall_limit: Option<f64>,
    /// Fields merged into the final result (the `firmware.load` report).
    partial: Value,
    /// A scenario-style label for events.
    label: &'static str,
}

struct ScenarioRun {
    scenario: Scenario,
    next: usize,
    started: Option<Instant>,
}

/// SHA-256 of the .syx the ready inputs of `audio` were made for: `SOURCE.json` next to the DSP image, else the file `expected_syx`.
fn ready_inputs_sha(audio: &AudioOptions) -> Option<String> {
    let source = audio
        .image
        .parent()
        .and_then(|dir| std::fs::read_to_string(dir.join("SOURCE.json")).ok())
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|v| v["syx_sha256"].as_str().map(str::to_string));
    source.or_else(|| {
        let bytes = std::fs::read(audio.expected_syx.as_ref()?).ok()?;
        Some(cards::sha256_hex(&bytes))
    })
}

struct Actor {
    rx: mpsc::Receiver<Command>,
    /// The audio options in force: a loaded card's own ready state replaces the
    /// launch options' until a firmware is loaded without a card.
    audio: Option<AudioOptions>,
    audio_default: Option<AudioOptions>,
    /// The ready inputs of other devices (`Config::audio_alt`): a firmware of one of them boots coupled on its own DSP.
    audio_alt: Vec<AudioOptions>,
    run_dir: PathBuf,
    events: Events,
    on_quit: Option<Box<dyn Fn() + Send>>,
    runtime: Option<Runtime>,
    /// The session id the webview protocol checks.
    session: u64,
    /// Raised whenever the runtime is replaced, whoever asked.
    epoch: u64,
    loaded: Option<Loaded>,
    read_error: Option<ReadError>,
    startup_snapshot: Option<Snapshot>,
    scenario_run: Option<ScenarioRun>,
    mode: Mode,
    job: Option<Job>,
    schedule: Vec<Scheduled>,
    /// A frame a control chunk produced that the webview has not seen yet.
    held_frame: Option<Vec<u8>>,
    /// Panel LED values (and RGB) a control chunk consumed that the webview has not seen yet.
    held_leds: Option<(String, Option<String>)>,
    /// A card being built and booted on a worker thread.
    card_job: Option<CardJob>,
    /// A patched MAIN being cold booted with DSP audio on a worker thread.
    patch_job: Option<PatchJob>,
    /// The last card job's outcome, for `card.status`.
    card_last: Value,
    /// The +Drive folder the loaded card was built from, with the card image it belongs to:
    /// `card.export` without a `dir` writes there, and a quit does so when `sync` was set.
    card_folder: Option<CardFolder>,
    /// The module store mirrored to the folder of the loaded device (`card.folder`, `card.sync`); `mirror_select` follows the device.
    mirror: Option<Mirror>,
    /// The mirror runs (`--mirror-dir` or the device folders with sync).
    mirror_on: bool,
    /// `--mirror-dir`: the folder of the first device that loads.
    mirror_first: Option<PathBuf>,
    /// The folder of each device (the user's choices and the defaults).
    folders: DeviceFolders,
    /// Build the card of a load from the device's folder (`device_folders` is set).
    device_cards: bool,
    /// Export the card's changes to the device's folder on quit (`device_folders.sync`).
    folder_sync: bool,
    /// The running card was built from this device folder: (image, folder). A card the user chose (samples) is not.
    auto_card: Option<(PathBuf, PathBuf)>,
    /// The card `default_card` just built from a device folder, taken by the load that mounts it.
    next_auto: Option<(PathBuf, PathBuf)>,
    /// Press NO once the runtime is ready (closes DT2's FILE SYSTEM OK modal).
    auto_no: bool,
    /// `firmware.load {auto_no}`: press NO once the runtime it installs is ready.
    want_auto_no: bool,
    remember_card: bool,
    /// `Config::pace_free`.
    pace_free: bool,
    /// The wall clock and the emulated instruction count at the start of the current paced stretch (see `must_wait_for_clock`).
    pace_anchor: Option<(Instant, u64)>,
    /// Taken by the first load that mounts a card image.
    startup_card_folder: Option<CardSource>,
    repo_root: Option<PathBuf>,
    /// What `firmware.load {runtime}` did to the image now running (`status.firmware.runtime`): the patch report, or null.
    runtime_info: Option<Value>,
    /// The ready state `dev.reload` restores before it writes module code (see `rpc_dev_reload`).
    reload: Option<ReloadState>,
    quit: bool,
    /// Fault, DSP halt and main-loop stall detection (events `fault`, `halt`, `stall`).
    health: Health,
    /// Live CPU meters (`status.load`).
    load_meter: load::Meter,
    health_tick: u32,
    /// The guest wrote SOFTRST to RCM.RCR (`Status::soft_reset`): the boot is restarted when the running job has finished.
    reset_pending: bool,
    /// The card writes of the run that asked for the reset, for the cold boot that follows (the card keeps its contents).
    reset_overlay: Option<Vec<emmc_card::CardOverlaySector>>,
    /// Soft resets in a row before the boot got ready (a bound for boot loops).
    boot_resets: u32,
    /// CPU exceptions run the guest's handler (RPC `cpu.exceptions`), for every runtime installed from now on.
    deliver_exceptions: bool,
    /// The RAM range the model keeps across a soft reset (`keep_ram` of the device profile), as the guest left it, for the cold boot that follows.
    reset_ram: Option<Vec<u8>>,
}

/// `plus_drive/` for the device folders: `$DIGI_PLUS_DRIVE`, else the repository's (the app runs from a checkout). None outside one.
fn default_plus_drive_root(repo_root: Option<&Path>) -> Option<PathBuf> {
    std::env::var_os("DIGI_PLUS_DRIVE")
        .map(PathBuf::from)
        .or_else(|| runtime_patch::find_root(repo_root).ok().map(|root| root.join("plus_drive")))
}

fn no_firmware<T>() -> Result<T, RpcError> {
    Err(RpcError::no_firmware())
}

#[cfg(feature = "coupled-audio")]
fn frames(runtime: &Runtime) -> u64 {
    runtime.frames_sent()
}
#[cfg(not(feature = "coupled-audio"))]
fn frames(_runtime: &Runtime) -> u64 {
    0
}

#[cfg(feature = "coupled-audio")]
fn dsp_halted(runtime: &Runtime) -> Option<String> {
    runtime.dsp_halted()
}
#[cfg(not(feature = "coupled-audio"))]
fn dsp_halted(_runtime: &Runtime) -> Option<String> {
    None
}
#[cfg(feature = "coupled-audio")]
fn sink_underruns(runtime: &Runtime) -> Option<u64> {
    runtime.sink_underruns()
}
#[cfg(not(feature = "coupled-audio"))]
fn sink_underruns(_runtime: &Runtime) -> Option<u64> {
    None
}

impl Actor {
    fn new(rx: mpsc::Receiver<Command>, config: Config) -> Self {
        Self {
            rx,
            audio_default: config.audio.clone(),
            audio_alt: config.audio_alt,
            audio: config.audio,
            run_dir: config.run_dir,
            events: config.events,
            on_quit: config.on_quit,
            runtime: None,
            session: 0,
            epoch: 0,
            loaded: None,
            read_error: None,
            startup_snapshot: None,
            scenario_run: config.scenario.map(|setup| ScenarioRun {
                scenario: setup.scenario,
                next: 0,
                started: None,
            }),
            mode: config.mode,
            job: None,
            schedule: Vec::new(),
            held_frame: None,
            held_leds: None,
            card_job: None,
            patch_job: None,
            card_last: json!({"state": "idle"}),
            card_folder: None,
            mirror: None,
            mirror_on: config.mirror_dir.is_some() || config.device_folders.as_ref().is_some_and(|f| f.sync),
            mirror_first: config.mirror_dir,
            device_cards: config.device_folders.is_some(),
            folder_sync: config.device_folders.as_ref().is_some_and(|f| f.sync),
            folders: match &config.device_folders {
                Some(f) => DeviceFolders::new(
                    f.root.clone().or_else(|| default_plus_drive_root(config.repo_root.as_deref())),
                    f.choices.clone(),
                ),
                None => DeviceFolders::default(),
            },
            auto_card: None,
            next_auto: None,
            auto_no: false,
            want_auto_no: false,
            remember_card: config.remember_card,
            pace_free: config.pace_free,
            pace_anchor: None,
            startup_card_folder: config.card_folder,
            repo_root: config.repo_root,
            runtime_info: None,
            reload: None,
            quit: false,
            health: Health::new(
                std::env::var("DIGI_STALL_MS")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(health::DEFAULT_STALL_MS),
            ),
            load_meter: load::Meter::default(),
            health_tick: 0,
            reset_pending: false,
            reset_overlay: None,
            boot_resets: 0,
            deliver_exceptions: false,
            reset_ram: None,
        }
    }

    /// The launch audio options of the device whose firmware has SHA-256 `base_sha` (the stock image a patched one is made from): the
    /// default or the alternate whose ready inputs were made for it, else the default (which then reports why it cannot be used).
    fn audio_for_base(&self, base_sha: &str) -> Option<AudioOptions> {
        let default = self.audio_default.as_ref();
        default
            .into_iter()
            .chain(self.audio_alt.iter())
            .find(|a| ready_inputs_sha(a).is_some_and(|sha| sha == base_sha))
            .or(default)
            .cloned()
    }

    /// The RAM range the device model keeps across a soft reset: `(base, bytes)` from the device file (`[digikit] keep_base`, `keep_bytes`).
    fn keep_ram_range(&mut self) -> Option<(u32, u32)> {
        let diag = self.runtime.as_mut()?.diagnostics();
        let device = diag["device"].as_str()?;
        plusdrive_format::device_profile::Registry::load().ok()?.keep_ram(device)
    }

    /// The guest asked for a soft reset (RCM.RCR): boot the same patched firmware again from the reset entry, on the card as the
    /// guest left it (its writes laid over the image), with coupled DSP audio when available.
    fn soft_reset(&mut self) {
        // The guest rewrote the flash container (the OS upgrade): the bootstrap boots what it wrote, so the emulator does too.
        let flashed = self.runtime.as_ref().and_then(|r| {
            (r.flash_writes().programs > 0).then(|| r.flash_bytes(0x80000, 0x300000).map(<[u8]>::to_vec)).flatten()
        });
        if let (Some(flash), Some(loaded)) = (flashed, self.loaded.as_mut()) {
            match patchkit::flash::image_from_flash(&flash, &loaded.current) {
                Ok(image) => {
                    let sha = firmware::sha256_hex(&image);
                    loaded.current = image;
                    loaded.current_analysis = None;
                    self.emit("flash.booted", json!({"sha256": sha}));
                }
                Err(e) => self.emit("flash.invalid", json!({"reason": e.to_string()})),
            }
        }
        // A firmware given on the command line is loaded as stock, without an analysis: make it here.
        let analysed = match self.loaded.as_mut() {
            Some(loaded) => loaded.find_base().and_then(|()| loaded.ensure_analysis()),
            None => Err("no firmware loaded".into()),
        };
        if let Err(reason) = analysed {
            self.reset_overlay = None;
            self.reset_ram = None;
            self.emit("cpu.reset", json!({"restart": "unsupported", "reason": reason}));
            self.finish_job(Err(RpcError::new(code::GUEST_FAULT, "boot loop: the restart failed")));
            self.mode = Mode::Paused;
            return;
        }
        let loaded = self.loaded.as_ref().expect("checked above");
        let (bytes, card, base_sha, base_bytes) = (
            loaded.current.clone(),
            loaded.card.clone(),
            loaded.base_sha.clone(),
            loaded.base.clone(),
        );
        let new = loaded.current_analysis.clone().expect("ensure_analysis made it");
        self.reset_overlay = self.runtime.as_ref().map(|r| r.card_overlay());
        self.reset_ram = self.keep_ram_range().and_then(|(base, bytes)| {
            self.runtime.as_mut().map(|r| r.peek_ram(base, bytes))
        });
        let (tx, _rx) = mpsc::channel();
        // A reset during boot continues the original load request, as the coupled worker does.
        let boot = self.job.take();
        let reply = boot.as_ref().map_or(&tx, |job| &job.reply);
        let mut report = boot.as_ref().map_or_else(|| json!({}), |job| job.partial.clone());
        match self.load_patched(bytes, card, new, base_sha, base_bytes, &mut report, "cold", reply) {
            Ok(true) => {
                // CF boot runs as a controller job and resumes free running. A coupled
                // worker preserves the caller's mode, including paused RPC boundaries.
                if matches!(self.job.as_ref().map(|job| &job.kind), Some(JobKind::Boot { .. })) {
                    self.mode = Mode::Free;
                }
                self.emit("cpu.reset", json!({"restart": "cold boot started"}));
            }
            Ok(false) => {
                self.reset_overlay = None;
                self.reset_ram = None;
                self.mode = Mode::Paused;
                let reason = "the boot was installed without a worker";
                self.emit("cpu.reset", json!({"restart": "unsupported", "reason": reason}));
                let _ = reply.send(Err(RpcError::new(code::GUEST_FAULT, reason)));
            }
            Err(e) => {
                self.reset_overlay = None;
                self.reset_ram = None;
                self.emit("cpu.reset", json!({"restart": "unsupported", "reason": e.message}));
                self.mode = Mode::Paused;
                let _ = reply.send(Err(e));
            }
        }
    }

    /// Finish an old-runtime target, or continue a cold boot with the coupled path's reset bound.
    fn handle_reset(&mut self) {
        if !self.reset_pending { return; }
        if matches!(self.job.as_ref().map(|j| &j.kind), Some(JobKind::Boot { .. })) {
            self.boot_resets += 1;
            self.reset_pending = false;
            if self.boot_resets > 8 {
                self.finish_job(Err(RpcError::new(code::GUEST_FAULT, "boot loop: the restart failed")));
                self.mode = Mode::Paused;
                self.emit("cpu.reset", json!({"restart": "unsupported", "reason": "boot loop", "count": self.boot_resets}));
            } else {
                self.emit("cpu.reset", json!({"during_boot": true, "count": self.boot_resets}));
                self.soft_reset();
            }
        } else {
            // Targets belong to the old runtime. Do not keep stepping it after RCM.RCR.
            self.finish_job(Ok(json!({"runtime_replaced": true})));
        }
    }

    /// Whether a free-running runtime without DSP audio is ahead of the wall clock (`Config::pace_free`). The paced stretch starts at the
    /// first check and again after any pause, job or clock rebase; a stretch that fell more than `PACE_SLACK_S` behind (a slow host, a
    /// long command) starts again instead of catching up in a burst.
    fn must_wait_for_clock(&mut self) -> bool {
        let paced = self.pace_free
            && self.mode == Mode::Free
            && self.job.is_none()
            && self.runtime.as_ref().is_some_and(|r| !r.has_audio());
        let elapsed = match self.runtime.as_ref() {
            Some(r) if paced => r.elapsed_instructions(),
            _ => {
                self.pace_anchor = None;
                return false;
            }
        };
        let now = Instant::now();
        let (start, base) = *self.pace_anchor.get_or_insert((now, elapsed));
        match pace_ahead(now.duration_since(start).as_secs_f64(), elapsed.checked_sub(base)) {
            Some(ahead) => ahead,
            // the clock went back (rebased) or the stretch lags: start a new one
            None => {
                self.pace_anchor = Some((now, elapsed));
                false
            }
        }
    }

    fn run(mut self) {
        while !self.quit {
            self.poll_card();
            self.poll_patch();
            self.mirror_tick();
            if self.reset_pending && self.job.is_none() && self.patch_job.is_none() {
                self.reset_pending = false;
                self.soft_reset();
            }
            // While a card or a patched MAIN boots on its worker thread the
            // running guest waits.
            let carding = self.card_job.is_some() || self.patch_job.is_some();
            let busy = self.runtime.is_some()
                && (self.job.is_some() || (self.mode == Mode::Free && !carding));
            let command = if busy {
                if self.job.is_none()
                    && (self.runtime.as_ref().is_some_and(Runtime::audio_ahead) || self.must_wait_for_clock())
                {
                    // Free running with the audio queue at its target, or (no audio) ahead of the wall clock: wait for
                    // a command instead of stepping.
                    match self.rx.recv_timeout(std::time::Duration::from_millis(1)) {
                        Ok(command) => Some(command),
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                } else {
                    match self.rx.try_recv() {
                        Ok(command) => Some(command),
                        Err(mpsc::TryRecvError::Empty) => None,
                        Err(mpsc::TryRecvError::Disconnected) => break,
                    }
                }
            } else if carding {
                match self.rx.recv_timeout(Duration::from_millis(50)) {
                    Ok(command) => Some(command),
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            } else if self.mirror.as_ref().is_some_and(Mirror::pending) {
                // a card write waits for its quiet period: wake up to mirror the store
                match self.rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(command) => Some(command),
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            } else {
                match self.rx.recv() {
                    Ok(command) => Some(command),
                    Err(_) => break,
                }
            };
            match command {
                Some(command) => self.command(command),
                None => self.advance(),
            }
        }
        // The handle went away without a `quit` (the desktop window closed): the card is still live.
        self.finish_card();
        if let Some(job) = self.job.take() {
            let _ = job
                .reply
                .send(Err(RpcError::io("controller stopped during a job")));
        }
        if let Some(job) = self.patch_job.take() {
            job.cancel.store(true, Ordering::Relaxed);
            let _ = job
                .reply
                .send(Err(RpcError::io("controller stopped during a cold boot")));
        }
    }

    // ----- shared helpers -------------------------------------------------

    /// Feed one chunk's status to the health watcher and emit its alerts.
    /// The DSP halt flag is read every 16th call (it needs the audio session).
    fn watch(&mut self, status: &elektron_native_boot::Status) {
        self.health_tick = self.health_tick.wrapping_add(1);
        let sample = self.health_tick % 16 == 1;
        let dsp_halted = if sample {
            self.runtime.as_ref().and_then(dsp_halted)
        } else {
            None
        };
        let sink_underruns = if sample {
            self.runtime.as_ref().and_then(sink_underruns)
        } else {
            None
        };
        let elapsed = self
            .runtime
            .as_ref()
            .map_or(0, Runtime::elapsed_instructions);
        let obs = health::Observation {
            elapsed,
            hits: status.mainloop_hits,
            main_ui: status.main_ui_reached,
            cpu_running: status.cpu_state == "running",
            error: status.error.clone(),
            dsp_halted,
            sink_underruns,
        };
        if status.soft_reset && !self.reset_pending && self.patch_job.is_none() {
            self.reset_pending = true;
        }
        for alert in self.health.observe(self.epoch, &obs) {
            match alert {
                Alert::Fault(error) => self.emit(
                    "fault",
                    json!({"error": error, "pc": status.pc, "cpu_state": status.cpu_state}),
                ),
                Alert::Halt(reason) => self.emit("halt", json!({"reason": reason})),
                Alert::Stall { hits, stalled_ms } => self.emit(
                    "stall",
                    json!({"mainloop_hits": hits, "stalled_ms": stalled_ms, "pc": status.pc,
                           "cpu_state": status.cpu_state}),
                ),
                Alert::StallEnd { hits } => self.emit("stall.end", json!({"mainloop_hits": hits})),
                Alert::Underrun { new, total } => {
                    self.emit("underrun", json!({"new": new, "total": total}))
                }
            }
        }
        if status.guard_hits > 0 {
            let firsts = self
                .runtime
                .as_mut()
                .map(|r| r.take_guard_first_hits())
                .unwrap_or_default();
            for hit in firsts {
                self.emit(
                    "guard",
                    json!({"label": hit.label, "pc": hit.pc, "addr": hit.addr, "size": hit.size,
                           "hit_icount": hit.icount, "total": status.guard_hits}),
                );
            }
        }
    }

    fn emit(&self, kind: &str, data: Value) {
        let (icount, elapsed) = self
            .runtime
            .as_ref()
            .map_or((0, 0), |r| (r.icount(), r.elapsed_instructions()));
        let mut event = json!({
            "kind": kind, "session": self.session, "epoch": self.epoch,
            "icount": icount, "elapsed": elapsed,
        });
        if let Value::Object(map) = data {
            for (key, value) in map {
                event[key] = value;
            }
        }
        self.events.emit(event);
    }

    fn envelope(&self, mut result: Value) -> Value {
        let (icount, elapsed) = self
            .runtime
            .as_ref()
            .map_or((0, 0), |r| (r.icount(), r.elapsed_instructions()));
        if !result.is_object() {
            result = json!({ "value": result });
        }
        result["session"] = self.session.into();
        result["epoch"] = self.epoch.into();
        result["icount"] = icount.into();
        result["elapsed"] = elapsed.into();
        result["mode"] = self.mode.name().into();
        result
    }

    /// Give the webview a frame a control chunk consumed.
    fn with_held(&mut self, mut snapshot: Snapshot) -> Snapshot {
        if snapshot.frame.is_none() {
            snapshot.frame = self.held_frame.take();
        } else {
            self.held_frame = None;
        }
        if snapshot.leds.is_none() {
            if let Some((leds, rgb)) = self.held_leds.take() {
                snapshot.leds = Some(leds);
                snapshot.rgb = rgb;
            }
        } else {
            self.held_leds = None;
        }
        snapshot
    }

    fn card_error(&self) -> Option<String> {
        self.read_error
            .as_ref()
            .and_then(|error| error.lock().ok().and_then(|mut slot| slot.take()))
    }

    fn install(&mut self, mut runtime: Runtime, error: Option<ReadError>) {
        runtime.set_deliver_exceptions(self.deliver_exceptions);
        self.runtime = Some(runtime);
        self.read_error = error;
        if std::mem::take(&mut self.want_auto_no) {
            self.auto_no = true;
        }
        self.schedule.clear();
        self.held_frame = None;
        self.held_leds = None;
        self.epoch += 1;
        self.load_meter.reset();
        self.mirror_select();
        self.mirror_reset();
    }

    // ----- webview commands (the pre-existing protocol) --------------------

    fn command(&mut self, command: Command) {
        let command = match command {
            Command::Diagnostics {
                session: token,
                reply,
            } => {
                let result = if token != self.session {
                    Err("stale emulator session".into())
                } else {
                    self.runtime
                        .as_mut()
                        .map(Runtime::diagnostics)
                        .ok_or_else(|| "No firmware selected".into())
                };
                let _ = reply.send(result);
                return;
            }
            Command::AudioStatus {
                session: token,
                reply,
            } => {
                let result = if token != self.session {
                    Err("stale emulator session".into())
                } else {
                    let meter = &mut self.load_meter;
                    self.runtime
                        .as_mut()
                        .map(|runtime| {
                            let mut report = runtime.audio_status().unwrap_or(Value::Null);
                            // The panel's one-second poll also carries the CPU meters.
                            if report.is_object() {
                                report["load"] = poll_load(meter, &runtime.status(), &report);
                            }
                            report
                        })
                        .ok_or_else(|| "No firmware selected".into())
                };
                let _ = reply.send(result);
                return;
            }
            Command::Rpc {
                method,
                params,
                reply,
            } => {
                self.rpc(&method, &params, reply);
                return;
            }
            command => command,
        };
        if !matches!(&command, Command::Load { .. } | Command::Startup { .. }) {
            self.startup_snapshot = None;
        }
        let (request_session, reply) = match &command {
            Command::Diagnostics { .. } | Command::AudioStatus { .. } | Command::Rpc { .. } => {
                unreachable!("handled above")
            }
            Command::Startup { reply } => (self.session, reply.clone()),
            Command::Load { session, reply, .. }
            | Command::Restart { session, reply, .. }
            | Command::Step { session, reply, .. }
            | Command::Button { session, reply, .. }
            | Command::Turn { session, reply, .. }
            | Command::Tap { session, reply, .. }
            | Command::Stop { session, reply } => (*session, reply.clone()),
        };
        if request_session != self.session
            && !matches!(command, Command::Load { .. } | Command::Stop { .. })
        {
            let _ = reply.send(Err("stale emulator session".into()));
            return;
        }
        let result = match command {
            Command::Diagnostics { .. } | Command::AudioStatus { .. } | Command::Rpc { .. } => {
                unreachable!("handled above")
            }
            Command::Startup { .. } => self
                .startup_snapshot
                .take()
                .map(Ok)
                .unwrap_or_else(|| self.ui_snapshot("No startup firmware selected")),
            Command::Load {
                session: next,
                bytes,
                card: image,
                ..
            } => self.ui_load(next, bytes, image),
            Command::Restart { next_session, .. } => self.ui_restart(next_session),
            Command::Step { budget, .. } => self.ui_step(budget),
            Command::Button { code, down, .. } => {
                self.ui_input(|runtime| runtime.button(code, down))
            }
            Command::Turn {
                encoder, detents, ..
            } => self.ui_input(|runtime| runtime.turn(encoder, detents)),
            Command::Tap { code, hold, .. } => self.ui_tap(code, hold),
            Command::Stop { .. } => {
                if request_session < self.session {
                    Err("stale emulator session".into())
                } else {
                    self.session = request_session;
                    let result = self.ui_snapshot("No firmware selected");
                    if result.is_ok() {
                        self.cancel_card();
                        self.runtime = None;
                        self.loaded = None;
                        self.job = None;
                    }
                    result
                }
            }
        };
        let _ = reply.send(result);
    }

    fn ui_snapshot(&mut self, missing: &str) -> Result<Snapshot, String> {
        let snapshot = self
            .runtime
            .as_mut()
            .ok_or_else(|| missing.to_string())
            .map(Runtime::snapshot)?;
        Ok(self.with_held(snapshot))
    }

    fn ui_load(
        &mut self,
        next: u64,
        bytes: Vec<u8>,
        image: Option<PathBuf>,
    ) -> Result<Snapshot, String> {
        self.startup_snapshot = None;
        if next <= self.session {
            return Err("stale emulator session".into());
        }
        // what the running firmware wrote goes to its own device's folder before the next firmware takes over
        self.mirror_flush();
        self.next_auto = None;
        self.cancel_card();
        self.audio = self.audio_for_base(&cards::sha256_hex(&bytes));
        self.card_last = json!({"state": "idle"});
        self.runtime = None;
        self.read_error = None;
        self.session = next;
        self.loaded = None;
        self.job = None;
        if bytes.len() > MAX_SYX {
            return Err("firmware image exceeds 32 MiB cache limit".into());
        }
        let mounted = if image.is_none() {
            self.remembered_card(&bytes)
        } else {
            None
        };
        let (runtime, error, image) = match mounted {
            Some((last, paths)) => {
                let mut audio = self.audio.clone();
                if let (Some(audio), true) = (audio.as_mut(), paths.ready_cached()) {
                    audio.snapshot = paths.snapshot();
                    audio.state = paths.dsp_state();
                }
                match runtime_from(&bytes, Some(paths.image.clone()), audio.as_ref()) {
                    Ok((runtime, error)) => {
                        self.audio = audio;
                        self.card_last = json!({
                            "state": "ready", "phase": "ready", "message": "Card mounted",
                            "fraction": 1.0, "key": paths.key, "name": last.name,
                            "samples": last.samples, "auto": true,
                            "card": paths.image.display().to_string(),
                        });
                        (runtime, error, Some(paths.image))
                    }
                    // A cache that no longer boots: start without a card.
                    Err(_) => {
                        let (runtime, error) = runtime_from(&bytes, None, self.audio.as_ref())?;
                        (runtime, error, None)
                    }
                }
            }
            None => {
                let (runtime, error) = runtime_from(&bytes, image.clone(), self.audio.as_ref())?;
                (runtime, error, image)
            }
        };
        self.adopt_card_folder(image.as_ref());
        self.loaded = Some(Loaded::stock(bytes, image));
        self.install(runtime, error);
        let snapshot = self.runtime.as_mut().expect("new runtime").snapshot();
        self.startup_snapshot = Some(snapshot.clone());
        self.emit("session.changed", json!({"reason": "load"}));
        Ok(snapshot)
    }

    /// The first load that mounts a card image takes the startup `--card-folder`: that card's +Drive folder.
    fn adopt_card_folder(&mut self, card: Option<&PathBuf>) {
        // A card `default_card` built from the device's folder: that folder is its +Drive folder.
        self.auto_card = None;
        if let Some((image, dir)) = self.next_auto.take()
            && card == Some(&image)
        {
            self.card_folder = Some(CardFolder {
                image: image.clone(),
                dir: dir.clone(),
                sync: self.folder_sync,
                out: None,
            });
            self.auto_card = Some((image, dir));
            return;
        }
        let (Some(card), Some(source)) = (card, self.startup_card_folder.take()) else { return };
        eprintln!(
            "card: {} is the +Drive folder of this card; {}",
            display_path(&source.dir),
            if source.sync {
                "the firmware's changes are exported to it on exit"
            } else {
                "exit does not export (card.export does)"
            }
        );
        self.card_folder = Some(CardFolder {
            image: card.clone(),
            dir: source.dir,
            sync: source.sync,
            out: source.out,
        });
    }

    /// The card `last.json` names, when it is this DT2 firmware's and its
    /// cache is whole (with audio the ready state must be cached too: a cold
    /// boot is never started here).
    fn remembered_card(&self, syx: &[u8]) -> Option<(cards::LastCard, CardPaths)> {
        if !self.remember_card || plusdrive_format::Dt2Seed::from_syx(syx).is_err() {
            return None;
        }
        let last = cards::read_last(&self.run_dir)?;
        if last.firmware_sha256 != cards::sha256_hex(syx) {
            return None;
        }
        let paths = CardPaths::new(&self.run_dir, &last.key);
        let whole = paths.image.is_file()
            && (self.audio.as_ref().is_none_or(|a| a.zero) || paths.ready_cached());
        whole.then_some((last, paths))
    }

    /// The card of an image loaded without a `card` parameter, first match:
    /// 1. the running card of the same device when the user chose it (samples picked in the app): it stays;
    /// 2. a card built from the device's folder (`plus_drive/<device>/` or the user's choice) when the folder holds something the
    ///    device's card takes: built again from the folder as it is now (cached by content), so a new folder applies at a load;
    /// 3. a DT2 image: the running DT2's card, else the card remembered for this stock firmware. An image of another device never
    ///    gets the DT2's card (opening a DN2 file in a DT2 session must not mount it).
    /// `stock` is the unpatched image when `bytes` is a patched one: the card belongs to the stock firmware.
    fn default_card(&mut self, bytes: &[u8], stock: Option<&[u8]>) -> Option<PathBuf> {
        self.next_auto = None;
        let device = runtime_patch::device_short(bytes)?;
        let running = self
            .loaded
            .as_ref()
            .filter(|l| runtime_patch::device_short(&l.current) == Some(device))
            .and_then(|l| l.card.clone());
        let own = self.auto_card.as_ref().map(|(image, _)| image);
        if let Some(card) = running.as_ref().filter(|card| Some(*card) != own) {
            return Some(card.clone());
        }
        if let Some(card) = self.card_from_device_folder(device, stock.unwrap_or(bytes), stock.is_some()) {
            return Some(card);
        }
        if device != "dt2" {
            return None;
        }
        running.or_else(|| self.remembered_card(stock.unwrap_or(bytes)).map(|(_, paths)| paths.image))
    }

    /// Build (or reuse) the card of `device`'s folder. A folder with nothing for the card, or a build that fails, gives no card (the
    /// load goes on without it); the failure is printed.
    fn card_from_device_folder(&mut self, device: &str, syx: &[u8], stock: bool) -> Option<PathBuf> {
        if !self.device_cards {
            return None;
        }
        let dir = self.folders.folder(device).filter(|d| d.is_dir())?;
        let files = cards::device_files(device, &dir).ok().filter(|f| !f.is_empty())?;
        // The card belongs to the stock firmware: a patched image that was not patched here names its base in its patch record.
        let found;
        let syx = if stock {
            syx
        } else {
            let mut image = Loaded::stock(syx.to_vec(), None);
            let _ = image.find_base();
            found = image.base.unwrap_or_default();
            &found
        };
        match cards::ensure_card(device, &self.run_dir, syx, &dir, &files, |_, _| {}) {
            Ok((paths, cached, _)) => {
                eprintln!(
                    "card: {} {} from {} ({} files)",
                    if cached { "reusing" } else { "built" },
                    display_path(&paths.image),
                    display_path(&dir),
                    files.len()
                );
                self.next_auto = Some((paths.image.clone(), dir));
                Some(paths.image)
            }
            Err(error) => {
                eprintln!("card: no card from {}: {error}", display_path(&dir));
                None
            }
        }
    }

    fn ui_restart(&mut self, next_session: u64) -> Result<Snapshot, String> {
        let Some(loaded) = self.loaded.as_ref() else {
            return Err("No firmware selected".into());
        };
        if next_session <= self.session {
            return Err("stale emulator session".into());
        }
        // The webview restarts from the base firmware (as it always has);
        // an injected DSP change is re-applied by `firmware.load`, not here.
        let (bytes, image) = (
            loaded
                .base
                .clone()
                .unwrap_or_else(|| loaded.current.clone()),
            loaded.card.clone(),
        );
        let (runtime, error) = runtime_from(&bytes, image, self.audio.as_ref())?;
        self.session = next_session;
        self.loaded = self.loaded.take().map(|mut l| {
            l.current = bytes;
            l.current_analysis = None;
            l
        });
        self.install(runtime, error);
        self.emit("session.changed", json!({"reason": "restart"}));
        Ok(self.runtime.as_mut().expect("restarted runtime").snapshot())
    }

    fn ui_input(
        &mut self,
        action: impl FnOnce(&mut Runtime) -> Result<(), String>,
    ) -> Result<Snapshot, String> {
        let runtime = self
            .runtime
            .as_mut()
            .ok_or_else(|| "No firmware selected".to_string())?;
        action(runtime)?;
        let snapshot = runtime.snapshot();
        Ok(self.with_held(snapshot))
    }

    fn ui_tap(&mut self, code: u8, hold: u32) -> Result<Snapshot, String> {
        let runtime = self
            .runtime
            .as_mut()
            .ok_or_else(|| "No firmware selected".to_string())?;
        runtime.button(code, true)?;
        let held = (hold.clamp(1, 100_000_000)) as u64;
        let initial = runtime.snapshot();
        let start = initial.status.icount;
        let mut status = initial.status;
        let mut frame = initial.frame;
        let mut leds = initial.leds;
        let mut rgb = initial.rgb;
        let result = loop {
            let remaining = held.saturating_sub(status.icount.saturating_sub(start));
            if remaining == 0 {
                break Ok(Snapshot {
                    status,
                    frame,
                    leds,
                    rgb,
                    polled: false,
                });
            }
            let snapshot = runtime.step_chunk(remaining.min(u64::from(STEP_CHUNK)) as u32);
            if let Some(error) = snapshot.status.error.clone() {
                break Err(error);
            }
            status = snapshot.status;
            if snapshot.frame.is_some() {
                frame = snapshot.frame;
            }
            if snapshot.leds.is_some() {
                leds = snapshot.leds;
                rgb = snapshot.rgb;
            }
        };
        let released = runtime.button(code, false);
        match (result, released) {
            (Ok(snapshot), Ok(())) => Ok(self.with_held(snapshot)),
            (Err(error), _) | (_, Err(error)) => Err(error),
        }
    }

    fn ui_step(&mut self, budget: u32) -> Result<Snapshot, String> {
        if self.runtime.is_none() {
            return Err("No firmware selected".into());
        }
        // Something else advances the guest: report, do not step.
        if self.mode == Mode::Free && self.job.is_none() {
            // Do not wait for the DSP frames in flight while free running.
            let snapshot = self.runtime.as_mut().expect("runtime").peek();
            let mut snapshot = self.with_held(snapshot);
            snapshot.polled = true;
            return Ok(snapshot);
        }
        if self.mode != Mode::Ui || self.job.is_some() {
            return self
                .ui_snapshot("No firmware selected")
                .map(|mut snapshot| {
                    snapshot.polled = true;
                    snapshot
                });
        }
        let mut budget = budget;
        if self.scenario_run.is_some() {
            budget = self.scenario_tick(budget)?;
        }
        let runtime = self.runtime.as_mut().expect("runtime");
        let snapshot = runtime.step_chunk(budget.min(STEP_CHUNK));
        self.watch(&snapshot.status);
        if let Some(error) = self.card_error() {
            Err(format!("card image read: {error}"))
        } else {
            Ok(self.with_held(snapshot))
        }
    }

    /// The desktop `--scenario` mode: fire due events, finish the run with a
    /// `scenario.done` event, and clip the chunk so exact events land exactly.
    fn scenario_tick(&mut self, mut budget: u32) -> Result<u32, String> {
        use scenario::When;
        let (Some(runtime), Some(run)) = (self.runtime.as_mut(), self.scenario_run.as_mut()) else {
            return Ok(budget);
        };
        let started = *run.started.get_or_insert_with(Instant::now);
        let elapsed = runtime.elapsed_instructions();
        while let Some(event) = run.scenario.events.get(run.next).copied() {
            let due = match event.when {
                When::Instr(n) => elapsed >= n,
                When::Frames(n) => frames(runtime) >= n,
            };
            if !due {
                break;
            }
            scenario::fire(runtime, event.action)?;
            run.next += 1;
        }
        let done = run.scenario.instructions.is_some_and(|n| elapsed >= n)
            || run
                .scenario
                .seconds
                .is_some_and(|s| frames(runtime) as f64 >= s * scenario::FRAMES_PER_SECOND);
        if done {
            let report = json!({
                "scenario": run.scenario.name,
                "wall_seconds": started.elapsed().as_secs_f64(),
                "elapsed_instructions": elapsed,
                "audio": runtime.audio_status().unwrap_or(Value::Null),
                "diagnostics": runtime.diagnostics(),
            });
            self.scenario_run = None;
            self.emit("scenario.done", json!({"report": report}));
            return Ok(0);
        }
        if run.scenario.exact_events
            && let Some(event) = run.scenario.events.get(run.next)
            && let When::Instr(n) = event.when
            && n > elapsed
        {
            budget = budget.min(u32::try_from(n - elapsed).unwrap_or(u32::MAX));
        }
        Ok(budget)
    }

    // ----- jobs and free running ------------------------------------------

    fn fire_due(&mut self) -> Result<(), RpcError> {
        if self.auto_no && self.runtime.as_mut().is_some_and(|r| r.status().ready) {
            self.auto_no = false;
            let elapsed = self
                .runtime
                .as_ref()
                .map_or(0, Runtime::elapsed_instructions);
            self.place_input(
                None,
                Action::Button {
                    code: 12,
                    down: true,
                },
            )?;
            self.place_input(
                Some(elapsed + NO_HOLD),
                Action::Button {
                    code: 12,
                    down: false,
                },
            )?;
        }
        let Some(runtime) = self.runtime.as_mut() else {
            return Ok(());
        };
        let elapsed = runtime.elapsed_instructions();
        while self.schedule.first().is_some_and(|s| s.at <= elapsed) {
            let due = self.schedule.remove(0);
            scenario::fire(runtime, due.action).map_err(RpcError::bad_params)?;
        }
        Ok(())
    }

    /// One chunk of the active job, or of free running.
    fn advance(&mut self) {
        if let Err(error) = self.fire_due() {
            self.finish_job(Err(error));
            return;
        }
        if let Some(done) = self.job_progress() {
            self.finish_job(done);
            return;
        }
        let Some(runtime) = self.runtime.as_mut() else {
            return;
        };
        let (icount, elapsed) = (runtime.icount(), runtime.elapsed_instructions());
        let mut budget = u64::from(STEP_CHUNK);
        if let Some(job) = &self.job {
            budget = budget.min(match &job.kind {
                JobKind::Steps { end } => end.saturating_sub(icount),
                JobKind::Until(Until::Elapsed(n)) => n.saturating_sub(elapsed),
                JobKind::Until(Until::Icount(n)) => n.saturating_sub(icount),
                JobKind::Until(Until::Frames(_)) => budget,
                JobKind::Boot { limit, .. } => limit.saturating_sub(icount),
            });
        }
        if let Some(next) = self.schedule.first() {
            budget = budget.min(next.at.saturating_sub(elapsed));
        }
        let snapshot = runtime.step_chunk(budget.max(1) as u32);
        self.watch(&snapshot.status);
        if snapshot.frame.is_some() {
            self.held_frame = snapshot.frame.clone();
        }
        if let Some(leds) = &snapshot.leds {
            self.held_leds = Some((leds.clone(), snapshot.rgb.clone()));
        }
        if let Some(error) = snapshot.status.error.clone() {
            self.finish_job(Err(RpcError::new(code::GUEST_FAULT, error)));
            self.mode = Mode::Paused;
            self.reset_pending = false;
            return;
        }
        if let Some(error) = self.card_error() {
            self.finish_job(Err(RpcError::io(format!("card image read: {error}"))));
            self.mode = Mode::Paused;
            self.reset_pending = false;
            return;
        }
        self.handle_reset();
    }

    /// Whether the active job is finished before another chunk.
    fn job_progress(&mut self) -> Option<RpcResult> {
        let job = self.job.as_ref()?;
        let runtime = self.runtime.as_mut()?;
        let (icount, elapsed) = (runtime.icount(), runtime.elapsed_instructions());
        if let Some(limit) = job.wall_limit
            && job.started.elapsed().as_secs_f64() > limit
        {
            return Some(Err(RpcError::new(code::TIMEOUT, "wall-clock timeout")));
        }
        match &job.kind {
            JobKind::Steps { end } => (icount >= *end).then(|| Ok(json!({}))),
            JobKind::Until(Until::Elapsed(n)) => (elapsed >= *n).then(|| Ok(json!({}))),
            JobKind::Until(Until::Icount(n)) => (icount >= *n).then(|| Ok(json!({}))),
            JobKind::Until(Until::Frames(n)) => (frames(runtime) >= *n).then(|| Ok(json!({}))),
            JobKind::Boot { cache, limit } => {
                if runtime.status().ready {
                    self.boot_resets = 0;
                    let saved = match cache {
                        Some(path) => save_cache(runtime, path),
                        None => Ok(None),
                    };
                    runtime.rebase_clock();
                    Some(saved.map(|cache| json!({"booted": true, "cache_saved": cache})))
                } else if icount >= *limit {
                    Some(Err(RpcError::new(
                        code::TIMEOUT,
                        format!("not ready after {limit} instructions"),
                    )))
                } else {
                    None
                }
            }
        }
    }

    fn finish_job(&mut self, result: RpcResult) {
        let Some(job) = self.job.take() else {
            return;
        };
        let seconds = job.started.elapsed().as_secs_f64();
        let label = job.label;
        let result = result.map(|mut value| {
            if let (Value::Object(into), Value::Object(from)) = (&mut value, &job.partial) {
                for (key, v) in from {
                    into.entry(key.clone()).or_insert_with(|| v.clone());
                }
            }
            if value.get("seconds").is_none() || label == "firmware.load" {
                value["seconds"] = seconds.into();
            }
            self.envelope(value)
        });
        let result = result.map_err(|mut error| {
            let status = self.envelope(json!({}));
            error.data = Some(error.data.take().unwrap_or(status));
            error
        });
        if label == "firmware.load" {
            let ok = result.is_ok();
            self.emit("firmware.loaded", json!({"ok": ok, "seconds": seconds}));
        }
        let _ = job.reply.send(result);
    }

    // ----- the control protocol --------------------------------------------

    fn rpc(&mut self, method: &str, params: &Value, reply: RpcReply) {
        let p = Params(params);
        // `session`: the webview's session token (`emu_call`). `firmware.load` and `restart` start a new session with it (it must
        // be newer than the current one, and is taken when the call succeeds); every other method must carry the current one.
        let token = match p.opt_u64("session") {
            Ok(token) => token,
            Err(error) => {
                let _ = reply.send(Err(error));
                return;
            }
        };
        let starts_session = matches!(method, "firmware.load" | "restart");
        if let Some(token) = token
            && if starts_session { token <= self.session } else { token != self.session }
        {
            let _ = reply.send(Err(RpcError::new(code::STALE_SESSION, "stale emulator session")));
            return;
        }
        let outcome: Result<Option<Value>, RpcError> = match method {
            "status" => self.rpc_status().map(Some),
            "run.mode" => self.rpc_run_mode(&p).map(Some),
            "step" => self.rpc_step(&p, &reply),
            "run_until" => self.rpc_run_until(&p, &reply),
            "firmware.load" => self.rpc_firmware_load(&p, &reply),
            "restart" => self.rpc_restart(&p, &reply),
            "input.button" | "input.turn" => self.rpc_input(method, &p).map(Some),
            "input.tap" => self.rpc_tap(&p).map(Some),
            "input.schedule" => self.rpc_schedule(&p).map(Some),
            "inputs.recorded" => self.rpc_recorded().map(Some),
            "framebuffer.get" => self.rpc_framebuffer(&p).map(Some),
            "leds.get" => self.rpc_leds().map(Some),
            "mem.cf.peek" => self.rpc_mem_peek(&p).map(Some),
            "mem.cf.poke" => self.rpc_mem_poke(&p).map(Some),
            "flash.status" => self.rpc_flash_status().map(Some),
            "flash.read" => self.rpc_flash_read(&p).map(Some),
            "mem.cf.write" => self.rpc_mem_write(&p).map(Some),
            "dev.reload" => self.rpc_dev_reload(&p, &reply),
            "guard.set" => self.rpc_guard_set(&p).map(Some),
            "guard.report" => self
                .with_runtime(|r| Ok(serde_json::to_value(r.guard_report()).unwrap_or(Value::Null)))
                .map(Some),
            "guard.clear" => self
                .with_runtime(|r| {
                    r.clear_guard_hits();
                    Ok(json!({"cleared": true}))
                })
                .map(Some),
            "card.overlay" => self.rpc_card_overlay(&p).map(Some),
            "midi.in" | "midi.usb.in" => self.rpc_midi_in(method, &p).map(Some),
            "midi.out" | "midi.usb.out" => self.rpc_midi_out(method, &p).map(Some),
            "card.export" => self.rpc_card_export(&p).map(Some),
            "watchdog.config" => self.rpc_watchdog(&p).map(Some),
            "cpu.exceptions" => self.rpc_cpu_exceptions(&p).map(Some),
            "diagnostics" => self.with_runtime(|r| Ok(r.diagnostics())).map(Some),
            "audio.report" => self
                .with_runtime(|r| Ok(r.audio_status().unwrap_or(Value::Null)))
                .map(|audio| Some(json!({ "audio": audio }))),
            "state.digest" => self.rpc_state_digest(&p).map(Some),
            "state.save" => self.rpc_state_save(&p).map(Some),
            "state.load" => self.rpc_state_load(&p).map(Some),
            "pcm.tap" => self.rpc_pcm_tap(&p).map(Some),
            "cf.profile" => self
                .with_runtime(|r| {
                    #[cfg(feature = "cf-profile")]
                    {
                        let mut v = r.cf_profile();
                        v["controller_cpu_ns"] = elektron_native_boot::thread_cpu_ns().into();
                        Ok(v)
                    }
                    #[cfg(not(feature = "cf-profile"))]
                    {
                        let _ = r;
                        Err(RpcError::new(
                            code::METHOD_NOT_FOUND,
                            "built without cf-profile",
                        ))
                    }
                })
                .map(Some),
            "dsp.peek" => self.rpc_dsp_peek(&p).map(Some),
            "dsp.peek_word" => self.rpc_dsp_peek_word(&p).map(Some),
            "dsp.patch" => self.rpc_dsp_patch(&p).map(Some),
            "dsp.ddr_monitor" => self.rpc_dsp_ddr_monitor(&p).map(Some),
            "card.load" => self.rpc_card_load(&p, &reply),
            "card.status" => Ok(Some(self.card_status())),
            "card.folder" => self.rpc_card_folder().map(Some),
            "card.folder_set" => self.rpc_card_folder_set(&p).map(Some),
            "card.sync" => self.rpc_card_sync().map(Some),
            "card.cancel" => Ok(Some(json!({"cancelled": self.cancel_card()}))),
            "quit" => {
                self.quit = true;
                self.finish_card();
                self.runtime = None;
                Ok(Some(json!({"quitting": true})))
            }
            other => Err(RpcError::new(
                code::METHOD_NOT_FOUND,
                format!("unknown method {other}"),
            )),
        };
        if let (Some(token), true, true) = (token, starts_session, outcome.is_ok()) {
            self.session = token;
        }
        match outcome {
            Ok(Some(value)) => {
                let _ = reply.send(Ok(self.envelope(value)));
            }
            Ok(None) => {} // a job now holds the reply
            Err(error) => {
                let _ = reply.send(Err(error));
            }
        }
        if self.quit
            && let Some(hook) = self.on_quit.take()
        {
            hook();
        }
    }

    fn with_runtime<T>(
        &mut self,
        action: impl FnOnce(&mut Runtime) -> Result<T, RpcError>,
    ) -> Result<T, RpcError> {
        match self.runtime.as_mut() {
            Some(runtime) => action(runtime),
            None => no_firmware(),
        }
    }

    /// `state.save {path}`: the ColdFire state (registers, RAM, peripherals, the card's write overlay)
    /// as a snapshot file, written whole (`.part`, then renamed). The scenario clock restarts here
    /// (`rebase_clock`), as after a cold boot that reached ready, so a run that goes on after the save
    /// and one that restores it count `elapsed` the same way.
    fn rpc_state_save(&mut self, p: &Params) -> RpcResult {
        let path = PathBuf::from(p.str("path")?);
        if self.job.is_some() {
            return Err(RpcError::new(code::BUSY, "a job is running; wait for it before saving"));
        }
        let runtime = self.runtime.as_mut().ok_or_else(RpcError::no_firmware)?;
        save_cache(runtime, &path)?;
        runtime.rebase_clock();
        let bytes = std::fs::metadata(&path).map_or(0, |m| m.len());
        Ok(json!({"path": path.display().to_string(), "bytes": bytes}))
    }

    /// `state.load {path}`: restore a `state.save` snapshot into the runtime a `firmware.load` just
    /// installed (not stepped yet; the same MAIN and the same card, which the snapshot checks by
    /// digest and capacity). A fresh state after a reset is `firmware.load` then `state.load`, about
    /// 0.3 s instead of a cold boot.
    fn rpc_state_load(&mut self, p: &Params) -> RpcResult {
        let path = PathBuf::from(p.str("path")?);
        if self.job.is_some() {
            return Err(RpcError::new(code::BUSY, "a job is running; wait for it before loading"));
        }
        let runtime = self.runtime.as_mut().ok_or_else(RpcError::no_firmware)?;
        if runtime.icount() != 0 {
            return Err(RpcError::new(
                code::BAD_PARAMS,
                "state.load needs a runtime that has not run: firmware.load first",
            ));
        }
        let data = std::fs::read(&path).map_err(|e| RpcError::io(format!("{}: {e}", path.display())))?;
        runtime
            .restore_state(&data)
            .map_err(|e| RpcError::new(code::FIRMWARE_REJECTED, format!("{}: {e}", path.display())))?;
        runtime.resume_watch();
        self.held_frame = None;
        self.held_leds = None;
        self.epoch += 1;
        Ok(json!({"path": path.display().to_string(), "bytes": data.len(), "icount": runtime.icount()}))
    }

    fn start_job(
        &mut self,
        kind: JobKind,
        reply: &RpcReply,
        wall_limit: Option<f64>,
        partial: Value,
        label: &'static str,
    ) -> Result<(), RpcError> {
        if self.job.is_some() {
            return Err(RpcError::new(code::BUSY, "another job is running"));
        }
        self.job = Some(Job {
            kind,
            reply: reply.clone(),
            started: Instant::now(),
            wall_limit,
            partial,
            label,
        });
        Ok(())
    }

    fn rpc_status(&mut self) -> RpcResult {
        let loaded = self.loaded.as_ref().map(|l| {
            json!({
                "base_sha256": l.base_sha,
                "syx_sha256": firmware::sha256_hex(&l.current),
                "injected": l.base.as_ref() != Some(&l.current),
                "runtime": self.runtime_info,
            })
        });
        let job = self
            .job
            .as_ref()
            .map(|j| json!({"kind": j.label, "wall_seconds": j.started.elapsed().as_secs_f64()}));
        let scheduled = self.schedule.len();
        let Some(runtime) = self.runtime.as_mut() else {
            return Ok(json!({"firmware": null, "job": job}));
        };
        let status = runtime.status();
        let cf = serde_json::to_value(&status).unwrap_or(Value::Null);
        let audio = summarize_audio(runtime);
        let load = poll_load(&mut self.load_meter, &status, &audio);
        let dsp = if runtime.has_audio() {
            "sharc"
        } else if runtime.is_zero() {
            "zero"
        } else {
            "none"
        };
        let frame = runtime.current_frame().map(|(revision, _)| revision);
        Ok(json!({
            "controller_cpu_ns": elektron_native_boot::thread_cpu_ns(),
            "firmware": loaded, "cf": cf, "audio": audio, "dsp": dsp, "job": job,
            "scheduled_inputs": scheduled, "frame_revision": frame,
            "heartbeat": {"mainloop_hits": cf["mainloop_hits"], "stalled": self.health.stalled(),
                          "stall_count": self.health.stall_count, "stall_ms": self.health.stall_ms,
                          "underrun_count": self.health.underrun_count,
                          "mainloop_pc": cf["mainloop_pc"]},
            "dsp_frames": frames(runtime),
            "load": load,
        }))
    }

    /// `watchdog.config {stall_ms}`: set the stall threshold in emulated ms (0 disables) and
    /// report the heartbeat.
    fn rpc_watchdog(&mut self, p: &Params) -> RpcResult {
        if let Ok(ms) = p.u64("stall_ms") {
            self.health.stall_ms = ms;
        }
        let hits = self.runtime.as_mut().map(|r| r.status().mainloop_hits);
        Ok(
            json!({"stall_ms": self.health.stall_ms, "stalled": self.health.stalled(),
                  "stall_count": self.health.stall_count,
                  "mainloop_hits": hits}),
        )
    }

    /// `cpu.exceptions {deliver}`: a CPU exception runs the guest's handler (`true`) or stops the run with an error (`false`, the default). Kept
    /// for every runtime installed later (a soft reset boots a new one). The digikit runtime's crash capture needs it
    /// (docs/findings/46-digikit-debug.md).
    fn rpc_cpu_exceptions(&mut self, p: &Params) -> RpcResult {
        if let Some(deliver) = p.opt_bool("deliver")? {
            self.deliver_exceptions = deliver;
            if let Some(r) = self.runtime.as_mut() {
                r.set_deliver_exceptions(deliver);
            }
        }
        Ok(json!({"deliver": self.deliver_exceptions}))
    }

    /// `state.digest {arch?}`: the digest of the saved state. `arch: true` leaves out the counters of how the instructions were run
    /// (interpreted, idle skipped, RAM clear skipped), so runs that reach the same guest state by different paths have the same digest.
    fn rpc_state_digest(&mut self, p: &Params) -> RpcResult {
        let arch = p.opt_bool("arch")?.unwrap_or(false);
        self.with_runtime(|r| {
            if arch { r.state_digest_arch() } else { r.state_digest() }
                .map(|digest| json!({"digest": digest}))
                .map_err(|e| RpcError::new(code::GUEST_FAULT, e))
        })
    }

    /// `mem.cf.peek {addr}`: one guest longword, read without side effects.
    fn rpc_mem_peek(&mut self, p: &Params) -> RpcResult {
        let addr = u32::try_from(p.u64("addr")?)
            .map_err(|_| RpcError::bad_params("addr is a 32-bit byte address"))?;
        let value = self.with_runtime(|r| Ok(r.peek32(addr)))?;
        Ok(json!({"addr": addr, "value": value}))
    }

    /// `mem.cf.poke {addr, value}`: write one longword of SDRAM (a test aid: break the guest on
    /// purpose to exercise `fault`/`stall`). Not part of the replay record.
    /// `card.export {dir?, out?, prune?, working_project?}`: bring a +Drive folder up to date with what the firmware wrote to the
    /// card (finding 24). `dir` defaults to the folder the card was loaded from.
    /// The whole tree goes to `<out>/export/<stamp>/` first (`out` defaults to `$DIGI_PLUSDRIVE_OUT` or
    /// `out/plusdrive`), then only new or changed files are copied, each replaced file moved to
    /// `<out>/backup/<stamp>/`. Files the firmware deleted are listed; `prune` moves them to the backup. The working
    /// project (`.mmcfs/boot/050000.bin`, rewritten by the firmware at every start) is left out unless `working_project`.
    fn rpc_card_export(&mut self, p: &Params) -> RpcResult {
        let explicit = p.opt_str("dir")?.map(PathBuf::from);
        let out = p.opt_str("out")?.map(PathBuf::from);
        let prune = p.opt_bool("prune")?.unwrap_or(false);
        let working = p.opt_bool("working_project")?.unwrap_or(false);
        let report = self
            .export_card(explicit, out, prune, working)
            .map_err(RpcError::io)?;
        Ok(json!({
            "added": report.added, "replaced": report.replaced, "unchanged": report.unchanged.len(),
            "deleted_by_firmware": report.deleted_by_firmware, "pruned": report.pruned, "kept": report.kept,
            "issues": report.issues,
            "export_dir": report.export_dir.display().to_string(),
            "backup_dir": report.backup_dir.display().to_string(),
        }))
    }

    fn export_card(
        &mut self,
        explicit: Option<PathBuf>,
        out: Option<PathBuf>,
        prune: bool,
        working_project: bool,
    ) -> Result<plusdrive_format::export::Report, String> {
        let image = self
            .loaded
            .as_ref()
            .and_then(|l| l.card.clone())
            .ok_or("no card is loaded")?;
        let folder = match (&explicit, &self.card_folder) {
            (Some(dir), _) => dir.clone(),
            (None, Some(f)) if f.image == image => f.dir.clone(),
            _ => return Err("no +Drive folder is known for this card; pass dir".into()),
        };
        let overlay = self
            .runtime
            .as_ref()
            .map(|r| r.card_overlay())
            .ok_or("no runtime")?;
        let base = plusdrive_format::export::FileImage::open(&image)?;
        let mut options = plusdrive_format::export::Options::new(out.unwrap_or_else(|| {
            std::env::var_os("DIGI_PLUSDRIVE_OUT").map_or_else(|| PathBuf::from("out/plusdrive"), PathBuf::from)
        }));
        options.prune = prune;
        options.include_working_project = working_project;
        let layout = self.card_layout()?;
        plusdrive_format::export::export(&base, &overlay, &folder, &layout, &options)
    }

    /// The card layout of the running device (its profile's `[card]` section), found by the base firmware's hash.
    fn card_layout(&self) -> Result<plusdrive_format::CardLayout, String> {
        let loaded = self.loaded.as_ref().ok_or("no firmware is loaded")?;
        let registry = plusdrive_format::device_profile::Registry::load().map_err(|e| e.to_string())?;
        let (device, _) = registry.identify(&loaded.base_sha).map_err(|e| e.to_string())?;
        device
            .card
            .clone()
            .ok_or_else(|| format!("device {} has no [card] layout", device.short))
    }

    /// At quit: save the firmware's card writes next to the image (`overlay.bin`, merged over an older one, so a
    /// session that did not start from it loses nothing), then, for a synced card, export them to its folder.
    /// Failures are printed: the quit goes on.
    fn finish_card(&mut self) {
        self.mirror_flush();
        let Some(folder) = self.card_folder.as_ref().filter(|f| f.sync) else { return };
        let (image, dir, out) = (folder.image.clone(), folder.dir.clone(), folder.out.clone());
        if self.loaded.as_ref().and_then(|l| l.card.as_ref()) != Some(&image) {
            return;
        }
        let Some(overlay) = self.runtime.as_ref().map(|r| r.card_overlay()) else { return };
        let path = overlay_path(&image);
        if !overlay.is_empty() {
            let merged = merge_overlay(std::fs::read(&path).ok(), overlay);
            let partial = path.with_extension("bin.part");
            let saved = std::fs::write(&partial, plusdrive_format::encode_overlay(&merged))
                .and_then(|()| std::fs::rename(&partial, &path));
            if let Err(error) = saved {
                eprintln!("card: could not save {}: {error}", path.display());
            }
        }
        let dir_shown = dir.clone();
        match self.export_card(Some(dir), out, false, false) {
            Ok(report) => eprintln!(
                "card: exported {} changed files to {} ({} added, {} replaced; {} deleted by the firmware, kept; {} issues); backup {}",
                report.added.len() + report.replaced.len(),
                display_path(&dir_shown),
                report.added.len(),
                report.replaced.len(),
                report.deleted_by_firmware.len(),
                report.issues.len(),
                report.backup_dir.display()
            ),
            Err(error) => eprintln!("card: export failed: {error}"),
        }
    }

    // ----- the module store as a folder (cardmirror.rs, plusdrive mirror.rs) -------------------------------------------

    /// What the mirror reads: the card as built (its image file, or nothing), the guest's writes over it, and the store's place on the
    /// card (the device profile).
    fn mirror_view(&mut self) -> Result<(Box<dyn RandomAccessRead>, Vec<emmc_card::CardOverlaySector>, plusdrive_format::device_profile::StoreLayout), String> {
        let range = self
            .card_layout()?
            .store
            .ok_or("this device has no module store on its card")?;
        let base: Box<dyn RandomAccessRead> = match self.loaded.as_ref().and_then(|l| l.card.clone()) {
            Some(image) => Box::new(plusdrive_format::export::FileImage::open(&image)?),
            None => Box::new(plusdrive_format::mirror::EmptyImage),
        };
        let overlay = self.runtime.as_ref().map(|r| r.card_overlay()).ok_or("no runtime")?;
        Ok((base, overlay, range))
    }

    /// The short id of the loaded device (`dt2`, `dn2`).
    fn loaded_device(&self) -> Option<&'static str> {
        let loaded = self.loaded.as_ref()?;
        runtime_patch::device_short(&loaded.current).or_else(|| loaded.base.as_deref().and_then(runtime_patch::device_short))
    }

    /// The mirror writes to the folder of the loaded device: the user's choice, else `plus_drive/<device>/` (made if missing). A
    /// change of folder starts a new mirror; the caller has flushed the old one to its own folder first.
    fn mirror_select(&mut self) {
        if !self.mirror_on {
            return;
        }
        let Some(device) = self.loaded_device() else { return };
        if let Some(dir) = self.mirror_first.take() {
            self.folders.set_session(device, dir);
        }
        let Some(dir) = self.folders.folder(device) else {
            self.mirror = None;
            return;
        };
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!("card: {}: {e}", dir.display());
        }
        if self.mirror.as_ref().map(|m| &m.dir) != Some(&dir) {
            self.mirror = Some(Mirror::new(dir));
        }
    }

    /// A new runtime: the mirror starts from the store the card holds now (an image's, and what a soft reset carried over), and from the
    /// runtime's own write count.
    fn mirror_reset(&mut self) {
        if self.mirror.is_none() {
            return;
        }
        let seen = match self.mirror_view() {
            Ok((base, overlay, range)) => {
                let view = plusdrive_format::OverlayImage::new(base.as_ref(), &overlay);
                plusdrive_format::mirror::store_files(&view, &range).map(|f| f.into_keys().collect()).unwrap_or_default()
            }
            Err(_) => Default::default(),
        };
        let writes = self.runtime.as_ref().map_or(0, |r| r.card_write_count());
        if let Some(m) = self.mirror.as_mut() {
            m.reset(seen, writes);
        }
    }

    /// Once per pass of the run loop: the card's write count, and the pass itself when the quiet period has run out.
    fn mirror_tick(&mut self) {
        let Some(writes) = self.runtime.as_ref().map(|r| r.card_write_count()) else { return };
        let now = Instant::now();
        let due = match self.mirror.as_mut() {
            Some(m) => {
                m.note_writes(writes, now);
                m.due(now)
            }
            None => false,
        };
        if due {
            let _ = self.mirror_pass();
        }
    }

    /// Mirror the store now. The outcome is kept for `card.folder`.
    fn mirror_pass(&mut self) -> Result<(), String> {
        let now = Instant::now();
        let Some(dir) = self.mirror.as_ref().map(|m| m.dir.clone()) else {
            return Err("no folder is set for the module store".into());
        };
        let outcome = (|| {
            let (base, overlay, range) = self.mirror_view()?;
            let view = plusdrive_format::OverlayImage::new(base.as_ref(), &overlay);
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let out = std::env::var_os("DIGI_PLUSDRIVE_OUT").map_or_else(|| PathBuf::from("out/plusdrive"), PathBuf::from);
            let backup = out.join("backup").join(format!("mirror-{stamp}"));
            let mut seen = self.mirror.as_mut().map(|m| std::mem::take(&mut m.seen)).unwrap_or_default();
            let report = plusdrive_format::mirror::mirror_store(&view, &range, &dir, &backup, &mut seen);
            if let Some(m) = self.mirror.as_mut() {
                m.seen = seen;
            }
            report
        })();
        let result = outcome.as_ref().map(|_| ()).map_err(Clone::clone);
        if let Some(m) = self.mirror.as_mut() {
            m.done(outcome, now);
        }
        result
    }

    /// At the end of the session: mirror what the quiet period had not reached yet.
    fn mirror_flush(&mut self) {
        if self.mirror.as_ref().is_some_and(|m| m.pending() || m.behind()) && self.runtime.is_some() {
            if let Err(e) = self.mirror_pass() {
                eprintln!("card: the module store was not mirrored: {e}");
            }
        }
    }

    /// `card.folder`: the module store's folder: its path, the files under `digikit/` with size and sha256, the time of the last pass
    /// (ms since the epoch) and what it did, and whether the card has writes the folder does not hold yet. And the folder's place in
    /// the device's choice: `device`, `default_folder`, `custom` (the user chose the folder), `chosen_missing` (a saved choice whose
    /// folder is gone: the default is used), `next_folder` (what the next load uses) and `reload_needed` (it is not `folder`: a device
    /// with a path filesystem takes the new folder when its card is built again, at the next load).
    fn rpc_card_folder(&mut self) -> RpcResult {
        let mut status = match &self.mirror {
            Some(m) => m.status(plusdrive_format::mirror::folder_files(&m.dir)),
            None => json!({"folder": null, "continuous": false, "files": [], "last_sync": null, "error": null}),
        };
        if let Some(device) = self.loaded_device() {
            let next = self.folders.folder(device);
            status["device"] = device.into();
            status["default_folder"] = self.folders.default_folder(device).map(|p| p.display().to_string()).into();
            status["custom"] = self.folders.choice(device).is_some().into();
            status["chosen_missing"] = self.folders.missing_choice(device).map(|p| p.display().to_string()).into();
            status["next_folder"] = next.as_ref().map(|p| p.display().to_string()).into();
            status["reload_needed"] = (self.mirror.as_ref().is_some_and(|m| Some(&m.dir) != next.as_ref())).into();
        }
        Ok(status)
    }

    /// `card.folder_set {dir, device?}`: the user's folder for a device (the loaded one by default); no `dir`: back to
    /// `plus_drive/<device>/`. Saved in the host's choices file. A device without a path filesystem (the DN2) mirrors to the new folder
    /// at once; a device whose card is built from the folder (the DT2) takes it at the next load (`reload_needed`), and a mirror pass
    /// into a folder that holds the user's files before they were read into the card would replace them. The answer is `card.folder`'s.
    fn rpc_card_folder_set(&mut self, p: &Params) -> RpcResult {
        let device = match p.opt_str("device")? {
            Some(d) => d.to_string(),
            None => self.loaded_device().map(str::to_string).ok_or_else(RpcError::no_firmware)?,
        };
        if !device_ok(&device) {
            return Err(RpcError::bad_params(format!("not a device name: {device:?}")));
        }
        let dir = p.opt_str("dir")?.filter(|d| !d.is_empty()).map(PathBuf::from);
        if dir.as_ref().is_some_and(|d| !d.is_absolute()) {
            return Err(RpcError::bad_params("dir must be an absolute path"));
        }
        self.folders.set_choice(&device, dir.as_deref()).map_err(RpcError::bad_params)?;
        if self.mirror_on && self.loaded_device() == Some(device.as_str()) {
            let path_fs = self.card_layout().is_ok_and(|l| l.path_fs.is_some());
            if !path_fs {
                self.mirror_flush();
                self.mirror_select();
                self.mirror_reset();
                // the outcome (or the error) is in the answer
                let _ = self.mirror_pass();
            }
        }
        self.rpc_card_folder()
    }

    /// `card.sync`: mirror the store now (no wait for the quiet period); the answer is `card.folder`'s.
    fn rpc_card_sync(&mut self) -> RpcResult {
        self.mirror_pass().map_err(RpcError::io)?;
        self.rpc_card_folder()
    }

    /// `card.overlay {path}`: write the guest's writes to the card as a sparse file: per touched sector
    /// `u64 le sector, [u8; 64] mask (bit i of byte i/8 names data byte i), [u8; 512] data`. Applied over the card's
    /// base image it gives the card the guest sees (`tools/emulator/card_overlay.py`).
    fn rpc_card_overlay(&mut self, p: &Params) -> RpcResult {
        let path = p.str("path")?.to_string();
        let sectors = self.with_runtime(|r| Ok(r.card_overlay()))?;
        let mut out = Vec::with_capacity(sectors.len() * (8 + 64 + 512));
        for s in &sectors {
            out.extend_from_slice(&s.sector.to_le_bytes());
            out.extend_from_slice(&s.written);
            out.extend_from_slice(&s.data);
        }
        std::fs::write(&path, &out).map_err(|e| RpcError::io(format!("{path}: {e}")))?;
        Ok(json!({"path": path, "sectors": sectors.len()}))
    }

    /// `midi.in {bytes | file, period?}` queues bytes on the MIDI DIN input (UART9), one per `period` instructions
    /// (default 42,240: the 31250 baud wire rate; 0 = as fast as the firmware takes its interrupts).
    /// `midi.usb.in {bytes | file, chunk?, period?}` hands the bytes to the firmware's USB MIDI receive callback (USB itself is not
    /// modelled; docs/CONTROL.md) in calls of `chunk` bytes (1 to 384, default 64) `period` instructions apart (default 132,000, a
    /// full-speed frame; 16,500 is a high-speed microframe). `bytes` is a hex string (spaces allowed) or an array of numbers.
    fn rpc_midi_in(&mut self, method: &str, p: &Params) -> RpcResult {
        let data = match (p.get("bytes"), p.opt_str("file")?) {
            (Some(Value::String(hex)), None) => {
                let digits: Vec<u8> = hex.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
                if digits.len() % 2 != 0 || !digits.iter().all(u8::is_ascii_hexdigit) {
                    return Err(RpcError::bad_params("bytes is an even number of hex digits"));
                }
                digits
                    .chunks(2)
                    .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap_or("0"), 16).unwrap_or(0))
                    .collect::<Vec<u8>>()
            }
            (Some(Value::Array(items)), None) => items
                .iter()
                .map(|v| v.as_u64().and_then(|n| u8::try_from(n).ok()))
                .collect::<Option<Vec<u8>>>()
                .ok_or_else(|| RpcError::bad_params("bytes holds numbers 0..255"))?,
            (None, Some(path)) => {
                std::fs::read(path).map_err(|e| RpcError::io(format!("{path}: {e}")))?
            }
            _ => return Err(RpcError::bad_params("give bytes (hex string or array) or file")),
        };
        let period = p
            .opt_u64("period")?
            .unwrap_or(elektron_native_boot::midi::WIRE_PERIOD);
        let usb = method == "midi.usb.in";
        let usb_chunk = p.opt_u64("chunk")?.map(|n| n as usize);
        let usb_period = p.opt_u64("period")?;
        let queued = self.with_runtime(|r| {
            if usb {
                r.usb_midi_in_paced(&data, usb_chunk, usb_period)
            } else {
                r.midi_in(&data, period)
            }
            .map_err(RpcError::bad_params)
        })?;
        Ok(json!({"bytes": data.len(), "queued": queued}))
    }

    /// `midi.out {file?}` returns (and clears) the bytes the firmware sent on MIDI DIN since the last call as a
    /// hex string; `midi.usb.out` the bytes it handed to the USB MIDI transmit function. The first
    /// `midi.usb.out` or `midi.usb.in` starts the USB capture. `file` also writes them raw.
    fn rpc_midi_out(&mut self, method: &str, p: &Params) -> RpcResult {
        let usb = method == "midi.usb.out";
        let (bytes, status) = self.with_runtime(|r| {
            if usb {
                r.usb_midi_watch().map_err(RpcError::bad_params)?;
            }
            let bytes = if usb { r.take_usb_out() } else { r.take_midi_out() };
            Ok((bytes, r.midi_status()))
        })?;
        if let Some(path) = p.opt_str("file")? {
            std::fs::write(path, &bytes).map_err(|e| RpcError::io(format!("{path}: {e}")))?;
        }
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        Ok(json!({"bytes": bytes.len(), "hex": hex, "status": status}))
    }

    /// `guard.set {protect: [{lo, hi, label?}], allow_pc: [{lo, hi}]}`: arm write-range guards
    /// (`hi` exclusive; addresses are integers or `"0x..."` strings). An empty `protect` disarms.
    fn rpc_guard_set(&mut self, p: &Params) -> RpcResult {
        let (protect, allow_pc) =
            elektron_native_boot::parse_guard_spec(p.0).map_err(RpcError::bad_params)?;
        let (ranges, allowed) = (protect.len(), allow_pc.len());
        self.with_runtime(|r| {
            r.set_write_guards(protect, allow_pc);
            Ok(())
        })?;
        Ok(json!({"protect": ranges, "allow_pc": allowed}))
    }

    /// `flash.status`: what the guest wrote to the container area of the flash since the boot (the OS upgrade): `{erases, programs, bytes}`.
    fn rpc_flash_status(&mut self) -> RpcResult {
        let w = self.with_runtime(|r| Ok(r.flash_writes()))?;
        Ok(json!({"erases": w.erases, "programs": w.programs, "bytes": w.bytes}))
    }

    /// `flash.read {offset, length}`: bytes of the model's flash as a hex string (the container is at 0x80000; at most 4 MiB per call).
    fn rpc_flash_read(&mut self, p: &Params) -> RpcResult {
        let offset = usize::try_from(p.u64("offset")?).map_err(|_| RpcError::bad_params("offset"))?;
        let length = usize::try_from(p.u64("length")?).map_err(|_| RpcError::bad_params("length"))?;
        if length > 4 << 20 {
            return Err(RpcError::bad_params("length is at most 4 MiB"));
        }
        let bytes = self.with_runtime(|r| {
            r.flash_bytes(offset, length)
                .map(<[u8]>::to_vec)
                .ok_or_else(|| RpcError::bad_params("the range is outside the 16 MiB flash"))
        })?;
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        Ok(json!({"offset": offset, "length": length, "hex": hex}))
    }

    fn rpc_mem_poke(&mut self, p: &Params) -> RpcResult {
        let addr = u32::try_from(p.u64("addr")?)
            .map_err(|_| RpcError::bad_params("addr is a 32-bit byte address"))?;
        let value = u32::try_from(p.u64("value")?)
            .map_err(|_| RpcError::bad_params("value is a 32-bit word"))?;
        self.with_runtime(|r| r.poke32(addr, value).map_err(RpcError::bad_params))?;
        Ok(json!({"addr": addr, "value": value}))
    }

    /// `mem.cf.write {addr, hex}`: write a byte range of SDRAM (any alignment) in one call, with one decode-cache invalidation for
    /// the range: code written here runs as new code. Not part of the replay record.
    fn rpc_mem_write(&mut self, p: &Params) -> RpcResult {
        let addr = u32::try_from(p.u64("addr")?)
            .map_err(|_| RpcError::bad_params("addr is a 32-bit byte address"))?;
        let bytes = decode_hex(p.str("hex")?).map_err(RpcError::bad_params)?;
        self.with_runtime(|r| r.write_ram(addr, &bytes).map_err(RpcError::bad_params))?;
        Ok(json!({"addr": addr, "len": bytes.len()}))
    }

    /// `dev.reload {modules_dir?, swap_modules?}`: bring the running session to the module objects as they are on disk now, by the
    /// cheapest correct way. The session's module set is the patch record of the running image; the image is made again from the
    /// stock image (the same patchkit apply as `firmware.load {runtime}`, nothing cached) and compared with the image whose ready
    /// state the boot cache holds (the baseline: the running image, or after a swap the one it was swapped from).
    ///
    /// - Only ranges of swap modules differ (`swap_modules`; default `core/ui`, `core/digikit-menu` and every module that requires
    ///   `core/ui`): restore the baseline's cached ready state, write the new bytes of every changed module over it (whole: code,
    ///   data and zeroed bss) and reply `{path: "swap", ranges, bytes, ms, ...}`. This equals a fresh boot of the new image as long
    ///   as nothing in the ready state holds a pointer into the swapped code: the firmware reaches UI code only through the SETTINGS
    ///   hook and the views opened from it, and the ready state is the main screen, SETTINGS never opened. A hook site that changes
    ///   is written too (it is a patch range of its module). The patch record is not compared or written.
    /// - Any other ColdFire range differs, a section other than MAIN or DSP differs, or there is no ready state to restore: load the
    ///   new image (`path: "boot"`, `reason`; a cold boot unless the new image was booted before). A DSP-only difference goes to the
    ///   hot injection of `firmware.load` (`path: "dsp"`).
    /// - Nothing differs: `{path: "none"}`.
    ///
    /// With `build: true` the stale objects of the session's modules are built first, in this process (`runtime_patch::build_modules`):
    /// a `core/*` module's into the module, a content module's into `out/cache/dko`. The reply gets `build: {built: [{id, objects,
    /// ms}], skipped: [id], ms}`. A compiler or assembler error is the error message (nothing is reloaded). SHARC+ objects are built too (Selache
    /// as a library, in this process).
    ///
    /// After a swap the boot cache has no entry for the new image: a plain `firmware.load` of it, or a `restart`, boots cold once.
    fn rpc_dev_reload(&mut self, p: &Params, reply: &RpcReply) -> Result<Option<Value>, RpcError> {
        let started = Instant::now();
        let rejected = |m: String| RpcError::new(code::FIRMWARE_REJECTED, format!("dev.reload: {m}"));
        if self.job.is_some() || self.patch_job.is_some() {
            return Err(RpcError::new(code::BUSY, "a job is running; wait for it before dev.reload"));
        }
        let loaded = self.loaded.as_ref().ok_or_else(RpcError::no_firmware)?;
        let current = loaded.current.clone();
        let record = patchkit::verify::patch_record(&current)
            .map_err(|e| rejected(e.to_string()))?
            .ok_or_else(|| rejected("the running image has no patch record (load a patched image or `runtime: true`)".into()))?;
        let root = runtime_patch::find_root(p.opt_str("modules_dir")?.map(Path::new).or(self.repo_root.as_deref()))
            .map_err(&rejected)?;
        let ids: Vec<String> = record.modules.iter().map(|t| patchkit::live::owner_id(t).to_string()).collect();
        let build = if p.opt_bool("build")?.unwrap_or(false) {
            let sum = runtime_patch::build_modules(&root, &ids).map_err(&rejected)?;
            Some(json!({
                "built": sum.built.iter().map(|b| json!({
                    "id": b.id,
                    "objects": b.objects.iter().map(|o| o.display().to_string()).collect::<Vec<_>>(),
                    "ms": b.ms,
                })).collect::<Vec<_>>(),
                "skipped": sum.skipped,
                "ms": started.elapsed().as_secs_f64() * 1e3,
            }))
        } else {
            None
        };
        let stock = loaded
            .base
            .clone()
            .filter(|b| firmware::sha256_hex(b) == record.base_sha256)
            .or_else(|| runtime_patch::find_stock(&root, &record.base_sha256))
            .ok_or_else(|| rejected(format!("the stock image {} is not in firmware/", record.base_sha256)))?;
        let card = {
            let own = self.auto_card.as_ref().map(|(image, _)| image);
            loaded.card.as_ref().filter(|c| Some(*c) != own).map(|c| c.display().to_string())
        };
        let stock_main_len = patchkit::firmware::Firmware::parse(&stock)
            .map_err(|e| rejected(e.to_string()))?
            .section(3)
            .map(|s| s.data.len())
            .ok_or_else(|| rejected("the stock image has no MAIN section".into()))?;
        let re = runtime_patch::reapply(&root, &stock, &ids, &["core/ui"]).map_err(&rejected)?;
        let (new_syx, plan) = (re.applied.syx, re.applied.plan);
        let baseline = match &self.reload {
            Some(r) if r.running_sha == firmware::sha256_hex(&current) => r.baseline.clone(),
            _ => current.clone(),
        };
        let diff_of = |from: &[u8]| patchkit::live::diff(from, &new_syx, stock_main_len, &plan).map_err(|e| rejected(e.to_string()));
        // Nothing to do when the running image already is the new one; otherwise the writes are the baseline's difference (a state
        // swapped back to its baseline's bytes is a swap with no ranges).
        let from_current = diff_of(&current)?;
        if from_current.is_empty() {
            let ms = started.elapsed().as_secs_f64() * 1e3;
            return Ok(Some(with_build(json!({"path": "none", "ms": ms, "apply_ms": ms}), &build)));
        }
        let diff = if baseline == current { from_current } else { diff_of(&baseline)? };
        let apply_ms = started.elapsed().as_secs_f64() * 1e3;
        let swap: Vec<String> = match p.get("swap_modules") {
            Some(Value::Array(a)) => a
                .iter()
                .map(|v| v.as_str().map(str::to_string).ok_or_else(|| RpcError::bad_params("swap_modules is a list of module ids")))
                .collect::<Result<_, _>>()?,
            Some(_) => return Err(RpcError::bad_params("swap_modules is a list of module ids")),
            None => SWAP_DEFAULT.iter().map(|s| s.to_string()).chain(re.requirers).collect(),
        };
        let differing: Vec<&patchkit::live::Change> = diff.changes.iter().filter(|c| c.differs).collect();
        let blocker = if !diff.other_sections.is_empty() {
            Some(format!("sections {:?} changed", diff.other_sections))
        } else if let Some(c) = differing.iter().find(|c| !swap.contains(&c.owner)) {
            let who = if c.owner.is_empty() { "unowned MAIN bytes" } else { c.owner.as_str() };
            Some(format!("{who} changed at {:#x}", c.addr))
        } else if diff.dsp_changed {
            Some("the DSP section changed".to_string())
        } else {
            None
        };
        // Everything but a swap loads the new image: forced when the ColdFire side differs, a hot DSP injection when it does not.
        let force = !differing.is_empty();
        let load_new = |this: &mut Self, reason: String| {
            let path = if force { "boot" } else { "dsp" };
            let params = json!({
                "bytes": true, "force": force, "start": "auto", "restart": true, "card": card,
                "report_extra": with_build(json!({"path": path, "reason": reason, "apply_ms": apply_ms}), &build),
            });
            this.reload = Some(ReloadState { running_sha: firmware::sha256_hex(&new_syx), baseline: new_syx.clone() });
            this.firmware_load(&Params(&params), new_syx.clone(), reply)
        };
        if let Some(reason) = blocker {
            return load_new(self, reason);
        }
        // The swap: the baseline's ready state, then the new module bytes.
        let restore_started = Instant::now();
        let restore = json!({"bytes": true, "force": true, "start": "snapshot", "restart": true, "card": card});
        let restored = match self.firmware_load(&Params(&restore), baseline.clone(), reply) {
            Ok(Some(report)) => report,
            Ok(None) => return Err(rejected("internal: restoring a ready state started a boot".into())),
            Err(e) => return load_new(self, format!("no ready state to restore: {}", e.message)),
        };
        let restore_ms = restore_started.elapsed().as_secs_f64() * 1e3;
        let write_started = Instant::now();
        let mut bytes = 0;
        for c in &diff.changes {
            let addr = u32::try_from(c.addr).map_err(|_| rejected(format!("{:#x} is not a 32-bit address", c.addr)))?;
            self.with_runtime(|r| r.write_ram(addr, &c.bytes).map_err(RpcError::bad_params))?;
            bytes += c.bytes.len();
        }
        let write_ms = write_started.elapsed().as_secs_f64() * 1e3;
        let analysis = Analysis::of(&new_syx).map_err(&rejected)?;
        if let Some(l) = self.loaded.as_mut() {
            l.current = new_syx.clone();
            l.current_analysis = Some(analysis);
        }
        self.reload = Some(ReloadState { running_sha: firmware::sha256_hex(&new_syx), baseline });
        Ok(Some(with_build(
            json!({
                "path": "swap", "ranges": diff.changes.len(), "bytes": bytes, "owners": diff.owners(),
                "ms": started.elapsed().as_secs_f64() * 1e3, "apply_ms": apply_ms, "restore_ms": restore_ms, "write_ms": write_ms,
                "restore": restored["start_used"],
            }),
            &build,
        )))
    }

    fn rpc_run_mode(&mut self, p: &Params) -> RpcResult {
        let mode = p.str("mode")?;
        let mode =
            Mode::parse(mode).ok_or_else(|| RpcError::bad_params("mode is ui, free or paused"))?;
        let previous = std::mem::replace(&mut self.mode, mode);
        self.emit("mode", json!({"mode": mode.name()}));
        Ok(json!({"previous": previous.name()}))
    }

    fn rpc_step(&mut self, p: &Params, reply: &RpcReply) -> Result<Option<Value>, RpcError> {
        let n = p.u64("instructions")?;
        let icount = self.with_runtime(|r| Ok(r.icount()))?;
        if n == 0 {
            return Ok(Some(json!({"stepped": 0})));
        }
        let wall = p.opt_f64("timeout_wall_s")?;
        self.start_job(
            JobKind::Steps { end: icount + n },
            reply,
            wall,
            json!({"requested": n}),
            "step",
        )?;
        Ok(None)
    }

    fn rpc_run_until(&mut self, p: &Params, reply: &RpcReply) -> Result<Option<Value>, RpcError> {
        let (icount, elapsed) =
            self.with_runtime(|r| Ok((r.icount(), r.elapsed_instructions())))?;
        let until = if let Some(n) = p.opt_u64("elapsed")? {
            Until::Elapsed(n)
        } else if let Some(n) = p.opt_u64("icount")? {
            Until::Icount(n)
        } else if let Some(n) = p.opt_u64("frames")? {
            Until::Frames(n)
        } else if let Some(s) = p.opt_f64("audio_s")? {
            Until::Frames((s * scenario::FRAMES_PER_SECOND).round() as u64)
        } else {
            return Err(RpcError::bad_params(
                "run_until needs one of elapsed, icount, frames, audio_s",
            ));
        };
        let _ = (icount, elapsed);
        if matches!(until, Until::Frames(_))
            && !self.runtime.as_ref().is_some_and(Runtime::has_audio)
        {
            return Err(RpcError::new(
                code::AUDIO_UNAVAILABLE,
                "frames and audio_s need a DSP audio session",
            ));
        }
        let wall = p.opt_f64("timeout_wall_s")?;
        self.start_job(JobKind::Until(until), reply, wall, json!({}), "run_until")?;
        Ok(None)
    }

    fn rpc_input(&mut self, method: &str, p: &Params) -> RpcResult {
        let action = if method == "input.button" {
            Action::Button {
                code: p.u8("code")?,
                down: p.opt_bool("down")?.unwrap_or(true),
            }
        } else {
            Action::Turn {
                encoder: p.u8("encoder")?,
                detents: i32::try_from(p.opt_i64("detents")?.unwrap_or(1))
                    .map_err(|_| RpcError::bad_params("detents out of range"))?,
            }
        };
        let at = p.opt_u64("at_instr")?;
        self.place_input(at, action)
            .map(|at| json!({"scheduled_at": at}))
    }

    /// Fire now (no `at`, or already due) or queue for `at`; the instruction
    /// count the input lands at.
    fn place_input(&mut self, at: Option<u64>, action: Action) -> Result<u64, RpcError> {
        let runtime = self.runtime.as_mut().ok_or_else(RpcError::no_firmware)?;
        let elapsed = runtime.elapsed_instructions();
        match at {
            None => {
                scenario::fire(runtime, action).map_err(RpcError::bad_params)?;
                Ok(elapsed)
            }
            Some(at) if at < elapsed => Err(RpcError::bad_params(format!(
                "at_instr {at} is in the past (elapsed {elapsed})"
            ))),
            Some(at) if at == elapsed => {
                scenario::fire(runtime, action).map_err(RpcError::bad_params)?;
                Ok(at)
            }
            Some(at) => {
                let index = self.schedule.partition_point(|s| s.at <= at);
                self.schedule.insert(index, Scheduled { at, action });
                Ok(at)
            }
        }
    }

    fn rpc_tap(&mut self, p: &Params) -> RpcResult {
        let code = p.u8("code")?;
        let hold = p
            .opt_u64("hold")?
            .unwrap_or(40_000_000)
            .clamp(1, 100_000_000);
        let at = p.opt_u64("at_instr")?;
        let down = self.place_input(at, Action::Button { code, down: true })?;
        let up = self.place_input(Some(down + hold), Action::Button { code, down: false })?;
        Ok(json!({"scheduled_at": down, "release_at": up}))
    }

    fn rpc_schedule(&mut self, p: &Params) -> RpcResult {
        let events = p
            .get("events")
            .and_then(Value::as_array)
            .ok_or_else(|| RpcError::bad_params("events must be an array"))?;
        let parsed = scenario::parse_scenario(&json!({
            "name": "schedule", "instructions": 1, "events": events,
        }))
        .map_err(RpcError::bad_params)?;
        let mut placed = 0;
        for event in parsed.events {
            match event.when {
                scenario::When::Instr(at) => {
                    self.place_input(Some(at), event.action)?;
                    placed += 1;
                }
                scenario::When::Frames(_) => {
                    return Err(RpcError::bad_params("input.schedule takes at_instr events"));
                }
            }
        }
        Ok(json!({"placed": placed}))
    }

    fn rpc_recorded(&mut self) -> RpcResult {
        self.with_runtime(|r| Ok(json!({"scenario": r.recorded_scenario()})))
    }

    /// `framebuffer.get {hash?, data?}`: the frame; `hash: true` adds the SHA-256
    /// of its bytes, `data: false` omits them.
    fn rpc_framebuffer(&mut self, p: &Params) -> RpcResult {
        let hash = p.opt_bool("hash")?.unwrap_or(false);
        let data = p.opt_bool("data")?.unwrap_or(true);
        self.with_runtime(|r| {
            let (revision, bytes) = r
                .current_frame()
                .map(|(revision, bytes)| (revision, bytes.to_vec()))
                .ok_or_else(|| RpcError::new(code::GUEST_FAULT, "no frame yet"))?;
            let mut frame = json!({
                "revision": revision, "width": 128, "height": 64,
                "format": "oled-1bpp-column-major",
                "note": "byte (7 - y/8) + 8*x, bit y%8",
            });
            if data {
                frame["data_b64"] = base64(&bytes).into();
            }
            if hash {
                frame["sha256"] = firmware::sha256_hex(&bytes).into();
            }
            Ok(frame)
        })
    }

    /// `leds.get`: the panel LEDs as the emulator holds them now, read without
    /// consuming them from the UI.
    fn rpc_leds(&mut self) -> RpcResult {
        self.with_runtime(|r| {
            let (led_revision, leds, rgb) = r.current_leds();
            Ok(json!({"led_revision": led_revision, "leds": leds, "rgb": rgb}))
        })
    }

    fn rpc_pcm_tap(&mut self, p: &Params) -> RpcResult {
        #[cfg(feature = "coupled-audio")]
        {
            let action = p.opt_str("action")?.unwrap_or("report");
            self.with_runtime(|r| {
                if !r.has_audio() {
                    return Err(RpcError::new(
                        code::AUDIO_UNAVAILABLE,
                        "no DSP audio session",
                    ));
                }
                match action {
                    "start" => {
                        r.observe_pcm(false);
                        Ok(json!({"started": true}))
                    }
                    "report" => {
                        r.sync_audio();
                        let tap = r
                            .pcm_tap()
                            .ok_or_else(|| RpcError::bad_params("pcm.tap was not started"))?;
                        Ok(json!({
                            "samples": tap.count, "seconds": tap.seconds(),
                            "rms": tap.rms(), "peak": tap.peak,
                            "nonzero": tap.nonzero, "non_finite": tap.non_finite,
                            "sha256": tap.sha256_hex(),
                            "window_rms": tap.window_rms,
                        }))
                    }
                    "capture" => {
                        let path = p.str("path")?;
                        r.sync_audio();
                        r.capture_pcm(Path::new(path))
                            .map(|()| json!({"capturing": path}))
                            .map_err(RpcError::io)
                    }
                    "capture_stop" => {
                        r.stop_pcm_capture();
                        Ok(json!({"capturing": null}))
                    }
                    _ => Err(RpcError::bad_params(
                        "action is start, report, capture or capture_stop",
                    )),
                }
            })
        }
        #[cfg(not(feature = "coupled-audio"))]
        {
            let _ = p;
            Err(RpcError::new(
                code::AUDIO_UNAVAILABLE,
                "this build has no coupled audio",
            ))
        }
    }

    /// `dsp.peek {addr, len}`: DSP memory bytes as a hex string (`??` where
    /// the engine does not know the byte), at most 64 KiB.
    fn rpc_dsp_peek(&mut self, p: &Params) -> RpcResult {
        let addr = u32::try_from(p.u64("addr")?)
            .map_err(|_| RpcError::bad_params("addr is a 32-bit byte address"))?;
        let len = usize::try_from(p.u64("len")?).unwrap_or(usize::MAX);
        if len == 0 || len > 65_536 {
            return Err(RpcError::bad_params("len is 1 to 65536"));
        }
        let bytes = self.with_runtime(|r| {
            r.peek_dsp(addr, len)
                .map_err(|e| RpcError::new(code::AUDIO_UNAVAILABLE, e))
        })?;
        let hex: String = bytes
            .iter()
            .map(|b| b.map_or("??".to_string(), |b| format!("{b:02x}")))
            .collect();
        let unknown = bytes.iter().filter(|b| b.is_none()).count();
        Ok(json!({"addr": addr, "len": len, "hex": hex, "unknown": unknown}))
    }

    /// `dsp.peek_word {addr}`: one exact 32-bit DSP word, including compact
    /// MMR addresses. Unknown/unmodeled words remain null; no byte alignment.
    fn rpc_dsp_peek_word(&mut self, p: &Params) -> RpcResult {
        let addr = u32::try_from(p.u64("addr")?)
            .map_err(|_| RpcError::bad_params("addr is a 32-bit DSP word address"))?;
        let value = self.with_runtime(|r| {
            r.peek_dsp_word(addr)
                .map_err(|e| RpcError::new(code::AUDIO_UNAVAILABLE, e))
        })?;
        Ok(json!({"addr": addr, "value": value}))
    }

    /// Actions apply to the active coupled DSP. Event kind names describe
    /// helper paths, including host peeks; they do not prove guest provenance.
    fn rpc_dsp_ddr_monitor(&mut self, p: &Params) -> RpcResult {
        use elektron_native_boot::ddr_monitor::{DdrMonitorCommand, DdrMonitorRange};
        let command = match p.opt_str("action")?.unwrap_or("status") {
            "status" => DdrMonitorCommand::Status,
            "clear" => DdrMonitorCommand::Clear,
            "disarm" => DdrMonitorCommand::Disarm,
            "arm" => {
                let address = |key| {
                    u32::try_from(p.u64(key)?)
                        .map_err(|_| RpcError::bad_params("start and end are 32-bit byte addresses"))
                };
                DdrMonitorCommand::Arm(DdrMonitorRange {
                    start: address("start")?,
                    end: address("end")?,
                })
            }
            _ => {
                return Err(RpcError::bad_params(
                    "action is arm, status, clear or disarm",
                ));
            }
        };
        command.validate().map_err(RpcError::bad_params)?;
        let status = self.with_runtime(|r| {
            r.dsp_ddr_monitor(command)
                .map_err(|e| RpcError::new(code::AUDIO_UNAVAILABLE, e))
        })?;
        Ok(json!(status))
    }

    fn rpc_dsp_patch(&mut self, p: &Params) -> RpcResult {
        let text = match (p.opt_str("text")?, p.opt_str("file")?) {
            (Some(text), _) => text.to_string(),
            (None, Some(file)) => {
                std::fs::read_to_string(file).map_err(|e| RpcError::io(format!("{file}: {e}")))?
            }
            _ => return Err(RpcError::bad_params("dsp.patch needs text or file")),
        };
        let patch = DspPatch::parse(&text).map_err(RpcError::bad_params)?;
        if let (Some(base), Some(loaded)) = (&patch.base_sha256, &self.loaded)
            && *base != loaded.base_sha
            && !base.bytes().all(|b| b == b'0')
        {
            return Err(RpcError::new(
                code::FIRMWARE_REJECTED,
                "the patch's BASE is not the running firmware",
            ));
        }
        let started = Instant::now();
        let injected = self.with_runtime(|r| {
            r.inject_dsp_patch(patch)
                .map_err(|e| RpcError::new(code::AUDIO_UNAVAILABLE, e))
        })?;
        Ok(json!({"injected": injected, "seconds": started.elapsed().as_secs_f64()}))
    }

    fn rpc_restart(&mut self, p: &Params, reply: &RpcReply) -> Result<Option<Value>, RpcError> {
        let Some(loaded) = self.loaded.as_ref() else {
            return no_firmware();
        };
        // The card stays when the user chose it; a card built from the device's folder is built again from the folder as it is now
        // (a folder chosen in the Card panel applies here).
        let own = self.auto_card.as_ref().map(|(image, _)| image);
        let params = json!({
            "bytes": true, "force": true, "start": "auto", "restart": true,
            "auto_no": p.opt_bool("auto_no")?.unwrap_or(false),
            "card": loaded.card.as_ref().filter(|c| Some(*c) != own).map(|c| c.display().to_string()),
        });
        let current = loaded.current.clone();
        self.firmware_load(&Params(&params), current, reply)
    }

    fn rpc_firmware_load(
        &mut self,
        p: &Params,
        reply: &RpcReply,
    ) -> Result<Option<Value>, RpcError> {
        let path = p.str("syx")?;
        let bytes = std::fs::read(path).map_err(|e| RpcError::io(format!("{path}: {e}")))?;
        self.firmware_load(p, bytes, reply)
    }

    /// `firmware.load`: decide how to get from the running firmware to `bytes`
    /// (see the design's "Firmware injection"), do it, and report.
    fn firmware_load(
        &mut self,
        p: &Params,
        mut bytes: Vec<u8>,
        reply: &RpcReply,
    ) -> Result<Option<Value>, RpcError> {
        let rejected = |m: String| RpcError::new(code::FIRMWARE_REJECTED, m);
        if self.job.is_some() || self.patch_job.is_some() {
            return Err(RpcError::new(
                code::BUSY,
                "a job is running; wait for it before loading firmware",
            ));
        }
        if bytes.len() > MAX_SYX {
            return Err(rejected("firmware image exceeds 32 MiB".into()));
        }
        // What the running firmware wrote (a pending debounce) goes to its own device's folder before the next load switches the
        // mirror to the folder of the device it loads.
        self.mirror_flush();
        self.next_auto = None;
        let started = Instant::now();
        let start = p.opt_str("start")?.unwrap_or("auto");
        if !matches!(start, "auto" | "snapshot" | "cold") {
            return Err(RpcError::bad_params("start is auto, snapshot or cold"));
        }
        let force = p.opt_bool("force")?.unwrap_or(false);
        self.want_auto_no = p.opt_bool("auto_no")?.unwrap_or(false);
        let mut base_param = p.opt_str("base_sha256")?.map(str::to_ascii_lowercase);
        // `runtime`: patch the stock image with the device's patch set here (cached); a patched input is left as it is.
        let runtime_requested = p.opt_bool("runtime")?.unwrap_or(false);
        let patch_set = p.opt_str("patch_set")?;
        if patch_set.is_some() && !runtime_requested {
            return Err(RpcError::bad_params("patch_set needs runtime: true"));
        }
        // `replace`: load as the first firmware (nothing of the running one is reused, and a different device is fine): the webview's
        // Open and Reload. Without it the image is a variant of the running one (injection).
        let replace = p.opt_bool("replace")?.unwrap_or(false);
        let mut stock_base: Option<Vec<u8>> = None;
        let mut runtime_report = None;
        if runtime_requested {
            let set = patch_set.unwrap_or("emulator");
            let started = Instant::now();
            let patch_error = |m: String| rejected(format!("runtime patch: {m}"));
            let root = runtime_patch::find_root(self.repo_root.as_deref()).map_err(&patch_error)?;
            match runtime_patch::prepare(&root, &bytes, set).map_err(&patch_error)? {
                runtime_patch::Outcome::AlreadyPatched => {
                    runtime_report = Some(json!({"patched": false, "reason": "the input has a patch record"}));
                }
                runtime_patch::Outcome::Patched(patched) => {
                    runtime_report = Some(json!({
                        "patched": true, "set": patched.set, "cache_hit": patched.cache_hit,
                        "modules": patched.modules, "base_sha256": patched.base_sha256,
                        "cache_file": patched.cache_file.display().to_string(),
                        "seconds": started.elapsed().as_secs_f64(),
                    }));
                    base_param.get_or_insert_with(|| patched.base_sha256.clone());
                    stock_base = Some(std::mem::replace(&mut bytes, patched.syx));
                }
            }
        }
        let card_param = p.opt_str("card")?.map(PathBuf::from);
        let patch = match p.opt_str("dsp_patch")? {
            Some(file) => {
                let text = std::fs::read_to_string(file)
                    .map_err(|e| RpcError::io(format!("{file}: {e}")))?;
                Some(DspPatch::parse(&text).map_err(RpcError::bad_params)?)
            }
            None => None,
        };
        let new = Analysis::of(&bytes).map_err(&rejected)?;
        if let Some(patch) = &patch
            && let Some(base) = &patch.base_sha256
            && *base != new.sha256
            && !base.bytes().all(|b| b == b'0')
        {
            return Err(rejected(format!(
                "dsp_patch BASE {base} is not this .syx ({})",
                new.sha256
            )));
        }
        let mut report = json!({
            "syx_sha256": new.sha256, "start_requested": start,
            "main_changed": false, "sections_changed": [],
            "dsp": null, "audio": false, "cache_hit": false, "snapshot_ok": false,
        });
        if let Some(r) = runtime_report {
            report["runtime"] = r;
        }
        // Fields a caller that loads on behalf of another method (`dev.reload`) adds to the report, the cold boot's included.
        if let Some(extra) = p.get("report_extra").and_then(Value::as_object) {
            for (key, value) in extra {
                report[key.as_str()] = value.clone();
            }
        }

        // Running firmware to compare with.
        if self.loaded.is_some() {
            let loaded = self.loaded.as_mut().expect("loaded");
            loaded.ensure_analysis().map_err(&rejected)?;
        }
        if replace {
            // As `ui_load` always did: forget the running firmware, its card job and its audio setup.
            self.cancel_card();
            self.audio = self.audio_for_base(base_param.as_deref().unwrap_or(&new.sha256));
            self.card_last = json!({"state": "idle"});
            self.read_error = None;
            self.runtime = None;
            self.loaded = None;
        }
        // Not an injection: the status keeps what the running image got, a restart keeps it.
        if !p.opt_bool("restart")?.unwrap_or(false) {
            self.runtime_info = report.get("runtime").cloned();
        }
        let have_runtime = self.runtime.is_some() && self.loaded.is_some();
        if let (Some(base), Some(loaded)) = (&base_param, &self.loaded)
            && have_runtime
            && *base != loaded.base_sha
        {
            return Err(rejected(format!(
                "base_sha256 {base} is not the running firmware's base {}",
                loaded.base_sha
            )));
        }

        let job_started = if !have_runtime {
            // First load: stock when the file is a known one (or base_sha256
            // names itself), otherwise a patched build of `base_sha256`.
            let base_sha = base_param.clone().unwrap_or_else(|| new.sha256.clone());
            let card = card_param.clone().or_else(|| self.default_card(&bytes, stock_base.as_deref()));
            if base_sha == new.sha256 {
                self.load_stock(bytes, card, new, &mut report, start)?
            } else {
                self.load_patched(bytes, card, new, base_sha, stock_base, &mut report, start, reply)?
            }
        } else {
            let loaded = self.loaded.as_ref().expect("loaded");
            let running = loaded.current_analysis.clone().expect("analysis");
            let base_analysis = loaded.base_analysis.clone();
            let base_bytes = loaded.base.clone();
            let base_sha = loaded.base_sha.clone();
            let changed = running.changed_sections(&new);
            report["sections_changed"] = json!(changed);
            let main_changed = changed.contains(&firmware::SECTION_MAIN);
            report["main_changed"] = main_changed.into();
            if !main_changed && !force {
                self.load_hot(bytes, new, running, &mut report)?
            } else {
                // not for a hot load: it keeps the running card
                let card = card_param.or_else(|| self.default_card(&bytes, base_bytes.as_deref()));
                let stock_main = base_analysis
                    .as_ref()
                    .is_some_and(|b| b.main_sha256() == new.main_sha256());
                match base_bytes {
                    Some(base_bytes) if stock_main => self.load_stock_over(
                        base_bytes,
                        bytes,
                        card,
                        new,
                        base_analysis,
                        &mut report,
                        start,
                    )?,
                    base_bytes => self.load_patched(
                        bytes,
                        card,
                        new,
                        base_sha,
                        base_bytes,
                        &mut report,
                        start,
                        reply,
                    )?,
                }
            }
        };
        // A patch file on top of the .syx.
        if let Some(patch) = patch
            && !job_started
        {
            let ops = patch.ops.len();
            let injected = self
                .runtime
                .as_mut()
                .ok_or_else(RpcError::no_firmware)?
                .inject_dsp_patch(patch)
                .map_err(|e| RpcError::new(code::AUDIO_UNAVAILABLE, e))?;
            report["dsp_patch_file"] = json!({"ops": ops, "injected": injected});
            if report["start_used"] == "none" {
                report["start_used"] = "hot-dsp".into();
            }
        }
        if job_started {
            // The cold boot answers when it is ready.
            return Ok(None);
        }
        self.want_auto_no = false;
        report["seconds"] = started.elapsed().as_secs_f64().into();
        self.emit(
            "firmware.loaded",
            json!({"ok": true, "start_used": report["start_used"]}),
        );
        Ok(Some(report))
    }

    /// First load of a known firmware, as the webview does it.
    fn load_stock(
        &mut self,
        bytes: Vec<u8>,
        card: Option<PathBuf>,
        new: Analysis,
        report: &mut Value,
        start: &str,
    ) -> Result<bool, RpcError> {
        if start == "cold" {
            return Err(RpcError::bad_params(
                "start cold needs a patched MAIN; a stock firmware restores its snapshot",
            ));
        }
        let (runtime, error) = runtime_from(&bytes, card.clone(), self.audio.as_ref())
            .map_err(|e| RpcError::new(code::FIRMWARE_REJECTED, e))?;
        report["audio"] = runtime.has_audio().into();
        report["snapshot_ok"] = (runtime.has_audio() || runtime.is_zero()).into();
        report["start_used"] = if runtime.has_audio() || runtime.is_zero() {
            "snapshot"
        } else {
            "cold"
        }
        .into();
        self.adopt_card_folder(card.as_ref());
        let mut loaded = Loaded::stock(bytes, card);
        loaded.base_analysis = Some(new.clone());
        loaded.current_analysis = Some(new);
        self.loaded = Some(loaded);
        self.install(runtime, error);
        self.session = self.session.max(1);
        self.emit("session.changed", json!({"reason": "firmware.load"}));
        Ok(false)
    }

    /// The running firmware has the same MAIN as `new` or `new`'s MAIN is the
    /// base's: rebuild from the base (snapshot restore with audio) and
    /// re-apply the DSP difference.
    fn load_stock_over(
        &mut self,
        base_bytes: Vec<u8>,
        bytes: Vec<u8>,
        card: Option<PathBuf>,
        new: Analysis,
        base_analysis: Option<Analysis>,
        report: &mut Value,
        start: &str,
    ) -> Result<bool, RpcError> {
        if start == "cold" {
            return Err(RpcError::bad_params(
                "start cold is for a patched MAIN; this MAIN is the base's",
            ));
        }
        let (runtime, error) = runtime_from(&base_bytes, card.clone(), self.audio.as_ref())
            .map_err(|e| RpcError::new(code::FIRMWARE_REJECTED, e))?;
        report["audio"] = runtime.has_audio().into();
        report["snapshot_ok"] = (runtime.has_audio() || runtime.is_zero()).into();
        report["start_used"] = if runtime.has_audio() || runtime.is_zero() {
            "snapshot"
        } else {
            "cold"
        }
        .into();
        let base_sha = firmware::sha256_hex(&base_bytes);
        self.adopt_card_folder(card.as_ref());
        self.loaded = Some(Loaded {
            base: Some(base_bytes),
            base_sha,
            current: bytes.clone(),
            card,
            base_analysis: base_analysis.clone(),
            current_analysis: Some(new.clone()),
        });
        self.install(runtime, error);
        self.emit("session.changed", json!({"reason": "firmware.load"}));
        // DSP words that differ from the base.
        if let (Some(base), Some(new_dsp)) = (base_analysis.and_then(|b| b.dsp), new.dsp)
            && base != new_dsp
        {
            let diff = firmware::dsp_diff(&base, &new_dsp)
                .map_err(|e| RpcError::new(code::FIRMWARE_REJECTED, e))?;
            report["dsp"] = self.inject_diff(diff)?;
        }
        Ok(false)
    }

    /// MAIN is the same: no restart; hot-inject the DSP words that changed.
    fn load_hot(
        &mut self,
        bytes: Vec<u8>,
        new: Analysis,
        running: Analysis,
        report: &mut Value,
    ) -> Result<bool, RpcError> {
        let dsp_changed = running
            .changed_sections(&new)
            .contains(&firmware::SECTION_DSP);
        report["start_used"] = "none".into();
        if dsp_changed {
            let has_audio = self.runtime.as_ref().is_some_and(Runtime::has_audio);
            report["audio"] = has_audio.into();
            match (has_audio, &running.dsp, &new.dsp) {
                (true, Some(old), Some(new_dsp)) => {
                    let diff = firmware::dsp_diff(old, new_dsp)
                        .map_err(|e| RpcError::new(code::FIRMWARE_REJECTED, e))?;
                    report["dsp"] = self.inject_diff(diff)?;
                    report["start_used"] = "hot-dsp".into();
                }
                (false, ..) => {
                    report["dsp"] = json!({"applied": false, "reason": "no DSP audio session"});
                }
                _ => {
                    return Err(RpcError::new(
                        code::FIRMWARE_REJECTED,
                        "the firmware has no DSP section to diff",
                    ));
                }
            }
        } else {
            report["audio"] = self.runtime.as_ref().is_some_and(Runtime::has_audio).into();
        }
        if let Some(loaded) = self.loaded.as_mut() {
            loaded.current = bytes;
            loaded.current_analysis = Some(new);
        }
        Ok(false)
    }

    fn inject_diff(&mut self, diff: firmware::DspDiff) -> Result<Value, RpcError> {
        let (changed, dropped, ranges) = (diff.changed_bytes, diff.dropped_bytes, diff.ranges);
        let injected = if diff.patch.ops.is_empty() {
            json!({"ops": 0})
        } else {
            self.runtime
                .as_mut()
                .ok_or_else(RpcError::no_firmware)?
                .inject_dsp_patch(diff.patch)
                .map_err(|e| RpcError::new(code::AUDIO_UNAVAILABLE, e))?
        };
        Ok(json!({
            "applied": true, "changed_bytes": changed, "ranges": ranges,
            "dropped_bytes": dropped, "injected": injected,
        }))
    }

    /// A MAIN the profile does not know: boot the patched build of
    /// `base_sha` (ColdFire only), from the snapshot cache when there is one
    /// and otherwise cold, saving the cache once it is ready.
    #[allow(clippy::too_many_arguments)]
    fn load_patched(
        &mut self,
        bytes: Vec<u8>,
        card: Option<PathBuf>,
        new: Analysis,
        base_sha: String,
        base_bytes: Option<Vec<u8>>,
        report: &mut Value,
        start: &str,
        reply: &RpcReply,
    ) -> Result<bool, RpcError> {
        if self.reset_overlay.is_none() && self.reset_ram.is_none() {
            self.boot_resets = 0;
        }
        let rejected = |m: String| RpcError::new(code::FIRMWARE_REJECTED, m);
        let main_sha = new
            .main_sha256()
            .ok_or_else(|| rejected("the firmware has no MAIN section".into()))?
            .to_string();
        // The ColdFire-only ready state holds the runtime's picture of the card (its store header and table), so it belongs to
        // the card it booted on: a state restored onto another card would write that picture back.
        let card_key = card_snapshot_key(card.as_deref(), std::env::var_os("DIGI_COLD_RELAX_FS").is_some());
        let zero = self.audio.as_ref().is_some_and(|audio| audio.zero);
        let mode = if zero { "-zero" } else { "" };
        let cache = self
            .run_dir
            .join("patched")
            .join(format!("{main_sha}{card_key}{mode}.snap"));
        // With coupled audio inputs for the base: boot the ColdFire and the
        // DSP together (or restore the cached pair).
        match self.load_patched_coupled(
            &bytes,
            &card,
            &new,
            &base_sha,
            &base_bytes,
            report,
            start,
            reply,
        )? {
            CoupledLoad::Done => return Ok(false),
            CoupledLoad::Started => return Ok(true),
            CoupledLoad::Unavailable(why) => {
                report["coupled_note"] = why.into();
            }
        }
        // A soft reset's private card/RAM state must neither restore nor replace the
        // initial-load cache for this MAIN. Keep it until installation succeeds.
        let resetting = self.reset_overlay.is_some() || self.reset_ram.is_some();
        let cache = (!resetting).then_some(cache);
        let (mut card_obj, error) = card_for(card.clone()).map_err(&rejected)?;
        if let Some(sectors) = &self.reset_overlay {
            apply_overlay_sectors(card_obj.get_or_insert_with(Card::default), sectors)
                .map_err(&rejected)?;
        }
        let mut runtime = Runtime::new_patched(&bytes, card_obj, &base_sha).map_err(&rejected)?;
        if zero {
            runtime.enable_zero_dsp_timing().map_err(&rejected)?;
        }
        if resetting || std::env::var_os("DIGI_COLD_RELAX_FS").is_some() {
            // A used card may skip the boot-time filesystem check, as on the coupled reset path.
            runtime.relax_fs_check();
        }
        if let Some(ram) = &self.reset_ram {
            let registry = plusdrive_format::device_profile::Registry::load().map_err(|e| rejected(e.to_string()))?;
            let (device, _) = registry.identify(&base_sha).map_err(|e| rejected(e.to_string()))?;
            let (base, size) = device.keep_ram
                .ok_or_else(|| rejected("soft reset RAM has no device keep range".into()))?;
            if ram.len() != size as usize || ram.len() % 4 != 0 {
                return Err(rejected("soft reset RAM does not match the device keep range".into()));
            }
            runtime.poke_ram(base, ram).map_err(&rejected)?;
        }
        let cache_hit = start != "cold" && cache.as_ref().is_some_and(|p| p.is_file());
        report["audio"] = false.into();
        report["dsp"] = json!({
            "applied": false,
            "reason": "a patched MAIN boots the ColdFire side only; DSP state cannot be rebuilt",
        });
        let mut boot_job = false;
        if cache_hit {
            match std::fs::read(cache.as_ref().expect("cache hit"))
                .map_err(|e| e.to_string())
                .and_then(|data| runtime.restore_state(&data))
            {
                Ok(()) => {
                    report["start_used"] = "snapshot-cache".into();
                    report["cache_hit"] = true.into();
                    report["snapshot_ok"] = true.into();
                }
                Err(e) => {
                    if start == "snapshot" {
                        return Err(rejected(format!("cached snapshot unusable: {e}")));
                    }
                    // A fresh emulator for the cold start: a failed restore
                    // may have changed the state.
                    let (card_obj, new_error) = card_for(card.clone()).map_err(&rejected)?;
                    runtime =
                        Runtime::new_patched(&bytes, card_obj, &base_sha).map_err(&rejected)?;
                    if zero {
                        runtime.enable_zero_dsp_timing().map_err(&rejected)?;
                    }
                    report["cache_note"] = format!("cached snapshot unusable: {e}").into();
                    boot_job = true;
                    let _ = new_error;
                }
            }
        } else if start == "snapshot" {
            return Err(rejected(format!(
                "no cached snapshot for this MAIN ({}); start snapshot never boots cold",
                cache.as_ref().map_or_else(|| "soft reset".into(), |p| p.display().to_string())
            )));
        } else {
            boot_job = true;
        }
        if boot_job {
            report["start_used"] = "cold".into();
        }
        self.loaded = Some(Loaded {
            base: base_bytes,
            base_sha,
            current: bytes,
            card,
            base_analysis: None,
            current_analysis: Some(new),
        });
        self.install(runtime, error);
        self.reset_overlay = None;
        self.reset_ram = None;
        self.session = self.session.max(1);
        self.emit("session.changed", json!({"reason": "firmware.load"}));
        if boot_job {
            let partial = report.clone();
            self.start_job(
                JobKind::Boot {
                    cache,
                    limit: BOOT_LIMIT,
                },
                reply,
                None,
                partial,
                "firmware.load",
            )?;
            return Ok(true);
        }
        Ok(false)
    }
}

/// Bytes from a hex string (no separators; an even number of digits).
fn decode_hex(text: &str) -> Result<Vec<u8>, String> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return Err("hex is an even number of hex digits".into());
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).map_err(|_| format!("hex: bad digits at {i}")))
        .collect()
}

// ----- patched MAIN with DSP audio: the coupled cold boot ---------------------

/// What `load_patched_coupled` did.
enum CoupledLoad {
    /// Installed now (a cached ready pair).
    Done,
    /// A worker is cold booting; the reply comes when it is ready.
    Started,
    /// No coupled inputs for this base; the ColdFire-only path takes over.
    Unavailable(String),
}

/// Everything `install_patched_audio` needs once the ready pair exists.
struct PatchInstall {
    bytes: Vec<u8>,
    card: Option<PathBuf>,
    new: Analysis,
    base_sha: String,
    base_bytes: Option<Vec<u8>>,
    /// The launch options with the snapshot and DSP state of this MAIN.
    audio: AudioOptions,
    report: Value,
    /// The cold boot ran with the DSP patch already applied: the saved DSP state has it.
    dsp_in_boot: bool,
}

/// What a finished coupled cold boot reports.
struct PatchBooted {
    icount: u64,
    frames: usize,
    wall_seconds: f64,
    dsp_start_seconds: f64,
}

struct PatchJob {
    done: mpsc::Receiver<Result<PatchBooted, String>>,
    cancel: Arc<AtomicBool>,
    reply: RpcReply,
    started: Instant,
    install: PatchInstall,
    /// The card writes of a boot that ended in a soft reset (see `digi_coupled::cold::SOFT_RESET`).
    reset_out: Arc<Mutex<Option<Vec<emmc_card::CardOverlaySector>>>>,
    /// The kept RAM range of a boot that ended in a soft reset (see `digi_coupled::cold::KeepRam`).
    keep_out: Arc<Mutex<Option<Vec<u8>>>>,
}

/// The file name suffix of a ColdFire-only ready state for `card` (empty without a card): the card file's identity and the readiness
/// contract the boot ran with.
fn card_snapshot_key(card: Option<&Path>, relax_fs: bool) -> String {
    use sha2::{Digest, Sha256};
    let Some(card) = card else {
        return String::new();
    };
    let mut hash = Sha256::new();
    if let Ok(meta) = std::fs::metadata(card) {
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        hash.update(format!("{}:{}:{modified}:{relax_fs}", card.display(), meta.len()).as_bytes());
    }
    format!("-{:x}", hash.finalize())[..17].to_string()
}

/// The file name stem of the ready pair of a (MAIN, DSP inputs, card): the
/// SHA-256 of the MAIN digest, of the DSP image and selected startup state, and
/// of the card file's identity.
#[cfg(feature = "coupled-audio")]
fn patched_pair_key(
    main_sha: &str,
    image: &Path,
    cold_state: &Path,
    card: Option<&Path>,
    extra: &[u8],
) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update(main_sha.as_bytes());
    // what else the cold boot started from: the DSP section it ran with, the card writes of a soft reset
    hash.update(extra);
    for path in [image, cold_state] {
        let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        hash.update(Sha256::digest(&data));
    }
    if let Some(card) = card {
        let meta = std::fs::metadata(card).map_err(|e| format!("{}: {e}", card.display()))?;
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        hash.update(format!("{}:{}:{modified}", card.display(), meta.len()).as_bytes());
    }
    let key = format!("{:x}", hash.finalize());
    Ok(format!(
        "{}-{}",
        &main_sha[..16.min(main_sha.len())],
        &key[..16]
    ))
}

impl Actor {
    /// A patched MAIN with DSP audio. The ColdFire boots from the patched
    /// bytes with its DSP initialized before ColdFire startup. The ready pair
    /// is cached under `<run_dir>/patched/<MAIN>-<key>/`.
    #[cfg(feature = "coupled-audio")]
    #[allow(clippy::too_many_arguments)]
    fn load_patched_coupled(
        &mut self,
        bytes: &[u8],
        card: &Option<PathBuf>,
        new: &Analysis,
        base_sha: &str,
        base_bytes: &Option<Vec<u8>>,
        report: &mut Value,
        start: &str,
        reply: &RpcReply,
    ) -> Result<CoupledLoad, RpcError> {
        let rejected = |m: String| RpcError::new(code::FIRMWARE_REJECTED, m);
        let Some(mut audio) = self.audio_for_base(base_sha).or_else(|| self.audio.clone()) else {
            return Ok(CoupledLoad::Unavailable("no coupled audio inputs".into()));
        };
        let unavailable = |why: String| Ok(CoupledLoad::Unavailable(why));
        let image = match std::fs::canonicalize(&audio.image) {
            Ok(image) => image,
            Err(e) => return unavailable(format!("DSP image {}: {e}", audio.image.display())),
        };
        let dir = image.parent().unwrap_or(Path::new(".")).to_path_buf();
        let cold_idle = dir.join("cold-idle.dsp");
        // The ready inputs must be the base's: SOURCE.json names their .syx.
        if let Some(source) = std::fs::read_to_string(dir.join("SOURCE.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v["syx_sha256"].as_str().map(str::to_string))
            && source != base_sha
        {
            return unavailable(format!(
                "the ready inputs in {} belong to another firmware ({})",
                dir.display(),
                &source[..12.min(source.len())]
            ));
        }
        let main_sha = new
            .main_sha256()
            .ok_or_else(|| rejected("the firmware has no MAIN section".into()))?;
        // A changed DSP starts from raw reset state with the diff applied once,
        // reaches idle, then attaches to the ColdFire. Unchanged DSP restores idle.
        let mut boot_patch = None;
        if let (Some(base), Some(new_dsp)) = (
            base_bytes
                .as_ref()
                .and_then(|b| Analysis::of(b).ok())
                .and_then(|a| a.dsp),
            new.dsp.as_ref(),
        ) && base != *new_dsp
        {
            boot_patch = Some(firmware::dsp_diff(&base, new_dsp).map_err(&rejected)?.patch);
        }
        let cold_state = boot_patch.as_ref().map(|_| dir.join("cold.state"));
        let startup_state = cold_state.as_deref().unwrap_or(&cold_idle);
        if !startup_state.is_file() {
            if cold_state.is_none() {
                return unavailable(format!(
                    "{} not found (run `mise run emu:ready -- <device>`)",
                    cold_idle.display()
                ));
            }
            return Err(rejected(format!(
                "{} not found; regenerate ready inputs with `mise run emu:ready -- <device> --force`",
                startup_state.display()
            )));
        }
        let reset_out = Arc::new(Mutex::new(None));
        let mut extra_key = Vec::new();
        // DIGI_COLD_RELAX_FS=1: a cold boot on a card an earlier run used (a saved project) never runs the DT2 filesystem check
        // the readiness contract waits for: ready at the main panel, as after a soft reset
        let relax_fs = std::env::var_os("DIGI_COLD_RELAX_FS").is_some();
        if relax_fs {
            extra_key.extend_from_slice(b"relax-fs:");
        }
        if let (Some(_), Some(dsp)) = (&boot_patch, new.dsp.as_deref()) {
            use sha2::{Digest, Sha256};
            extra_key.extend_from_slice(b"dsp-raw-init-v1:");
            extra_key.extend_from_slice(&Sha256::digest(dsp));
        }
        let reset_overlay = self.reset_overlay.take();
        if let Some(o) = &reset_overlay {
            extra_key.extend_from_slice(b"soft-reset:");
            extra_key.extend_from_slice(&overlay_digest(o));
        }
        // the RAM the model keeps across a soft reset (the guest's crash record): written before the first instruction, so the boot and its
        // cached ready pair depend on it
        let reset_ram = self.reset_ram.take().filter(|r| r.iter().any(|b| *b != 0));
        let keep_out = Arc::new(Mutex::new(None));
        let keep_range = plusdrive_format::device_profile::Registry::load()
            .ok()
            .and_then(|reg| {
                reg.identify(base_sha)
                    .ok()
                    .and_then(|(device, _)| device.keep_ram)
            });
        let keep_ram = match (keep_range, &reset_ram) {
            (Some((base, size)), r) => {
                if let Some(r) = r {
                    use sha2::{Digest, Sha256};
                    extra_key.extend_from_slice(b"keep-ram:");
                    extra_key.extend_from_slice(&Sha256::digest(r));
                }
                Some(digi_coupled::cold::KeepRam {
                    base,
                    bytes: size,
                    input: r.clone().unwrap_or_default(),
                    out: keep_out.clone(),
                })
            }
            _ => None,
        };
        let stem = patched_pair_key(main_sha, &image, startup_state, card.as_deref(), &extra_key)
            .map_err(&rejected)?;
        let pair = self.run_dir.join("patched").join(stem);
        let (snapshot, dsp_state) = (
            pair.join("digi-audio-m5.snap"),
            pair.join("digi-audio-m5.snap.dsp"),
        );
        audio.image = image.clone();
        audio.snapshot = snapshot.clone();
        audio.state = dsp_state;
        audio.dsp_patch = None;
        audio.automatic = false;
        audio.expected_syx = None;
        let mut install = PatchInstall {
            bytes: bytes.to_vec(),
            card: card.clone(),
            new: new.clone(),
            base_sha: base_sha.to_string(),
            base_bytes: base_bytes.clone(),
            audio,
            report: report.clone(),
            dsp_in_boot: boot_patch.is_some(),
        };
        let cache_hit = start != "cold" && snapshot.is_file();
        if cache_hit {
            install.report["start_used"] = "snapshot-cache".into();
            install.report["cache_hit"] = true.into();
            install.report["snapshot_ok"] = true.into();
            install.report["cache_dir"] = pair.display().to_string().into();
            match self.install_patched_audio(install) {
                Ok(done) => {
                    *report = done;
                    return Ok(CoupledLoad::Done);
                }
                Err(e) if start != "snapshot" => {
                    // A bad cache: drop it and boot cold.
                    report["cache_note"] = format!("cached pair unusable: {}", e.message).into();
                    let _ = std::fs::remove_file(&snapshot);
                    return self.load_patched_coupled(
                        bytes, card, new, base_sha, base_bytes, report, "cold", reply,
                    );
                }
                Err(e) => return Err(e),
            }
        }
        if start == "snapshot" {
            return Err(rejected(format!(
                "no cached pair for this MAIN ({}); start snapshot never boots cold",
                pair.display()
            )));
        }
        // The pair's presence is its snapshot: drop an old one first.
        let _ = std::fs::remove_file(&snapshot);
        install.report["start_used"] = "cold".into();
        install.report["cache_dir"] = pair.display().to_string().into();
        let cancel = Arc::new(AtomicBool::new(false));
        let (done_tx, done) = mpsc::channel();
        let (worker_cancel, worker_bytes, worker_card, worker_base) = (
            cancel.clone(),
            bytes.to_vec(),
            card.clone(),
            base_sha.to_string(),
        );
        let inputs = digi_coupled::cold::ColdInputs {
            image,
            cold_idle,
            cold_state,
            dsp_patch: boot_patch,
            relax_fs_check: reset_overlay.is_some() || relax_fs,
            reset_out: Some(reset_out.clone()),
            keep_ram,
        };
        thread::Builder::new()
            .name("digi-patched-boot".into())
            .spawn(move || {
                let result = card_for(worker_card).and_then(|(mut card_obj, _)| {
                    if let Some(sectors) = &reset_overlay {
                        // A guest with no card file still writes to the default blank card: lay its writes over a new one,
                        // or a soft reset loses them (an upload then a RESTART booted on an empty store).
                        let card = card_obj.get_or_insert_with(Card::default);
                        apply_overlay_sectors(card, sectors)?;
                    }
                    digi_coupled::cold::boot_patched_to_ready(
                        &worker_bytes,
                        card_obj,
                        &worker_base,
                        &inputs,
                        &pair,
                        &worker_cancel,
                        |_| {},
                    )
                });
                let _ = done_tx.send(result.map(|r| PatchBooted {
                    icount: r.icount,
                    frames: r.frames,
                    wall_seconds: r.wall_seconds,
                    dsp_start_seconds: r.dsp_start_seconds,
                }));
            })
            .map_err(|e| RpcError::io(format!("boot thread: {e}")))?;
        self.patch_job = Some(PatchJob {
            done,
            cancel,
            reply: reply.clone(),
            started: Instant::now(),
            install,
            reset_out,
            keep_out,
        });
        Ok(CoupledLoad::Started)
    }

    #[cfg(not(feature = "coupled-audio"))]
    #[allow(clippy::too_many_arguments)]
    fn load_patched_coupled(
        &mut self,
        _bytes: &[u8],
        _card: &Option<PathBuf>,
        _new: &Analysis,
        _base_sha: &str,
        _base_bytes: &Option<Vec<u8>>,
        _report: &mut Value,
        _start: &str,
        _reply: &RpcReply,
    ) -> Result<CoupledLoad, RpcError> {
        Ok(CoupledLoad::Unavailable(
            "this build has no coupled audio".into(),
        ))
    }

    /// Build the runtime from a ready pair, make it the running one and apply
    /// the DSP difference to the base. Leaves the controller untouched on error.
    fn install_patched_audio(&mut self, install: PatchInstall) -> Result<Value, RpcError> {
        let rejected = |m: String| RpcError::new(code::FIRMWARE_REJECTED, m);
        let PatchInstall {
            bytes,
            card,
            new,
            base_sha,
            base_bytes,
            audio,
            mut report,
            dsp_in_boot,
        } = install;
        let (card_obj, error) = card_for(card.clone()).map_err(&rejected)?;
        let runtime =
            Runtime::new_patched_audio(&bytes, card_obj, &base_sha, &audio).map_err(&rejected)?;
        report["audio"] = runtime.has_audio().into();
        let base_analysis = match &base_bytes {
            Some(base) => Some(Analysis::of(base).map_err(&rejected)?),
            None => None,
        };
        self.loaded = Some(Loaded {
            base: base_bytes,
            base_sha,
            current: bytes,
            card,
            base_analysis: base_analysis.clone(),
            current_analysis: Some(new.clone()),
        });
        self.install(runtime, error);
        self.session = self.session.max(1);
        self.emit("session.changed", json!({"reason": "firmware.load"}));
        report["dsp"] = match (base_analysis.and_then(|b| b.dsp), new.dsp) {
            (Some(base), Some(new_dsp)) if base != new_dsp && dsp_in_boot => {
                json!({"applied": true, "at": "cold boot", "reason": "the boot ran with the DSP patch"})
            }
            (Some(base), Some(new_dsp)) if base != new_dsp => {
                let diff = firmware::dsp_diff(&base, &new_dsp).map_err(&rejected)?;
                self.inject_diff(diff)?
            }
            (Some(_), Some(_)) => {
                json!({"applied": false, "reason": "the DSP section is the base's"})
            }
            _ => json!({
                "applied": false,
                "reason": "the base .syx is not known, so the DSP difference cannot be computed",
            }),
        };
        Ok(report)
    }

    /// Install the finished coupled cold boot, or report why not.
    fn poll_patch(&mut self) {
        let Some(job) = self.patch_job.as_ref() else {
            return;
        };
        let outcome = match job.done.try_recv() {
            Ok(outcome) => outcome,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err("boot worker stopped".into()),
        };
        let job = self.patch_job.take().expect("patch job");
        // The guest reset the machine before it was ready (a module that resets in `init`, the firmware's exception path): boot
        // again on the card as the guest left it. A boot loop is bounded.
        if matches!(&outcome, Err(e) if e == "soft reset") {
            let overlay = job.reset_out.lock().expect("reset overlay").take();
            self.reset_ram = job.keep_out.lock().expect("kept ram").take();
            self.boot_resets += 1;
            if let (Some(overlay), true) = (overlay, self.boot_resets <= 8) {
                self.emit("cpu.reset", json!({"during_boot": true, "count": self.boot_resets}));
                let PatchInstall { bytes, card, new, base_sha, base_bytes, report, .. } = job.install;
                self.reset_overlay = Some(overlay);
                let mut report = report;
                let reply = job.reply.clone();
                match self.load_patched_coupled(&bytes, &card, &new, &base_sha, &base_bytes, &mut report, "cold", &reply) {
                    Ok(CoupledLoad::Started) => return,
                    _ => {
                        let _ = reply.send(Err(RpcError::new(code::GUEST_FAULT, "boot loop: the restart failed")));
                        return;
                    }
                }
            }
        }
        let seconds = job.started.elapsed().as_secs_f64();
        let result = match outcome {
            Err(e) => Err(RpcError::new(code::GUEST_FAULT, e)),
            Ok(booted) => {
                self.boot_resets = 0;
                let mut install = job.install;
                install.report["boot"] = json!({
                    "icount": booted.icount, "frames": booted.frames,
                    "wall_seconds": booted.wall_seconds,
                    "dsp_start_seconds": booted.dsp_start_seconds,
                });
                self.install_patched_audio(install)
            }
        };
        let installed = result.is_ok();
        self.emit(
            "firmware.loaded",
            json!({"ok": result.is_ok(), "seconds": seconds,
                   "error": result.as_ref().err().map(|e| e.message.clone())}),
        );
        let _ = job.reply.send(result.map(|mut report| {
            report["seconds"] = seconds.into();
            self.envelope(report)
        }));
        // The old runtime kept stepping while the boot ran on its worker. A step or run-until job in flight has its target counted
        // on that runtime (`icount + n`): left alone, it would run the new runtime up to that count, a number of instructions that
        // depends on how far the old runtime had got when the boot finished (host timing). End it at the install instead, so every
        // run starts the new runtime at the same instruction.
        if installed
            && matches!(
                self.job.as_ref().map(|j| &j.kind),
                Some(JobKind::Steps { .. } | JobKind::Until(_))
            )
        {
            self.finish_job(Ok(json!({"runtime_replaced": true})));
        }
    }
}

// ----- cards: a folder of WAV files to a running DT2 -------------------------

/// What a finished card job hands to the controller.
struct CardReady {
    paths: CardPaths,
    samples: usize,
    /// The ready snapshot and DSP state, when the app has coupled audio.
    ready: Option<(PathBuf, PathBuf)>,
    card_cached: bool,
    ready_cached: bool,
    build_seconds: f64,
    boot_seconds: f64,
}

/// Where the loaded card came from and whether a quit writes the firmware's changes back to it.
struct CardFolder {
    image: PathBuf,
    dir: PathBuf,
    sync: bool,
    /// Where exports and backups go (`card.load {export_out}`); default `$DIGI_PLUSDRIVE_OUT` or `out/plusdrive`.
    out: Option<PathBuf>,
}

struct CardJob {
    status: Arc<Mutex<Value>>,
    done: mpsc::Receiver<Result<CardReady, String>>,
    cancel: Arc<AtomicBool>,
    reply: Option<RpcReply>,
    started: Instant,
    syx: Vec<u8>,
    /// What the UI calls the card (the chosen folder's name).
    name: String,
    /// The folder the card is built from, and `sync`: write the firmware's changes back to it on quit.
    dir: PathBuf,
    sync: bool,
    export_out: Option<PathBuf>,
}

/// A path as shown in a log line: relative to the working directory when it is below it.
fn display_path(path: &Path) -> String {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| path.strip_prefix(cwd).ok().map(|p| p.display().to_string()))
        .unwrap_or_else(|| path.display().to_string())
}

fn set_status(status: &Mutex<Value>, value: Value) {
    if let Ok(mut slot) = status.lock() {
        *slot = value;
    }
}

/// Build the card of `dir` and its ready state; runs on a worker thread.
fn run_card_job(
    syx: Vec<u8>,
    dir: PathBuf,
    run_dir: PathBuf,
    audio: Option<AudioOptions>,
    status: &Mutex<Value>,
    cancel: &AtomicBool,
) -> Result<CardReady, String> {
    let started = Instant::now();
    set_status(
        status,
        json!({"state": "working", "phase": "scanning", "message": "Reading the +Drive folder", "fraction": 0.0}),
    );
    let files = cards::list_files(&dir)?;
    let (paths, card_cached, total) = cards::ensure_card("dt2", &run_dir, &syx, &dir, &files, |done, total| {
        set_status(
            status,
            json!({
                "state": "working", "phase": "building",
                "message": format!("Building the card ({done}/{total} files)"),
                "fraction": 0.1 * done as f64 / total.max(1) as f64,
                "done": done, "total": total,
            }),
        );
    })?;
    let build_seconds = started.elapsed().as_secs_f64();
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    let ready_cached = paths.ready_cached();
    #[allow(unused_mut)] // assigned only with the coupled-audio feature
    let mut boot_seconds = 0.0;
    let mut ready = None;
    // Zero DSP has no DSP image and no card ready pair: the runtime restores
    // the base snapshot and attaches the card on the ColdFire side only.
    if let Some(audio) = audio.as_ref().filter(|a| !a.zero) {
        if ready_cached {
            set_status(
                status,
                json!({"state": "working", "phase": "restoring", "message": "Restoring the cached ready state", "fraction": 0.95}),
            );
        } else {
            #[cfg(feature = "coupled-audio")]
            {
                let image = std::fs::canonicalize(&audio.image)
                    .map_err(|e| format!("DSP image {}: {e}", audio.image.display()))?;
                let cold_idle = image
                    .parent()
                    .map(|parent| parent.join("cold-idle.dsp"))
                    .filter(|path| path.is_file())
                    .ok_or("cold-idle.dsp not found next to the DSP image (run `mise run emu:ready -- dt2` once)")?;
                let card = elektron_native_boot::card_file::card_from_image(&paths.image)?;
                let boot_started = Instant::now();
                digi_coupled::cold::boot_to_ready(
                    &syx,
                    card,
                    &digi_coupled::cold::ColdInputs {
                        image,
                        cold_idle,
                        cold_state: None,
                        dsp_patch: None,
                        relax_fs_check: false,
                        reset_out: None,
                        keep_ram: None,
                    },
                    &paths.ready,
                    cancel,
                    |progress| {
                        set_status(
                            status,
                            json!({
                                "state": "working", "phase": "booting",
                                "message": format!(
                                    "Booting the Digitakt II with the card ({:.0}%)",
                                    100.0 * (0.1 + 0.85 * progress.fraction)
                                ),
                                "fraction": 0.1 + 0.85 * progress.fraction,
                                "icount": progress.icount, "wall_seconds": progress.wall_seconds,
                            }),
                        );
                    },
                )?;
                boot_seconds = boot_started.elapsed().as_secs_f64();
            }
            #[cfg(not(feature = "coupled-audio"))]
            {
                let _ = audio;
                return Err("this build has no coupled audio to boot the card with".into());
            }
        }
        ready = Some((paths.snapshot(), paths.dsp_state()));
    }
    Ok(CardReady {
        paths,
        samples: total,
        ready,
        card_cached,
        ready_cached,
        build_seconds,
        boot_seconds,
    })
}

impl Actor {
    fn card_status(&self) -> Value {
        match &self.card_job {
            Some(job) => {
                let mut status = job.status.lock().map(|s| s.clone()).unwrap_or(Value::Null);
                status["wall_seconds"] = job.started.elapsed().as_secs_f64().into();
                status
            }
            None => self.card_last.clone(),
        }
    }

    /// Stop a running card job; whether there was one.
    fn cancel_card(&mut self) -> bool {
        let Some(job) = self.card_job.take() else {
            return false;
        };
        job.cancel.store(true, Ordering::Relaxed);
        if let Some(reply) = job.reply {
            let _ = reply.send(Err(RpcError::new(code::BUSY, "card load cancelled")));
        }
        self.card_last = json!({"state": "idle", "message": "Card load cancelled"});
        true
    }

    fn rpc_card_load(&mut self, p: &Params, reply: &RpcReply) -> Result<Option<Value>, RpcError> {
        let dir = PathBuf::from(p.str("dir")?);
        let name = p
            .opt_str("name")?
            .filter(|n| !n.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                dir.file_name()
                    .map_or_else(|| dir.display().to_string(), |n| n.to_string_lossy().into())
            });
        let wait = p.opt_bool("wait")?.unwrap_or(true);
        let auto_no = p.opt_bool("auto_no")?.unwrap_or(true);
        let sync = p.opt_bool("sync")?.unwrap_or(false);
        let export_out = p.opt_str("export_out")?.map(PathBuf::from);
        if self.job.is_some() || self.card_job.is_some() || self.patch_job.is_some() {
            return Err(RpcError::new(code::BUSY, "another job is running"));
        }
        let loaded = self.loaded.as_ref().ok_or_else(RpcError::no_firmware)?;
        let syx = loaded
            .base
            .clone()
            .unwrap_or_else(|| loaded.current.clone());
        plusdrive_format::Dt2Seed::from_syx(&syx).map_err(|e| {
            RpcError::new(
                code::FIRMWARE_REJECTED,
                format!("not a DT2 1.16 firmware: {e}"),
            )
        })?;
        let status = Arc::new(Mutex::new(
            json!({"state": "working", "phase": "scanning", "fraction": 0.0}),
        ));
        let cancel = Arc::new(AtomicBool::new(false));
        let (done_tx, done) = mpsc::channel();
        let (worker_status, worker_cancel) = (status.clone(), cancel.clone());
        let (worker_syx, run_dir, audio) = (syx.clone(), self.run_dir.clone(), self.audio.clone());
        let source_dir = dir.clone();
        thread::Builder::new()
            .name("digi-card".into())
            .spawn(move || {
                let result = run_card_job(
                    worker_syx,
                    dir,
                    run_dir,
                    audio,
                    &worker_status,
                    &worker_cancel,
                );
                let _ = done_tx.send(result);
            })
            .map_err(|e| RpcError::io(format!("card thread: {e}")))?;
        self.auto_no = auto_no;
        self.card_job = Some(CardJob {
            status,
            done,
            cancel,
            reply: wait.then(|| reply.clone()),
            started: Instant::now(),
            syx,
            name,
            dir: source_dir,
            sync,
            export_out,
        });
        self.emit("card.started", json!({}));
        if wait {
            Ok(None)
        } else {
            Ok(Some(json!({"started": true})))
        }
    }

    /// Install the finished card's runtime, or report why not.
    fn poll_card(&mut self) {
        let Some(job) = self.card_job.as_ref() else {
            return;
        };
        let outcome = match job.done.try_recv() {
            Ok(outcome) => outcome,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err("card worker stopped".into()),
        };
        let job = self.card_job.take().expect("card job");
        let seconds = job.started.elapsed().as_secs_f64();
        let result =
            outcome.and_then(|ready| self.install_card(&job.syx, &job.name, ready, seconds));
        match &result {
            Ok(report) => {
                self.auto_card = None;
                self.card_folder = report["card"].as_str().map(|image| CardFolder {
                    image: PathBuf::from(image),
                    dir: job.dir.clone(),
                    sync: job.sync,
                    out: job.export_out.clone(),
                });
                self.card_last = report.clone();
                self.emit("card.ready", report.clone());
            }
            Err(error) => {
                self.auto_no = false;
                self.card_last = json!({"state": "error", "message": error, "fraction": 0.0});
                self.emit("card.error", json!({"message": error}));
            }
        }
        if let Some(reply) = job.reply {
            let _ = reply.send(match result {
                Ok(report) => Ok(self.envelope(report)),
                Err(error) => Err(RpcError::io(error)),
            });
        }
    }

    fn install_card(
        &mut self,
        syx: &[u8],
        name: &str,
        ready: CardReady,
        seconds: f64,
    ) -> Result<Value, String> {
        let mut audio = self.audio.clone();
        if let (Some(audio), Some((snapshot, dsp))) = (audio.as_mut(), ready.ready.as_ref()) {
            audio.snapshot = snapshot.clone();
            audio.state = dsp.clone();
        }
        let (runtime, error) = runtime_from(syx, Some(ready.paths.image.clone()), audio.as_ref())?;
        let has_audio = runtime.has_audio();
        if let Some(loaded) = self.loaded.as_mut() {
            loaded.card = Some(ready.paths.image.clone());
        }
        self.audio = audio;
        self.install(runtime, error);
        // The webview's pump never sees the restore; hand it the first frame.
        if let Some(runtime) = self.runtime.as_mut() {
            let first = runtime.snapshot();
            self.held_frame = first.frame;
            self.held_leds = first.leds.map(|leds| (leds, first.rgb));
        }
        self.emit("session.changed", json!({"reason": "card"}));
        if self.remember_card {
            let last = cards::LastCard {
                key: ready.paths.key.clone(),
                name: name.to_string(),
                samples: ready.samples,
                firmware_sha256: cards::sha256_hex(syx),
            };
            // Not fatal: the card works, it just is not mounted next time.
            if let Err(error) = cards::write_last(&self.run_dir, &last) {
                eprintln!("card: could not remember the card: {error}");
            }
        }
        Ok(json!({
            "state": "ready", "phase": "ready", "message": "Card ready", "fraction": 1.0,
            "key": ready.paths.key, "name": name, "card": ready.paths.image.display().to_string(),
            "samples": ready.samples, "audio": has_audio,
            "card_cached": ready.card_cached, "ready_cached": ready.ready_cached,
            "build_seconds": ready.build_seconds, "boot_seconds": ready.boot_seconds,
            "seconds": seconds,
        }))
    }
}

/// `newer` laid over the writes of an older `overlay.bin` (byte by byte, by mask).
fn merge_overlay(
    older: Option<Vec<u8>>,
    newer: Vec<emmc_card::CardOverlaySector>,
) -> Vec<emmc_card::CardOverlaySector> {
    let mut sectors: std::collections::BTreeMap<u64, emmc_card::CardOverlaySector> = older
        .and_then(|bytes| plusdrive_format::parse_overlay(&bytes).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|s| (s.sector, s))
        .collect();
    for new in newer {
        match sectors.get_mut(&new.sector) {
            Some(old) => {
                for i in 0..512 {
                    if new.written[i / 8] & (1 << (i % 8)) != 0 {
                        old.written[i / 8] |= 1 << (i % 8);
                        old.data[i] = new.data[i];
                    }
                }
            }
            None => {
                sectors.insert(new.sector, new);
            }
        }
    }
    sectors.into_values().collect()
}

/// Save a ready runtime's ColdFire snapshot; the path on success.
fn save_cache(runtime: &mut Runtime, path: &Path) -> Result<Option<String>, RpcError> {
    let data = runtime
        .save_state()
        .map_err(|e| RpcError::new(code::GUEST_FAULT, format!("snapshot: {e}")))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| RpcError::io(format!("{parent:?}: {e}")))?;
    }
    let partial = path.with_extension("snap.part");
    std::fs::write(&partial, data).map_err(|e| RpcError::io(format!("{partial:?}: {e}")))?;
    std::fs::rename(&partial, path).map_err(|e| RpcError::io(format!("{path:?}: {e}")))?;
    Ok(Some(path.display().to_string()))
}

/// One poll of the CPU meters (`status.load`): null until the first window has closed.
fn poll_load(
    meter: &mut load::Meter,
    status: &elektron_native_boot::Status,
    audio: &Value,
) -> Value {
    let Some(factors) = load::Factors::for_device(&status.device) else {
        return Value::Null;
    };
    let tiers = &audio["dsp_tiers"];
    let counters = load::Counters {
        cf_busy: status.interpreted_instructions,
        cf_total: status.interpreted_instructions
            + status.idle_fast_forwarded_instructions
            + status.ram_clear_fast_forwarded_instructions,
        dsp: tiers["audio_s"].as_f64().map(|audio_s| load::DspCounters {
            audio_s,
            instructions: tiers["instructions"].as_u64().unwrap_or(0),
            idle_skip: tiers["idle_skip"].as_u64().unwrap_or(0),
        }),
    };
    meter
        .poll(&factors, counters)
        .map_or(Value::Null, |l| l.to_json())
}

/// The few audio fields a status poll wants (never syncs the DSP).
fn summarize_audio(runtime: &mut Runtime) -> Value {
    let Some(report) = runtime.audio_status() else {
        return Value::Null;
    };
    json!({
        "frames": report["frames"], "missing_blocks": report["missing_blocks"],
        "halted": report["halted"], "sink_connected": report["sink_connected"],
        "source_audio_seconds": report["source_audio_seconds"],
        "dsp_tiers": report["dsp_tiers"], "dsp_core": report["dsp_core"],
    })
}

#[cfg(test)]
mod pace_tests {
    use super::{CF_IPS, pace_ahead};

    #[test]
    fn pacing_waits_when_ahead_steps_when_on_time_and_restarts_when_lagging() {
        let seconds = |s: f64| Some((s * CF_IPS) as u64);
        assert_eq!(pace_ahead(1.0, seconds(1.5)), Some(true));
        assert_eq!(pace_ahead(1.0, seconds(1.0)), Some(false));
        assert_eq!(pace_ahead(1.0, seconds(0.97)), Some(false));
        assert_eq!(pace_ahead(1.0, seconds(0.5)), None);
        assert_eq!(pace_ahead(1.0, None), None);
    }
}

#[cfg(all(test, feature = "coupled-audio"))]
mod tests {
    use super::patched_pair_key;

    #[test]
    fn the_ready_pair_key_follows_main_dsp_inputs_and_card() {
        let dir = std::env::temp_dir().join(format!("digi-pair-key-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (image, idle, card) = (dir.join("image"), dir.join("idle"), dir.join("card"));
        std::fs::write(&image, b"image one").unwrap();
        std::fs::write(&idle, b"idle one").unwrap();
        std::fs::write(&card, vec![0u8; 512]).unwrap();
        let main = "ab".repeat(32);
        let key = |main: &str, card: Option<&std::path::Path>| {
            patched_pair_key(main, &image, &idle, card, b"").unwrap()
        };
        let base = key(&main, None);
        assert_eq!(base, key(&main, None), "stable");
        assert!(base.starts_with("abababababababab-"));
        assert_ne!(base, key(&"cd".repeat(32), None), "another MAIN");
        assert_ne!(base, key(&main, Some(&card)), "a card");
        assert_ne!(
            base,
            patched_pair_key(&main, &image, &idle, None, b"keep-ram:x").unwrap(),
            "another cold boot input"
        );
        std::fs::write(&idle, b"idle two").unwrap();
        assert_ne!(base, key(&main, None), "another DSP cold state");
        std::fs::write(&idle, b"idle one").unwrap();
        std::fs::write(&image, b"image two").unwrap();
        assert_ne!(base, key(&main, None), "another DSP image");
        let raw = dir.join("cold.state");
        std::fs::write(&raw, b"raw reset one").unwrap();
        let changed = patched_pair_key(&main, &image, &raw, None, b"dsp-raw-init-v1:").unwrap();
        std::fs::write(&raw, b"raw reset two").unwrap();
        assert_ne!(changed, patched_pair_key(&main, &image, &raw, None, b"dsp-raw-init-v1:").unwrap(), "changed DSP follows raw reset bytes");
        std::fs::remove_file(&idle).unwrap();
        assert!(patched_pair_key(&main, &image, &raw, None, b"dsp-raw-init-v1:").is_ok(), "changed DSP does not use initialized idle state");
        std::fs::write(&idle, b"idle one").unwrap();
        std::fs::remove_file(&raw).unwrap();
        assert!(key(&main, None).starts_with("abababababababab-"), "unchanged DSP does not require raw reset state");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod reset_tests {
    use super::*;

    #[test]
    #[ignore = "requires DT2 1.17 and DN2 1.12 firmware"]
    fn cf_reset_restores_card_and_kept_ram_before_boot_without_using_the_main_cache() {
        // These controller transitions do not execute guest instructions.
        let (_commands, rx) = mpsc::channel();
        let mut controller = Actor::new(rx, Config::new(None));
        let (reply, result) = mpsc::channel();
        controller.start_job(JobKind::Steps { end: 100 }, &reply, None, json!({}), "step").unwrap();
        controller.reset_pending = true;
        controller.handle_reset();
        assert_eq!(result.try_recv().unwrap().unwrap()["runtime_replaced"], true);
        assert!(controller.job.is_none() && controller.reset_pending);
        controller.start_job(JobKind::Boot { cache: None, limit: BOOT_LIMIT }, &reply,
            None, json!({}), "firmware.load").unwrap();
        controller.boot_resets = 8;
        controller.mode = Mode::Free;
        controller.handle_reset();
        assert_eq!(controller.boot_resets, 9);
        assert!(controller.mode == Mode::Paused && controller.job.is_none() && !controller.reset_pending);
        assert_eq!(result.try_recv().unwrap().unwrap_err().code, code::GUEST_FAULT);
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(3).unwrap();
        for (env, file) in [
            ("DT2_SYX", "Digitakt_II_OS1.17.syx"),
            ("DN2_SYX", "Digitone_II_OS1.12.syx"),
        ] {
            let path = std::env::var_os(env).map(PathBuf::from)
                .unwrap_or_else(|| root.join("firmware").join(file));
            assert!(path.is_file(), "required firmware missing: {}", path.display());
            let bytes = std::fs::read(path).unwrap();
            let new = Analysis::of(&bytes).unwrap();
            let base_sha = firmware::sha256_hex(&bytes);
            let registry = plusdrive_format::device_profile::Registry::load().unwrap();
            let (device, _) = registry.identify(&base_sha).unwrap();
            let (base, size) = device.keep_ram.unwrap();
            let ram: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
            let mut sector = emmc_card::CardOverlaySector {
                sector: 17, written: [0; 64], data: [0; 512],
            };
            sector.written[0] = 3;
            sector.data[0] = 0xa5; // Includes an explicitly written zero at byte 1.
            let overlay = vec![sector];
            let dir = std::env::temp_dir().join(format!("digi-cf-reset-{}-{env}", std::process::id()));
            let mut config = Config::new(None);
            config.run_dir = dir.clone();
            let (_tx, rx) = mpsc::channel();
            let mut actor = Actor::new(rx, config);
            let cache = dir.join("patched").join(format!("{}.snap", new.main_sha256().unwrap()));
            std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
            std::fs::write(&cache, b"initial cache must remain untouched").unwrap();
            actor.reset_overlay = Some(overlay.clone());
            actor.reset_ram = Some(ram.clone());
            let (reply, result) = mpsc::channel();
            let mut report = json!({});
            actor.reset_ram = Some(vec![0; 3]);
            assert!(actor.load_patched(bytes.clone(), None, new.clone(), base_sha.clone(),
                Some(bytes.clone()), &mut report, "cold", &reply).is_err());
            assert_eq!(actor.reset_overlay.as_ref(), Some(&overlay), "failed construction keeps captured card state");
            assert_eq!(actor.reset_ram.as_deref(), Some(&[0; 3][..]), "failed construction keeps captured RAM");
            actor.reset_ram = Some(ram.clone());
            assert!(actor.load_patched(bytes.clone(), None, new.clone(), base_sha.clone(),
                Some(bytes.clone()), &mut report, "auto", &reply).unwrap());
            let runtime = actor.runtime.as_mut().unwrap();
            assert_eq!(runtime.elapsed_instructions(), 0, "retained state precedes boot");
            assert_eq!(runtime.peek_ram(base, size), ram);
            assert_eq!(runtime.card_overlay(), overlay, "blank-card writes survive reset");
            assert!(report.get("cache_note").is_none(), "reset must not read the cache");
            assert!(matches!(actor.job.as_ref().unwrap().kind, JobKind::Boot { cache: None, .. }),
                "reset must not write the cache when ready");
            assert!(actor.reset_overlay.is_none() && actor.reset_ram.is_none());
            assert_eq!(std::fs::read(&cache).unwrap(), b"initial cache must remain untouched");
            actor.job = None;
            actor.mode = Mode::Paused;
            actor.soft_reset();
            assert!(actor.mode == Mode::Free, "guest reset resumes free running after the boot job");
            let runtime = actor.runtime.as_mut().unwrap();
            assert_eq!(runtime.elapsed_instructions(), 0);
            assert_eq!(runtime.peek_ram(base, size), ram);
            assert_eq!(runtime.card_overlay(), overlay);
            assert!(matches!(actor.job.as_ref().unwrap().kind, JobKind::Boot { cache: None, .. }));
            actor.boot_resets = 7;
            actor.reset_pending = true;
            actor.handle_reset();
            assert_eq!(actor.boot_resets, 8, "the eighth reset still starts a boot");
            assert!(actor.job.is_some() && !actor.reset_pending);
            assert_eq!(actor.runtime.as_ref().unwrap().elapsed_instructions(), 0);
            actor.job.as_mut().unwrap().reply = reply.clone();
            actor.loaded.as_mut().unwrap().card = Some(dir.join("missing-card.img"));
            actor.soft_reset();
            assert!(result.try_recv().unwrap().is_err(), "terminal reset failure replies to the original boot caller");
            assert!(actor.reset_overlay.is_none() && actor.reset_ram.is_none(), "terminal reset state cannot leak into a new load");
            actor.job = None;
            actor.load_patched(bytes, None, new, base_sha, None, &mut json!({}), "cold", &reply).unwrap();
            assert!(matches!(&actor.job.as_ref().unwrap().kind, JobKind::Boot { cache: Some(path), .. } if path == &cache),
                "ordinary initial cold boots still save the MAIN cache");
            assert_eq!(actor.boot_resets, 0, "independent loads start a fresh reset sequence");
            let runtime = actor.runtime.as_mut().unwrap();
            assert!(runtime.card_overlay().is_empty(), "fresh default card has none of the failed reset's writes");
            assert_ne!(runtime.peek_ram(base, size), ram, "failed reset RAM is not reused");
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}

#[cfg(test)]
mod mem_write_tests {
    use super::decode_hex;

    #[test]
    fn hex_decodes_bytes_and_refuses_bad_text() {
        assert_eq!(decode_hex("").unwrap(), Vec::<u8>::new());
        assert_eq!(decode_hex("00ff7A10").unwrap(), [0, 0xff, 0x7a, 0x10]);
        assert!(decode_hex("abc").is_err(), "odd length");
        assert!(decode_hex("zz").is_err(), "not hex");
        assert!(decode_hex("é1").is_err(), "not ASCII");
    }
}

#[cfg(test)]
mod ddr_monitor_tests {
    use super::*;

    #[test]
    fn ddr_monitor_bad_params_are_rejected_before_runtime_access() {
        let (_commands, rx) = mpsc::channel();
        let mut actor = Actor::new(rx, Config::new(None));
        for params in [
            json!({"action":"unknown"}),
            json!({"action":3}),
            json!({"action":"arm"}),
            json!({"action":"arm","start":0x80000000u64,"end":0x100000000u64}),
            json!({"action":"arm","start":0x7fffffffu64,"end":0x80000004u64}),
            json!({"action":"arm","start":0x80000000u64,"end":0x80000000u64}),
            json!({"action":"arm","start":0x80000000u64,"end":0xa0000001u64}),
        ] {
            assert_eq!(
                actor
                    .rpc_dsp_ddr_monitor(&Params(&params))
                    .unwrap_err()
                    .code,
                code::BAD_PARAMS
            );
        }
        // Omitted action is status, and valid commands reach the ordinary
        // no-firmware error rather than being rejected as malformed.
        for params in [
            json!({}),
            json!({"action":"clear"}),
            json!({"action":"disarm"}),
            json!({"action":"arm","start":0x80000000u64,"end":0xa0000000u64}),
        ] {
            assert_ne!(
                actor
                    .rpc_dsp_ddr_monitor(&Params(&params))
                    .unwrap_err()
                    .code,
                code::BAD_PARAMS
            );
        }
    }
}
