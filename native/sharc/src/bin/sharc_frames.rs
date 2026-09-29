//! Replay capture frames through the native core without Python, for
//! timing, profiling and a fast regression check.
//!
//! The pack comes from `tools/sharc_transpile_run.py pack` (the image blob,
//! the start state, and each frame's DMA data); every frame does what
//! `NativeFrames.frame` does: the DMA transfer poke, the DMA completion
//! call, then the block handler call, each run until the call returns.
//!
//!     sharc-frames PACK [--frames N] [--repeat R] [--record F | --check F]
//!                       [--no-blocks] [--coverage F] [-v]
//!
//! `--coverage` runs everything through the one-instruction interpreter
//! and writes, per executed instruction, "pc MODE1 known count" (the
//! block set and MODE1 facts tools/sharc_rsgen.py --coverage specialises).
//!
//! `--record` writes the SHA-256 of each frame's exported canonical state
//! (the state `sharc_transpile_run.py frames` compares with Python);
//! `--check` compares against such a file, so a build whose frames were
//! checked against the Python replay once can be re-checked in seconds.

use sharc_native::rt::{Int, NUREG, V};
use sharc_native::sha256::Sha256;
use sharc_native::{Engine, canon, trap_name};
use std::time::Instant;

struct Rd<'a> {
    b: &'a [u8],
    off: usize,
}

impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> &'a [u8] {
        let s = &self.b[self.off..self.off + n];
        self.off += n;
        s
    }
    fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.take(4).try_into().unwrap())
    }
    fn i64(&mut self) -> i64 {
        i64::from_le_bytes(self.take(8).try_into().unwrap())
    }
    fn blob(&mut self) -> &'a [u8] {
        let n = self.u32() as usize;
        self.take(n)
    }
}

struct Pack<'a> {
    image: &'a [u8],
    state: &'a [u8],
    opts: Vec<(u32, i64)>,
    shift_src: u32,
    command_word: u32,
    ring: u32,
    dma_cb: u32,
    r8: u32,
    r8_value: u32,
    block_handler: u32,
    first: u32,
    frames: Vec<&'a [u8]>,
}

fn parse(b: &[u8]) -> Pack<'_> {
    let mut r = Rd { b, off: 0 };
    assert_eq!(r.take(4), b"SHFP", "not a frame pack");
    assert_eq!(r.u32(), 1, "frame pack version");
    let image = r.blob();
    let state = r.blob();
    let n = r.u32();
    let opts = (0..n).map(|_| (r.u32(), r.i64())).collect();
    let c: Vec<u32> = (0..7).map(|_| r.u32()).collect();
    let first = r.u32();
    let n = r.u32();
    let frames = (0..n).map(|_| r.blob()).collect();
    Pack {
        image,
        state,
        opts,
        shift_src: c[0],
        command_word: c[1],
        ring: c[2],
        dma_cb: c[3],
        r8: c[4],
        r8_value: c[5],
        block_handler: c[6],
        first,
        frames,
    }
}

const CALL_END: &str = "return without followed call";

/// Run the current call until it returns (the clean 9a/9b stop the
/// Python replay also ends on). Instructions, including the return.
fn run_call(e: &mut Engine, max: u32) -> Result<u64, String> {
    // One step call runs until a trap halts the engine or the budget ends.
    let done = e.step(max) as u64;
    if done >= max as u64 {
        return Err("max-steps".into());
    }
    let t = e.last_trap.map(trap_name).unwrap_or_default();
    if t.contains(CALL_END)
        && (t.starts_with("sharc_core.forms_flow._type_9b_abs:")
            || t.starts_with("sharc_core.forms_flow._type_9a_abs:"))
    {
        return Ok(done + 1);
    }
    Err(format!(
        "{} (the Python core would run this instruction)",
        e.halt.clone().unwrap_or_default()
    ))
}

