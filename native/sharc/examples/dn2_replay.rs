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

use sharc_native::Engine;
use sharc_native::frames::hex;
use sharc_native::sha256::Sha256;
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
        let idle0 = e.idle_stats.instructions;
        let t = Instant::now();
        let c = cpu_time();
        let n = if chunk > 0 {
            // native/boot's SharcPeer: CHUNK-instruction steps until the PC
            // is in the idle range at a step's end, then the rest at once.
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
        };
        let dc = cpu_time() - c;
        let dt = t.elapsed().as_secs_f64();
        total += n;
        if (m0..m1).contains(&i) {
            timed += n;
            timed_idle += e.idle_stats.instructions - idle0;
            timed_wall += dt;
            timed_cpu += dc;
        }
        if let Some(h) = &e.halt {
            println!("halt at frame {i}: {h}");
            break;
        }
        if let Some(block) = e.sport_block(None).expect("sport block") {
            pcm.update(&block);
        }
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
    println!("stats={:?}", e.stats);
    println!("model={:?}", e.model_stats);
}
