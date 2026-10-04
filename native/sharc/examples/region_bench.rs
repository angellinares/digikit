//! Region microbenchmark: capture the engine state at a generated region's
//! entry during the DN2 note replay, then call that region's generated entry
//! function over and over from the captured state and time it.
//! `tools/sharc_regionbench.py` drives it against a gen dir holding only the
//! selected regions (the rest of the code is interpreted, which is fine for
//! capture).
//!
//!     region_bench capture IMAGE STATE FRAMES OUTDIR PC[,PC...] \
//!         [--start 4600] [--min-frame 4620] [--end 5600]
//!     region_bench bench IMAGE OUTDIR PC[,PC...] [--calls 4000] \
//!         [--warm 500] [--reps 5]
//!
//!     region_bench fast-bench IMAGE OUTDIR PC[,PC...] [--calls 4000] \
//!         [--warm 500] [--reps 5]
//!
//! fast-bench times the fast tier (native/sharc/src/fast, Cranelift backend)
//! on the same entries; sol-* run the hand-written versions when built with
//! SHARC_REGIONBENCH_SOL (see build.rs).
//!
//! PCs are hex (the region's first block start). capture writes
//! OUTDIR/r_<PC>.state (canonical state before the region call) and
//! OUTDIR/r_<PC>.meta (key=value lines). bench prints one JSON line per
//! (region, rep).
//!
//! The call mirrors what Engine::step does around a block in the audio runs
//! (models on: in_block, s.limit capped by the core timer count, chain 0),
//! without the dispatcher, interrupt checks or profiling. Between calls only
//! the non-memory state is restored; memory stays mutated.

// The hand-written speed-of-light versions are a translation of firmware DSP
// loops: they stay under out/ and are compiled in only when build.rs finds
// them (SHARC_REGIONBENCH_SOL names the file, see tools/sharc_regionbench.py).
#[cfg(region_bench_sol)]
mod sol {
    include!(env!("SHARC_REGIONBENCH_SOL_PATH"));
}

use sharc_native::rt::{Int, Loop, Pending, Spec, St, Stk, V};
use sharc_native::{EXIT_TRAP, Engine};
use std::collections::BTreeMap;
use std::time::Instant;

const CLOCK_BASE: u64 = 573_627_620;
const GAP: u32 = 667_000;
// Register codes of the core timer registers (native/sharc/src/lib.rs).
const R_TCOUNT: usize = 111;
const R_MODE2: usize = 116;
const R_EMUCLK: usize = 105;
const R_EMUCLK2: usize = 106;
const BUDGET: u64 = 1_000_000;

/// The state a region call reads and changes, apart from memory.
struct Light {
    r: [V; sharc_native::rt::NUREG],
    bank_alt: [V; 96],
    bank_active_mask: Int,
    bank_pending_mask: Int,
    bank_requested_mask: Int,
    timer_written: bool,
    loop_depth: Int,
    loop_slots: Stk<(V, V), 6>,
    pc_stack_pending: Int,
    pc_stack_requested: Int,
    special: [Spec; 7],
    special_present: [bool; 7],
    pc_sw: Int,
    pending: Option<Pending>,
    steps: Int,
    at_loaded_entry: bool,
    loops: Stk<Loop, { sharc_native::rt::MAX_LOOPS }>,
    call_stack: Stk<Int, { sharc_native::rt::MAX_CALLS }>,
    pc_stack: Stk<Int, { sharc_native::rt::MAX_CALLS }>,
    status_stack: Stk<(V, V, V), { sharc_native::rt::MAX_STATUS }>,
    mmrs: Vec<(u32, V)>,
    icount: u64,
    trap: Option<sharc_native::rt::Trap>,
    loops_ok: bool,
}

impl Light {
    fn save(s: &St) -> Light {
        Light {
            r: s.r,
            bank_alt: s.bank_alt,
            bank_active_mask: s.bank_active_mask,
            bank_pending_mask: s.bank_pending_mask,
            bank_requested_mask: s.bank_requested_mask,
            timer_written: s.timer_written,
            loop_depth: s.loop_depth,
            loop_slots: s.loop_slots,
            pc_stack_pending: s.pc_stack_pending,
            pc_stack_requested: s.pc_stack_requested,
            special: s.special,
            special_present: s.special_present,
            pc_sw: s.pc_sw,
            pending: s.pending,
            steps: s.steps,
            at_loaded_entry: s.at_loaded_entry,
            loops: s.loops,
            call_stack: s.call_stack,
            pc_stack: s.pc_stack,
            status_stack: s.status_stack,
            mmrs: s.mmrs.clone(),
            icount: s.icount,
            trap: s.trap,
            loops_ok: s.loops_ok,
        }
    }

