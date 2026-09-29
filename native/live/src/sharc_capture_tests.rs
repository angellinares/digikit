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
use std::sync::Arc;

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
