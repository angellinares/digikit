//! A ColdFire-only runtime that free-runs (a DT2 opened in a session that has the DN2's audio inputs has no DSP audio to pace it)
//! follows the wall clock when `Config::pace_free` is set, so a mouse click on the faceplate is a short press for the firmware.
//! Unpaced it ran about 20 times real time: a 100 ms click lasted seconds of emulated time, the firmware saw a long press of
//! SETTINGS and opened nothing (the desktop workbench, 2026-10-23). Skipped without the DT2 1.17 firmware.

use std::{
    path::PathBuf,
    thread::sleep,
    time::{Duration, Instant},
};

use digi_control::{Config, Handle, spawn};
use serde_json::{Value, json};

const MS: u64 = 132_000;
const SETTINGS: u64 = 8;
const NO: u64 = 12;

fn dt2_syx() -> Option<PathBuf> {
    std::env::var_os("DT2_SYX").map(PathBuf::from).or_else(|| {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .map(|dir| dir.join("firmware").join("Digitakt_II_OS1.17.syx"))
            .find(|path| path.exists())
    })
}

fn step_ms(handle: &Handle, ms: u64) {
    handle
        .call(
            "step",
            json!({"instructions": ms * MS, "timeout_wall_s": 300}),
        )
        .unwrap();
}

fn elapsed(handle: &Handle) -> u64 {
    handle.call("status", json!({})).unwrap()["elapsed"]
        .as_u64()
        .unwrap()
}

fn frame_hash(handle: &Handle) -> String {
    let frame = handle
        .call("framebuffer.get", json!({"hash": true, "data": false}))
        .unwrap();
    frame["sha256"].as_str().unwrap().to_string()
}

/// Press `code`, hold it for `hold` of wall time, release (what the faceplate does), and let the firmware answer.
fn click(handle: &Handle, code: u64, hold: Duration) {
    handle
        .call("input.button", json!({"code": code, "down": true}))
        .unwrap();
    sleep(hold);
    handle
        .call("input.button", json!({"code": code, "down": false}))
        .unwrap();
    sleep(Duration::from_millis(600));
}

#[test]
fn a_free_running_coldfire_only_runtime_follows_the_wall_clock_and_a_click_is_a_short_press() {
    let Some(syx) = dt2_syx() else { return };
    let dir = std::env::temp_dir().join(format!("digi-free-pace-{}", std::process::id()));
    let mut config = Config::new(None);
    config.run_dir = dir.join("run");
    config.pace_free = true;
    let (handle, _join) = spawn(config);
    handle
        .call("firmware.load", json!({"syx": syx.display().to_string()}))
        .unwrap();
    // Boot to the main screen with steps (a job is never paced), close FILE SYSTEM OK.
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
    handle
        .call("input.tap", json!({"code": NO, "hold": 20 * MS}))
        .unwrap();
    step_ms(&handle, 400);
    let main_screen = frame_hash(&handle);

    handle.call("run.mode", json!({"mode": "free"})).unwrap();
    sleep(Duration::from_millis(300));
    let (start, before) = (Instant::now(), elapsed(&handle));
    sleep(Duration::from_millis(1500));
    let (wall, ran) = (
        start.elapsed().as_secs_f64(),
        (elapsed(&handle) - before) as f64 / 132e6,
    );
    assert!(
        (0.5 * wall..=1.3 * wall).contains(&ran),
        "free running paced: {ran:.2} emulated s in {wall:.2} s of wall time"
    );

    // a click of 150 ms of wall time opens the settings, as on a Digitone II
    click(&handle, SETTINGS, Duration::from_millis(150));
    let settings = frame_hash(&handle);
    assert_ne!(
        settings, main_screen,
        "SETTINGS did not open the settings screen"
    );
    handle.call("quit", json!({})).ok();
}