    fn restore(&self, s: &mut St) {
        s.r = self.r;
        s.bank_alt = self.bank_alt;
        s.bank_active_mask = self.bank_active_mask;
        s.bank_pending_mask = self.bank_pending_mask;
        s.bank_requested_mask = self.bank_requested_mask;
        s.timer_written = self.timer_written;
        s.loop_depth = self.loop_depth;
        s.loop_slots = self.loop_slots;
        s.pc_stack_pending = self.pc_stack_pending;
        s.pc_stack_requested = self.pc_stack_requested;
        s.special = self.special;
        s.special_present = self.special_present;
        s.pc_sw = self.pc_sw;
        s.pending = self.pending;
        s.steps = self.steps;
        s.at_loaded_entry = self.at_loaded_entry;
        s.loops = self.loops;
        s.call_stack = self.call_stack;
        s.pc_stack = self.pc_stack;
        s.status_stack = self.status_stack;
        if s.mmrs != self.mmrs {
            s.mmrs.clone_from(&self.mmrs);
        }
        s.icount = self.icount;
        s.trap = self.trap;
        s.loops_ok = self.loops_ok;
        s.probe = None;
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

fn parse_pcs(text: &str) -> Vec<u32> {
    text.split(',')
        .filter(|x| !x.is_empty())
        .map(|x| u32::from_str_radix(x.trim_start_matches("0x"), 16).expect("hex pc"))
        .collect()
}

fn flag(args: &[String], name: &str, default: u64) -> u64 {
    args.iter()
        .position(|a| a == name)
        .map_or(default, |i| args[i + 1].parse().expect("number"))
}

fn open(image: &[u8], state: &[u8], clock_base: u64) -> Engine {
    let mut e = Engine::from_image(image).expect("image blob");
    // Import first: the options act on the imported state (as dn2_replay).
    e.import(state).expect("state blob");
    // The option set of native/boot open_dn2_engine (the audio runs).
    let mut options = vec![
        (1, 1),
        (4, 1),
        (5, 1),
        (6, clock_base as i64),
        (9, 1),
        (10, 0),
        (11, 1),
        (12, 1),
        (21, 1),
        (22, 1),
        (23, 0xB8_8AAB),
        (24, 0xB8_8A49),
        (25, 0xB8_8ABB),
    ];
    for (k, v) in options.drain(..) {
        assert_eq!(e.set_option(k, v), 0, "option {k}");
    }
    e
}

/// The Engine::step bookkeeping around one block call (models on).
fn call_region(e: &mut Engine, f: sharc_native::BlockFn) -> (u32, u64) {
    call_region_with(e, f)
}

fn call_region_with(e: &mut Engine, f: impl FnOnce(&mut St) -> u32) -> (u32, u64) {
    let s = &mut e.s;
    let before = s.icount;
    let mut limit = before + BUDGET;
    let (mode, count) = (s.r[R_MODE2], s.r[R_TCOUNT]);
    let mut timer = None;
    if mode.is_c() && mode.b & 0x20 != 0 && count.is_c() && count.b >= 2 {
        limit = limit.min(before + count.b as u64 - 1);
        timer = Some(count.b);
    }
    s.limit = limit;
    s.chain = 0;
    s.in_block = true;
    let code = f(s);
    s.in_block = false;
    let k = s.icount - before;
    if k > 0 {
        if let Some(c) = timer {
            s.r[R_TCOUNT] = V::c((c as u64 - k) as Int);
        }
        let tick = e.instruction_clock_base.wrapping_add(e.s.icount - 1);
        e.s.r[R_EMUCLK] = V::c((tick as u32) as Int);
        e.s.r[R_EMUCLK2] = V::c((tick >> 32) as Int);
    }
    (code, k)
}

fn capture(args: &[String]) {
    let image = std::fs::read(&args[0]).expect("image");
    let state = std::fs::read(&args[1]).expect("state");
    let frames = read_frames(&args[2]);
    let outdir = &args[3];
    let pcs = parse_pcs(&args[4]);
    let (start, end) = (
        flag(args, "--start", 4600) as usize,
        flag(args, "--end", 5600) as usize,
    );
    let min_frame = flag(args, "--min-frame", 4620) as usize;
    let clock = flag(args, "--clock", CLOCK_BASE);
    std::fs::create_dir_all(outdir).expect("outdir");
    let mut e = open(&image, &state, clock);
    e.one_dispatch = true;
    let window = flag(args, "--window", 20) as usize;
    let last = end.min(min_frame + window - 1);
    // Per region: every completed call's instruction count in the window
    // (the arrivals), and the largest one's entry state.
    let mut ks: BTreeMap<u32, Vec<u64>> = pcs.iter().map(|&p| (p, Vec::new())).collect();
    let mut bails: BTreeMap<u32, u64> = BTreeMap::new();
    let mut want = std::collections::BTreeSet::new();
    // In-context call times (cold-ish: the rest of the frame runs between).
    let mut ctx: BTreeMap<u32, Vec<u64>> = BTreeMap::new();
    let mut best: BTreeMap<u32, (u64, String, Vec<u8>)> = BTreeMap::new();
    let t0 = Instant::now();
    for (i, frame) in frames.iter().enumerate().take(last + 1).skip(start) {
        e.spi2_exchange(frame).expect("spi2 exchange");
        let mut done = 0u32;
        while done < GAP {
            let pc = e.s.pc_sw as u32;
            if i >= min_frame
                && ks.contains_key(&pc)
                && let Some(entry) = e.dispatch.get_entry(pc as Int)
            {
                // Export (slow) only for a pc's first arrival, and after a
                // call that ran longer than any before.
                let need = (!best.contains_key(&pc) && bails.get(&pc).is_none_or(|&b| b < 3))
                    || want.contains(&pc);
                let blob = need.then(|| {
                    e.export_ranges = true;
                    let b = e.export();
                    e.export_ranges = false;
                    b
                });
                let light = Light::save(&e.s);
                let tc = Instant::now();
                let (code, k) = call_region(&mut e, entry.f);
                let ns = tc.elapsed().as_nanos() as u64;
                if k > 0 && code != EXIT_TRAP {
                    ctx.entry(pc).or_default().push(ns);
                    let list = ks.get_mut(&pc).unwrap();
                    list.push(k);
                    if k > best.get(&pc).map_or(0, |b| b.0) {
                        if let Some(blob) = blob {
                            let meta = format!(
                                "pc={pc:X}\nframe={i}\narrival={}\nclock={}\ninsns={k}\ncode={code}\nmodel_safe={}\n",
                                list.len(),
                                clock + light.icount,
                                entry.model_safe
                            );
                            best.insert(pc, (k, meta, blob));
                            want.remove(&pc);
                        } else {
                            want.insert(pc);
                        }
                    }
                    done = done.saturating_add(k as u32);
                    continue;
                }
                // A bail (nothing ran): the state is as before.
                *bails.entry(pc).or_default() += 1;
                light.restore(&mut e.s);
                e.s.trap = None;
            }
            let ran = e.step(GAP - done);
            done += ran;
            if ran == 0 || e.halt.is_some() {
                break;
            }
        }
        if let Some(h) = &e.halt {
            println!("halt at frame {i}: {h}");
            break;
        }
        let _ = e.sport_block(None).expect("sport block");
    }
    let frames_seen = (last + 1 - min_frame) as f64;
    let mut missing = Vec::new();
    for (pc, list) in &ks {
        let Some((k, meta, blob)) = best.get(pc) else {
            if !list.is_empty() {
                missing.push(format!("{pc:X}"));
            }
            continue;
        };
        let mut sorted = list.clone();
        sorted.sort_unstable();
        let total: u64 = sorted.iter().sum();
        let mut c = ctx.get(pc).cloned().unwrap_or_default();
        c.sort_unstable();
        let meta = format!(
            "{meta}ctx_median_ns={}\nctx_mean_ns={:.0}\ncalls_in_window={}\nbails_in_window={}\nwindow_frames={frames_seen}\ntotal_insns_in_window={total}\nmedian_insns={}\n",
            c[c.len() / 2],
            c.iter().sum::<u64>() as f64 / c.len() as f64,
            sorted.len(),
            bails.get(pc).copied().unwrap_or(0),
            sorted[sorted.len() / 2],
        );
        std::fs::write(format!("{outdir}/r_{pc:X}.state"), blob).expect("write");
        std::fs::write(format!("{outdir}/r_{pc:X}.meta"), &meta).expect("write");
        println!(
            "r_{pc:X}: {} calls, {} bails in {frames_seen} frames; insns/call min {} median {} max {k}; in-context median {} ns",
            sorted.len(),
            bails.get(pc).copied().unwrap_or(0),
            sorted[0],
            sorted[sorted.len() / 2],
            c[c.len() / 2],
        );
    }
    println!(
        "capture done in {:.1}s; missing: {missing:?}",
        t0.elapsed().as_secs_f64()
    );
    if !missing.is_empty() {
        std::process::exit(1);
    }
}

fn meta(path: &str) -> BTreeMap<String, String> {
    std::fs::read_to_string(path)
        .expect("meta")
        .lines()
        .filter_map(|l| {
            l.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
        })
        .collect()
}

fn pct(sorted: &[u64], p: f64) -> u64 {
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn bench(args: &[String]) {
    let image = std::fs::read(&args[0]).expect("image");
    let dir = &args[1];
    let pcs = parse_pcs(&args[2]);
    let calls = flag(args, "--calls", 4000) as usize;
    let warm = flag(args, "--warm", 500) as usize;
    let reps = flag(args, "--reps", 5) as usize;
    for rep in 0..reps {
        for &pc in &pcs {
            let m = meta(&format!("{dir}/r_{pc:X}.meta"));
            let clock: u64 = m["clock"].parse().expect("clock");
            let state = std::fs::read(format!("{dir}/r_{pc:X}.state")).expect("state");
            let mut e = open(&image, &state, clock);
            assert!(e.s.cfg.block_base_ok && e.s.loops_ok, "block gates closed");
            let entry = e
                .dispatch
                .get_entry(pc as Int)
                .unwrap_or_else(|| panic!("no block at {pc:X}"));
            assert_eq!(e.s.pc_sw, pc as Int, "state is not at the entry");
            let light = Light::save(&e.s);
            let mut times = Vec::with_capacity(calls);
            let (mut kmin, mut kmax, mut ksum) = (u64::MAX, 0u64, 0u64);
            let (mut code0, mut pc0) = (None, None);
            let mut varying = false;
            for i in 0..warm + calls {
                light.restore(&mut e.s);
                let t = Instant::now();
                let (code, k) = std::hint::black_box(call_region(&mut e, entry.f));
                let dt = t.elapsed().as_nanos() as u64;
                if code0.is_none() {
                    code0 = Some(code);
                    pc0 = Some(e.s.pc_sw);
                } else if code0 != Some(code) || pc0 != Some(e.s.pc_sw) {
                    varying = true;
                }
                if i >= warm {
                    times.push(dt);
                    kmin = kmin.min(k);
                    kmax = kmax.max(k);
                    ksum += k;
                }
            }
            times.sort_unstable();
            let mean = times.iter().sum::<u64>() as f64 / times.len() as f64;
            // Mean of the p10..p90 samples: the clock ticks in 41.7 ns steps,
            // which a median of near-constant calls would inherit.
            let (lo, hi) = (times.len() / 10, times.len() - times.len() / 10);
            let trim = times[lo..hi].iter().sum::<u64>() as f64 / (hi - lo) as f64;
            println!(
                "{{\"region\":\"{pc:X}\",\"rep\":{rep},\"calls\":{calls},\"insns_min\":{kmin},\"insns_max\":{kmax},\"insns_mean\":{:.3},\"code\":{},\"pc_after\":{},\"exit_varies\":{varying},\"median_ns\":{},\"trim_ns\":{trim:.1},\"p10_ns\":{},\"p90_ns\":{},\"mean_ns\":{mean:.1}}}",
                ksum as f64 / calls as f64,
                code0.unwrap(),
                pc0.unwrap(),
                pct(&times, 0.5),
                pct(&times, 0.1),
                pct(&times, 0.9),
            );
        }
    }
}

/// Time the fast tier on the same captured entries as `bench`: per rep, the
/// generated region (`aot`), the fast kernel alone (`fast_loop`), and the
/// kernel plus the generated block at the pc it leaves (`fast_total`, what
/// an engine with both would run). Same call plan, restore and trimming as
/// `bench`. The fast region is built (and compiled) before timing starts;
/// its compile time is reported.
fn fast_bench(args: &[String]) {
    use sharc_native::fast::{FastEngine, FastTier};
    let image = std::fs::read(&args[0]).expect("image");
    let dir = &args[1];
    let pcs = parse_pcs(&args[2]);
    let calls = flag(args, "--calls", 4000) as usize;
    let warm = flag(args, "--warm", 500) as usize;
    let reps = flag(args, "--reps", 5) as usize;
    for rep in 0..reps {
        for &pc in &pcs {
            let m = meta(&format!("{dir}/r_{pc:X}.meta"));
            let clock: u64 = m["clock"].parse().expect("clock");
            let state = std::fs::read(format!("{dir}/r_{pc:X}.state")).expect("state");
            let mut e = open(&image, &state, clock);
            let entry = e
                .dispatch
                .get_entry(pc as Int)
                .unwrap_or_else(|| panic!("no block at {pc:X}"));
            let light = Light::save(&e.s);
            let mut fe = FastEngine::new(Box::<sharc_fast_cl::ClBackend>::default(), &[pc]);
            // Build and check it runs from the captured state.
            let built = call_region_with(&mut e, |s| fe.run(s, pc).unwrap_or(EXIT_TRAP));
            assert!(
                built.1 > 0,
                "fast tier declined at the captured entry: {}",
                fe.report()
            );
            let one_iter = fe.region(pc).map_or(1, |r| r.len() as u64);
            // The generated block the engine would run where the kernel stops.
            let tail = e.dispatch.get_entry(e.s.pc_sw);
            light.restore(&mut e.s);
            let (compile_us, build_us) = (fe.stats.compile_ns / 1000, fe.stats.build_ns / 1000);
            let mut series: Vec<(&str, Vec<u64>, u64)> = vec![
                ("aot", Vec::with_capacity(calls), 0),
                ("fast_loop", Vec::with_capacity(calls), 0),
                ("fast_total", Vec::with_capacity(calls), 0),
                ("fast_1iter", Vec::with_capacity(calls), 0),
            ];
            for i in 0..warm + calls {
                for (which, (_, times, ksum)) in series.iter_mut().enumerate() {
                    light.restore(&mut e.s);
                    let t = Instant::now();
                    let k = match which {
                        0 => call_region(&mut e, entry.f).1,
                        1 => {
                            std::hint::black_box(call_region_with(&mut e, |s| {
                                fe.run(s, pc).unwrap_or(EXIT_TRAP)
                            }))
                            .1
                        }
                        3 => {
                            // One iteration only: the fixed cost of a call.
                            std::hint::black_box(call_region_with(&mut e, |s| {
                                s.limit = s.icount + one_iter;
                                fe.run(s, pc).unwrap_or(EXIT_TRAP)
                            }))
                            .1
                        }
                        _ => {
                            std::hint::black_box(call_region_with(&mut e, |s| {
                                let c = fe.run(s, pc).unwrap_or(EXIT_TRAP);
                                if let Some(t) = tail {
                                    (t.f)(s);
                                }
                                c
                            }))
                            .1
                        }
                    };
                    let dt = t.elapsed().as_nanos() as u64;
                    if i >= warm {
                        times.push(dt);
                        *ksum += k;
                    }
                }
            }
            for (name, mut times, ksum) in series {
                times.sort_unstable();
                let (lo, hi) = (times.len() / 10, times.len() - times.len() / 10);
                let trim = times[lo..hi].iter().sum::<u64>() as f64 / (hi - lo) as f64;
                println!(
                    "{{\"region\":\"{pc:X}\",\"mode\":\"{name}\",\"rep\":{rep},\"calls\":{calls},\"insns_mean\":{:.3},\"median_ns\":{},\"trim_ns\":{trim:.1},\"p10_ns\":{},\"p90_ns\":{},\"compile_us\":{compile_us},\"build_us\":{build_us}}}",
                    ksum as f64 / calls as f64,
                    pct(&times, 0.5),
                    pct(&times, 0.1),
                    pct(&times, 0.9),
                );
            }
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("capture") => capture(&args[1..]),
        Some("bench") => bench(&args[1..]),
        Some("fast-bench") => fast_bench(&args[1..]),
        #[cfg(region_bench_sol)]
        Some("sol-check") => sol::check(&args[1..]),
        #[cfg(region_bench_sol)]
        Some("sol-fuzz") => sol::fuzz(&args[1..]),
        #[cfg(region_bench_sol)]
        Some("sol-bench") => sol::bench(&args[1..]),
        #[cfg(region_bench_sol)]
        Some("sol-gen") => sol::gen_run(&args[1..]),
        #[cfg(region_bench_sol)]
        Some("sol-dump") => sol::dump(&args[1..]),
        _ => {
            eprintln!("usage: region_bench capture|bench ... (see the file header)");
            std::process::exit(2);
        }
    }
}
