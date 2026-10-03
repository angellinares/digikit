//! DN2 note-capture replay without Python, for timing and profiling the
//! block path (what `tools/sharc_dn2_replay.py` does, with the option set of
//! `native/boot`'s `open_dn2_engine`).
//!
//!     cargo run --release --example dn2_replay -- IMAGE STATE FRAMES \
//!         [START END [MEAS_START MEAS_END]]
//!
//! IMAGE is the packed image blob (`tools/sharc_pack_image.py`), STATE a
//! canonical state blob, FRAMES the capture's DSPI2 TX frames as
//! `u32 count` then `u32 length, bytes` each. Per frame: the SPI2
//! exchange, 667,000 instructions, one SPORT4 block. It prints the PCM
//! SHA-256, the canonical state SHA-256 after the last frame, and the
//! non-idle instructions per second in the timed frame range. Environment:
//! NO_BLOCKS=1, NO_SKIP=1; SAVE_AT=FRAME SAVE=PATH writes the canonical
//! state before that frame (and prints the CLOCK base that continues it);
//! CLOCK=BASE starts the instruction clock there (default: the note
//! capture's).

use sharc_native::frames::hex;
use sharc_native::sha256::Sha256;
use sharc_native::{Engine, ModelStats, Stats};
use std::time::Instant;

const CLOCK_BASE: i64 = 573_627_620;

/// This thread's CPU time in seconds (steadier than wall time on a busy
/// machine); 0 where there is no thread clock.
fn cpu_time() -> f64 {
    #[cfg(target_os = "macos")]
    {
        #[repr(C)]
        struct Timespec {
            sec: i64,
            nsec: i64,
        }
        unsafe extern "C" {
            fn clock_gettime(clock: u32, tp: *mut Timespec) -> i32;
        }
        const CLOCK_THREAD_CPUTIME_ID: u32 = 16;
        let mut t = Timespec { sec: 0, nsec: 0 };
        // SAFETY: T is a valid out-pointer for the libc call.
        if unsafe { clock_gettime(CLOCK_THREAD_CPUTIME_ID, &mut t) } == 0 {
            return t.sec as f64 + t.nsec as f64 * 1e-9;
        }
    }
    0.0
}
const GAP: u32 = 667_000;

fn stats_delta(after: Stats, before: Stats) -> Stats {
    Stats {
        block_entries: after.block_entries - before.block_entries,
        block_instructions: after.block_instructions - before.block_instructions,
        single_steps: after.single_steps - before.single_steps,
        traps: after.traps - before.traps,
        block_traps: after.block_traps - before.block_traps,
    }
}

fn add_stats(total: &mut Stats, delta: Stats) {
    total.block_entries += delta.block_entries;
    total.block_instructions += delta.block_instructions;
    total.single_steps += delta.single_steps;
    total.traps += delta.traps;
    total.block_traps += delta.block_traps;
}

fn model_stats_delta(after: ModelStats, before: ModelStats) -> ModelStats {
    ModelStats {
        gated: after.gated - before.gated,
        unsafe_block: after.unsafe_block - before.unsafe_block,
        irq_deferred: after.irq_deferred - before.irq_deferred,
        timer_gated: after.timer_gated - before.timer_gated,
        mmr_exits: after.mmr_exits - before.mmr_exits,
        code_mismatch: after.code_mismatch - before.code_mismatch,
    }
}

fn add_model_stats(total: &mut ModelStats, delta: ModelStats) {
    total.gated += delta.gated;
    total.unsafe_block += delta.unsafe_block;
    total.irq_deferred += delta.irq_deferred;
    total.timer_gated += delta.timer_gated;
    total.mmr_exits += delta.mmr_exits;
    total.code_mismatch += delta.code_mismatch;
}

