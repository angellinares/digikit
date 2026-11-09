//! The +Drive development loop end to end (docs/plans/digikit-runtime.md, Phase 1 gate): boot the DT2 on a card
//! built from a `plus_drive/dt2/`-style folder, save a project from the panel, quit (the synced folder is exported
//! on exit), build a new card from the folder, boot again and find the saved project on it, sector for sector.
//! ColdFire only. Skipped without the DT2 1.17 firmware. Prints the numbers it measures.

use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::Instant,
};

use digi_control::{Config, Handle, spawn};
use serde_json::{Value, json};

const MS: u64 = 132_000; // ColdFire instructions per emulated millisecond (the oracle clock)
const FUNC: u64 = 17;
const SETTINGS: u64 = 8;
const YES: u64 = 10;
const NO: u64 = 12;
const PROJECT_SLOT_0: u64 = 0x58000;

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

fn dt2_syx() -> Option<PathBuf> {
    std::env::var_os("DT2_SYX").map(PathBuf::from).or_else(|| {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .map(|dir| dir.join("firmware").join("Digitakt_II_OS1.17.syx"))
            .find(|path| path.exists())
    })
}

fn step_ms(handle: &Handle, ms: u64) {
    let mut left = ms * MS;
    while left > 0 {
        let chunk = left.min(200_000_000);
        handle
            .call(
                "step",
                json!({"instructions": chunk, "timeout_wall_s": 300}),
            )
            .unwrap();
        left -= chunk;
    }
}

fn tap(handle: &Handle, code: u64) {
    handle
        .call("input.tap", json!({"code": code, "hold": 20 * MS}))
        .unwrap();
    step_ms(handle, 25);
}

fn boot_to_main(handle: &Handle) -> f64 {
    let started = Instant::now();
    for _ in 0..20 {
        step_ms(handle, 500);
        let status = handle.call("status", json!({})).unwrap();
        if status["cf"]["ready"] == Value::Bool(true)
            && status["cf"]["main_ui_reached"] == Value::Bool(true)
        {
            tap(handle, NO);
            step_ms(handle, 300);
            return started.elapsed().as_secs_f64();
        }
    }
    panic!("the DT2 did not reach the main screen");
}

fn read_image(image: &Path, sector: u64, len: usize) -> Vec<u8> {
    let mut f = fs::File::open(image).unwrap();
    f.seek(SeekFrom::Start(sector * 512)).unwrap();
    let mut b = vec![0u8; len];
    f.read_exact(&mut b).unwrap();
    b
}