/// Back to the pack's start state (the image stays parsed).
fn reload(e: &mut Engine, p: &Pack) {
    e.import(p.state).expect("state blob");
    for &(k, v) in &p.opts {
        assert_eq!(e.set_option(k, v), 0, "option {k}");
    }
    e.s.icount = 0;
}

fn load(p: &Pack) -> Engine {
    let mut e = Engine::from_image(p.image).expect("image blob");
    e.import(p.state).expect("state blob");
    for &(k, v) in &p.opts {
        assert_eq!(e.set_option(k, v), 0, "option {k}");
    }
    e
}

fn hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

struct FrameOut {
    handler_ns: u64,
    instructions: u64,
}

fn frame(e: &mut Engine, p: &Pack, data: &[u8]) -> Result<FrameOut, String> {
    let shift = e
        .peek(p.shift_src as u64, 4)
        .map_err(|_| "shift source is an unmodelled MMR".to_string())?
        .unwrap_or(0)
        & 1;
    let base = (p.command_word as u64 + (1 - shift as u64) * p.ring as u64) & 0xFFFF_FFFF;
    if e.poke(base, data, 1) != data.len() as i32 {
        return Err("DMA transfer poke did not take effect".into());
    }
    e.fresh_call(p.dma_cb, None);
    e.set_reg(p.r8 as usize, V::c(p.r8_value as Int));
    run_call(e, 64).map_err(|w| format!("DMA completion call: {w}"))?;
    e.fresh_call(p.block_handler, None);
    let t = Instant::now();
    let n = run_call(e, 4_000_000)?;
    let ns = t.elapsed().as_nanos() as u64;
    Ok(FrameOut {
        handler_ns: ns,
        instructions: n,
    })
}

/// Registers, flags, PC, stacks and pending transfer of two engines:
/// the differences, as text.
fn differences(a: &Engine, b: &Engine) -> Vec<String> {
    let mut out = Vec::new();
    if a.s.pc_sw != b.s.pc_sw {
        out.push(format!("pc {:#x} != {:#x}", a.s.pc_sw, b.s.pc_sw));
    }
    for c in 0..NUREG {
        if a.s.r[c] != b.s.r[c] {
            out.push(format!("r{c} {:?} != {:?}", a.s.r[c], b.s.r[c]));
        }
    }
    if a.s.special != b.s.special || a.s.special_present != b.s.special_present {
        out.push("special registers differ".into());
    }
    if a.s.loops.items() != b.s.loops.items() {
        out.push(format!(
            "loops {:?} != {:?}",
            a.s.loops.items(),
            b.s.loops.items()
        ));
    }
    if a.s.call_stack.items() != b.s.call_stack.items() {
        out.push("call stack differs".into());
    }
    if a.s.pending != b.s.pending {
        out.push(format!("pending {:?} != {:?}", a.s.pending, b.s.pending));
    }
    if a.s.steps != b.s.steps {
        out.push(format!("steps {} != {}", a.s.steps, b.s.steps));
    }
    out
}