fn write_profile(e: &Engine, prefix: &str) {
    for (kind, suffix) in [
        (0, ".coverage.tsv"),
        (1, ".entries.tsv"),
        (2, ".transitions.tsv"),
        (3, ".exits.tsv"),
        (4, ".entry-bails.tsv"),
    ] {
        let text = e.profile_text(kind).expect("profile enabled");
        std::fs::write(format!("{prefix}{suffix}"), text).expect("write profile");
    }
}

fn read_frames(path: &str) -> Vec<Vec<u8>> {
    let data = std::fs::read(path).expect("frames file");
    let u32_at = |o: usize| u32::from_le_bytes(data[o..o + 4].try_into().unwrap()) as usize;
    let n = u32_at(0);
    let mut out = Vec::with_capacity(n);
    let mut o = 4;
    for _ in 0..n {
        let len = u32_at(o);
        out.push(data[o + 4..o + 4 + len].to_vec());
        o += 4 + len;
    }
    out
}

fn step_workload(e: &mut Engine, chunk: u32) -> u64 {
    if chunk > 0 {
        let mut done = 0u32;
        let mut idle = false;
        while done < GAP {
            let want = if idle {
                GAP - done
            } else {
                chunk.min(GAP - done)
            };
            let ran = e.step(want);
            done += ran;
            idle |= (0xB8_8A49..0xB8_8ABB).contains(&e.s.pc_sw);
            if ran < want {
                break;
            }
        }
        done as u64
    } else {
        e.step(GAP) as u64
    }
}