#[test]
fn a_saved_project_survives_quit_export_rebuild_and_boot() {
    let Some(syx) = dt2_syx() else { return };
    let dir = std::env::temp_dir().join(format!("digi-card-roundtrip-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let folder = dir.join("plus_drive").join("dt2");
    fs::create_dir_all(folder.join("digikit/modules")).unwrap();
    for (name, frequency) in [("a.wav", 220.0), ("b.wav", 880.0), ("c.wav", 1760.0)] {
        fs::write(folder.join(name), wav(frequency)).unwrap();
    }
    let module = b"module bytes for the store".to_vec();
    fs::write(folder.join("digikit/modules/m.dkm"), &module).unwrap();
    let out = dir.join("out");
    let run = dir.join("run");

    // Session 1: build, boot, save the project from the panel, quit.
    let mut config = Config::new(None);
    config.run_dir = run.clone();
    let (handle, join) = spawn(config);
    handle
        .call("firmware.load", json!({"syx": syx.display().to_string()}))
        .unwrap();
    let started = Instant::now();
    let card1 = handle
        .call(
            "card.load",
            json!({"dir": folder.display().to_string(), "sync": true, "export_out": out.display().to_string()}),
        )
        .unwrap();
    assert_eq!(card1["state"], "ready");
    let build1 = started.elapsed().as_secs_f64();
    let image1 = PathBuf::from(card1["card"].as_str().unwrap());
    let boot1 = boot_to_main(&handle);
    handle
        .call("input.button", json!({"code": FUNC, "down": true}))
        .unwrap();
    step_ms(&handle, 5);
    tap(&handle, SETTINGS);
    handle
        .call("input.button", json!({"code": FUNC, "down": false}))
        .unwrap();
    step_ms(&handle, 300);
    tap(&handle, YES);
    step_ms(&handle, 3000);
    let quit_started = Instant::now();
    handle.call("quit", json!({})).unwrap();
    drop(handle);
    join.join().unwrap();
    let quit1 = quit_started.elapsed().as_secs_f64();

    // The quit exported the changes to the folder; digikit/ is untouched.
    let project = fs::read(folder.join(".mmcfs/projects/0000.bin")).unwrap();
    assert_eq!(&project[..4], &[0xbe, 0xef, 0xba, 0xce]);
    // The working project is not exported: the firmware rewrites it from the boot copy at every start.
    assert!(
        !folder.join(".mmcfs/boot/050000.bin").exists(),
        "the working project is not exported by default"
    );
    assert_eq!(
        fs::read(folder.join("digikit/modules/m.dkm")).unwrap(),
        module
    );
    eprintln!(
        "session 1: card built in {build1:.1} s, boot {boot1:.1} s, quit+export {quit1:.1} s; \
         project slot 0 {} bytes",
        project.len()
    );

    // Session 2: the folder now holds the project, so the card is a new one.
    let mut config = Config::new(None);
    config.run_dir = run;
    let (handle, join) = spawn(config);
    handle
        .call("firmware.load", json!({"syx": syx.display().to_string()}))
        .unwrap();
    let started = Instant::now();
    let card2 = handle
        .call(
            "card.load",
            json!({"dir": folder.display().to_string(), "sync": true, "export_out": out.display().to_string()}),
        )
        .unwrap();
    assert_eq!(card2["state"], "ready");
    let build2 = started.elapsed().as_secs_f64();
    let image2 = PathBuf::from(card2["card"].as_str().unwrap());
    assert_ne!(image1, image2, "the exported project makes a new card");

    // The saved project is on the new card, sector for sector (trailing zeros are not stored).
    let on_card = read_image(&image2, PROJECT_SLOT_0, project.len());
    assert_eq!(on_card, project, "project slot 0 on the new card");
    // The module store is on it too.
    let area = plusdrive_format::card_layout("dt2").unwrap().store.unwrap();
    let store = plusdrive_format::read_store(
        &plusdrive_format::export::FileImage::open(&image2).unwrap(),
        area.first_sector,
        area.store_limit(),
        area.header_b(),
    )
    .unwrap()
    .expect("module store");
    assert_eq!(store.entries.len(), 1);
    assert!(
        store.entries[0].path.ends_with("m.dkm"),
        "{:?}",
        store.entries[0]
    );
    let boot2 = boot_to_main(&handle);

    // The firmware boots on it. A second export adds and replaces nothing (the working project, which the firmware
    // rewrites at every start, is not exported).
    let report = handle
        .call("card.export", json!({"out": out.display().to_string()}))
        .unwrap();
    assert_eq!(report["added"].as_array().unwrap().len(), 0, "{report:#}");
    let replaced = report["replaced"].as_array().unwrap();
    assert!(replaced.is_empty(), "{report:#}");
    eprintln!(
        "session 2: card rebuilt in {build2:.1} s, booted to the main screen in {boot2:.1} s; \
         slot 0 on the card = the exported {} bytes; second export: added 0, replaced 0, unchanged {}",
        project.len(),
        report["unchanged"]
    );
    handle.call("quit", json!({})).unwrap();
    drop(handle);
    join.join().unwrap();
    if std::env::var_os("DIGI_KEEP_TEST_DIR").is_none() {
        let _ = fs::remove_dir_all(&dir);
    }
}