/// Run frames up to K on a block engine and an interpreter-only engine,
/// then frame K one block call at a time, comparing after each; report
/// the first block whose result differs.
fn find_divergence(p: &Pack, k: usize) {
    let mut a = load(p);
    let mut b = load(p);
    b.use_blocks = false;
    for f in 0..k {
        frame(&mut a, p, p.frames[f]).expect("frame");
        frame(&mut b, p, p.frames[f]).expect("frame");
        let d = differences(&a, &b);
        let (ha, hb) = (
            canon::export_state(&a.s, false),
            canon::export_state(&b.s, false),
        );
        if !d.is_empty() || ha != hb {
            println!(
                "frame {f}: engines differ already: {:?}",
                &d[..d.len().min(8)]
            );
            return;
        }
    }
    let data = p.frames[k];
    for e in [&mut a, &mut b] {
        let shift = e.peek(p.shift_src as u64, 4).unwrap().unwrap_or(0) & 1;
        let base = (p.command_word as u64 + (1 - shift as u64) * p.ring as u64) & 0xFFFF_FFFF;
        e.poke(base, data, 1);
        e.fresh_call(p.dma_cb, None);
        e.set_reg(p.r8 as usize, V::c(p.r8_value as Int));
        run_call(e, 64).expect("dma call");
        e.fresh_call(p.block_handler, None);
    }
    a.one_dispatch = true;
    let every: u64 = std::env::var("DIVERGE_MEM_EVERY")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20_000);
    let mut done = 0u64;
    let mut calls = 0u64;
    loop {
        let pc = a.s.pc_sw;
        let before = a.stats;
        let n = a.step(1_000_000);
        let blocked = a.stats.block_entries > before.block_entries;
        if n == 0 {
            println!("block engine halted at {pc:#x}: {:?}", a.halt);
            let (ea, eb) = (
                canon::export_state(&a.s, false),
                canon::export_state(&b.s, false),
            );
            println!("exports identical: {} (dispatches {calls})", ea == eb);
            if ea != eb {
                let (da, db) = (a.s.mem.dirty_bytes(), b.s.mem.dirty_bytes());
                let mut shown = 0;
                for (x, y) in da.iter().zip(db.iter()) {
                    if x != y && shown < 10 {
                        println!("    mem {:#x}={:#x} vs {:#x}={:#x}", x.0, x.1, y.0, y.1);
                        shown += 1;
                    }
                }
                println!("    dirty bytes {} vs {}", da.len(), db.len());
                println!("    mmrs {:?}", a.s.mmrs == b.s.mmrs);
            }
            return;
        }
        let m = b.step(n);
        let d = differences(&a, &b);
        calls += 1;
        let mem = calls.is_multiple_of(every)
            && canon::export_state(&a.s, false) != canon::export_state(&b.s, false);
        if !d.is_empty() || m != n || mem {
            println!(
                "frame {k}: after {} instructions, {} {} from {pc:#x} ({n} instructions, interpreter ran {m}):",
                done,
                if blocked { "block" } else { "interpreted step" },
                if mem { "(memory differs too)" } else { "" }
            );
            for line in d.iter().take(20) {
                println!("    {line}");
            }
            return;
        }
        done += n as u64;
        if a.halt.is_some() {
            println!("frame {k}: no divergence in {done} instructions");
            return;
        }
    }
}

/// The machine state a block reads besides memory.
#[derive(Clone)]
struct Entry {
    r: [V; NUREG],
    special: [sharc_native::rt::Spec; 7],
    loops: Vec<sharc_native::rt::Loop>,
    calls: Vec<Int>,
    pc: Int,
}

fn entry_of(e: &Engine) -> Entry {
    Entry {
        r: e.s.r,
        special: e.s.special,
        loops: e.s.loops.items().to_vec(),
        calls: e.s.call_stack.items().to_vec(),
        pc: e.s.pc_sw,
    }
}

fn restore(e: &mut Engine, x: &Entry) {
    e.s.r = x.r;
    e.s.special = x.special;
    e.s.loops.clear();
    for l in &x.loops {
        let _ = e.s.loops.push_raw(*l);
    }
    e.s.call_stack.clear();
    for c in &x.calls {
        let _ = e.s.call_stack.push_raw(*c);
    }
    e.s.pc_sw = x.pc;
    e.s.pending = None;
}

