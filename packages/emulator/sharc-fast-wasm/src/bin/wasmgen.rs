//! `wasmgen DUMP_DIR OUT_DIR`: turn the kernels of a `SHARC_FAST_DUMP`
//! directory into WebAssembly modules for the V8 gate (`gate.mjs`).
//!
//! Writes `OUT_DIR/kernel-<kid>.wasm` (one module each), `OUT_DIR/all.wasm`
//! (every kernel in one module) and prints the emit time and size.

use sharc_native::fast::ir::Kernel;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: wasmgen DUMP_DIR OUT_DIR");
        std::process::exit(2);
    }
    let (dump, out) = (&args[1], &args[2]);
    std::fs::create_dir_all(out).unwrap();
    let mut kernels: Vec<(String, Kernel)> = Vec::new();
    let mut names: Vec<_> = std::fs::read_dir(dump)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("kernel-") && n.ends_with(".bin"))
        .collect();
    names.sort();
    for n in names {
        let bytes = std::fs::read(format!("{dump}/{n}")).unwrap();
        let words: Vec<u32> = bytes
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
            .collect();
        match Kernel::decode(&words) {
            Ok(k) => kernels.push((n.trim_end_matches(".bin").to_string(), k)),
            Err(e) => eprintln!("{n}: {e}"),
        }
    }
    let t0 = Instant::now();
    let mut total = 0usize;
    let mut failed = 0;
    for (n, k) in &kernels {
        match sharc_fast_wasm::emit_kernel(k) {
            Ok(m) => {
                total += m.len();
                std::fs::write(format!("{out}/{n}.wasm"), m).unwrap();
            }
            Err(e) => {
                failed += 1;
                eprintln!("{n}: {e}");
            }
        }
    }
    let dt = t0.elapsed();
    let all: Vec<&Kernel> = kernels.iter().map(|(_, k)| k).collect();
    let t1 = Instant::now();
    match sharc_fast_wasm::emit_module(&all, 1) {
        Ok(m) => {
            println!(
                "all.wasm: {} kernels, {} bytes, emitted in {:?}",
                all.len(),
                m.len(),
                t1.elapsed()
            );
            std::fs::write(format!("{out}/all.wasm"), m).unwrap();
        }
        Err(e) => eprintln!("all: {e}"),
    }
    let insts: usize = kernels.iter().map(|(_, k)| k.inst_count()).sum();
    println!(
        "{} kernels ({} IR insts), {} failed, {} wasm bytes, emit {:?} ({:.1} us per kernel)",
        kernels.len(),
        insts,
        failed,
        total,
        dt,
        dt.as_secs_f64() * 1e6 / kernels.len().max(1) as f64
    );
}
