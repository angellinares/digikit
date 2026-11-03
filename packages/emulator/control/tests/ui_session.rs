//! The webview's session token on protocol calls (`emu_call`): `firmware.load {replace, runtime, session}` and `restart {session}`
//! start the session, every other method must carry the current one, and the status says what the runtime patch did. Needs the
//! stock DT2 1.17 and DN2 1.12 images (`DT2_SYX`, `DN2_SYX`, else `firmware/`); without them the test returns. CF only: a runtime-patched
//! image cold-boots once (about 10 s), then comes from the snapshot cache.

use std::path::PathBuf;

use digi_control::{Config, spawn};
use serde_json::json;

fn stock(var: &str, name: &str) -> Option<PathBuf> {
    std::env::var_os(var).map(PathBuf::from).or_else(|| {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        root.ancestors().map(|dir| dir.join("firmware").join(name)).find(|path| path.exists())
    })
}

#[test]
fn a_session_is_adopted_by_the_load_that_starts_it() {
    let (Some(dt2), Some(dn2)) = (stock("DT2_SYX", "Digitakt_II_OS1.17.syx"), stock("DN2_SYX", "Digitone_II_OS1.12.syx")) else {
        return;
    };
    let dir = std::env::temp_dir().join(format!("digi-ui-session-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut config = Config::new(None);
    config.run_dir = dir.clone();
    let (handle, join) = spawn(config);
    let ok = |method: &str, session: u64| handle.call(method, json!({"session": session}));
    let stale = |r: Result<serde_json::Value, digi_control::RpcError>| r.unwrap_err().message == "stale emulator session";

    // Open, runtime on: the DT2 image is patched here and becomes session 3.
    let load = json!({"syx": dt2.display().to_string(), "runtime": true, "replace": true, "session": 3});
    let report = handle.call("firmware.load", load.clone()).unwrap();
    assert_eq!(report["runtime"]["patched"], true, "{report}");
    assert!(ok("status", 3).is_ok());
    assert!(stale(ok("status", 2)));
    // The status keeps the patch report.
    let status = handle.call("status", json!({})).unwrap();
    assert_eq!(status["firmware"]["runtime"]["set"], "emulator", "{status}");
    assert_eq!(status["firmware"]["runtime"]["modules"].as_array().unwrap().len(), 16);
    // The same token again is not newer; a reset takes the next one and keeps the runtime report.
    assert!(stale(handle.call("firmware.load", load)));
    handle.call("restart", json!({"session": 4})).unwrap();
    assert!(stale(ok("status", 3)));
    let status = handle.call("status", json!({"session": 4})).unwrap();
    assert_eq!(status["firmware"]["runtime"]["patched"], true, "{status}");

    // Open a file of the other device without the runtime: replaces the session, no DT2 card follows it.
    let other = json!({"syx": dn2.display().to_string(), "replace": true, "session": 5});
    handle.call("firmware.load", other).unwrap();
    let status = handle.call("status", json!({"session": 5})).unwrap();
    assert!(status["firmware"]["runtime"].is_null(), "{status}");
    assert_eq!(status["cf"]["device"], "dn2", "{status}");
    drop(handle);
    join.join().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