/// Time each block of PCS on its own: replay frames until the dispatcher
/// enters it, keep that entry state (registers, stacks), then call the
/// block again and again from it (memory keeps what earlier runs wrote).
fn bench_blocks(p: &Pack, pcs: &[u32]) {
    let mut e = load(p);
    let mut found: Vec<Option<Entry>> = vec![None; pcs.len()];
    e.one_dispatch = true;
    'frames: for k in 0..p.frames.len() {
        let data = p.frames[k];
        let shift = e.peek(p.shift_src as u64, 4).unwrap().unwrap_or(0) & 1;
        let base = (p.command_word as u64 + (1 - shift as u64) * p.ring as u64) & 0xFFFF_FFFF;
        e.poke(base, data, 1);
        e.fresh_call(p.dma_cb, None);
        e.set_reg(p.r8 as usize, V::c(p.r8_value as Int));
        e.one_dispatch = false;
        run_call(&mut e, 64).expect("dma call");
        e.one_dispatch = true;
        e.fresh_call(p.block_handler, None);
        loop {
            let pc = e.s.pc_sw as u32;
            if let Some(i) = pcs.iter().position(|&x| x == pc)
                && found[i].is_none()
                && e.s.pending.is_none()
            {
                found[i] = Some(entry_of(&e));
                if found.iter().all(Option::is_some) {
                    break 'frames;
                }
            }
            if e.step(1_000_000) == 0 {
                break;
            }
            if e.halt.is_some() {
                break;
            }
        }
    }
    for (i, pc) in pcs.iter().enumerate() {
        let Some(x) = &found[i] else {
            println!("{pc:#x}: never entered");
            continue;
        };
        let Some(f) = e.dispatch.get(*pc as Int) else {
            println!("{pc:#x}: no block");
            continue;
        };
        e.halt = None;
        restore(&mut e, x);
        e.s.icount = 0;
        e.s.limit = u64::MAX;
        let code = f(&mut e.s);
        let n = e.s.icount.max(1);
        let mut best = f64::INFINITY;
        for _ in 0..7 {
            let t = Instant::now();
            let mut reps = 0u64;
            while t.elapsed().as_micros() < 20_000 {
                for _ in 0..16 {
                    restore(&mut e, x);
                    e.s.icount = 0;
                    std::hint::black_box(f(&mut e.s));
                }
                reps += 16;
            }
            best = best.min(t.elapsed().as_nanos() as f64 / reps as f64);
        }
        // The restore alone.
        let t = Instant::now();
        for _ in 0..100_000 {
            restore(&mut e, x);
            std::hint::black_box(&e.s.r);
        }
        let base = t.elapsed().as_nanos() as f64 / 100_000.0;
        println!(
            "{pc:#x}: exit {code} after {n} instructions: {:.1} ns per call ({:.1} restore), {:.2} ns/instr",
            best,
            base,
            (best - base) / n as f64
        );
    }
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if v.is_empty() {
        return f64::NAN;
    }
    v[v.len() / 2]
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut path = None;
    let mut nframes = usize::MAX;
    let mut repeat = 1;
    let mut record = None;
    let mut check = None;
    let mut verbose = false;
    let mut blocks = true;
    let mut coverage = None;
    let mut exits = false;
    let mut block_profile: Option<String> = None;
    let mut transitions: Option<String> = None;
    let mut diverge: Option<usize> = None;
    let mut bench: Option<Vec<u32>> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--frames" => {
                i += 1;
                nframes = args[i].parse().expect("--frames N");
            }
            "--repeat" => {
                i += 1;
                repeat = args[i].parse().expect("--repeat R");
            }
            "--record" => {
                i += 1;
                record = Some(args[i].clone());
            }
            "--check" => {
                i += 1;
                check = Some(args[i].clone());
            }
            "--no-blocks" => blocks = false,
            "--exits" => exits = true,
            "--block-profile" => {
                i += 1;
                block_profile = Some(args[i].clone());
            }
            "--transitions" => {
                i += 1;
                transitions = Some(args[i].clone());
            }
            "--bench-blocks" => {
                i += 1;
                bench = Some(
                    args[i]
                        .split(',')
                        .map(|x| u32::from_str_radix(x.trim_start_matches("0x"), 16).unwrap())
                        .collect(),
                );
            }
            "--diverge" => {
                i += 1;
                diverge = Some(args[i].parse().expect("--diverge FRAME"));
            }
            "--coverage" => {
                i += 1;
                coverage = Some(args[i].clone());
                blocks = false;
            }
            "--interp-coverage" => {
                // What the interpreter runs with block code on.
                i += 1;
                coverage = Some(args[i].clone());
            }
            "-v" => verbose = true,
            a => path = Some(a.to_string()),
        }
        i += 1;
    }
    let path = path.expect("usage: sharc-frames PACK [options]");
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let p = parse(&bytes);
    let n = nframes.min(p.frames.len());
    let expect: Option<Vec<String>> = check.as_ref().map(|f| {
        std::fs::read_to_string(f)
            .unwrap_or_else(|e| panic!("{f}: {e}"))
            .lines()
            .map(|l| l.split_whitespace().nth(1).unwrap_or("").to_string())
            .collect()
    });
    if let Some(k) = diverge {
        find_divergence(&p, k);
        return;
    }
    if let Some(pcs) = &bench {
        bench_blocks(&p, pcs);
        return;
    }
    let hashing = record.is_some() || expect.is_some();
    let mut recorded = String::new();
    let mut per_frame_ns: Vec<Vec<f64>> = vec![Vec::new(); n];
    let mut insns = vec![0u64; n];
    let mut generic = 0u64;
    let mut total = 0u64;
    let mut entries = 0u64;
    let mut bad = 0;
    let mut e = load(&p);
    for rep in 0..repeat {
        if rep > 0 {
            reload(&mut e, &p);
        }
        e.use_blocks = blocks;
        if coverage.is_some() && rep == 0 {
            e.cov = Some(Default::default());
            e.entries = Some(Default::default());
        }
        if exits && rep == 0 {
            e.exits = Some(Default::default());
        }
        if block_profile.is_some() && rep + 1 == repeat {
            e.prof = Some(Default::default());
        }
        if transitions.is_some() && rep == 0 {
            e.trans = Some(Default::default());
        }
        for k in 0..n {
            let before = e.stats;
            let ic = e.s.icount;
            let out = match frame(&mut e, &p, p.frames[k]) {
                Ok(o) => o,
                Err(w) => {
                    eprintln!("frame {}: {w}", p.first as usize + k);
                    std::process::exit(2);
                }
            };
            per_frame_ns[k].push(out.handler_ns as f64);
            insns[k] = out.instructions;
            if rep == 0 {
                generic += e.stats.single_steps - before.single_steps;
                entries += e.stats.block_entries - before.block_entries;
                total += e.s.icount - ic;
            }
            if hashing && rep == 0 {
                let mut h = Sha256::new();
                h.update(&canon::export_state(&e.s, false));
                let d = hex(&h.finish());
                let idx = p.first as usize + k;
                recorded.push_str(&format!("{idx} {d}\n"));
                if let Some(x) = &expect
                    && x.get(k).map(String::as_str) != Some(d.as_str())
                {
                    bad += 1;
                    if bad <= 5 {
                        println!("frame {idx}: state differs from the recorded one");
                    }
                }
            }
            if k + 1 == n
                && rep == 0
                && let (Some(f), Some(t)) = (&transitions, &e.trans)
            {
                let mut rows: Vec<_> = t.iter().collect();
                rows.sort();
                let text: String = rows
                    .iter()
                    .map(|((a, b), c)| format!("{a:#x} {b:#x} {c}\n"))
                    .collect();
                std::fs::write(f, text).unwrap_or_else(|e| panic!("{f}: {e}"));
            }
            if k + 1 == n
                && rep == 0
                && let Some(x) = &e.exits
            {
                let mut rows: Vec<_> = x.iter().collect();
                rows.sort_by_key(|r| std::cmp::Reverse(*r.1));
                let total: u64 = rows.iter().map(|r| *r.1).sum();
                println!(
                    "block exits into the interpreter: {total} (kind 0 bail, 1 trap, 2 budget)"
                );
                for ((b, kind, at), c) in rows.iter().take(25) {
                    if *kind == 1 {
                        let t = trap_name(sharc_native::rt::Trap(*at));
                        println!("    block {b:#x} trap {t}: {c}");
                    } else {
                        println!("    block {b:#x} kind {kind} at {at:#x}: {c}");
                    }
                }
            }
            if k + 1 == n
                && rep == 0
                && let (Some(f), Some(cov)) = (&coverage, &e.cov)
            {
                let mut rows: Vec<_> = cov.iter().collect();
                rows.sort();
                let text: String = rows
                    .iter()
                    .map(|((pc, m, known), c)| format!("{pc:#x} {m:#x} {} {c}\n", *known as u8))
                    .collect();
                std::fs::write(f, text).unwrap_or_else(|e| panic!("{f}: {e}"));
                if let Some(en) = &e.entries {
                    let mut rows: Vec<_> = en.iter().collect();
                    rows.sort();
                    let text: String = rows
                        .iter()
                        .map(|(pc, c)| format!("{pc:#x} {c}\n"))
                        .collect();
                    let g = format!("{f}.entries");
                    std::fs::write(&g, text).unwrap_or_else(|e| panic!("{g}: {e}"));
                }
            }
            if verbose && rep == 0 && k < 2 {
                let unk: Vec<String> = (0..NUREG)
                    .filter(|&c| e.s.r[c].m != u32::MAX)
                    .map(|c| format!("{c}:{:#x}", e.s.r[c].m))
                    .collect();
                println!("not fully known registers: {}", unk.join(" "));
            }
            if verbose && rep == 0 {
                println!(
                    "frame {} instructions {} handler {:.1} us",
                    p.first as usize + k,
                    out.instructions,
                    out.handler_ns as f64 / 1e3
                );
            }
        }
    }
    if let Some(f) = &record {
        std::fs::write(f, &recorded).unwrap_or_else(|e| panic!("{f}: {e}"));
    }
    // Frame 0 is the start-up call; the median is over the rest.
    let skip = if n > 1 && p.first == 0 { 1 } else { 0 };
    let mut best: Vec<f64> = per_frame_ns[skip..]
        .iter()
        .map(|v| v.iter().cloned().fold(f64::INFINITY, f64::min))
        .collect();
    let mut nsi: Vec<f64> = best
        .iter()
        .zip(&insns[skip..])
        .map(|(ns, &k)| ns / k as f64)
        .collect();
    let us = median(&mut best) / 1e3;
    println!(
        "frames {}-{} x{}: median {:.1} us/frame, {:.2} ns/instr, generic {:.2}%, {:.1} instr/entry, {} instructions",
        p.first,
        p.first as usize + n - 1,
        repeat,
        us,
        median(&mut nsi),
        100.0 * generic as f64 / total.max(1) as f64,
        total as f64 / entries.max(1) as f64,
        total
    );
    if let (Some(f), Some(p)) = (&block_profile, &e.prof) {
        let hz = sharc_native::tick_hz() as f64;
        let mut rows: Vec<_> = p.iter().collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.1.0));
        let text: String = rows
            .iter()
            .map(|(pc, (t, n, k))| format!("{pc:#x} {:.1} {n} {k}\n", *t as f64 * 1e9 / hz))
            .collect();
        std::fs::write(f, text).unwrap_or_else(|e| panic!("{f}: {e}"));
        let tot: u64 = p.values().map(|v| v.0).sum();
        let mut last: Vec<f64> = per_frame_ns[skip..]
            .iter()
            .map(|v| *v.last().unwrap_or(&0.0))
            .collect();
        println!(
            "profiled repetition: median {:.1} us/frame (frame totals, not the median of blocks)",
            median(&mut last) / 1e3
        );
        println!(
            "block profile: {:.1} us in blocks per frame (ns per block file: {f})",
            tot as f64 * 1e6 / hz / n as f64
        );
    }
    if let Some(f) = &check {
        println!(
            "check against {f}: {} of {n} frames identical",
            n - bad.min(n)
        );
        if bad > 0 {
            std::process::exit(1);
        }
    }
}
