//! Run a DSP cold-start state (tools/sharc_dev/sharc_cold_state.py) with no ColdFire
//! until the engine first rests in the DN2 idle range, then write its
//! canonical state as a DT2DSP01 sidecar for `sharc_live` / `coupled-run`.
//!
//!   dsp_cold IMAGE STATE_IN OUT.dsp [MAX_INSTRUCTIONS]
//!
//! env: DSP_PATCH=FILE overlays a DSP patch (packages/emulator/boot/src/dsp_patch.rs).
//! Build: SHARC_GEN_DIR=<generated dir> cargo build --release --features
//! sharc --example dsp_cold. Output is firmware-derived: keep it out of the
//! repository.

use elektron_native_boot::dsp_patch::DspPatch;
use elektron_native_boot::sharc_peer::{DN2_IDLE, DT2_IDLE, open_engine_patched};
use sha2::{Digest, Sha256};
use std::{env, fs, time::Instant};

fn main() {
    let a: Vec<String> = env::args().collect();
    if a.len() < 4 {
        eprintln!("usage: dsp_cold IMAGE STATE_IN OUT.dsp [MAX_INSTRUCTIONS]");
        std::process::exit(2);
    }
    let image = fs::read(&a[1]).unwrap();
    let state = fs::read(&a[2]).unwrap();
    let max: u64 = a.get(4).map_or(800_000_000, |v| v.parse().unwrap());
    let patch = env::var_os("DSP_PATCH").map(|p| DspPatch::load(p.as_ref()).unwrap());
    let idle = if env::var("DSP_DEVICE").as_deref() == Ok("dt2") {
        DT2_IDLE
    } else {
        DN2_IDLE
    };
    let mut e = open_engine_patched(&image, &state, 0, patch.as_ref(), idle).expect("engine");
    let t0 = Instant::now();
    let mut done = 0u64;
    let mut rested = 0u32;
    while done < max {
        let n = e.step(250_000) as u64;
        done += n;
        let pc = e.s.pc_sw as u32;
        if e.halt.is_some() {
            println!("halt at {done}: {:?} pc={pc:#x}", e.halt);
            break;
        }
        if (idle.range.0..=idle.range.1).contains(&pc) {
            rested += 1;
            if rested >= 8 {
                println!("idle: pc={pc:#x} after {} instructions", e.s.icount);
                break;
            }
        } else {
            rested = 0;
        }
        if done % 50_000_000 < 250_000 {
            println!(
                "{done} instructions pc={pc:#x} wall={:.0}s",
                t0.elapsed().as_secs_f64()
            );
        }
    }
    let exported = e.export();
    let mut f = b"DT2DSP01".to_vec();
    f.extend(e.s.icount.to_le_bytes()); // clock tick (base 0)
    f.extend(e.s.icount.to_le_bytes()); // DSP instructions so far
    f.extend(&exported);
    fs::write(&a[3], &f).unwrap();
    println!(
        "wrote {} ({} KB) state_sha256={:x} icount={} wall={:.0}s",
        a[3],
        f.len() >> 10,
        Sha256::digest(&exported),
        e.s.icount,
        t0.elapsed().as_secs_f64()
    );
}