#[inline(never)]
fn measured_window_step(e: &mut Engine, chunk: u32) -> u64 {
    std::hint::black_box(step_workload(e, chunk))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("usage: dn2_replay IMAGE STATE FRAMES [START END [MEAS_START MEAS_END]]");
        std::process::exit(2);
    }
    let num = |i: usize, d: usize| args.get(i).map_or(d, |s| s.parse().expect("number"));
    let (start, end) = (num(4, 4600), num(5, 5600));
    let (m0, m1) = (num(6, 5400), num(7, 5600));
    let image = std::fs::read(&args[1]).expect("image");
    let state = std::fs::read(&args[2]).expect("state");
    let frames = read_frames(&args[3]);
    // CLOCK: the instruction-clock base (a state SAVE wrote says which).
    let clock_base: i64 = std::env::var("CLOCK").map_or(CLOCK_BASE, |v| v.parse().expect("CLOCK"));
    // CHUNK=N: step as native/boot's SharcPeer does (N-instruction steps
    // until the idle range, then the rest of the frame).
    let chunk: u32 = std::env::var("CHUNK").map_or(0, |v| v.parse().expect("CHUNK"));
    let save_at: Option<usize> = std::env::var("SAVE_AT")
        .ok()
        .map(|v| v.parse().expect("SAVE_AT"));
    let profile_prefix = std::env::var("DN2_REPLAY_PROFILE_PREFIX").ok();
    let repeats: usize =
        std::env::var("DN2_REPLAY_REPEATS").map_or(1, |v| v.parse().expect("DN2_REPLAY_REPEATS"));
    assert!(
        (1..=100).contains(&repeats),
        "DN2_REPLAY_REPEATS must be 1..=100"
    );
    for repeat in 0..repeats {
        let mut e = Engine::from_image(&image).expect("image blob");
        e.import(&state).expect("state blob");
        let blocks = std::env::var("NO_BLOCKS").as_deref() != Ok("1");
        let mut options = vec![
            (1, blocks as i64),
            (4, 1),
            (5, 1),
            (6, clock_base),
            (9, 1),
            (10, 0),
            (11, 1),
            (12, 1),
            (21, 1),
            (22, 1),
        ];
        if std::env::var("NO_SKIP").as_deref() != Ok("1") {
            options.extend([(23, 0xB8_8AAB), (24, 0xB8_8A49), (25, 0xB8_8ABB)]);
        }
        for (k, v) in options {
            assert_eq!(e.set_option(k, v), 0, "option {k}");
        }
        let mut pcm = Sha256::new();
        let (mut total, mut timed, mut timed_idle) = (0u64, 0u64, 0u64);
        let (mut timed_wall, mut timed_cpu) = (0.0f64, 0.0f64);
        let (mut window_stats, mut window_model_stats) = (Stats::default(), ModelStats::default());
        let mut profile_started = false;
        let t0 = Instant::now();
        let c0 = cpu_time();
        for (i, frame) in frames.iter().enumerate().take(end + 1).skip(start) {
            if save_at == Some(i) {
                // A continuation point: the state before this frame, and the
                // clock base that continues the instruction clock.
                e.export_ranges = true;
                let path = std::env::var("SAVE").expect("SAVE");
                std::fs::write(&path, e.export()).expect("write state");
                e.export_ranges = false;
                println!(
                    "saved {path} before frame {i}: CLOCK={}",
                    clock_base as u64 + e.s.icount
                );
            }
            e.spi2_exchange(frame).expect("spi2 exchange");
            let measured = (m0..m1).contains(&i);
            if measured && !profile_started && profile_prefix.is_some() {
                assert_eq!(e.set_option(26, 1), 0, "profile option");
                profile_started = true;
            }
            let idle0 = e.idle_stats.instructions;
            let before_stats = measured.then_some(e.stats);
            let before_model_stats = measured.then_some(e.model_stats);
            let t = Instant::now();
            let c = cpu_time();
            let n = if measured && repeats > 1 {
                measured_window_step(&mut e, chunk)
            } else {
                step_workload(&mut e, chunk)
            };
            let dc = cpu_time() - c;
            let dt = t.elapsed().as_secs_f64();
            total += n;
            if measured {
                timed += n;
                timed_idle += e.idle_stats.instructions - idle0;
                timed_wall += dt;
                timed_cpu += dc;
                add_stats(
                    &mut window_stats,
                    stats_delta(e.stats, before_stats.unwrap()),
                );
                add_model_stats(
                    &mut window_model_stats,
                    model_stats_delta(e.model_stats, before_model_stats.unwrap()),
                );
                if i + 1 == m1 && profile_started {
                    write_profile(&e, profile_prefix.as_deref().unwrap());
                    assert_eq!(e.set_option(26, 0), 0, "profile option");
                    profile_started = false;
                }
            }
            if let Some(h) = &e.halt {
                println!("halt at frame {i}: {h}");
                break;
            }
            if let Some(block) = e.sport_block(None).expect("sport block") {
                pcm.update(&block);
            }
        }
        if profile_started {
            write_profile(&e, profile_prefix.as_deref().unwrap());
            assert_eq!(e.set_option(26, 0), 0, "profile option");
        }
        let wall = t0.elapsed().as_secs_f64();
        let cpu = cpu_time() - c0;
        e.export_ranges = true;
        let mut h = Sha256::new();
        h.update(&e.export());
        let busy = timed - timed_idle;
        println!("pcm={}", hex(&pcm.finish()));
        println!("state={}", hex(&h.finish()));
        println!("total={total} wall={wall:.3} cpu={cpu:.3}");
        println!(
            "meas frames {m0}..{m1}: instr={timed} idle={timed_idle} busy={busy} wall={timed_wall:.3} cpu={timed_cpu:.3} busy_per_s={:.1}M busy_per_cpu_s={:.1}M",
            busy as f64 / timed_wall / 1e6,
            busy as f64 / timed_cpu / 1e6
        );
        let unknown: Vec<usize> = (0..e.s.r.len()).filter(|&c| !e.s.r[c].is_c()).collect();
        println!("registers not known at the end: {unknown:?}");
        println!("windowed stats={window_stats:?}");
        println!("windowed model={window_model_stats:?}");
        println!("cumulative stats={:?}", e.stats);
        println!("cumulative model={:?}", e.model_stats);
        if repeats > 1 {
            println!("repeat={}", repeat + 1);
        }
    }
}
