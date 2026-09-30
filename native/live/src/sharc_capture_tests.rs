//! The real native SHARC core on the drive3 live pack, without a device.
//!
//! Needs firmware-derived files under out/ (never committed), so each test
//! returns early, saying why, when they are missing:
//! - the core library: $SHARC_NATIVE_LIB, else
//!   out/native/opt/target-final/release/libsharc_native.dylib;
//! - the live pack: $LIVE_SHARC_PACK, else the newest capture pack
//!   out/native/live/*.pack (`tools/sharc_transpile_run.py live-pack`; the
//!   frameless state packs, `state-*.pack`, are skipped);
//! - for the bit-exact check, the Python replay's reference for that pack:
//!   out/native/live/ref-<key>-<frames>.json (`... live-ref`).

use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc, mpsc::RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use crate::player::LivePlayer;
use crate::q31::f32_to_q31;
use crate::ring::{FRAME_LEN, StereoSample};
use crate::sharc_lib::LibCore;
use crate::sharc_source::{
    AfterEnd, CaptureSource, FrameEnd, LivePack, LiveSource, VOICE_SAMPLES, swap16_into,
};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn lib() -> Option<PathBuf> {
    let p = std::env::var_os("SHARC_NATIVE_LIB")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            root().join("out/native/opt/target-final/release/libsharc_native.dylib")
        });
    p.exists().then_some(p)
}

fn pack() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("LIVE_SHARC_PACK") {
        return Some(PathBuf::from(p));
    }
    let dir = root().join("out/native/live");
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "pack"))
        .filter(|e| !e.file_name().to_string_lossy().starts_with("state-"))
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        .map(|e| e.path())
}

fn source() -> Option<(CaptureSource<LibCore>, Arc<LivePack>)> {
    let (Some(lib), Some(pack)) = (lib(), pack()) else {
        eprintln!("skipped: no native core library or live pack under out/");
        return None;
    };
    let pack = Arc::new(LivePack::load(&pack).expect("live pack"));
    let core = LibCore::open(&lib, pack.image()).expect("core library");
    let src = CaptureSource::new(core, Arc::clone(&pack), AfterEnd::Hold, 1.0).expect("source");
    Some((src, pack))
}

/// The numbers of `"KEY": [a, b, ...]` in a JSON text (the reference file's
/// flat float lists; Rust's float parsing of Python's repr is exact).
fn json_floats(text: &str, key: &str) -> Option<Vec<f64>> {
    let at = text.find(&format!("\"{key}\": ["))?;
    let rest = &text[at..];
    let open = rest.find('[')?;
    let close = rest.find(']')?;
    Some(
        rest[open + 1..close]
            .split(',')
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.trim().parse().expect("float"))
            .collect(),
    )
}

fn json_int(text: &str, key: &str) -> Option<usize> {
    let at = text.find(&format!("\"{key}\": "))? + key.len() + 4;
    let end = text[at..].find([',', '}'])?;
    text[at..at + end].trim().parse().ok()
}

#[test]
fn frames_end_cleanly_and_voices_start_at_the_arm_frame() {
    let Some((mut src, pack)) = source() else {
        return;
    };
    let mut first_sound = None;
    for k in 0..40 {
        let end = src.step();
        assert!(matches!(end, FrameEnd::Clean { .. }), "frame {k}: {end:?}");
        let loud = src
            .output()
            .voices
            .iter()
            .any(|v| v.iter().any(|&s| s != 0.0));
        if loud && first_sound.is_none() {
            first_sound = Some(pack.first as usize + k);
        }
    }
    // drive3: trig in frame 77, voices loaded and armed in frame 78.
    assert_eq!(first_sound, Some(78));
}

/// The live path (wire-order frames, swapped at the SHARC edge) on the
/// capture's frames, un-swapped back to wire order, renders exactly what
/// the capture path renders from the pack's pre-swapped frames.
#[test]
fn live_source_on_wire_frames_equals_the_capture_source() {
    let Some((mut cap, pack)) = source() else {
        return;
    };
    let core = LibCore::open(&lib().expect("lib"), pack.image()).expect("core library");
    let mut live = LiveSource::new(core, &pack, 1.0).expect("live source");
    let mut wire = Vec::new();
    let mut loud = 0;
    for k in 0..40 {
        swap16_into(pack.frame(k), &mut wire, usize::MAX);
        let a = cap.step();
        let b = live.step(&wire).expect("a frame ran");
        assert_eq!(a, b, "frame {k}");
        assert_eq!(cap.output(), live.output(), "frame {k}");
        loud += live
            .output()
            .voices
            .iter()
            .any(|v| v.iter().any(|&s| s != 0.0)) as usize;
    }
    assert!(loud > 0, "the capture's trig sounds on the live path too");
}

