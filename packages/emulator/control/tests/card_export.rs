//! A project saved by the DT2 firmware reaches the +Drive folder: boot the DT2 on a card built from a folder of
//! WAV files, save the project with the panel keys (FUNC+SETTINGS, YES), then `card.export` and check the
//! folder. ColdFire only (no DSP): the save path does not need audio. Skipped without the DT2 1.17 firmware.

use std::{fs, path::PathBuf};

use digi_control::{Config, Handle, spawn};
use serde_json::{Value, json};

const MS: u64 = 132_000; // ColdFire instructions per emulated millisecond (the oracle clock)
const FUNC: u64 = 17;
const SETTINGS: u64 = 8;
const YES: u64 = 10;
const NO: u64 = 12;

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
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
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

fn overlay_of(handle: &Handle, path: &std::path::Path) -> Vec<emmc_card::CardOverlaySector> {
    handle
        .call("card.overlay", json!({"path": path.display().to_string()}))
        .unwrap();
    plusdrive_format::parse_overlay(&fs::read(path).unwrap()).unwrap()
}

#[test]
fn a_project_saved_by_the_firmware_is_exported_to_the_folder() {
    let Some(syx) = dt2_syx() else { return };
    let dir = std::env::temp_dir().join(format!("digi-card-export-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let folder = dir.join("plus_drive").join("dt2");
    fs::create_dir_all(&folder).unwrap();
    for (name, frequency) in [("a.wav", 220.0), ("b.wav", 880.0), ("c.wav", 1760.0)] {
        fs::write(folder.join(name), wav(frequency)).unwrap();
    }

    let mut config = Config::new(None);
    config.run_dir = dir.join("run");
    let (handle, join) = spawn(config);
    handle
        .call("firmware.load", json!({"syx": syx.display().to_string()}))
        .unwrap();
    let card = handle
        .call(
            "card.load",
            json!({"dir": folder.display().to_string(), "sync": true, "export_out": dir.join("out").display().to_string()}),
        )
        .unwrap();
    assert_eq!(card["state"], "ready");

    // Boot to the main screen, close the FILE SYSTEM OK dialog.
    let mut ready = false;
    for _ in 0..20 {
        step_ms(&handle, 500);
        let status = handle.call("status", json!({})).unwrap();
        if status["cf"]["ready"] == Value::Bool(true)
            && status["cf"]["main_ui_reached"] == Value::Bool(true)
        {
            ready = true;
            break;
        }
    }
    assert!(ready, "the DT2 did not reach the main screen");
    tap(&handle, NO);
    step_ms(&handle, 300);
    let before = overlay_of(&handle, &dir.join("before.overlay"));
    eprintln!("overlay sectors after boot: {}", before.len());

    // Save the project: FUNC+SETTINGS, then YES; the firmware writes slot 0 (about 14 MB) in well under a second
    // of emulated time, so 3 s is a wide margin.
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
    let after = overlay_of(&handle, &dir.join("after.overlay"));
    eprintln!(
        "overlay sectors: {} after boot, {} after the save",
        before.len(),
        after.len()
    );
    assert!(
        after.iter().any(|s| s.sector == 0x58000),
        "the save did not write project slot 0"
    );

    // card.export without a dir: the folder the card was loaded from.
    let out = dir.join("out");
    let report = handle
        .call("card.export", json!({"out": out.display().to_string()}))
        .unwrap();
    eprintln!("export: {report:#}");
    let added: Vec<&str> = report["added"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(added.contains(&".mmcfs/projects/0000.bin"), "{added:?}");
    // The working project is rewritten by the firmware at every start: not exported unless asked for.
    assert!(!added.contains(&".mmcfs/boot/050000.bin"), "{added:?}");
    assert_eq!(report["replaced"].as_array().unwrap().len(), 0);
    assert_eq!(
        report["unchanged"], 3,
        "the three WAV samples are unchanged"
    );
    let project = fs::read(folder.join(".mmcfs/projects/0000.bin")).unwrap();
    assert_eq!(
        &project[..4],
        &[0xbe, 0xef, 0xba, 0xce],
        "project container magic"
    );
    // The stock project is the COKi boot project the builder seeds at sector 0x40000.
    let image = PathBuf::from(card["card"].as_str().unwrap());
    let stock = {
        use std::io::{Read, Seek, SeekFrom};
        let mut f = fs::File::open(&image).unwrap();
        f.seek(SeekFrom::Start(0x40000 * 512)).unwrap();
        let mut b = vec![0u8; 0x1000];
        f.read_exact(&mut b).unwrap();
        b
    };
    assert_eq!(&stock[..4], b"COKi");
    assert_ne!(
        &project[..0x1000],
        &stock[..],
        "the saved project differs from the stock one"
    );
    eprintln!(
        "project slot 0: {} bytes; stock boot project starts {:02x?}",
        project.len(),
        &stock[..8]
    );

    // A second export changes nothing.
    let again = handle
        .call("card.export", json!({"out": out.display().to_string()}))
        .unwrap();
    assert_eq!(again["added"].as_array().unwrap().len(), 0, "{again:#}");
    assert_eq!(again["replaced"].as_array().unwrap().len(), 0, "{again:#}");
    assert!(!folder.join(".mmcfs/boot/050000.bin").exists());

    // `working_project` brings the working project along.
    let with = handle
        .call(
            "card.export",
            json!({"out": out.display().to_string(), "working_project": true}),
        )
        .unwrap();
    assert_eq!(with["added"], json!([".mmcfs/boot/050000.bin"]), "{with:#}");

    // Quit: overlay.bin is saved next to the image and the synced folder is exported once more.
    handle.call("quit", json!({})).unwrap();
    drop(handle);
    join.join().unwrap();
    let saved = image.with_file_name("overlay.bin");
    assert!(saved.is_file(), "overlay.bin next to the image");

    // The command-line path gives the same files from the image and the saved overlay.
    let offline = dir.join("offline");
    let base = plusdrive_format::export::FileImage::open(&image).unwrap();
    let sectors = plusdrive_format::parse_overlay(&fs::read(&saved).unwrap()).unwrap();
    let mut options = plusdrive_format::export::Options::new(dir.join("out2"));
    options.stamp = Some("offline".into());
    let report = plusdrive_format::export::export(
        &base,
        &sectors,
        &offline,
        &plusdrive_format::dt2_layout(),
        &options,
    )
    .unwrap();
    assert_eq!(
        fs::read(offline.join(".mmcfs/projects/0000.bin")).unwrap(),
        project
    );
    eprintln!(
        "offline: added {} issues {:?}",
        report.added.len(),
        report.issues
    );
    if std::env::var_os("DIGI_KEEP_TEST_DIR").is_none() {
        let _ = fs::remove_dir_all(&dir);
    } else {
        eprintln!("kept {}", dir.display());
    }
}
