//! CLI for the live audio path (device open -> ring -> producer ->
//! callback), the same path `tools/live_audio.py` drives over the C ABI.
//!
//!     live_play [FILE]                   a WAV file, or a 440 Hz tone
//!     live_play --capture CAPTURE [...]  the native SHARC core, live
//!     live_play --pack PACK [...]        the same from a built live pack
//!
//! SHARC options:
//!   --lp0 LOG          FlexBus log fed over LP0 before the first frame
//!                      (default: the one flexbus*.raw next to CAPTURE)
//!   --image NAME       firmware image (default dt2-1.16)
//!   --start-frame N    first capture frame (default 74, as the listen)
//!   --lib PATH         native core library (default $SHARC_NATIVE_LIB, else
//!                      out/native/opt/target-final/release/libsharc_native.dylib)
//!   --seconds S        play time (default 10)
//!   --gap N            held frames (trigs cleared) before the capture loops
//!                      (default 0)
//!   --hold             hold the last frame for ever instead of looping
//!   --latency N        target latency in frames (ring = 4x; default 512)
//!   --gain G           output gain (default 1.0)
//!   --no-prefill       start the device before the ring is full
//!   --bench N          no device: render N frames as fast as possible
//!   --dump PATH        with --bench: write each frame's voice outputs
//!                      (voice 0 then voice 1, 32 f64 LE each)
//!   --times PATH       with --bench: each frame's render time, ns, one per line
//!
//! `--capture` builds (or finds) the pack with
//! `tools/sharc_transpile_run.py live-pack` (Python, cached under
//! out/native/live/ by image, capture, LP0 log and core hashes); nothing in
//! the audio path runs Python.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use live_audio::LivePlayer;
use live_audio::sharc_lib::capture_source;
use live_audio::sharc_source::{AfterEnd, RenderLog};

const TARGET_LATENCY_FRAMES: u32 = 512;
const TONE_SECONDS: f32 = 3.0;
/// The SHARC frame budget at 48 kHz: 32 samples.
const FRAME_BUDGET_US: f64 = 32.0 / 48_000.0 * 1e6;

/// This thread's CPU time, ns (0 where unsupported).
fn thread_cpu_ns() -> u64 {
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn clock_gettime_nsec_np(clock_id: u32) -> u64;
        }
        const CLOCK_THREAD_CPUTIME_ID: u32 = 16;
        // SAFETY: a plain libSystem call.
        unsafe { clock_gettime_nsec_np(CLOCK_THREAD_CPUTIME_ID) }
    }
    #[cfg(not(target_os = "macos"))]
    {
        0
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[derive(Default)]
struct Args {
    file: Option<String>,
    capture: Option<String>,
    pack: Option<String>,
    lp0: Option<String>,
    image: Option<String>,
    start_frame: Option<u32>,
    lib: Option<String>,
    seconds: Option<f64>,
    gap: usize,
    hold: bool,
    latency: Option<u32>,
    gain: Option<f32>,
    no_prefill: bool,
    bench: Option<usize>,
    dump: Option<String>,
    times: Option<String>,
}

fn die(msg: &str) -> ! {
    eprintln!("live_play: {msg}");
    std::process::exit(2);
}

fn parse_args() -> Args {
    let mut a = Args::default();
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut val = |name: &str| {
            it.next()
                .unwrap_or_else(|| die(&format!("{name} needs a value")))
        };
        let num = |s: String, name: &str| -> f64 {
            s.parse()
                .unwrap_or_else(|_| die(&format!("{name}: not a number: {s}")))
        };
        match arg.as_str() {
            "--capture" => a.capture = Some(val("--capture")),
            "--pack" => a.pack = Some(val("--pack")),
            "--lp0" => a.lp0 = Some(val("--lp0")),
            "--image" => a.image = Some(val("--image")),
            "--start-frame" => a.start_frame = Some(num(val(&arg), &arg) as u32),
            "--lib" => a.lib = Some(val("--lib")),
            "--seconds" => a.seconds = Some(num(val(&arg), &arg)),
            "--gap" => a.gap = num(val(&arg), &arg) as usize,
            "--hold" => a.hold = true,
            "--latency" => a.latency = Some(num(val(&arg), &arg) as u32),
            "--gain" => a.gain = Some(num(val(&arg), &arg) as f32),
            "--no-prefill" => a.no_prefill = true,
            "--bench" => a.bench = Some(num(val(&arg), &arg) as usize),
            "--dump" => a.dump = Some(val("--dump")),
            "--times" => a.times = Some(val("--times")),
            "-h" | "--help" => {
                println!("see the module docs in native/live/src/bin/live_play.rs");
                std::process::exit(0);
            }
            s if s.starts_with("--") => die(&format!("unknown option {s}")),
            _ => a.file = Some(arg),
        }
    }
    a
}

/// The one flexbus*.raw next to CAPTURE, if there is exactly one.
fn default_lp0(capture: &Path) -> Option<PathBuf> {
    let dir = capture.parent()?;
    let found: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            n.starts_with("flexbus") && n.ends_with(".raw")
        })
        .collect();
    (found.len() == 1).then(|| found[0].clone())
}

