//! The card folder belongs to the device, not to the process (docs/plans/card-folder.md): a load of another device switches the
//! mirror to that device's folder, a folder the user chose is kept per device (and across a restart of the controller), and what the
//! first device had not mirrored yet goes to its own folder before the switch. Needs the stock DT2 1.17 and DN2 1.12 images
//! (`DT2_SYX`, `DN2_SYX`, else `firmware/`); without them the tests return. ColdFire only.

use std::{
    fs,
    path::{Path, PathBuf},
};

use digi_control::{Config, DeviceFoldersConfig, Handle, cards, spawn};
use serde_json::{Value, json};

fn stock(var: &str, name: &str) -> Option<PathBuf> {
    std::env::var_os(var).map(PathBuf::from).or_else(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        root.ancestors()
            .map(|dir| dir.join("firmware").join(name))
            .find(|path| path.exists())
    })
}

fn images() -> Option<(PathBuf, PathBuf)> {
    Some((
        stock("DT2_SYX", "Digitakt_II_OS1.17.syx")?,
        stock("DN2_SYX", "Digitone_II_OS1.12.syx")?,
    ))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("digi-devfolders-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn start(dir: &Path, sync: bool) -> (Handle, std::thread::JoinHandle<()>) {
    let mut config = Config::new(None);
    config.run_dir = dir.join("run");
    config.device_folders = Some(DeviceFoldersConfig {
        root: Some(dir.join("plus_drive")),
        choices: Some(dir.join("config/card-folders.json")),
        sync,
    });
    spawn(config)
}

fn open(handle: &Handle, syx: &Path) {
    handle
        .call(
            "firmware.load",
            json!({"syx": syx.display().to_string(), "replace": true}),
        )
        .unwrap();
}

fn folder(handle: &Handle) -> Value {
    handle.call("card.folder", json!({})).unwrap()
}

#[test]
fn each_device_mirrors_to_its_own_folder() {
    let Some((dt2, dn2)) = images() else { return };
    let dir = scratch("own");
    let root = dir.join("plus_drive");
    let (handle, join) = start(&dir, true);

    open(&handle, &dn2);
    let f = folder(&handle);
    assert_eq!(f["device"], "dn2", "{f}");
    assert_eq!(f["folder"], root.join("dn2").display().to_string(), "{f}");
    assert!(root.join("dn2").is_dir(), "the default folder is made");
    assert_eq!(f["custom"], false);
    assert_eq!(f["reload_needed"], false);

    open(&handle, &dt2);
    let f = folder(&handle);
    assert_eq!(f["device"], "dt2", "{f}");
    assert_eq!(f["folder"], root.join("dt2").display().to_string(), "{f}");
    assert!(root.join("dt2").is_dir());

    open(&handle, &dn2);
    assert_eq!(
        folder(&handle)["folder"],
        root.join("dn2").display().to_string()
    );

    drop(handle);
    join.join().unwrap();
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_chosen_folder_is_per_device_saved_and_applies_as_the_device_needs() {
    let Some((dt2, dn2)) = images() else { return };
    let dir = scratch("choice");
    let root = dir.join("plus_drive");
    let (mine_dn2, mine_dt2) = (dir.join("mine-dn2"), dir.join("mine-dt2"));
    fs::create_dir_all(&mine_dn2).unwrap();
    fs::create_dir_all(&mine_dt2).unwrap();
    let canon = |p: &Path| fs::canonicalize(p).unwrap().display().to_string();
    let (handle, join) = start(&dir, true);

    // DN2: the mirror goes to the new folder at once.
    open(&handle, &dn2);
    let f = handle
        .call(
            "card.folder_set",
            json!({"dir": mine_dn2.display().to_string()}),
        )
        .unwrap();
    assert_eq!(f["folder"], canon(&mine_dn2), "{f}");
    assert_eq!(f["custom"], true);
    assert_eq!(f["reload_needed"], false);
    assert_eq!(f["default_folder"], root.join("dn2").display().to_string());
    assert!(f["error"].is_null(), "{f}");

    // DT2: the card is built from the folder, so the new folder applies at the next load.
    open(&handle, &dt2);
    assert_eq!(
        folder(&handle)["folder"],
        root.join("dt2").display().to_string(),
        "the DN2's choice is not the DT2's"
    );
    let f = handle
        .call(
            "card.folder_set",
            json!({"dir": mine_dt2.display().to_string()}),
        )
        .unwrap();
    assert_eq!(f["folder"], root.join("dt2").display().to_string(), "{f}");
    assert_eq!(f["next_folder"], canon(&mine_dt2), "{f}");
    assert_eq!(f["reload_needed"], true, "{f}");
    handle.call("restart", json!({})).unwrap();
    let f = folder(&handle);
    assert_eq!(f["folder"], canon(&mine_dt2), "{f}");
    assert_eq!(f["reload_needed"], false, "{f}");

    // Both choices are in the file, per device; a new controller reads them at its first load.
    let saved: Value =
        serde_json::from_str(&fs::read_to_string(dir.join("config/card-folders.json")).unwrap())
            .unwrap();
    assert_eq!(saved["dn2"], canon(&mine_dn2));
    assert_eq!(saved["dt2"], canon(&mine_dt2));
    drop(handle);
    join.join().unwrap();
    let (handle, join) = start(&dir, true);
    open(&handle, &dn2);
    assert_eq!(folder(&handle)["folder"], canon(&mine_dn2));

    // "Use default" goes back to plus_drive/<device>/.
    let f = handle.call("card.folder_set", json!({})).unwrap();
    assert_eq!(f["folder"], root.join("dn2").display().to_string(), "{f}");
    assert_eq!(f["custom"], false);
    assert!(
        handle
            .call("card.folder_set", json!({"dir": "relative/path"}))
            .is_err()
    );
    assert!(
        handle
            .call(
                "card.folder_set",
                json!({"dir": dir.join("missing").display().to_string()})
            )
            .is_err()
    );
    drop(handle);
    join.join().unwrap();
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn without_device_folders_there_is_no_mirror_and_the_first_folder_is_only_the_first_loads() {
    let Some((dt2, dn2)) = images() else { return };
    let dir = scratch("first");
    let first = dir.join("first");
    let mut config = Config::new(None);
    config.run_dir = dir.join("run");
    config.mirror_dir = Some(first.clone());
    let (handle, join) = spawn(config);
    open(&handle, &dn2);
    assert_eq!(folder(&handle)["folder"], first.display().to_string());
    // another device has no folder of its own here: no mirror, and nothing is written to the first device's folder
    open(&handle, &dt2);
    let f = folder(&handle);
    assert!(f["folder"].is_null() && f["continuous"] == false, "{f}");
    drop(handle);
    join.join().unwrap();
    let _ = fs::remove_dir_all(&dir);
}

/// A DN2 card whose module store holds one file (built from a folder, as a load does it): the boot's own card writes make the mirror
/// pending, and the file is what the pass writes, so the folder that receives it shows which device's mirror ran.
#[test]
fn what_the_first_device_has_not_mirrored_yet_goes_to_its_own_folder() {
    let Some((dt2, dn2)) = images() else { return };
    let dir = scratch("flush");
    let root = dir.join("plus_drive");
    let seed = dir.join("seed");
    fs::create_dir_all(seed.join("digikit/modules")).unwrap();
    fs::write(seed.join("digikit/modules/m.dkm"), b"module bytes").unwrap();
    let files = cards::device_files("dn2", &seed).unwrap();
    let (paths, _, _) = cards::ensure_card(
        "dn2",
        &dir.join("run"),
        &fs::read(&dn2).unwrap(),
        &seed,
        &files,
        |_, _| {},
    )
    .unwrap();

    let (handle, join) = start(&dir, true);
    handle
        .call("firmware.load", json!({"syx": dn2.display().to_string(), "replace": true, "card": paths.image.display().to_string()}))
        .unwrap();
    // the boot writes to the card: wait (in emulated time) until the mirror has a change waiting for its quiet period
    let mut behind = false;
    for _ in 0..30 {
        handle
            .call(
                "step",
                json!({"instructions": 100_000_000u64, "timeout_wall_s": 300}),
            )
            .unwrap();
        behind = folder(&handle)["behind"] == true;
        if behind {
            break;
        }
    }
    assert!(behind, "the DN2 boot made no card write");
    let module = |device: &str| root.join(device).join("digikit/modules/m.dkm");
    assert!(
        !module("dn2").exists(),
        "not mirrored yet (the quiet period has not run out)"
    );

    // The switch to the DT2 finishes the DN2's mirror first, into the DN2's folder.
    open(&handle, &dt2);
    assert_eq!(
        folder(&handle)["folder"],
        root.join("dt2").display().to_string()
    );
    assert_eq!(
        fs::read(module("dn2")).unwrap(),
        b"module bytes",
        "the DN2's folder got its state"
    );
    assert!(!module("dt2").exists(), "and the DT2's folder did not");
    assert!(
        folder(&handle)["files"].as_array().unwrap().is_empty(),
        "the DT2's folder has no DN2 file"
    );
    drop(handle);
    join.join().unwrap();
    let _ = fs::remove_dir_all(&dir);
}

fn card_images(run: &Path, device: &str) -> usize {
    fs::read_dir(run.join("cards"))
        .map(|d| {
            d.flatten()
                .filter(|e| e.path().join(format!("{device}.img")).is_file())
                .count()
        })
        .unwrap_or(0)
}

/// A load without a `card` builds the card from the device's folder; a folder chosen for the device is what the next load builds it from.
#[test]
fn a_load_builds_the_card_from_the_devices_folder_and_a_new_folder_applies_at_the_next_load() {
    let Some((_dt2, dn2)) = images() else { return };
    let dir = scratch("card");
    let run = dir.join("run");
    let module = |folder: &Path, name: &str, bytes: &[u8]| {
        fs::create_dir_all(folder.join("digikit/modules")).unwrap();
        fs::write(folder.join("digikit/modules").join(name), bytes).unwrap();
    };
    module(
        &dir.join("plus_drive/dn2"),
        "m.dkm",
        b"in the default folder",
    );
    let other = dir.join("other");
    module(&other, "o.dkm", b"in the chosen folder");
    let (handle, join) = start(&dir, true);

    open(&handle, &dn2);
    assert_eq!(card_images(&run, "dn2"), 1, "the card of plus_drive/dn2");
    // the same folder again: the cached card is reused
    open(&handle, &dn2);
    assert_eq!(card_images(&run, "dn2"), 1);
    // a chosen folder does not rebuild the running card (the DN2's mirror switches at once) ...
    handle
        .call(
            "card.folder_set",
            json!({"dir": other.display().to_string()}),
        )
        .unwrap();
    assert_eq!(card_images(&run, "dn2"), 1);
    // ... the next load builds the card from it
    handle.call("restart", json!({})).unwrap();
    assert_eq!(card_images(&run, "dn2"), 2);
    // "Use default" and a load: the first card again (cached)
    handle.call("card.folder_set", json!({})).unwrap();
    handle.call("restart", json!({})).unwrap();
    assert_eq!(card_images(&run, "dn2"), 2);
    drop(handle);
    join.join().unwrap();
    let _ = fs::remove_dir_all(&dir);
}
