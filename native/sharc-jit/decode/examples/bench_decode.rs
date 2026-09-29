//! Speed check (not part of the library): ns/`decode_at` over a real image,
//! repeated across its VISA code range(s) enough times for a stable median.
//! Firmware-derived input only (a `pack_image` blob), read by this binary.
//!
//! Usage:
//!     cargo run --release --example bench_decode -- IMAGE.pack

use std::env;
use std::fs;
use std::hint::black_box;
use std::process::ExitCode;
use std::time::Instant;

use sharc_decode::{Decoder, ShortWords, parse_pack_image};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() != 2 {
        eprintln!("usage: bench_decode IMAGE.pack");
        return ExitCode::FAILURE;
    }
    let pack_bytes = fs::read(&args[1]).expect("read pack image");
    let image = parse_pack_image(&pack_bytes).expect("parse pack image");
    let decoder = Decoder::new();

    // A short-word PC range that is mapped (VISA alias window seen in every
    // shipped image so far): pick the first PC in it that actually decodes,
    // then walk forward -- covers a realistic mix of confident/uncertain/
    // unknown decodes rather than one instruction's own fast path.
    let base: u32 = 0x120000;
    let span: u32 = 0x10000;

    // Warm up (branch predictor, and this process's own allocator).
    let mut sink: u64 = 0;
    for pc in base..base + span {
        sink = sink.wrapping_add(decoder.decode_at(&image, pc).fields.len() as u64);
    }
    black_box(sink);

    let iters = 20u32;
    let mut best_ns_per_decode = f64::INFINITY;
    for _ in 0..iters {
        let t0 = Instant::now();
        let mut acc: u64 = 0;
        for pc in base..base + span {
            let d = decoder.decode_at(&image, pc);
            acc = acc.wrapping_add(d.length_bytes.unwrap_or(0) as u64);
        }
        black_box(acc);
        let dt = t0.elapsed();
        let ns_per = dt.as_nanos() as f64 / span as f64;
        if ns_per < best_ns_per_decode {
            best_ns_per_decode = ns_per;
        }
    }

    println!("decodes per pass: {span}");
    println!("passes: {iters}");
    println!("best ns/decode_at: {best_ns_per_decode:.2}");

    // Confirm SegmentImage's own read_sw is not the bottleneck: raw reads
    // without any decode logic, same PC range, largest window each time.
    let t0 = Instant::now();
    let mut acc: u64 = 0;
    for pc in base..base + span {
        if let Some(w) = image.read_sw(pc, 6) {
            acc = acc.wrapping_add(w[0] as u64);
        }
    }
    black_box(acc);
    let dt = t0.elapsed();
    println!("ns/read_sw(6): {:.2}", dt.as_nanos() as f64 / span as f64);

    ExitCode::SUCCESS
}