/// `sharc_transpile_run.py live-pack`: the cached pack's path.
fn resolve_pack(a: &Args, capture: &str) -> PathBuf {
    let root = repo_root();
    let venv = root.join(".venv/bin/python");
    let mut cmd = if venv.exists() {
        Command::new(venv)
    } else {
        let mut c = Command::new("uv");
        c.args(["run", "python"]);
        c
    };
    cmd.arg(root.join("tools/sharc_transpile_run.py"))
        .arg("live-pack")
        .arg(a.image.as_deref().unwrap_or("dt2-1.16"))
        .arg(capture)
        .arg("--start-frame")
        .arg(a.start_frame.unwrap_or(74).to_string());
    let lp0 = a
        .lp0
        .clone()
        .map(PathBuf::from)
        .or_else(|| default_lp0(Path::new(capture)));
    match &lp0 {
        Some(p) => {
            println!("lp0: {}", p.display());
            cmd.arg("--lp0").arg(p);
        }
        None => println!("lp0: none (no --lp0 and no single flexbus*.raw next to the capture)"),
    }
    println!("pack: resolving (builds the start state in Python on the first run, ~2 min)");
    let t = Instant::now();
    let out = cmd
        .output()
        .unwrap_or_else(|e| die(&format!("running live-pack: {e}")));
    if !out.status.success() {
        let _ = std::io::stderr().write_all(&out.stderr);
        die("live-pack failed");
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().last().unwrap_or("");
    let path = line
        .split("\"path\": \"")
        .nth(1)
        .and_then(|r| r.split('"').next())
        .unwrap_or_else(|| die(&format!("live-pack printed no path: {line}")));
    println!("pack: {path} ({line}; {:.1} s)", t.elapsed().as_secs_f64());
    PathBuf::from(path)
}

fn default_lib() -> PathBuf {
    std::env::var_os("SHARC_NATIVE_LIB")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            repo_root().join("out/native/opt/target-final/release/libsharc_native.dylib")
        })
}

fn print_render(log: &RenderLog) {
    // Frame 0 carries the cold start (page faults, first use of each
    // block); it is reported on its own.
    let s = log.summary(1);
    let first = log.frame_ns.first().copied().unwrap_or(0) as f64 / 1000.0;
    println!(
        "render: frames {} clean {} stopped {} dma_failures {}",
        log.frames, log.clean, log.stopped, log.dma_failures
    );
    println!(
        "render_us: median {:.1} p99 {:.1} max {:.1} mean {:.1} first {:.1} (budget {:.0}: headroom {:.2}x median, {:.2}x max)",
        s.median_us,
        s.p99_us,
        s.max_us,
        s.mean_us,
        first,
        FRAME_BUDGET_US,
        FRAME_BUDGET_US / s.median_us.max(1e-9),
        FRAME_BUDGET_US / s.max_us.max(1e-9),
    );
    let per_frame = log.instructions as f64 / log.frames.max(1) as f64;
    println!(
        "interpreter: {} instructions ({:.4}% of {}) in {} frames, max {} per frame, those frames took {:.1} us on average",
        log.single_steps,
        100.0 * log.single_steps as f64 / log.instructions.max(1) as f64,
        log.instructions,
        log.frames_with_single_steps,
        log.max_single_steps,
        log.single_step_frame_ns as f64 / 1000.0 / log.frames_with_single_steps.max(1) as f64,
    );
    println!("instructions_per_frame: {per_frame:.0}");
    if let Some((k, t)) = &log.first_stop {
        println!("first_stop: render frame {k}: {t}");
    }
}

