//! Replay a frame pack through the SHARC+ core built for WebAssembly
//! (`native/sharc-jit/wasm-frames`) under wasmtime, timed like
//! `sharc-frames` (and `js/wasm_frames.mjs` for V8).
//!
//!     wasm-frames MODULE.wasm PACK [--repeat R] [--frames N] [--check HASHES]
//!                 [--no-blocks]

use sharc_jit::host;
use std::time::Instant;
use wasmtime::{Config, Engine, Module, OptLevel, Store};

fn main() -> wasmtime::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let (wasm, pack) = (&args[1], &args[2]);
    let o = host::options(&args);
    let mut cfg = Config::new();
    cfg.cranelift_opt_level(OptLevel::Speed);
    let engine = Engine::new(&cfg)?;
    let t0 = Instant::now();
    let module = Module::from_file(&engine, wasm)?;
    let compile = t0.elapsed();
    let mut store = Store::new(&engine, Instant::now());
    let inst = host::linker(&engine)?.instantiate(&mut store, &module)?;
    let s = host::run_pack(&mut store, &inst, pack, &o)?;
    s.print("wasmtime", o.repeat);
    println!(
        "compile {:.0} ms (parallel), module {:.1} MB",
        compile.as_secs_f64() * 1e3,
        std::fs::metadata(wasm)?.len() as f64 / 1e6
    );
    if s.differing.unwrap_or(0) > 0 {
        std::process::exit(1);
    }
    Ok(())
}
