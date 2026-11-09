//! The saved card writes (`overlay.bin` next to the card image): a quit saves them only when the firmware reached ready in
//! the session, and `card.reset_overlay` sets them aside for good. ColdFire only. A real overlay saved by a ready session did not reach ready again within 20 emulated seconds
//! (measured once, not explained), so the session that is reset starts from a one-sector overlay. Skipped without the DT2 1.17 firmware.

use std::{
    fs,
    path::{Path, PathBuf},
};

use digi_control::{Config, Handle, spawn};
use serde_json::{Value, json};

const MS: u64 = 132_000; // ColdFire instructions per emulated millisecond (the oracle clock)

fn dt2_syx() -> Option<PathBuf> {
    std::env::var_os("DT2_SYX").map(PathBuf::from).or_else(|| {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .map(|dir| dir.join("firmware").join("Digitakt_II_OS1.17.syx"))
            .find(|path| path.exists())
    })
}

fn wav(frequency: f64) -> Vec<u8> {
    let data: Vec<u8> = (0..2400)
        .flat_map(|i| {
            let v = (0.5
                * (std::f64::consts::TAU * frequency * f64::from(i) / 48_000.0).sin()
                * 32767.0) as i16;
            v.to_le_bytes()
        })
        .collect();
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    for field in [16u32, 0x0001_0001, 48_000, 96_000, 0x0010_0002] {
        out.extend_from_slice(&field.to_le_bytes());
    }
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&data);
    out
}

/// A session on a synced card built from `folder`; the card's image path.
fn session(syx: &Path, run: &Path, folder: &Path, out: &Path) -> (Handle, std::thread::JoinHandle<()>, PathBuf) {
    let mut config = Config::new(None);
    config.run_dir = run.to_path_buf();
    let (handle, join) = spawn(config);
    handle
        .call("firmware.load", json!({"syx": syx.display().to_string()}))
        .unwrap();
    let card = handle
        .call(
            "card.load",
            json!({"dir": folder.display().to_string(), "sync": true, "export_out": out.display().to_string()}),
        )
        .unwrap();
    assert_eq!(card["state"], "ready");
    let image = PathBuf::from(card["card"].as_str().unwrap());
    (handle, join, image)
}

fn quit(handle: Handle, join: std::thread::JoinHandle<()>) {
    handle.call("quit", json!({})).unwrap();
    drop(handle);
    join.join().unwrap();
}

fn step_ms(handle: &Handle, ms: u64) {
    handle
        .call(
            "step",
            json!({"instructions": ms * MS, "timeout_wall_s": 300}),
        )
        .unwrap();
}

fn boot_to_ready(handle: &Handle, session: u32) {
    for _ in 0..40 {
        step_ms(handle, 500);
        let status = handle.call("status", json!({})).unwrap();
        if status["cf"]["ready"] == Value::Bool(true) {
            return;
        }
    }
    panic!("the DT2 did not reach ready in session {session}");
}

/// An overlay with one write in the last sector of the card (the firmware never reads it).
fn fake_overlay(image: &Path) -> Vec<u8> {
    let mut sector = emmc_card::CardOverlaySector {
        sector: fs::metadata(image).unwrap().len() / 512 - 1,
        written: [0; 64],
        data: [0; 512],
    };
    sector.written[0] = 1;
    sector.data[0] = 0x5a;
    plusdrive_format::encode_overlay(&[sector])
}

fn folder_of(dir: &Path) -> PathBuf {
    let folder = dir.join("plus_drive").join("dt2");
    fs::create_dir_all(&folder).unwrap();
    fs::create_dir_all(folder.join("digikit/modules")).unwrap();
    for (name, frequency) in [("a.wav", 220.0), ("b.wav", 880.0), ("c.wav", 1760.0)] {
        fs::write(folder.join(name), wav(frequency)).unwrap();
    }
    fs::write(folder.join("digikit/modules/m.dkm"), b"module bytes for the store").unwrap();
    folder
}

#[test]
fn a_quit_before_ready_leaves_the_saved_overlay_and_exports_nothing() {
    let Some(syx) = dt2_syx() else { return };
    let dir = std::env::temp_dir().join(format!("digi-overlay-notready-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let (folder, out) = (folder_of(&dir), dir.join("out"));
    let (handle, join, image) = session(&syx, &dir.join("run"), &folder, &out);
    // An overlay from an earlier session.
    let overlay = digi_control::controller::overlay_path(&image);
    fs::write(&overlay, fake_overlay(&image)).unwrap();
    let before = fs::read(&overlay).unwrap();
    // The boot has only started: the guest has written half-finished state at most.
    step_ms(&handle, 300);
    assert_eq!(
        handle.call("status", json!({})).unwrap()["cf"]["ready"],
        false
    );
    quit(handle, join);
    assert_eq!(fs::read(&overlay).unwrap(), before, "the overlay is as it was");
    assert!(!overlay.with_extension("bin.part").exists());
    assert!(!out.exists(), "nothing is exported from a session that never was ready");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_ready_session_saves_and_reset_overlay_sets_the_saved_one_aside_for_good() {
    let Some(syx) = dt2_syx() else { return };
    let dir = std::env::temp_dir().join(format!("digi-overlay-reset-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let (folder, out, run) = (folder_of(&dir), dir.join("out"), dir.join("run"));

    // Session 1: ready, then quit: the card writes are saved.
    let (handle, join, image) = session(&syx, &run, &folder, &out);
    let overlay = digi_control::controller::overlay_path(&image);
    assert!(!overlay.exists());
    boot_to_ready(&handle, 1);
    quit(handle, join);
    let saved = fs::read(&overlay).expect("a ready session saves its card writes");
    assert!(!saved.is_empty());
    // The next session starts from an overlay that is known to boot (see the note at the top).
    let saved = fake_overlay(&image);
    fs::write(&overlay, &saved).unwrap();

    // Session 2: the overlay is loaded and reaches ready; reset sets it aside.
    let (handle, join, image2) = session(&syx, &run, &folder, &out);
    assert_eq!(image, image2);
    assert_eq!(handle.call("card.folder", json!({})).unwrap()["overlay"], true);
    boot_to_ready(&handle, 2);
    let reset = handle.call("card.reset_overlay", json!({})).unwrap();
    let aside = PathBuf::from(reset["set_aside"].as_str().expect("set aside"));
    assert!(
        aside
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("overlay.bin.bad-")
    );
    assert_eq!(fs::read(&aside).unwrap(), saved, "kept, not deleted");
    assert!(!overlay.exists());
    assert_eq!(handle.call("card.folder", json!({})).unwrap()["overlay"], false);
    // Nothing to reset a second time.
    let again = handle.call("card.reset_overlay", json!({})).unwrap();
    assert_eq!(again["set_aside"], Value::Null);
    // The session still holds the sectors; its quit must not write them back.
    quit(handle, join);
    assert!(!overlay.exists(), "the quit did not bring the old sectors back");

    // Session 3: a card without an overlay.
    let (handle, join, _) = session(&syx, &run, &folder, &out);
    let path = dir.join("session3-overlay.bin");
    let written = handle
        .call("card.overlay", json!({"path": path.display().to_string()}))
        .unwrap();
    assert_eq!(written["sectors"], 0, "the next load has no overlay");
    quit(handle, join);
    let _ = fs::remove_dir_all(&dir);
}