fn run_sharc(a: &Args) {
    let pack = match (&a.pack, &a.capture) {
        (Some(p), _) => PathBuf::from(p),
        (None, Some(c)) => resolve_pack(a, c),
        _ => unreachable!(),
    };
    let lib = a.lib.clone().map(PathBuf::from).unwrap_or_else(default_lib);
    let after = if a.hold {
        AfterEnd::Hold
    } else {
        AfterEnd::Loop { gap: a.gap }
    };
    let t = Instant::now();
    let (mut source, info) =
        capture_source(&lib, &pack, after, a.gain.unwrap_or(1.0)).unwrap_or_else(|e| die(&e));
    println!("lib: {} {info}", lib.display());
    println!(
        "load: {:.1} ms (library, image, state import)",
        t.elapsed().as_secs_f64() * 1e3
    );
    let log = source.log();

    if let Some(n) = a.bench {
        if std::env::var_os("LIVE_NO_QOS").is_none() {
            println!(
                "qos: user-interactive {}",
                live_audio::priority::raise_current_thread()
            );
        }
        let mut dump = a.dump.as_ref().map(|p| {
            std::io::BufWriter::new(
                std::fs::File::create(p).unwrap_or_else(|e| die(&format!("{p}: {e}"))),
            )
        });
        let t = Instant::now();
        let mut cpu = Vec::with_capacity(n);
        for _ in 0..n {
            let c0 = thread_cpu_ns();
            source.step();
            cpu.push(thread_cpu_ns() - c0);
            if let Some(w) = &mut dump {
                for v in &source.output().voices {
                    for s in v {
                        w.write_all(&s.to_le_bytes()).expect("dump write");
                    }
                }
            }
        }
        println!("bench: {n} frames in {:.3} s", t.elapsed().as_secs_f64());
        let log = log.lock().expect("render log");
        if let Some(p) = &a.times {
            let text: String = (0..log.frame_ns.len())
                .map(|k| {
                    format!(
                        "{} {} {}\n",
                        log.frame_ns[k], log.frame_instructions[k], log.frame_single_steps[k]
                    )
                })
                .collect();
            std::fs::write(p, text).unwrap_or_else(|e| die(&format!("{p}: {e}")));
        }
        let mut c = cpu.clone();
        c.sort_unstable();
        if c.len() > 1 && c[0] > 0 {
            let q = |x: f64| c[((c.len() - 1) as f64 * x).round() as usize] as f64 / 1000.0;
            println!(
                "thread_cpu_us: median {:.1} p99 {:.1} max {:.1} (CPU time on this thread; wall minus this is time the OS ran something else)",
                q(0.5),
                q(0.99),
                q(1.0)
            );
        }
        print_render(&log);
        return;
    }

    let latency = a.latency.unwrap_or(TARGET_LATENCY_FRAMES);
    let prefill = (!a.no_prefill).then_some(Duration::from_secs(5));
    let mut player = LivePlayer::open_with_source(latency, Box::new(source), prefill, Some(48_000))
        .unwrap_or_else(|e| die(&e));
    player.attach_render_log(log.clone());
    let st = player.stats();
    println!("device: {}", player.device_name());
    println!(
        "sample_rate: {} (requested {}, fallback: {}) channels: {}",
        st.sample_rate, st.requested_sample_rate, st.used_fallback, st.channels
    );
    let seconds = a.seconds.unwrap_or(10.0);
    println!(
        "playing {seconds:.0} s: ring {} frames ({:.1} ms), prefilled {} frames",
        st.ring_capacity_frames,
        st.ring_capacity_frames as f64 / st.sample_rate as f64 * 1e3,
        st.ring_fill_frames,
    );
    let u0 = st.underruns;
    thread::sleep(Duration::from_secs_f64(seconds));
    let st = player.stats();
    println!(
        "played: {:.1} s; underruns {} (callbacks short of samples; {} before the start); samples_rendered {}; max callback {} frames ({:.1} ms); ring fill {}/{}",
        seconds,
        st.underruns - u0,
        u0,
        st.frames_rendered,
        st.max_callback_frames,
        st.max_callback_frames as f64 / st.sample_rate as f64 * 1e3,
        st.ring_fill_frames,
        st.ring_capacity_frames,
    );
    println!(
        "latency: ring {:.1} ms + device buffer {:.1} ms",
        st.ring_capacity_frames as f64 / st.sample_rate as f64 * 1e3,
        st.max_callback_frames as f64 / st.sample_rate as f64 * 1e3,
    );
    if st.sample_rate != 48_000 {
        println!(
            "rate: device {} Hz, the SHARC's 48 kHz resampled (linear)",
            st.sample_rate
        );
    }
    drop(player);
    print_render(&log.lock().expect("render log"));
}

fn main() {
    let a = parse_args();
    if a.capture.is_some() || a.pack.is_some() {
        run_sharc(&a);
        return;
    }

    let player = match LivePlayer::open(TARGET_LATENCY_FRAMES) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("live_play: failed to open output device: {e}");
            std::process::exit(1);
        }
    };

    let stats = player.stats();
    println!("device: {}", player.device_name());
    println!(
        "sample_rate: {} (requested {}, fallback: {})",
        stats.sample_rate, stats.requested_sample_rate, stats.used_fallback
    );
    println!(
        "channels: {} target_latency_frames: {} ring_capacity_frames: {}",
        stats.channels, stats.target_latency_frames, stats.ring_capacity_frames
    );

    let run_seconds = match a.file {
        Some(path) => {
            let path = PathBuf::from(path);
            match player.play_wav(&path, false) {
                Ok((total_samples, wav_rate)) => {
                    println!(
                        "playing {} ({total_samples} samples @ {wav_rate} Hz)",
                        path.display()
                    );
                    (total_samples as f32 / wav_rate as f32) + 0.5 // small tail
                }
                Err(e) => {
                    eprintln!("live_play: failed to load {}: {e}", path.display());
                    std::process::exit(1);
                }
            }
        }
        None => {
            println!("no FILE given: playing a {TONE_SECONDS:.0}s, 440 Hz test tone");
            player.play_tone(440.0, 0.3);
            TONE_SECONDS
        }
    };

    thread::sleep(Duration::from_secs_f32(run_seconds));

    let stats = player.stats();
    println!(
        "frames_rendered: {} underruns: {} ring_fill_frames: {}/{}",
        stats.frames_rendered, stats.underruns, stats.ring_fill_frames, stats.ring_capacity_frames
    );
}
