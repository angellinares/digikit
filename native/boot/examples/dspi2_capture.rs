//! Headless Emulator run that records every ColdFire->DSP DSPI2 frame.
//!
//! usage: dspi2_capture SYX OUT.dt2cap [EXTRA_AFTER_READY=60000000] [SSI_HZ=0 (off; 96000 = audio)]
//! Follows the handover QA sequence (NO button at ready and +20M, encoder A
//! at +40M) so the instruction counts match its table.

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
    emu.record_dspi2(&sha);
    let ssi_hz: u64 = args.get(4).map_or(0, |v| v.parse().unwrap());
    if ssi_hz != 0 {
        emu.enable_ssi_diagnostic(ssi_hz).unwrap();
    }
    let mut ready_at: Option<u64> = None;
    let mut stage = 0;
    let mut next = 0;
    let end = loop {
        let snap = emu.step_chunk(250_000);
        let s = &snap.status;
        if s.icount >= next {
            println!("icount={} ready={}", s.icount, s.ready);
            next = s.icount + 50_000_000;
        }
        if s.ready && ready_at.is_none() {
            ready_at = Some(s.icount);
            println!("ready_at={}", s.icount);
            emu.button(12, true).unwrap();
            stage = 1;
        }
        if let Some(at) = ready_at {
            let elapsed = s.icount - at;
            if stage == 1 && elapsed >= 1_000_000 {
                emu.button(12, false).unwrap();
                stage = 2;
            }
            if stage == 2 && elapsed >= 20_000_000 {
                emu.button(12, true).unwrap();
                stage = 3;
            }
            if stage == 3 && elapsed >= 21_000_000 {
                emu.button(12, false).unwrap();
                stage = 4;
            }
            if stage == 4 && elapsed >= 40_000_000 {
                emu.turn(1, 1).unwrap();
                stage = 5;
            }
        }
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
    fs::write(&args[2], &bytes).unwrap();
    println!("final_icount={end} capture_bytes={}", bytes.len());
}
