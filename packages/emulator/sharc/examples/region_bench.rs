//! Region microbenchmark: capture the engine state at a generated region's
//! entry during the DN2 note replay, then call that region's generated entry
//! function over and over from the captured state and time it.
//! `tools/devtools/sharc_regionbench.py` drives it against a gen dir holding only the
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
//!     region_bench fast-compare IMAGE OUTDIR PC[,PC...] [--calls 16] \
//!         [--warm 2] [--reps 3]
//!     region_bench concrete-compare IMAGE OUTDIR PC [--calls 2] \
//!         [--warm 1] [--reps 1] [--trials 100]
//!
//! fast-bench times the fast tier (packages/emulator/sharc/src/fast, Cranelift backend)
//! on the same entries; sol-* run the hand-written versions when built with
//! SHARC_REGIONBENCH_SOL (see build.rs).
//!
//! PCs are hex (the region's first block start). capture writes
//! OUTDIR/r_<PC>.state (canonical state before the region call) and
//! OUTDIR/r_<PC>.meta (key=value lines). bench prints one JSON line per
//! (region, rep).
//!
//! Entry calls use the installed-fast-tier policy: models on, timer/budget
//! capped, and generated chaining capped at one link. The legacy bench and
//! fast-bench restore only non-memory state. Fresh fast-compare and
//! concrete-compare restore the whole capture outside every execution timer
//! and check canonical/control state; tier-dependent steps/chain are reported.
//! This is an entry microbenchmark, without the full dispatcher's model and
//! interrupt processing at every intermediate boundary.

// The hand-written speed-of-light versions are a translation of firmware DSP
// loops: they stay under out/ and are compiled in only when build.rs finds
// them (SHARC_REGIONBENCH_SOL names the file, see tools/devtools/sharc_regionbench.py).
#[cfg(region_bench_sol)]
mod sol {
    include!(env!("SHARC_REGIONBENCH_SOL_PATH"));
}

#[cfg(sharc_gen)]
#[path = "region_bench/concrete.rs"]
mod concrete;

use sharc_native::rt::{Int, Loop, Pending, Spec, St, Stk, V};
use sharc_native::{CHAIN_MAX, EXIT_TRAP, Engine};
#[cfg(not(target_family = "wasm"))]
use sharc_native::{EXIT_BUDGET, EXIT_NEXT};
use std::collections::BTreeMap;
use std::time::Instant;

const CLOCK_BASE: u64 = 573_627_620;
const GAP: u32 = 667_000;
// Register codes of the core timer registers (packages/emulator/sharc/src/lib.rs).
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

