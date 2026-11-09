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
        root.ancestors()
            .map(|dir| dir.join("firmware").join(name))
            .find(|path| path.exists())
    })
}

#[test]
fn a_session_is_adopted_by_the_load_that_starts_it() {
    let (Some(dt2), Some(dn2)) = (
        stock("DT2_SYX", "Digitakt_II_OS1.17.syx"),
        stock("DN2_SYX", "Digitone_II_OS1.12.syx"),
    ) else {
        return;
    };
    let dir = std::env::temp_dir().join(format!("digi-ui-session-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut config = Config::new(None);
    config.run_dir = dir.clone();
    let (handle, join) = spawn(config);
    let ok = |method: &str, session: u64| handle.call(method, json!({"session": session}));
    let stale = |r: Result<serde_json::Value, digi_control::RpcError>| {
        r.unwrap_err().message == "stale emulator session"
    };

    // Open, runtime on: the DT2 image is patched here and becomes session 3.
    let load =
        json!({"syx": dt2.display().to_string(), "runtime": true, "replace": true, "session": 3});
    let report = handle.call("firmware.load", load.clone()).unwrap();
    assert_eq!(report["runtime"]["patched"], true, "{report}");
    assert!(ok("status", 3).is_ok());
    assert!(stale(ok("status", 2)));
    // The status keeps the patch report.
    let status = handle.call("status", json!({})).unwrap();
    assert_eq!(status["firmware"]["runtime"]["set"], "emulator", "{status}");
    assert_eq!(
        status["firmware"]["runtime"]["modules"]
            .as_array()
            .unwrap()
            .len(),
        16
    );
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

#[test]
fn patched_zero_keeps_the_block_clock_through_cache_fallback_and_restart() {
    let Some(syx) = stock("DT2_SYX", "Digitakt_II_OS1.17.syx") else {
        return;
    };
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let snapshot = root.join("out/ready-pk-dt2-117/digi-audio-m5.snap");
    if !snapshot.is_file() {
        return;
    }
    let firmware = patchkit::firmware::Firmware::parse(&std::fs::read(&syx).unwrap()).unwrap();
    let ports = patchkit::port::Registry::from_dir(&root.join("devices/ports")).unwrap();
    let literal = ports
        .find(&firmware.syx_sha256)
        .unwrap()
        .anchor("cf.link.site.block_isr.counter_lit")
        .unwrap()
        .addr;
    let dir = std::env::temp_dir().join(format!("digi-patched-zero-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut config = Config::new(Some(digi_coupled::AudioOptions {
        zero: true,
        snapshot,
        ..Default::default()
    }));
    config.run_dir = dir.clone();
    let (handle, join) = spawn(config);
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let peek = |addr| {
            handle.call("mem.cf.peek", json!({"addr": addr})).unwrap()["value"]
                .as_u64()
                .unwrap() as u32
        };
        let check_clock = || {
            let status = handle.call("status", json!({})).unwrap();
            assert_eq!(status["dsp"], "zero", "{status}");
            assert!(
                status["audio"].is_null(),
                "no SHARC audio session: {status}"
            );
            let counter = peek(literal);
            let before = peek(counter as u64);
            handle
                .call(
                    "step",
                    json!({"instructions": 132_000_000u64, "timeout_wall_s": 30}),
                )
                .unwrap();
            let blocks = peek(counter as u64).wrapping_sub(before);
            // 1500 blocks per emulated second; tolerate interrupt service at the step boundary.
            assert!(
                (1490..=1510).contains(&blocks),
                "block clock advanced {blocks} in one emulated second"
            );
        };
        let load = || {
            handle
                .call(
                    "firmware.load",
                    json!({"syx": syx, "runtime": true, "force": true}),
                )
                .unwrap()
        };
        let cold = load();
        assert_eq!(cold["start_used"], "cold", "{cold}");
        check_clock();
        let caches: Vec<_> = std::fs::read_dir(dir.join("patched"))
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "snap"))
            .collect();
        assert_eq!(caches.len(), 1);
        assert!(
            caches[0]
                .file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with("-zero.snap")
        );
        let restored = load();
        assert_eq!(restored["start_used"], "snapshot-cache", "{restored}");
        check_clock();
        std::fs::write(&caches[0], b"deliberately unusable test snapshot").unwrap();
        let fallback = load();
        assert_eq!(fallback["start_used"], "cold", "{fallback}");
        assert!(fallback["cache_note"].is_string(), "{fallback}");
        check_clock();
        handle.call("restart", json!({})).unwrap();
        check_clock();
    }));
    drop(handle);
    join.join().unwrap();
    if let Err(error) = outcome {
        eprintln!("patched zero diagnostics retained: {}", dir.display());
        std::panic::resume_unwind(error);
    }
    let _ = std::fs::remove_dir_all(dir);
}
