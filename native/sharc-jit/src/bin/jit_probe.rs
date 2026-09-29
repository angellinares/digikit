//! P1 gate probe: block code as separately compiled WebAssembly modules.
//!
//!     jit-probe split MAIN.wasm OUTDIR
//!         Split a wasm-frames build (src/split.rs) into OUTDIR/runtime.wasm
//!         and OUTDIR/parts/*.wasm, with OUTDIR/manifest.tsv
//!         (file, functions, bytes, slot:export list, region).
//!     jit-probe run OUTDIR PACK [--check H] [--repeat R] [--frames N]
//!                   [--threads N] [--in-module] [--opt none|speed]
//!         Compile every part (one at a time, single-threaded: the per-block
//!         latency; then all parts over N threads: the hot set's wall
//!         time), instantiate them against the runtime, install their
//!         entries in its table and replay the pack. --in-module keeps the
//!         runtime's own block functions (the baseline).

use sharc_jit::{host, split};
use std::time::Instant;
use wasmtime::{Config, Engine, Module, OptLevel, Ref, Store};

fn pct(v: &mut [f64], q: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() - 1) as f64 * q).round() as usize]
}

fn main() -> wasmtime::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("split") => {
            let bytes = std::fs::read(&args[2])?;
            let out = std::path::Path::new(&args[3]);
            std::fs::create_dir_all(out.join("parts"))?;
            let t = Instant::now();
            let s = split::split(&bytes);
            let dt = t.elapsed();
            std::fs::write(out.join("runtime.wasm"), &s.runtime)?;
            let mut manifest = String::new();
            let mut total = 0;
            for (i, p) in s.parts.iter().enumerate() {
                let file = format!("parts/{i:04}.wasm");
                std::fs::write(out.join(&file), &p.bytes)?;
                total += p.bytes.len();
                let ents: Vec<String> = p.entries.iter().map(|(s, e)| format!("{s}:{e}")).collect();
                manifest.push_str(&format!(
                    "{file}\t{}\t{}\t{}\t{}\n",
                    p.functions,
                    p.bytes.len(),
                    ents.join(","),
                    p.name
                ));
            }
            std::fs::write(out.join("manifest.tsv"), manifest)?;
            println!(
                "{} parts, {} entries, {:.2} MB of block code, runtime {:.2} MB; split {:.0} ms",
                s.parts.len(),
                s.parts.iter().map(|p| p.entries.len()).sum::<usize>(),
                total as f64 / 1e6,
                s.runtime.len() as f64 / 1e6,
                dt.as_secs_f64() * 1e3
            );
        }
        Some("run") => run(&args)?,
        _ => {
            eprintln!("usage: jit-probe split MAIN.wasm OUTDIR | run OUTDIR PACK [options]");
            std::process::exit(2);
        }
    }
    Ok(())
}

struct PartInfo {
    file: String,
    bytes: usize,
    entries: Vec<(u32, String)>,
}

fn run(args: &[String]) -> wasmtime::Result<()> {
    let dir = std::path::Path::new(&args[2]);
    let pack = &args[3];
    let o = host::options(args);
    let threads: usize = host::arg(args, "--threads").unwrap_or(8);
    let in_module = args.iter().any(|a| a == "--in-module");
    let opt = match host::arg::<String>(args, "--opt").as_deref() {
        Some("none") => OptLevel::None,
        _ => OptLevel::Speed,
    };
    let parts: Vec<PartInfo> = std::fs::read_to_string(dir.join("manifest.tsv"))?
        .lines()
        .map(|l| {
            let c: Vec<&str> = l.split('\t').collect();
            PartInfo {
                file: c[0].to_string(),
                bytes: c[2].parse().unwrap(),
                entries: c[3]
                    .split(',')
                    .map(|e| {
                        let (s, n) = e.split_once(':').unwrap();
                        (s.parse().unwrap(), n.to_string())
                    })
                    .collect(),
            }
        })
        .collect();
    let bytes: Vec<Vec<u8>> = parts
        .iter()
        .map(|p| std::fs::read(dir.join(&p.file)))
        .collect::<Result<_, _>>()?;

    // Per-part latency: one thread, one part at a time.
    let mut cfg = Config::new();
    cfg.cranelift_opt_level(opt).parallel_compilation(false);
    let serial = Engine::new(&cfg)?;
    let mut lat = Vec::new();
    let t = Instant::now();
    for b in &bytes {
        let t = Instant::now();
        Module::new(&serial, b)?;
        lat.push(t.elapsed().as_secs_f64() * 1e6);
    }
    let serial_total = t.elapsed();
    let per_kb: Vec<f64> = lat
        .iter()
        .zip(&parts)
        .map(|(us, p)| us / (p.bytes as f64 / 1024.0))
        .collect();
    let total_kb = parts.iter().map(|p| p.bytes).sum::<usize>() as f64 / 1024.0;
    println!(
        "compile per part (1 thread): median {:.0} us, p90 {:.0}, max {:.0}; {:.1} us/KB median; all {} parts {:.0} ms ({:.0} KB)",
        pct(&mut lat.clone(), 0.5),
        pct(&mut lat.clone(), 0.9),
        pct(&mut lat.clone(), 1.0),
        pct(&mut per_kb.clone(), 0.5),
        parts.len(),
        serial_total.as_secs_f64() * 1e3,
        total_kb
    );

    // The whole set over THREADS threads (engine-internal parallelism off).
    let mut cfg = Config::new();
    cfg.cranelift_opt_level(opt).parallel_compilation(false);
    let engine = Engine::new(&cfg)?;
    let t = Instant::now();
    let chunk = bytes.len().div_ceil(threads);
    let modules: Vec<Module> = std::thread::scope(|s| {
        let hs: Vec<_> = bytes
            .chunks(chunk)
            .map(|c| {
                let e = &engine;
                s.spawn(move || {
                    c.iter()
                        .map(|b| Module::new(e, b).unwrap())
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        hs.into_iter().flat_map(|h| h.join().unwrap()).collect()
    });
    println!(
        "compile all parts over {threads} threads: {:.0} ms wall",
        t.elapsed().as_secs_f64() * 1e3
    );

    let t = Instant::now();
    let runtime = Module::from_file(&engine, dir.join("runtime.wasm"))?;
    let rt_compile = t.elapsed();
    let mut store = Store::new(&engine, Instant::now());
    let mut linker = host::linker(&engine)?;
    let rt = linker.instantiate(&mut store, &runtime)?;
    linker.instance(&mut store, "env", rt)?;
    let table = rt.get_table(&mut store, "t0").expect("t0");
    let t = Instant::now();
    let mut installed = 0;
    if !in_module {
        for (m, p) in modules.iter().zip(&parts) {
            let inst = linker.instantiate(&mut store, m)?;
            for (slot, name) in &p.entries {
                let f = inst.get_func(&mut store, name).expect("entry export");
                table.set(&mut store, *slot as u64, Ref::Func(Some(f)))?;
                installed += 1;
            }
        }
    }
    println!(
        "runtime compile {:.0} ms (1 thread); instantiate {} parts and install {installed} entries: {:.1} ms",
        rt_compile.as_secs_f64() * 1e3,
        if in_module { 0 } else { parts.len() },
        t.elapsed().as_secs_f64() * 1e3
    );
    let s = host::run_pack(&mut store, &rt, pack, &o)?;
    s.print(
        if in_module {
            "blocks in the runtime module"
        } else {
            "blocks in their own modules"
        },
        o.repeat,
    );
    if s.differing.unwrap_or(0) > 0 {
        std::process::exit(1);
    }
    Ok(())
}