#[test]
fn voices_equal_the_python_replay_bit_for_bit() {
    let Some((mut src, pack)) = source() else {
        return;
    };
    let dir = root().join("out/native/live");
    let prefix = format!("ref-{}-", pack.key);
    let Some(refp) = std::fs::read_dir(&dir).ok().and_then(|d| {
        d.filter_map(|e| e.ok()).map(|e| e.path()).find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(&prefix))
        })
    }) else {
        eprintln!("skipped: no {prefix}*.json (tools/sharc_transpile_run.py live-ref)");
        return;
    };
    let text = std::fs::read_to_string(&refp).expect("reference");
    let arm = json_int(&text, "arm_frame").expect("arm_frame");
    let want = [
        json_floats(&text, "0").expect("voice 0"),
        json_floats(&text, "1").expect("voice 1"),
    ];
    let frames = want[0].len() / VOICE_SAMPLES;
    assert!(frames > 0);
    for _ in pack.first as usize..arm {
        src.step();
    }
    for f in 0..frames {
        src.step();
        for (v, w) in want.iter().enumerate() {
            let got = &src.output().voices[v];
            let exp = &w[f * VOICE_SAMPLES..(f + 1) * VOICE_SAMPLES];
            for i in 0..VOICE_SAMPLES {
                assert_eq!(
                    got[i].to_bits(),
                    exp[i].to_bits(),
                    "frame {} voice {v} sample {i}: {} vs {}",
                    arm + f,
                    got[i],
                    exp[i]
                );
            }
        }
    }
    eprintln!(
        "{} frames x 2 voices identical ({})",
        frames,
        refp.display()
    );
}

/// FNV-1a over interleaved signed Q31 PCM, little-endian L then R. This is a
/// reproducibility fingerprint, not an authenticated firmware checksum.
fn pcm_fnv1a64(samples: &[StereoSample]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for sample in samples {
        for channel in [sample.l, sample.r] {
            assert!(channel.is_finite(), "DSP output contains non-finite PCM");
            for byte in f32_to_q31(channel).to_le_bytes() {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x100_0000_01b3);
            }
        }
    }
    hash
}

#[test]
fn offline_pcm_fingerprint_has_fixed_order_and_encoding() {
    assert_eq!(
        pcm_fnv1a64(&[StereoSample { l: 0.5, r: -0.25 }]),
        0xd47f_e2d1_7bf3_55e5
    );
}

/// Local DSP-only acceptance; does NOT exercise ColdFire-generated frames.
/// Explicit artifacts avoid the test module's "newest pack" selection.
#[test]
#[ignore = "requires explicitly chosen ignored live pack and native core library"]
fn dsp_only_offline_audio_gate() {
    const FRAMES: usize = 160;
    const MAX_ELAPSED: Duration = Duration::from_secs(30);

    let under_out = |name: &str| -> PathBuf {
        let path = PathBuf::from(std::env::var(name).unwrap_or_else(|_| panic!("missing {name}")));
        assert!(path.is_absolute(), "{name} must be an absolute local path");
        let path = path.canonicalize().expect("local artifact must exist");
        let out = root()
            .join("out")
            .canonicalize()
            .expect("ignored out/ must exist");
        assert!(path.starts_with(out), "{name} must stay under ignored out/");
        path
    };
    let pack = under_out("LIVE_SHARC_PACK");
    let lib = under_out("SHARC_NATIVE_LIB");
    let selected = LivePack::load(&pack).expect("local live pack");
    if selected.core.is_none() {
        eprintln!(
            "DSP-only local gate: legacy pack has no core source hash; compatibility unverified"
        );
    }
    let (tx, rx) = mpsc::sync_channel(1);
    let worker = thread::spawn(move || {
        let mut first_digest = None;
        for attempt in 0..2 {
            // A v3 pack checks its core source hash; older packs do not.
            let (source, _info) =
                crate::sharc_lib::capture_source(&lib, &pack, AfterEnd::Hold, 1.0)
                    .expect("checked capture source");
            let log = source.log();
            let player = LivePlayer::offline(Box::new(source));
            let mut samples = vec![StereoSample::default(); FRAMES * FRAME_LEN];
            let start = Instant::now();
            let frames = player.render_offline(&mut samples).expect("offline render");
            let elapsed = start.elapsed();
            assert_eq!(frames, FRAMES);
            assert_eq!(player.stats().frames_rendered, (FRAMES * FRAME_LEN) as u64);
            let log = log.lock().expect("render log");
            assert_eq!(log.frames, FRAMES as u64);
            assert_eq!(log.clean, FRAMES as u64, "DSP frame did not finish cleanly");
            assert!(
                log.nonzero_frames > 0,
                "selected capture rendered no voice taps"
            );
            assert!(
                samples.iter().any(|s| s.l != 0.0 || s.r != 0.0),
                "selected capture emitted silent stereo PCM"
            );
            let digest = pcm_fnv1a64(&samples);
            if let Some(first) = first_digest {
                assert_eq!(digest, first, "fresh DSP-only render changed PCM");
            }
            first_digest = Some(digest);
            eprintln!(
                "DSP-only local run {}: {} frames, {} stereo samples, PCM FNV-1a64 {digest:016x}, {:.3}s (no ColdFire)",
                attempt + 1,
                FRAMES,
                samples.len(),
                elapsed.as_secs_f64()
            );
        }
        tx.send(()).expect("gate result receiver");
    });
    // Bounds the test's wait even if a native frame never returns; the test
    // process must exit on failure (a thread cannot cancel a hung core).
    match rx.recv_timeout(MAX_ELAPSED) {
        Ok(()) | Err(RecvTimeoutError::Disconnected) => {
            worker.join().expect("DSP-only renderer panicked");
        }
        Err(RecvTimeoutError::Timeout) => {
            panic!("DSP-only render did not finish within 30 seconds");
        }
    }
}
