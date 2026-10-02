//! Headless Emulator run that records every ColdFire->DSP DSPI2 frame.
//!
//! usage: dspi2_capture SYX OUT.dt2cap [EXTRA_AFTER_READY=60000000] [SSI_HZ=0 (off; 96000 = audio)]
//! Follows the handover QA sequence (NO button at ready and +20M, encoder A
//! at +40M) so the instruction counts match its table.
//! CF_SNAPSHOT=path (env): restore the machine from `path` if it exists (skips
//! the boot), else boot and save it at ready, before the QA inputs; the
//! capture then starts at ready in both modes. The SSI_HZ argument must match
//! the saving run. CF_DIGEST=1 prints the capture hash and a machine-state
//! digest at the end (determinism check).
//! NOTE_EVENTS=trig|play (env) adds panel key events after that sequence:
//! trig = TRIG 1 (code 25) held 40M instructions at +60M/+110M/+160M;
//! play = PLAY (code 20) at +60M held 1M, STOP (code 21) at +200M.

mod common;

use elektron_native_boot::Emulator;
use sha2::{Digest, Sha256};
use std::{env, fs};

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: dspi2_capture SYX OUT.dt2cap [EXTRA_AFTER_READY]");
        std::process::exit(2);
    }
    let syx = fs::read(&args[1]).unwrap();
    let extra: u64 = args.get(3).map_or(60_000_000, |v| v.parse().unwrap());
    let sha: String = Sha256::digest(&syx)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let mut emu = Emulator::new(&syx, None).unwrap();
    let ssi_hz: u64 = args.get(4).map_or(0, |v| v.parse().unwrap());
    if ssi_hz != 0 {
        emu.enable_ssi_diagnostic(ssi_hz).unwrap();
    }
    let mut script = common::Script::from_env();
    // CF_SNAPSHOT=path: restore (skip boot) or boot-and-save at ready; the
    // capture then starts at ready in both modes (see common::snapshot_path).
    let snap_path = common::snapshot_path();
    let mut saved = false;
    let restored = snap_path
        .as_deref()
        .and_then(|p| common::restore(&mut emu, p, &mut script))
        .is_some();
    if snap_path.is_none() || restored {
        emu.record_dspi2(&sha);
    }
    let mut next = 0;
    let end = loop {
        let snap = emu.step_chunk(250_000);
        let s = &snap.status;
        if s.icount >= next {
            println!("icount={} ready={}", s.icount, s.ready);
            next = s.icount + 50_000_000;
        }
        if let Some(p) = &snap_path
            && common::save_if_ready(&mut emu, p, &script, s.ready, &mut saved)
        {
            emu.record_dspi2(&sha);
        }
        script.poll(&mut emu, s.icount, s.ready);
        let ready_at = script.ready_at;
        if ready_at.is_some_and(|at| s.icount - at >= extra)
            || s.error.is_some()
            || s.icount >= 1_100_000_000
        {
            println!("end icount={} error={:?}", s.icount, s.error);
            break s.icount;
        }
    };
    let d = emu.diagnostics();
    println!(
        "dspi_exchanges={} dspi_tx_bytes={}",
        d.dspi_exchanges, d.dspi_tx_bytes
    );
    println!("ssi_counters={:?}", emu.ssi_diagnostic_counters());
    let bytes = emu.dspi2_capture_bytes().unwrap();
    if env::var_os("CF_DIGEST").is_some() {
        // Determinism check: machine-state digest (CPU, RAM, peripherals)
        // and the hash of the emitted DSPI2 frames.
        let cap: String = Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        println!("capture_sha256={cap}");
        println!("state_digest={}", emu.state_digest().unwrap());
    }
    fs::write(&args[2], &bytes).unwrap();
    println!("final_icount={end} capture_bytes={}", bytes.len());
}
