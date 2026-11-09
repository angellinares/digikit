//! `compile_par KDUMP_DIR [THREADS...]`: compile the kernels of a
//! `SHARC_FAST_TL_DUMP` directory to machine code on 1..N threads, then load
//! every blob back with `define_function_bytes` and report the wall times,
//! sizes and relocation counts. Measurement for
//! docs/design/fast-compile-offload.md (background compile and code cache).

use sharc_fast_cl::{ClBackend, CodeBlob, load_blob};
use sharc_native::fast::ir::{EntryChecks, Kernel};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = &args[1];
    let threads: Vec<usize> = if args.len() > 2 {
        args[2..].iter().map(|a| a.parse().unwrap()).collect()
    } else {
        vec![1, 4, 8]
    };
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("kernel-") && n.ends_with(".bin"))
        .collect();
    names.sort();
    let kernels: Vec<Kernel> = names
        .iter()
        .filter_map(|n| {
            let bytes = std::fs::read(format!("{dir}/{n}")).ok()?;
            let words: Vec<u32> = bytes
                .chunks_exact(4)
                .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
                .collect();
            Kernel::decode(&words).ok()
        })
        .collect();
    println!("{} kernels", kernels.len());
    let mut blobs: Vec<CodeBlob> = Vec::new();
    for &t in &threads {
        let next = AtomicUsize::new(0);
        let t0 = Instant::now();
        let mut out: Vec<Vec<(usize, CodeBlob)>> = Vec::new();
        std::thread::scope(|s| {
            let hs: Vec<_> = (0..t)
                .map(|_| {
                    s.spawn(|| {
                        let mut be = ClBackend::default();
                        let mut mine = Vec::new();
                        loop {
                            let i = next.fetch_add(1, Ordering::Relaxed);
                            if i >= kernels.len() {
                                break;
                            }
                            if let Ok(b) = be.compile_blob(&kernels[i], &EntryChecks::default()) {
                                mine.push((i, b));
                            }
                        }
                        mine
                    })
                })
                .collect();
            for h in hs {
                out.push(h.join().unwrap());
            }
        });
        let wall = t0.elapsed();
        let mut all: Vec<(usize, CodeBlob)> = out.into_iter().flatten().collect();
        all.sort_by_key(|x| x.0);
        let bytes: usize = all.iter().map(|x| x.1.bytes.len()).sum();
        let relocs: usize = all.iter().map(|x| x.1.relocs).sum();
        println!(
            "compile on {t} thread(s): {} kernels, wall {:.2} s ({:.2} ms per kernel per wall), {:.1} MB code, relocs {relocs}",
            all.len(),
            wall.as_secs_f64(),
            wall.as_secs_f64() * 1e3 / all.len() as f64,
            bytes as f64 / 1e6,
        );
        blobs = all.into_iter().map(|x| x.1).collect();
    }
    let t0 = Instant::now();
    let mut loaded = Vec::new();
    let mut worst = 0f64;
    for b in &blobs {
        let t = Instant::now();
        loaded.push(load_blob(b).expect("load"));
        worst = worst.max(t.elapsed().as_secs_f64());
    }
    let wall = t0.elapsed();
    println!(
        "load {} blobs with define_function_bytes: {:.3} s ({:.1} us each, worst {:.2} ms)",
        loaded.len(),
        wall.as_secs_f64(),
        wall.as_secs_f64() * 1e6 / loaded.len() as f64,
        worst * 1e3
    );
    // Is the machine code the same when compiled twice (a stable cache key)?
    let mut be = ClBackend::default();
    let a = be
        .compile_blob(&kernels[0], &EntryChecks::default())
        .unwrap();
    let b = be
        .compile_blob(&kernels[0], &EntryChecks::default())
        .unwrap();
    println!("deterministic bytes: {}", a.bytes == b.bytes);
}
