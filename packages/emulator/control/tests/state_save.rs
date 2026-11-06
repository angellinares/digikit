//! `state.save` and `state.load`: a snapshot taken after some steps, restored into a freshly loaded
//! runtime, gives the same state digest, and both go on the same way. Needs a stock firmware
//! (`DT2_SYX`, else the DT2 1.17 image in `firmware/`); without one the test returns.

use std::{fs, path::PathBuf};

use digi_control::{Config, Handle, spawn};
use serde_json::json;

fn dt2_syx() -> Option<PathBuf> {
    std::env::var_os("DT2_SYX").map(PathBuf::from).or_else(|| {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        ["Digitakt_II_OS1.17.syx"].iter().find_map(|name| {
            root.ancestors()
                .map(|dir| dir.join("firmware").join(name))
                .find(|path| path.exists())
        })
    })
}

fn digest(handle: &Handle) -> String {
    handle.call("state.digest", json!({})).unwrap()["digest"]
        .as_str()
        .unwrap()
        .to_string()
}

fn run(handle: &Handle, budget: u64) {
    handle
        .call("step", json!({"instructions": budget}))
        .unwrap();
}

#[test]
fn state_needs_a_firmware_and_a_path() {
    let (handle, join) = spawn(Config::new(None));
    for method in ["state.save", "state.load"] {
        let err = handle
            .call(method, json!({"path": "/nonexistent/x.snap"}))
            .unwrap_err();
        assert_eq!(err.message, "No firmware selected", "{method}");
        let err = handle.call(method, json!({})).unwrap_err();
        assert!(err.message.contains("path"), "{method}: {}", err.message);
    }
    drop(handle);
    join.join().unwrap();
}

#[test]
fn a_restored_state_goes_on_as_the_saved_one_did() {
    let Some(syx) = dt2_syx() else { return };
    let dir = std::env::temp_dir().join(format!("digi-state-save-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join("boot.snap");
    let load = json!({"syx": syx.display().to_string()});

    // run 1: some steps, save, some more
    let (a, join_a) = spawn(Config::new(None));
    a.call("firmware.load", load.clone()).unwrap();
    run(&a, 3_000_000);
    let saved = a
        .call("state.save", json!({"path": file.display().to_string()}))
        .unwrap();
    assert!(saved["bytes"].as_u64().unwrap() > 1000, "{saved}");
    assert!(file.is_file() && !file.with_extension("snap.part").exists());
    let at_save = digest(&a);
    run(&a, 3_000_000);
    let later = digest(&a);
    assert_ne!(at_save, later, "the guest did not run");

    // run 2: a fresh load, the snapshot, the same steps
    let (b, join_b) = spawn(Config::new(None));
    b.call("firmware.load", load.clone()).unwrap();
    let restored = b
        .call("state.load", json!({"path": file.display().to_string()}))
        .unwrap();
    assert!(restored["icount"].as_u64().unwrap() > 0, "{restored}");
    assert_eq!(
        digest(&b),
        at_save,
        "the restored state differs from the saved one"
    );
    run(&b, 3_000_000);
    assert_eq!(digest(&b), later, "the restored run went another way");

    // a runtime that has run refuses a restore; a missing file is an error and changes nothing
    let err = b
        .call("state.load", json!({"path": file.display().to_string()}))
        .unwrap_err();
    assert!(err.message.contains("has not run"), "{}", err.message);
    let (c, join_c) = spawn(Config::new(None));
    c.call("firmware.load", load).unwrap();
    let err = c
        .call(
            "state.load",
            json!({"path": dir.join("none.snap").display().to_string()}),
        )
        .unwrap_err();
    assert!(err.message.contains("none.snap"), "{}", err.message);

    for (handle, join) in [(a, join_a), (b, join_b), (c, join_c)] {
        drop(handle);
        join.join().unwrap();
    }
    let _ = fs::remove_dir_all(&dir);
}
