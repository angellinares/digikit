//! wasmtime host for the `wasm-frames` module: the clock import and the
//! timed frame-pack replay (the same figure as `sharc-frames` and
//! `js/wasm_frames.mjs`).

use std::time::Instant;
use wasmtime::{Engine, Instance, Linker, Memory, Store, TypedFunc};

pub type St = Store<Instant>;

/// A linker with `host.now_ns`.
pub fn linker(engine: &Engine) -> wasmtime::Result<Linker<Instant>> {
    let mut l = Linker::new(engine);
    l.func_wrap("host", "now_ns", |c: wasmtime::Caller<'_, Instant>| {
        c.data().elapsed().as_nanos() as f64
    })?;
    Ok(l)
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

pub struct Options {
    pub repeat: usize,
    pub frames: Option<usize>,
    pub check: Option<String>,
    pub blocks: bool,
}

pub struct Summary {
    pub first: usize,
    pub frames: usize,
    pub us_per_frame: f64,
    pub ns_per_instr: f64,
    pub instructions: f64,
    pub generic_pct: f64,
    /// Frames whose state hash differs from --check (None: not checked).
    pub differing: Option<usize>,
    pub memory_mb: usize,
}

fn error(store: &mut St, inst: &Instance, mem: Memory) -> String {
    let alloc: TypedFunc<u32, u32> = inst.get_typed_func(&mut *store, "sw_alloc").unwrap();
    let err: TypedFunc<(u32, u32), i32> = inst.get_typed_func(&mut *store, "sw_error").unwrap();
    let p = alloc.call(&mut *store, 4096).unwrap();
    let n = err.call(&mut *store, (p, 4096)).unwrap() as usize;
    String::from_utf8_lossy(&mem.data(&*store)[p as usize..p as usize + n]).into_owned()
}

/// Load PACK into the instance and replay it.
pub fn run_pack(
    store: &mut St,
    inst: &Instance,
    pack: &str,
    o: &Options,
) -> wasmtime::Result<Summary> {
    let mem = inst.get_memory(&mut *store, "memory").expect("memory");
    let alloc: TypedFunc<u32, u32> = inst.get_typed_func(&mut *store, "sw_alloc")?;
    let open: TypedFunc<(u32, u32), i32> = inst.get_typed_func(&mut *store, "sw_open")?;
    let set_blocks: TypedFunc<i32, ()> = inst.get_typed_func(&mut *store, "sw_blocks")?;
    let reload: TypedFunc<(), i32> = inst.get_typed_func(&mut *store, "sw_reload")?;
    let frame: TypedFunc<u32, f64> = inst.get_typed_func(&mut *store, "sw_frame")?;
    let last_ns: TypedFunc<(), f64> = inst.get_typed_func(&mut *store, "sw_last_ns")?;
    let first: TypedFunc<(), u32> = inst.get_typed_func(&mut *store, "sw_first")?;
    let hash: TypedFunc<u32, ()> = inst.get_typed_func(&mut *store, "sw_hash")?;
    let single: TypedFunc<(), f64> = inst.get_typed_func(&mut *store, "sw_single_steps")?;

    let bytes = std::fs::read(pack)?;
    let p = alloc.call(&mut *store, bytes.len() as u32)?;
    mem.data_mut(&mut *store)[p as usize..p as usize + bytes.len()].copy_from_slice(&bytes);
    let n_all = open.call(&mut *store, (p, bytes.len() as u32))?;
    if n_all < 0 {
        wasmtime::bail!("{}", error(store, inst, mem));
    }
    let n = o.frames.unwrap_or(usize::MAX).min(n_all as usize);
    set_blocks.call(&mut *store, o.blocks as i32)?;
    let first = first.call(&mut *store, ())? as usize;
    let expect: Option<Vec<String>> = match &o.check {
        Some(f) => Some(
            std::fs::read_to_string(f)?
                .lines()
                .map(|l| l.split(' ').nth(1).unwrap_or("").to_string())
                .collect(),
        ),
        None => None,
    };
    let hb = alloc.call(&mut *store, 32)?;
    let mut best = vec![f64::INFINITY; n];
    let mut insns = vec![0f64; n];
    let (mut bad, mut total) = (0, 0f64);
    for rep in 0..o.repeat {
        if rep > 0 && reload.call(&mut *store, ())? != 0 {
            wasmtime::bail!("{}", error(store, inst, mem));
        }
        for k in 0..n {
            let c = frame.call(&mut *store, k as u32)?;
            if c < 0.0 {
                wasmtime::bail!("{}", error(store, inst, mem));
            }
            best[k] = best[k].min(last_ns.call(&mut *store, ())?);
            if rep == 0 {
                insns[k] = c;
                total += c;
                if let Some(x) = &expect {
                    hash.call(&mut *store, hb)?;
                    let d: String = mem.data(&*store)[hb as usize..hb as usize + 32]
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect();
                    if x.get(k).map(String::as_str) != Some(d.as_str()) {
                        bad += 1;
                        if bad <= 5 {
                            println!("frame {}: state differs from the recorded one", first + k);
                        }
                    }
                }
            }
        }
    }
    let skip = usize::from(n > 1 && first == 0);
    let mut b: Vec<f64> = best[skip..].to_vec();
    let mut nsi: Vec<f64> = best[skip..]
        .iter()
        .zip(&insns[skip..])
        .map(|(ns, k)| ns / k)
        .collect();
    let steps = single.call(&mut *store, ())?;
    Ok(Summary {
        first,
        frames: n,
        us_per_frame: median(&mut b) / 1e3,
        ns_per_instr: median(&mut nsi),
        instructions: total,
        generic_pct: 100.0 * steps / (total * o.repeat as f64),
        differing: expect.map(|_| bad),
        memory_mb: mem.data_size(&*store) >> 20,
    })
}

impl Summary {
    pub fn print(&self, label: &str, repeat: usize) {
        println!(
            "{label}: frames {}-{} x{repeat}: median {:.1} us/frame, {:.2} ns/instr, {} instructions, generic {:.2}%, memory {} MB",
            self.first,
            self.first + self.frames - 1,
            self.us_per_frame,
            self.ns_per_instr,
            self.instructions,
            self.generic_pct,
            self.memory_mb
        );
        if let Some(bad) = self.differing {
            println!(
                "check: {} of {} frames identical",
                self.frames - bad,
                self.frames
            );
        }
    }
}

pub fn arg<T: std::str::FromStr>(args: &[String], name: &str) -> Option<T> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(|v| v.parse().ok().unwrap_or_else(|| panic!("{name} {v}")))
}

pub fn options(args: &[String]) -> Options {
    Options {
        repeat: arg(args, "--repeat").unwrap_or(5),
        frames: arg(args, "--frames"),
        check: arg(args, "--check"),
        blocks: !args.iter().any(|a| a == "--no-blocks"),
    }
}