fn configure(e: &mut Engine, clock_base: u64) {
    // The option set of packages/emulator/boot open_dn2_engine (the audio runs).  A
    // canonical import resets Cfg, so reset_capture calls this after every
    // restore too.
    let mut options = vec![
        (1, 1),
        (4, 1),
        (5, 1),
        (6, clock_base as i64),
        (9, 1),
        (10, 0),
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
}

pub(crate) fn open(image: &[u8], state: &[u8], clock_base: u64) -> Engine {
    let mut e = Engine::from_image(image).expect("image blob");
    // Import first: the options act on the imported state (as dn2_replay).
    e.import(state).expect("state blob");
    configure(&mut e, clock_base);
    e
}

/// Restore the complete architectural capture.  `import` resets the memory
/// overlay (and changes Mem::layout_gen), so a FastEngine must resolve its
/// windows again on its next call; it never retains a usable raw pointer.
pub(crate) fn reset_capture(e: &mut Engine, state: &[u8], clock_base: u64) {
    e.import(state).expect("restore state");
    configure(e, clock_base);
    // Canonical state deliberately omits execution/dispatcher scratch.  A
    // reused Engine must therefore reset every field that would otherwise
    // leak from the preceding sample.
    e.s.icount = 0;
    e.s.steps = 0;
    e.s.limit = 0;
    e.s.chain = 0;
    e.s.in_block = false;
    e.s.timer_written = false;
}

/// The Engine::step bookkeeping around one block call (models on).
#[derive(Clone, Copy)]
pub(crate) struct CallPlan {
    before: u64,
    limit: u64,
    timer: Option<u32>,
}

pub(crate) fn begin_call(e: &mut Engine) -> CallPlan {
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
    // Match Engine::step with an installed fast tier: generated AOT fallback
    // is capped at one link so every following block boundary is dispatched.
    s.chain = CHAIN_MAX;
    s.in_block = true;
    CallPlan {
        before,
        limit,
        timer,
    }
}

pub(crate) fn finish_call(e: &mut Engine, plan: CallPlan, code: u32) -> (u32, u64) {
    let s = &mut e.s;
    s.in_block = false;
    let k = s.icount - plan.before;
    if k > 0 {
        if let Some(c) = plan.timer {
            s.r[R_TCOUNT] = V::c((c as u64 - k) as Int);
        }
        let tick = e.instruction_clock_base.wrapping_add(e.s.icount - 1);
        e.s.r[R_EMUCLK] = V::c((tick as u32) as Int);
        e.s.r[R_EMUCLK2] = V::c((tick >> 32) as Int);
    }
    (code, k)
}

fn call_region(e: &mut Engine, f: sharc_native::BlockFn) -> (u32, u64) {
    call_region_with(e, f)
}

fn call_region_with(e: &mut Engine, f: impl FnOnce(&mut St) -> u32) -> (u32, u64) {
    let plan = begin_call(e);
    let code = f(&mut e.s);
    finish_call(e, plan, code)
}

/// Expose the same one-link generated boundaries as an installed fast tier,
/// while capturing with AOT alone. `one_dispatch` stops Engine::step after a
/// dispatch; by itself it cannot stop generated links inside that dispatch.
struct CaptureDispatch;
impl sharc_native::fast::FastTier for CaptureDispatch {
    fn wants(&self, _: u32) -> bool {
        false
    }
    fn run(&mut self, _: &mut St, _: u32) -> Option<u32> {
        None
    }
    fn caps_chain(&self) -> bool {
        true
    }
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
    e.set_fast(Some(Box::new(CaptureDispatch)));
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
    let agg = args.iter().any(|a| a == "--agg");
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
            let mut t_all = None;
            for i in 0..warm + calls {
                if agg && i == warm {
                    t_all = Some(Instant::now());
                }
                light.restore(&mut e.s);
                let t = (!agg).then(Instant::now);
                let (code, k) = std::hint::black_box(call_region(&mut e, entry.f));
                let dt = t.map_or(0, |t| t.elapsed().as_nanos() as u64);
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
            if let Some(t0) = t_all {
                let total = t0.elapsed().as_nanos();
                // The restore alone, timed the same way, is subtracted.
                let t1 = Instant::now();
                for _ in 0..calls {
                    light.restore(&mut e.s);
                    std::hint::black_box(&mut e.s);
                }
                let restore_total = t1.elapsed().as_nanos();
                light.restore(&mut e.s);
                let per = total.saturating_sub(restore_total) as f64 / calls as f64;
                println!(
                    "{{\"region\":\"{pc:X}\",\"rep\":{rep},\"calls\":{calls},\"insns_min\":{kmin},\"insns_max\":{kmax},\"insns_mean\":{:.3},\"code\":{},\"pc_after\":{},\"exit_varies\":{varying},\"median_ns\":{per:.1},\"trim_ns\":{per:.1},\"p10_ns\":{per:.1},\"p90_ns\":{per:.1},\"mean_ns\":{per:.1},\"agg\":true,\"total_ns\":{total},\"restore_total_ns\":{restore_total}}}",
                    ksum as f64 / calls as f64,
                    code0.unwrap(),
                    pc0.unwrap(),
                );
                continue;
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
/// `fe.run` (the bucket argument is unused now the per-phase marks are gone).
#[cfg(not(target_family = "wasm"))]
fn prof_run(fe: &mut sharc_native::fast::FastEngine, s: &mut St, pc: u32, _b: usize) -> u32 {
    use sharc_native::fast::FastTier;
    let c = fe.run(s, pc).unwrap_or(EXIT_TRAP);
    c
}

#[cfg(not(target_family = "wasm"))]
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
            if sharc_native::fast::blocktrans_roots().contains(&pc) {
                // SHARC_BLOCKTRANS names PC: a block-translator cluster root.
                fe = fe.with_clusters(&[pc]);
            }
            // Build and check it runs from the captured state.
            let built = call_region_with(&mut e, |s| prof_run(&mut fe, s, pc, 0));
            assert!(
                built.1 > 0,
                "fast tier declined at the captured entry: {}",
                fe.report()
            );
            let one_iter = fe.region(pc).map_or(1, |r| r.len() as u64);
            if let Some(r) = fe.region(pc) {
                eprintln!(
                    "region {pc:X}: {} insns{} lp={} inputs={} outputs={} wins={} known={} eqs={} nw={} misc={} flags={} groups={:?} kernel_ops={}",
                    r.len(),
                    if r.cfg.is_some() { " (cfg)" } else { "" },
                    r.lp.is_some(),
                    r.inputs.len(),
                    r.outputs.len(),
                    r.wins.len(),
                    r.known.len(),
                    r.eqs.len(),
                    r.nw.len(),
                    r.misc.len(),
                    r.flags.len(),
                    r.flag_groups,
                    r.kernel.inst_count()
                );
            }
            // The generated block the engine would run where the kernel stops.
            let tail = e.dispatch.get_entry(e.s.pc_sw);
            light.restore(&mut e.s);
            let (compile_us, build_us) = (fe.stats.compile_ns / 1000, fe.stats.build_ns / 1000);
            let mut series: Vec<(&str, Vec<u64>, u64)> = vec![
                ("aot", Vec::with_capacity(calls), 0),
                ("fast_loop", Vec::with_capacity(calls), 0),
                ("fast_total", Vec::with_capacity(calls), 0),
                ("fast_1iter", Vec::with_capacity(calls), 0),
                // The harness alone: restore, timer, the closure that does nothing.
                ("null", Vec::with_capacity(calls), 0),
            ];
            for i in 0..warm + calls {
                for (which, (_, times, ksum)) in series.iter_mut().enumerate() {
                    light.restore(&mut e.s);
                    let t = Instant::now();
                    let k = match which {
                        0 => call_region(&mut e, entry.f).1,
                        1 => {
                            std::hint::black_box(call_region_with(&mut e, |s| {
                                prof_run(&mut fe, s, pc, 1)
                            }))
                            .1
                        }
                        4 => std::hint::black_box(call_region_with(&mut e, |_| 0)).1,
                        3 => {
                            // One iteration only: the fixed cost of a call.
                            std::hint::black_box(call_region_with(&mut e, |s| {
                                s.limit = s.icount + one_iter;
                                prof_run(&mut fe, s, pc, 3)
                            }))
                            .1
                        }
                        _ => {
                            std::hint::black_box(call_region_with(&mut e, |s| {
                                let c = prof_run(&mut fe, s, pc, 2);
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

fn json_text(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// State not represented by canon::export_state that must agree between the
/// AOT reference and the fast path. `chain` is excluded: it is dispatcher
/// scratch and a successful fast kernel has no generated-link count; both raw
/// values are emitted for audit. `steps` is also tier-dependent bookkeeping:
/// the generator deliberately omits it, while interpreter/fast count it (the
/// fast_diff AOT comparison uses the same exclusion). Preserve it in reports
/// and check it within each mode, without changing the executed state.
/// `limit` is restored after the temporary
/// exact-boundary cap before this comparison.
pub(crate) fn host_state(e: &Engine) -> String {
    format!(
        "icount={} limit={} in_block={} at_loaded_entry={} timer_written={} trap={:?}",
        e.s.icount, e.s.limit, e.s.in_block, e.s.at_loaded_entry, e.s.timer_written, e.s.trap,
    )
}

/// Run the fast entry, then dispatch successors through fast/AOT as the
/// installed tier does, until exactly the AOT reference's work is retired.
/// This deliberately does not assume that one tail
/// block is sufficient: CFG exits and a capped generated chain may need zero,
/// one, or several successors.
#[cfg(not(target_family = "wasm"))]
fn fast_to_boundary(
    e: &mut Engine,
    fe: &mut sharc_native::fast::FastEngine,
    pc: u32,
    plan: CallPlan,
    target: u64,
) -> Result<(u32, u64), String> {
    use sharc_native::fast::FastTier;
    e.s.limit = target;
    let mut code = fe
        .run(&mut e.s, pc)
        .ok_or_else(|| format!("fast declined: {}", fe.report()))?;
    let mut hops = 0;
    while e.s.icount < target {
        if code == EXIT_TRAP {
            return Err("fast trapped before reference boundary".into());
        }
        let at = e.s.pc_sw;
        let before = e.s.icount;
        code = if let Some(c) = fe.run(&mut e.s, at as u32) {
            c
        } else {
            let entry = e
                .dispatch
                .get_entry(at)
                .ok_or_else(|| format!("no generated successor at {at:X}"))?;
            e.s.chain = CHAIN_MAX;
            (entry.f)(&mut e.s)
        };
        hops += 1;
        if e.s.icount <= before {
            return Err(format!("successor {at:X} made no progress"));
        }
        if hops > 128 {
            return Err("more than 128 successors before reference boundary".into());
        }
    }
    if e.s.icount != target {
        return Err(format!(
            "retired {} instructions, reference boundary is {}",
            e.s.icount - plan.before,
            target - plan.before
        ));
    }
    // `limit` is an Engine::step scratch field.  The reference leaves the
    // dispatcher's original timer/budget cap; retain that after the bounded
    // successor sequence so it is checked, rather than silently ignored.
    e.s.limit = plan.limit;
    Ok(finish_call(e, plan, code))
}

#[cfg(not(target_family = "wasm"))]
fn emit_fast_refusal(pc: u32, reason: &str) {
    println!(
        "{{\"region\":\"{pc:X}\",\"status\":\"refused\",\"reason\":\"{}\"}}",
        json_text(reason)
    );
}

/// A temporary limit at the exact AOT boundary can make a fast loop report a
/// budget exit where the uncapped reference reports next.  That is acceptable
/// only after the full state/boundary checks below; traps and every other exit
/// mismatch remain refusals.
#[cfg(not(target_family = "wasm"))]
fn exits_match(aot: u32, fast: u32) -> bool {
    aot == fast || (aot == EXIT_NEXT && fast == EXIT_BUDGET)
}

#[cfg(not(target_family = "wasm"))]
fn exit_normalized(aot: u32, fast: u32) -> bool {
    aot == EXIT_NEXT && fast == EXIT_BUDGET
}

/// Fresh AOT-versus-fast measurement.  Unlike fast-bench, each call begins
/// from the complete canonical capture and timing starts after that restore.
/// AOT establishes a retired-instruction boundary; fast plus generated
/// successors must reach it and produce byte-identical canonical exports and
/// identical dispatcher-visible state before any timing is emitted.
#[cfg(not(target_family = "wasm"))]
fn fast_compare(args: &[String]) {
    use sharc_native::fast::FastEngine;
    let image = std::fs::read(&args[0]).expect("image");
    let dir = &args[1];
    let pcs = parse_pcs(&args[2]);
    let calls = flag(args, "--calls", 16) as usize;
    let warm = flag(args, "--warm", 2) as usize;
    let reps = flag(args, "--reps", 3) as usize;
    if calls == 0 || reps == 0 {
        for &pc in &pcs {
            emit_fast_refusal(pc, "positive calls/reps required");
        }
        return;
    }
    for &pc in &pcs {
        let meta_path = format!("{dir}/r_{pc:X}.meta");
        let state_path = format!("{dir}/r_{pc:X}.state");
        if !std::path::Path::new(&meta_path).is_file()
            || !std::path::Path::new(&state_path).is_file()
        {
            emit_fast_refusal(pc, "missing captured entry state");
            continue;
        }
        let m = meta(&meta_path);
        let clock: u64 = match m.get("clock").and_then(|s| s.parse().ok()) {
            Some(v) => v,
            None => {
                emit_fast_refusal(pc, "captured entry metadata has no valid clock");
                continue;
            }
        };
        let state = std::fs::read(&state_path).expect("state");
        let mut aot = open(&image, &state, clock);
        let mut fast = open(&image, &state, clock);
        let Some(entry) = aot.dispatch.get_entry(pc as Int) else {
            emit_fast_refusal(pc, "production AOT dispatch has no entry");
            continue;
        };
        if aot.s.pc_sw != pc as Int || fast.s.pc_sw != pc as Int {
            emit_fast_refusal(pc, "captured state PC is not the requested entry");
            continue;
        }
        let mut fe = FastEngine::new(Box::<sharc_fast_cl::ClBackend>::default(), &[pc]);
        // Report full-loop lowering failures separately from the fallback CFG
        // shape. This diagnostic build is outside every execution timer.
        let loop_refusal = sharc_native::fast::region::build_loop(&aot.s, pc)
            .err()
            .map(|e| e.0)
            .unwrap_or_default();

        // Differential gate.  The first fast call also builds the kernel, but
        // it is never included in a timed sample.
        reset_capture(&mut aot, &state, clock);
        let aot_plan = begin_call(&mut aot);
        let aot_result = (entry.f)(&mut aot.s);
        let (aot_code, aot_insns) = finish_call(&mut aot, aot_plan, aot_result);
        if aot_insns == 0 || aot_code == EXIT_TRAP {
            emit_fast_refusal(pc, "AOT reference made no usable progress");
            continue;
        }
        let aot_export = aot.export();
        let aot_host = host_state(&aot);
        let aot_steps = aot.s.steps;
        reset_capture(&mut fast, &state, clock);
        let fast_plan = begin_call(&mut fast);
        let fast_result = fast_to_boundary(
            &mut fast,
            &mut fe,
            pc,
            fast_plan,
            fast_plan.before + aot_insns,
        );
        let (fast_code, fast_insns) = match fast_result {
            Ok(v) => v,
            Err(reason) => {
                emit_fast_refusal(pc, &reason);
                continue;
            }
        };
        let same_export = aot_export == fast.export();
        let same_host = aot_host == host_state(&fast);
        if !exits_match(aot_code, fast_code)
            || aot_insns != fast_insns
            || !same_export
            || !same_host
        {
            emit_fast_refusal(
                pc,
                &format!(
                    "differential mismatch code {aot_code}/{fast_code} insns {aot_insns}/{fast_insns} export={same_export} host={same_host} {} || {}",
                    aot_host,
                    host_state(&fast)
                ),
            );
            continue;
        }
        let (compile_us, build_us) = (fe.stats.compile_ns / 1000, fe.stats.build_ns / 1000);
        let fast_steps = fast.s.steps;
        let shape = fe.region(pc).expect("successful fast entry has a region");
        println!(
            "{{\"region\":\"{pc:X}\",\"status\":\"proved\",\"insns\":{aot_insns},\"pc_after\":{},\"aot_code\":{aot_code},\"fast_code\":{fast_code},\"exit_normalized\":{},\"aot_chain\":{},\"fast_chain\":{},\"aot_steps\":{aot_steps},\"fast_steps\":{fast_steps},\"loop_refusal\":\"{}\",\"shape\":\"{}\",\"shape_insns\":{},\"compile_us\":{compile_us},\"build_us\":{build_us}}}",
            aot.s.pc_sw,
            exit_normalized(aot_code, fast_code),
            aot.s.chain,
            fast.s.chain,
            json_text(&loop_refusal),
            if shape.cfg.is_some() {
                "cfg"
            } else if shape.lp.is_some() {
                "loop"
            } else {
                "linear"
            },
            shape.len(),
        );

        for rep in 0..reps {
            let mut aot_times = Vec::with_capacity(calls);
            let mut fast_times = Vec::with_capacity(calls);
            let mut aot_reset = Vec::with_capacity(calls);
            let mut fast_reset = Vec::with_capacity(calls);
            for i in 0..warm + calls {
                // Counterbalance host drift: the first timed mode alternates
                // every sample and reverses with each repetition.
                let mut failed = false;
                for aot_first in [(rep + i) % 2 == 0, (rep + i) % 2 != 0] {
                    if aot_first {
                        let r = Instant::now();
                        reset_capture(&mut aot, &state, clock);
                        let reset_ns = r.elapsed().as_nanos() as u64;
                        let plan = begin_call(&mut aot);
                        let t = Instant::now();
                        let code = (entry.f)(&mut aot.s);
                        let (_, insns) = finish_call(&mut aot, plan, code);
                        let elapsed = t.elapsed().as_nanos() as u64;
                        if code != aot_code
                            || insns != aot_insns
                            || aot.export() != aot_export
                            || host_state(&aot) != aot_host
                            || aot.s.steps != aot_steps
                        {
                            emit_fast_refusal(pc, "AOT reference varied after a full reset");
                            failed = true;
                            break;
                        }
                        if i >= warm {
                            aot_times.push(elapsed);
                            aot_reset.push(reset_ns);
                        }
                    } else {
                        let r = Instant::now();
                        reset_capture(&mut fast, &state, clock);
                        let reset_ns = r.elapsed().as_nanos() as u64;
                        let plan = begin_call(&mut fast);
                        let t = Instant::now();
                        let result =
                            fast_to_boundary(&mut fast, &mut fe, pc, plan, plan.before + aot_insns);
                        let elapsed = t.elapsed().as_nanos() as u64;
                        let Ok((code, insns)) = result else {
                            emit_fast_refusal(
                                pc,
                                "fast path stopped passing its differential boundary during timing",
                            );
                            failed = true;
                            break;
                        };
                        if !exits_match(aot_code, code)
                            || insns != aot_insns
                            || fast.export() != aot_export
                            || host_state(&fast) != aot_host
                            || fast.s.steps != fast_steps
                        {
                            emit_fast_refusal(pc, "fast path varied after a full reset");
                            failed = true;
                            break;
                        }
                        if i >= warm {
                            fast_times.push(elapsed);
                            fast_reset.push(reset_ns);
                        }
                    }
                }
                if failed {
                    break;
                }
            }
            if aot_times.len() != calls || fast_times.len() != calls {
                continue;
            }
            for (mode, mut times, mut resets) in [
                ("aot", aot_times, aot_reset),
                ("fast_total", fast_times, fast_reset),
            ] {
                times.sort_unstable();
                resets.sort_unstable();
                let (lo, hi) = (times.len() / 10, times.len() - times.len() / 10);
                let trim = times[lo..hi].iter().sum::<u64>() as f64 / (hi - lo) as f64;
                let reset_trim = resets[lo..hi].iter().sum::<u64>() as f64 / (hi - lo) as f64;
                println!(
                    "{{\"region\":\"{pc:X}\",\"status\":\"measured\",\"mode\":\"{mode}\",\"rep\":{rep},\"calls\":{calls},\"insns_mean\":{:.3},\"ns_per_insn\":{:.4},\"median_ns\":{},\"trim_ns\":{trim:.1},\"p10_ns\":{},\"p90_ns\":{},\"reset_trim_ns\":{reset_trim:.1},\"compile_us\":{compile_us},\"build_us\":{build_us}}}",
                    aot_insns as f64,
                    trim / aot_insns as f64,
                    pct(&times, 0.5),
                    pct(&times, 0.1),
                    pct(&times, 0.9),
                );
            }
        }
    }
}

/// Focused compiled-kernel checks for a guarded dynamic read.  These are
/// deliberately separate from timing: they exercise the u32 containment
/// rule, including a span whose exclusive end is 2^32, before a captured
/// loop is allowed to claim a full completion.
#[cfg(not(target_family = "wasm"))]
fn fast_guard_check() {
    use sharc_native::fast::glue::Ctx;
    use sharc_native::fast::ir::{
        Bin, CTX_EXIT_K, CTX_ITERS, CTX_WIN_DYN_LEN, CTX_WIN_EXT, Inst, Kernel, KernelBackend,
        NREGS, Op, Ty, st_reg_b, win_ptr,
    };
    use sharc_native::fast::lower::{Env, Lower, Plan};
    use sharc_native::mem::Mem;
    let k = Kernel {
        vars: Vec::new(),
        val_ty: vec![
            Ty::I64,
            Ty::I32,
            Ty::I32,
            Ty::I32,
            Ty::I32,
            Ty::I32,
            Ty::I32,
            Ty::I32,
            Ty::I32,
            Ty::I32,
            Ty::I32,
            Ty::I32,
            Ty::I32,
        ],
        pre: vec![Inst::Def(0, Op::Ctx64(win_ptr(0)))],
        body: vec![
            Inst::Def(1, Op::St32(st_reg_b(18))),
            Inst::Def(2, Op::St32(st_reg_b(36))),
            Inst::Def(3, Op::CI32(4)),
            Inst::Def(4, Op::Bin(Bin::Mul, 2, 3)),
            Inst::Def(5, Op::Bin(Bin::Add, 1, 4)),
            Inst::Def(6, Op::Ctx32(CTX_WIN_EXT)),
            Inst::Def(7, Op::Ctx32(CTX_WIN_DYN_LEN)),
            Inst::Def(8, Op::CI32(4)),
            Inst::Def(9, Op::Bin(Bin::Sub, 7, 8)),
            Inst::Def(10, Op::Bin(Bin::Sub, 5, 6)),
            Inst::Def(11, Op::Bin(Bin::LeU, 10, 9)),
            Inst::Guard {
                ok: 11,
                k: 0,
                exit: 0,
            },
            Inst::Def(12, Op::Load32 { base: 0, off: 5 }),
            Inst::StSt32 {
                off: st_reg_b(12),
                v: 12,
            },
        ],
        post: Vec::new(),
        exits: vec![Vec::new()],
        nlabels: 0,
        cfg: false,
    };
    k.validate().expect("guard kernel");
    let mut backend = sharc_fast_cl::ClBackend::default();
    let kernel = backend.compile(&k).expect("compile guard kernel");
    let env = Env {
        regs: [None; NREGS as usize],
        dag_l: [None; 16],
        dag_b: [None; 16],
        assume_nw32: true,
        mode1: 0,
        flag_v: false,
        guarded_dyn: false,
        bank_model: false,
        cacc_known: true,
        flat_windows: false,
    };
    let mut lower = Lower::new(env, Plan::default(), 2);
    let static_addr = lower.ci(0x20_0000);
    lower.dynamic_read_window(static_addr, 18).unwrap();
    lower.window_access(static_addr, 4, false).unwrap();
    lower.window_access(static_addr, 4, true).unwrap();
    assert_eq!(lower.wins.len(), 2, "dynamic and static slots differ");
    assert_eq!(lower.wins[0].dynamic_anchor, Some(18));
    assert_eq!(lower.wins[1].dynamic_anchor, None);
    assert!(lower.wins[1].read && lower.wins[1].write);
    let mut bytes = [0u8; 32];
    let mut checked = 0;
    let cases: [(&str, u32, u32, u32, u32, bool); 9] = [
        ("low", 0x1000u32, 16, 0x1000, 0, true),
        ("last", 0x1000, 16, 0x100c, 0, true),
        ("cross", 0x1000, 16, 0x100d, 0, false),
        ("negative", 0x1000, 16, 0x1000, u32::MAX, false),
        ("wrapped-inside", 0, 16, 0xffff_fff0, 4, true),
        ("wrapped-outside", 0x1000, 16, 0xffff_fff0, 4, false),
        ("u32-end", 0xffff_fff0, 16, 0xffff_fff0, 0, true),
        ("u32-end-last", 0xffff_fff0, 16, 0xffff_fffc, 0, true),
        ("u32-end-outside", 0xffff_fff0, 16, 0, 0, false),
    ];
    for (name, lo, len, i, m, pass) in cases {
        let mut s = St::new(Mem::new());
        s.r[18] = V::c(i as Int);
        s.r[36] = V::c(m as Int);
        s.r[12] = V::c(0xfeed_beef);
        let addr = i.wrapping_add(m.wrapping_mul(4));
        if pass {
            let off = addr.wrapping_sub(lo) as usize;
            bytes[off..off + 4].copy_from_slice(&0x1234_5678u32.to_le_bytes());
        }
        let mut ctx = Ctx::default();
        ctx.set64(
            win_ptr(0),
            (bytes.as_mut_ptr() as u64).wrapping_sub(lo as u64),
        );
        ctx.set32(CTX_ITERS, 1);
        ctx.set32(CTX_WIN_EXT, lo);
        ctx.set32(CTX_WIN_DYN_LEN, len);
        // SAFETY: the test context provides the one guarded read window and
        // `s` has the register layout named by the checked IR above.
        let code = unsafe { kernel.run(ctx.ptr(), std::ptr::from_mut(&mut *s).cast()) };
        if pass {
            assert_eq!(code, 0, "{name}");
            assert_eq!(s.r[12].b, 0x1234_5678, "{name}");
        } else {
            assert_eq!(code, 1, "{name}");
            assert_eq!(ctx.get32(CTX_EXIT_K), 0, "{name}");
            assert_eq!(s.r[12].b, 0xfeed_beef, "{name}");
        }
        checked += 1;
    }
    println!("{{\"status\":\"guard-check\",\"backend\":\"cl\",\"cases\":{checked}}}");
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("capture") => capture(&args[1..]),
        Some("bench") => bench(&args[1..]),
        #[cfg(not(target_family = "wasm"))]
        Some("fast-bench") => fast_bench(&args[1..]),
        #[cfg(not(target_family = "wasm"))]
        Some("fast-compare") => fast_compare(&args[1..]),
        #[cfg(not(target_family = "wasm"))]
        Some("fast-guard-check") => fast_guard_check(),
        #[cfg(sharc_gen)]
        Some("concrete-compare") => concrete::compare(&args[1..]),
        #[cfg(not(sharc_gen))]
        Some("concrete-compare") => {
            eprintln!("concrete-compare requires SHARC_GEN_DIR");
            std::process::exit(2);
        }
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
