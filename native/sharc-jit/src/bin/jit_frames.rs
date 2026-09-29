//! A frame pack through the JIT (tools/sharc_transpile_run.py pack), timed
//! like `sharc-frames`: per frame, the best of N repetitions of the block
//! handler call; the median over frames 1.. is reported. The first
//! repetition warms the JIT (regions are translated as they turn hot).
//!
//!   jit-frames PACK [--repeat N] [--frames N] [--threshold T] [--no-jit]
//!              [--check HASHES] [--record HASHES] [--modules DIR]
//!              [--patch ADDR:BYTE,...]

use sharc_jit::jit::Machine;
use std::time::Instant;
use wasmtime::TypedFunc;

fn arg<T: std::str::FromStr>(args: &[String], name: &str) -> Option<T> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
}

fn main() -> wasmtime::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let pack_path = args.get(1).expect("usage: jit-frames PACK [options]");
    let repeat: usize = arg(&args, "--repeat").unwrap_or(3);
    let nframes: Option<usize> = arg(&args, "--frames");
    let threshold: u32 = arg(&args, "--threshold").unwrap_or(2);
    let nojit = args.iter().any(|a| a == "--no-jit");
    let check: Option<String> = arg(&args, "--check");
    let record: Option<String> = arg(&args, "--record");
    let modules: Option<String> = arg(&args, "--modules");
    let patch: Option<String> = arg(&args, "--patch");
    let verify = args.iter().any(|a| a == "--verify");
    let profile = args.iter().any(|a| a == "--profile");

    let mut pack = std::fs::read(pack_path)?;
    // The pack's image blob (frames.rs layout: magic, version, image blob).
    let img_len = u32::from_le_bytes(pack[8..12].try_into().unwrap()) as usize;
    if let Some(p) = &patch {
        // Patch loader bytes inside the image blob: ADDR:BYTE pairs (loader
        // byte addresses), applied before anything runs.
        let n = patch_image(&mut pack[12..12 + img_len], p);
        println!("patched {n} image bytes");
    }
    let image = pack[12..12 + img_len].to_vec();
    let t0 = Instant::now();
    let mut m = Machine::new(&image)?;
    let t_create = t0.elapsed();
    m.store.data_mut().keep_modules = modules.is_some();
    m.configure(threshold, !nojit)?;
    if verify {
        // --verify-from ICOUNT --verify-to ICOUNT (instruction counts of the
        // current call, as `icount` runs in the runtime).
        let lo: i64 = arg(&args, "--verify-from").unwrap_or(0);
        let hi: i64 = arg(&args, "--verify-to").unwrap_or(0);
        let every: u32 = arg(&args, "--verify-every").unwrap_or(1);
        let f: TypedFunc<(i32, i64, i64, u32), ()> = m.func("jit_set_verify")?;
        f.call(&mut m.store, (1, lo, hi, every))?;
    }
    let p = m.put(&pack)?;
    let open: TypedFunc<(u32, u32), i32> = m.func("jit_pack_open")?;
    let n_all = open.call(&mut m.store, (p, pack.len() as u32))?;
    if n_all < 0 {
        let e = m.out_call("jit_error", 4096)?;
        wasmtime::bail!("{}", String::from_utf8_lossy(&e));
    }
    let n = nframes.unwrap_or(usize::MAX).min(n_all as usize);
    let first: TypedFunc<(), u32> = m.func("jit_pack_first")?;
    let first = first.call(&mut m.store, ())? as usize;
    let reload: TypedFunc<(), i32> = m.func("jit_pack_reload")?;
    let frame: TypedFunc<u32, f64> = m.func("jit_pack_frame")?;
    let last_ns: TypedFunc<(), f64> = m.func("jit_last_ns")?;
    let hash: TypedFunc<u32, ()> = m.func("jit_hash")?;
    let hb = m.alloc(32)?;
    let expect: Option<Vec<String>> = match &check {
        Some(f) => Some(
            std::fs::read_to_string(f)?
                .lines()
                .map(|l| l.split(' ').nth(1).unwrap_or("").to_string())
                .collect(),
        ),
        None => None,
    };
    let mut best = vec![f64::INFINITY; n];
    let mut insns = vec![0f64; n];
    let mut hashes = Vec::new();
    let mut bad = 0;
    let mut warm = Vec::new();
    for rep in 0..repeat {
        let tr = Instant::now();
        if rep > 0 && reload.call(&mut m.store, ())? != 0 {
            wasmtime::bail!("reload failed");
        }
        if profile && rep == repeat - 1 {
            let f: TypedFunc<i32, ()> = m.func("jit_set_profile")?;
            f.call(&mut m.store, 1)?;
        }
        for k in 0..n {
            let c = frame.call(&mut m.store, k as u32)?;
            if c < 0.0 {
                let r = m.out_call("jit_verify_report", 1 << 16)?;
                if !r.is_empty() {
                    println!("{}", String::from_utf8_lossy(&r));
                }
                let e = m.out_call("jit_error", 4096)?;
                wasmtime::bail!("frame {}: {}", first + k, String::from_utf8_lossy(&e));
            }
            best[k] = best[k].min(last_ns.call(&mut m.store, ())?);
            if rep == 0 {
                insns[k] = c;
                hash.call(&mut m.store, hb)?;
                let d: String = m.bytes(hb, 32).iter().map(|b| format!("{b:02x}")).collect();
                if let Some(x) = &expect
                    && x.get(k).map(String::as_str) != Some(d.as_str())
                {
                    bad += 1;
                    if bad <= 5 {
                        println!("frame {}: state differs from the recorded one", first + k);
                    }
                }
                hashes.push(d);
            }
        }
        warm.push(tr.elapsed());
    }
    if let Some(f) = &record {
        let text: String = hashes.iter().enumerate().map(|(k, h)| format!("{} {}\n", first + k, h)).collect();
        std::fs::write(f, text)?;
    }
    let mut us: Vec<f64> = best.iter().skip(1).map(|x| x / 1e3).collect();
    let mut ns: Vec<f64> = best.iter().zip(&insns).skip(1).map(|(t, c)| t / c.max(1.0)).collect();
    let med = |v: &mut Vec<f64>| -> f64 {
        if v.is_empty() {
            return 0.0;
        }
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    };
    let (us_med, ns_med) = (med(&mut us), med(&mut ns));
    let stats = sharc_jit::stats_json(&m);
    let rstats = {
        let p = m.alloc(8 * 9)?;
        let f: TypedFunc<(u32, u32), i32> = m.func("jit_stats")?;
        let k = f.call(&mut m.store, (p, 9))? as usize;
        let b = m.bytes(p, 8 * k);
        b.chunks(8).map(|c| u64::from_le_bytes(c.try_into().unwrap())).collect::<Vec<u64>>()
    };
    let total: u64 = rstats[0];
    let single = rstats[3];
    println!(
        "frames {}-{}: {:.1} us/frame median (frames {}.., best of {}), {:.2} ns/instr; interpreter {:.3}% of {} instructions",
        first,
        first + n - 1,
        us_med,
        first + 1,
        repeat,
        ns_med,
        100.0 * single as f64 / total.max(1) as f64,
        total
    );
    println!(
        "machine {:.0} ms; passes (wall): {}",
        t_create.as_secs_f64() * 1e3,
        warm.iter().map(|d| format!("{:.0} ms", d.as_secs_f64() * 1e3)).collect::<Vec<_>>().join(", ")
    );
    println!("region entries {} traps {} requests {}", rstats[1], rstats[7], rstats[5]);
    println!("jit {stats}");
    if profile {
        let p = m.out_call("jit_profile", 1 << 16)?;
        println!("{}", String::from_utf8_lossy(&p));
    }
    if let Some(x) = &expect {
        println!("check: {}/{} frames match {}", n - bad, n, check.as_deref().unwrap_or(""));
        let _ = x;
    }
    if let Some(dir) = modules {
        std::fs::create_dir_all(&dir)?;
        for (k, (pc, w)) in m.store.data().modules.iter().enumerate() {
            std::fs::write(format!("{dir}/{k:04}_{pc:x}.wasm"), w)?;
        }
    }
    Ok(())
}

/// Apply ADDR:BYTE,... to the loader segments of an image blob in place.
fn patch_image(img: &mut [u8], spec: &str) -> usize {
    let mut n = 0;
    // "SHIM", version, count, then (addr, len, bytes)*.
    let count = u32::from_le_bytes(img[8..12].try_into().unwrap()) as usize;
    for item in spec.split(',') {
        let Some((a, b)) = item.split_once(':') else { continue };
        let a = u32::from_str_radix(a.trim_start_matches("0x"), 16).unwrap();
        let b = u8::from_str_radix(b.trim_start_matches("0x"), 16).unwrap();
        let mut off = 12;
        for _ in 0..count {
            let lo = u32::from_le_bytes(img[off..off + 4].try_into().unwrap());
            let len = u32::from_le_bytes(img[off + 4..off + 8].try_into().unwrap());
            if a >= lo && a < lo + len {
                img[off + 8 + (a - lo) as usize] = b;
                n += 1;
            }
            off += 8 + len as usize;
        }
    }
    n
}
