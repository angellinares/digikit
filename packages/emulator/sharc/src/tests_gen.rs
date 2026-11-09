//! Fuzz test of the generated lowering (`fast/lower/gen_forms.rs`, written by
//! tools/sharc_dev/sharc_lowergen.py from tools/sharc_dev/sharc_core): random encodings of each
//! generated form run on the interpreter and on the fast tier, one step at a
//! time, and the machine state must agree. The hand-written lowering is run
//! on the same vectors to count how many each accepts.
//!
//! `cargo test --lib gen_lowering_table -- --ignored --nocapture` prints the per-form table.

use crate::fast::lower::set_gen_force;
use crate::mem::Mem;
use crate::rt::*;
use crate::tests_gen_programs::PROGRAMS;
use crate::tests_gen_vectors::VECTORS;
use std::collections::BTreeMap;

const SPIN_ALIAS: u32 = 0x2800_0000;
const DATA: u32 = 0x25_f000;
const DATA_WORDS: u32 = 0x100;
const PEYEN: u32 = 0x20_0000;

/// A tiny deterministic generator (xorshift64*).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 32) as u32
    }
    fn below(&mut self, n: u32) -> u32 {
        self.next() % n
    }
}

/// `JUMP` to itself (48 bits), the spin after the instruction under test.
const SPIN: [u16; 3] = [0x073e, 0, 0];

/// CFG 1: the runtime's own configuration (bank and stack models, and the
/// explicit_memory_model setting of the generated image, if any).
fn engine_cfg(parcels: &[u16], mode1: u32, seed: u64, fast: bool, cfg: u8) -> crate::Engine {
    let mut bytes = Vec::new();
    for p in parcels.iter().chain(SPIN.iter()) {
        bytes.extend_from_slice(&p.to_le_bytes());
    }
    let mut mem = Mem::new();
    mem.load(SPIN_ALIAS, &bytes);
    mem.reset();
    let mut e = crate::Engine::new(mem);
    assert_eq!(e.set_option(4, 1), 0);
    if cfg == 1 {
        // Setting a model refreshes `block_base_ok`, which also compares
        // explicit_memory_model with the one the generated image was built
        // for (false for the DN2/DT2 images); a mismatch gates the fast tier
        // off as well. The runtime sets the image's value, so do the same.
        assert_eq!(
            e.set_option(10, crate::gen_explicit_memory_model() as i64),
            0
        );
        for key in [19, 20] {
            assert_eq!(e.set_option(key, 1), 0);
        }
    }
    e.s.r[114] = V::c(mode1 as Int); // MODE1
    e.s.r[122] = V::c(0); // IRPTL
    e.s.r[123] = V::c(0); // IMASK
    e.s.r[124] = V::c(0); // IMASKP
    // R, I, M, B, L, S and the flag registers known.
    for code in (0..96).chain(118..122) {
        e.s.r[code] = V::c(0);
    }
    for k in 0..DATA_WORDS {
        let a = e.s.mem.canonical_of(DATA + 4 * k);
        for b in 0..4u32 {
            let v = (k as u8).wrapping_mul(37).wrapping_add(b as u8 * 11 + 3);
            e.s.mem.write_byte(a + b, v);
        }
    }
    let mut rng = Rng(seed | 1);
    for c in 0..16 {
        e.s.r[c] = V::c(rng.next() as Int); // R
        e.s.r[80 + c] = V::c(rng.next() as Int); // S
        // I: inside the data block, word aligned.
        e.s.r[16 + c] = V::c((DATA + 0x100 + 4 * rng.below(0x30)) as Int);
        // M: small, both signs.
        e.s.r[32 + c] = V::c((rng.below(13) as i32 - 6) as u32 as Int);
    }
    // The system registers the core treats as plain words (`sysops::plain_sys`).
    for code in [108, 109, 112, 113, 115, 126, 127] {
        e.s.r[code] = V::c(rng.next() as Int);
    }
    e.s.r[116] = V::c(0); // MODE2: no timer
    if fast {
        let mut fe =
            crate::fast::FastEngine::new(Box::new(crate::fast::interp::InterpBackend), &[0]);
        fe.force_cfg(true);
        e.set_fast(Some(Box::new(fe)));
    }
    e
}

/// MODE1.CBUFEN (circular buffering of post-modifies).
const CBUFEN: u32 = 1 << 24;

/// Circular-buffer variant of a fuzz state: most index registers get B and L
/// (words of 4 bytes, 4..15 units, so a buffer is 4..60 bytes at scale 1 and
/// more at scale 2 and 4), the index inside the buffer (often exactly at an
/// edge), and MODE1.CBUFEN set. With M in -6..6 the modifier is 0..24 bytes
/// at scale 4: inside, exactly at, and larger than a 4-unit buffer.
fn make_circular(e: &mut crate::Engine, seed: u64) {
    let mut rng = Rng(seed ^ 0xc1c1_c1c1_5a5a);
    let mode1 = e.s.r[114].b as u32;
    e.s.r[114] = V::c((mode1 | CBUFEN) as Int);
    for c in 0..16 {
        if rng.below(4) == 0 {
            continue;
        }
        let len = 4 + rng.below(12);
        let base = DATA + 0x100 + 4 * rng.below(0x10);
        let index = match rng.below(4) {
            0 => base,
            1 => base + len - 4,
            _ => base + 4 * rng.below(len / 4),
        };
        e.s.r[48 + c] = V::c(len as Int);
        e.s.r[64 + c] = V::c(base as Int);
        e.s.r[16 + c] = V::c(index as Int);
    }
}

fn engine_circ(
    parcels: &[u16],
    mode1: u32,
    seed: u64,
    fast: bool,
    cfg: u8,
    circ: bool,
) -> crate::Engine {
    let mut e = engine_cfg(parcels, mode1, seed, fast, cfg);
    if circ {
        make_circular(&mut e, seed);
    }
    e
}

fn same(plain: &crate::Engine, fast: &crate::Engine, why: &str) {
    let (p, f) = (&plain.s, &fast.s);
    assert_eq!(p.icount, f.icount, "icount {why}");
    assert_eq!(p.pc_sw, f.pc_sw, "pc {why}");
    for code in 0..NUREG {
        assert_eq!(p.r[code], f.r[code], "r{code} {why}");
    }
    assert_eq!(p.pending, f.pending, "pending {why}");
    let mut diffs = Vec::new();
    for k in 0..DATA_WORDS * 4 {
        let a = p.mem.canonical_of(DATA + k);
        if p.mem.byte(a) != f.mem.byte(a) {
            diffs.push((k, p.mem.byte(a), f.mem.byte(a)));
        }
    }
    if !diffs.is_empty() {
        eprintln!(
            "I0-15 {:x?}",
            (16..32).map(|c| p.r[c].b).collect::<Vec<_>>()
        );
    }
    assert!(
        diffs.is_empty(),
        "data bytes (offset, plain, fast) {diffs:x?} {why}"
    );
}

/// (vectors, ran in the fast tier) per form for the current lowering.
fn run_all(label: &str) -> BTreeMap<&'static str, (u32, u32)> {
    run_shard(label, 0, 1)
}

/// The vectors whose index is `shard` modulo `shards`: the fuzz runs in shards so the test threads share the work.
fn run_shard(label: &str, shard: usize, shards: usize) -> BTreeMap<&'static str, (u32, u32)> {
    let mut table: BTreeMap<&'static str, (u32, u32)> = BTreeMap::new();
    let only = std::env::var("GEN_FORMS").ok();
    for (vi, (form, parcels, words)) in VECTORS.iter().enumerate() {
        if vi % shards != shard {
            continue;
        }
        if only
            .as_ref()
            .is_some_and(|o| !o.split(',').any(|f| f == *form))
        {
            continue;
        }
        let n = *parcels as usize;
        let code = &words[..n];
        for (mode1, cfg) in [(0, 0), (PEYEN, 0), (0, 1), (PEYEN, 1)] {
            for k in 0..4u64 {
                // Seeds 2 and 3 run with circular buffers set up.
                let circ = k >= 2;
                let seed = 0x9e37_79b9_7f4a_7c15u64 ^ ((vi as u64) << 8) ^ k;
                let why = format!("{label} {form} {code:04x?} mode1 {mode1:#x} cfg {cfg} seed {k}");
                let mut plain = engine_circ(code, mode1, seed, false, cfg, circ);
                let mut fast = engine_circ(code, mode1, seed, true, cfg, circ);
                // Overlapping encodings: the decoder's pick is the form.
                match crate::fast::region::decode_one(&plain.s, 0) {
                    Ok(dec) if dec.form == *form => {}
                    _ => continue,
                }
                let a = plain.step(1);
                let b = fast.step(1);
                assert_eq!(a, b, "{why}");
                if plain.halt.is_some() || fast.halt.is_some() {
                    assert_eq!(
                        format!("{:?}", plain.halt),
                        format!("{:?}", fast.halt),
                        "{why}"
                    );
                    continue;
                }
                same(&plain, &fast, &why);
                // And the spin that follows, in one more chunk of 3.
                assert_eq!(plain.step(3), 3, "{why}: {:?}", plain.halt);
                assert_eq!(fast.step(3), 3, "{why}: {:?}", fast.halt);
                same(&plain, &fast, &why);
                let st = fast.fast.as_ref().unwrap().fast_stats().unwrap();
                let e = table.entry(form).or_default();
                e.0 += 1;
                if st.insns > 0 {
                    e.1 += 1;
                }
            }
        }
    }
    table
}

/// The per-form table (`cargo test --lib gen_lowering_table -- --ignored --nocapture`), the hand-written lowering
/// next to the generated one. The sharded tests below run the same vectors and compare the same states.
#[test]
#[ignore]
fn gen_lowering_table() {
    set_gen_force(1);
    let hand = run_all("hand");
    set_gen_force(2);
    let gen_ = run_all("gen");
    set_gen_force(0);
    eprintln!("{:8} {:>6} {:>6} {:>6}", "form", "n", "hand", "gen");
    for (form, (n, g)) in &gen_ {
        let h = hand[form].1;
        eprintln!("{form:8} {n:6} {h:6} {g:6}");
    }
}

/// One shard of the lowering fuzz: the lowering `force` (1 hand-written, 2 generated) over every fourth vector.
macro_rules! lowering_shard {
    ($name:ident, $force:expr, $label:expr, $shard:expr) => {
        #[test]
        fn $name() {
            set_gen_force($force);
            run_shard($label, $shard, 4);
            set_gen_force(0);
        }
    };
}
lowering_shard!(gen_lowering_hand_0, 1, "hand", 0);
lowering_shard!(gen_lowering_hand_1, 1, "hand", 1);
lowering_shard!(gen_lowering_hand_2, 1, "hand", 2);
lowering_shard!(gen_lowering_hand_3, 1, "hand", 3);
lowering_shard!(gen_lowering_gen_0, 2, "gen", 0);
lowering_shard!(gen_lowering_gen_1, 2, "gen", 1);
lowering_shard!(gen_lowering_gen_2, 2, "gen", 2);
lowering_shard!(gen_lowering_gen_3, 2, "gen", 3);

// -- whole programs -------------------------------------------------------------

/// An engine over a program (parcels), then the spin. Registers: R and S small
/// floats (or random bits), I12/I13 and M5 as the SIMD loop expects.
fn program_engine(
    parcels: &[u16],
    floats: bool,
    seed: u64,
    entries: &[u32],
    fast: bool,
    cfg: u8,
    circ: bool,
) -> crate::Engine {
    let mut e = engine_cfg(parcels, 0, seed, false, cfg);
    // The spin that `engine` placed follows the program already.
    let mut rng = Rng(seed ^ 0x5555);
    for k in 0..DATA_WORDS {
        let a = e.s.mem.canonical_of(DATA + 4 * k);
        let v: u32 = if floats {
            // 1.0 + a few ulps of spread, either sign.
            let f = 1.0f32 + (rng.below(1000) as f32) * 0.001;
            (if rng.below(2) == 0 { f } else { -f }).to_bits()
        } else {
            rng.next()
        };
        for (b, byte) in v.to_le_bytes().iter().enumerate() {
            e.s.mem.write_byte(a + b as u32, *byte);
        }
    }
    for c in 0..16 {
        e.s.r[c] = V::c(if floats {
            (1.5f32 + c as f32).to_bits()
        } else {
            rng.next()
        } as Int);
        e.s.r[80 + c] = V::c(if floats {
            (0.5f32 + c as f32).to_bits()
        } else {
            rng.next()
        } as Int);
    }
    e.s.r[16 + 12] = V::c((DATA + 0x40) as Int); // I12
    e.s.r[16 + 13] = V::c((DATA + 0x180) as Int); // I13
    e.s.r[32 + 5] = V::c(2); // M5
    if circ {
        // I2 in a 16-unit (64-byte) buffer; I12 is below it, I13 above it.
        e.s.r[114] = V::c((e.s.r[114].b | CBUFEN) as Int);
        e.s.r[48 + 2] = V::c(16);
        e.s.r[64 + 2] = V::c((DATA + 0xc0) as Int);
        e.s.r[16 + 2] = V::c((DATA + 0xc0 + 8) as Int);
    }
    if fast {
        let mut fe =
            crate::fast::FastEngine::new(Box::new(crate::fast::interp::InterpBackend), entries);
        fe.force_cfg(true);
        e.set_fast(Some(Box::new(fe)));
    }
    e
}

#[test]
fn gen_programs_match_the_interpreter() {
    set_gen_force(2);
    for (name, parcels) in PROGRAMS {
        for (floats, cfg) in [(true, 0), (false, 0), (true, 1), (false, 1)] {
            let seed = 0x1234_5678u64;
            // One entry at the start (the region covers the whole program).
            let entries = [0u32];
            let mut best = 0u64;
            for chunk in [1u32, 2, 3, 5, 7, 11, 64, 1000] {
                let circ = name.starts_with("circ");
                let mut plain = program_engine(parcels, floats, seed, &entries, false, cfg, circ);
                let mut fast = program_engine(parcels, floats, seed, &entries, true, cfg, circ);
                let total = 400u32;
                let mut done = 0;
                while done < total {
                    let n = chunk.min(total - done);
                    let why =
                        format!("{name} floats {floats} cfg {cfg} chunk {chunk} after {done}");
                    assert_eq!(plain.step(n), n, "{why}: {:?}", plain.halt);
                    assert_eq!(fast.step(n), n, "{why}: {:?}", fast.halt);
                    done += n;
                    same(&plain, &fast, &why);
                }
                let st = fast.fast.as_ref().unwrap().fast_stats().unwrap();
                best = best.max(st.insns);
            }
            eprintln!("{name} floats {floats} cfg {cfg}: fast tier ran {best} instructions");
            assert!(best > 0, "{name}: the fast tier never ran");
        }
    }
    set_gen_force(0);
}

// -- control flow (lane F3) ----------------------------------------------------

use crate::tests_gen_programs::CONTROL;

fn same_stacks(plain: &crate::Engine, fast: &crate::Engine, why: &str) {
    let (p, f) = (&plain.s, &fast.s);
    assert_eq!(p.pc_stack.items(), f.pc_stack.items(), "pc stack {why}");
    assert_eq!(
        p.call_stack.items(),
        f.call_stack.items(),
        "call stack {why}"
    );
    assert_eq!(
        p.at_loaded_entry, f.at_loaded_entry,
        "at_loaded_entry {why}"
    );
    assert_eq!(p.steps, f.steps, "steps {why}");
}

/// Calls, returns and transfers with a compute: the whole state agrees after
/// every chunk of steps, whatever the chunk size cuts (delay slots included).
#[test]
fn gen_control_programs_match_the_interpreter() {
    for force in [1u8, 2] {
        set_gen_force(force);
        for (name, parcels, entries, cfgs) in CONTROL {
            if std::env::var("GEN_CTL").is_ok_and(|o| !name.contains(&o)) {
                continue;
            }
            for cfg in [0u8, 1] {
                if *cfgs == 2 && cfg == 0 {
                    continue;
                }
                let mut best = 0u64;
                for chunk in [1u32, 2, 3, 4, 5, 7, 11, 64] {
                    let build = |fast: bool| {
                        let mut e =
                            program_engine(parcels, false, 0x4321, entries, fast, cfg, false);
                        if *name == "ctl_return_slots" {
                            // I6 a circular index register, M14 = 0.
                            e.s.r[48 + 6] = V::c(0x40);
                            e.s.r[64 + 6] = V::c(DATA as Int + 0x100);
                            e.s.r[16 + 6] = V::c(DATA as Int + 0x110);
                        }
                        e
                    };
                    let (mut plain, mut fast) = (build(false), build(true));
                    let total = 120u32;
                    let mut done = 0;
                    while done < total {
                        let n = chunk.min(total - done);
                        let why =
                            format!("{name} gen {force} cfg {cfg} chunk {chunk} after {done}");
                        assert_eq!(plain.step(n), n, "{why}: {:?}", plain.halt);
                        assert_eq!(fast.step(n), n, "{why}: {:?}", fast.halt);
                        done += n;
                        same(&plain, &fast, &why);
                        same_stacks(&plain, &fast, &why);
                    }
                    best = best.max(fast.fast.as_ref().unwrap().fast_stats().unwrap().insns);
                }
                eprintln!("{name} gen {force} cfg {cfg}: fast tier ran {best} instructions");
                // (The hand-written lowering has no MODE1 change, so SIMD programs
                // never reach the fast tier with it.)
                assert!(
                    best > 0 || (force == 1 && name.contains("simd")),
                    "{name}: the fast tier never ran"
                );
            }
        }
    }
    set_gen_force(0);
}

// -- helper bodies ------------------------------------------------------------

/// Every vector of a form that has a generated helper body: the body on the
/// engine state against one interpreter step. A body that returns 1 must have
/// left every register as it was.
#[test]
fn gen_helpers_match_the_interpreter() {
    use crate::fast::gen_helpers;
    use crate::fast::helper;
    let mut ran = BTreeMap::<&str, (u32, u32)>::new();
    let mut skipped = BTreeMap::<&str, u32>::new();
    for (vi, (form, parcels, words)) in VECTORS.iter().enumerate() {
        if !gen_helpers::FORMS.contains(form) {
            continue;
        }
        let code = &words[..*parcels as usize];
        for (mode1, cfg) in [(0, 0), (0, 1), (PEYEN, 0)] {
            for k in 0..3u64 {
                let seed = 0x1357_9bdf_2468_ace0u64 ^ ((vi as u64) << 8) ^ k;
                let why = format!("helper {form} {code:04x?} mode1 {mode1:#x} cfg {cfg} seed {k}");
                let mut plain = engine_cfg(code, mode1, seed, false, cfg);
                let mut hel = engine_cfg(code, mode1, seed, false, cfg);
                match crate::fast::region::decode_one(&plain.s, 0) {
                    Ok(dec) if dec.form == *form => {
                        let Some(p) = helper::plan(&dec, &*hel.s) else {
                            skipped.entry(form).and_modify(|n| *n += 1).or_insert(1);
                            continue;
                        };
                        let before: Vec<_> = (0..NUREG).map(|c| hel.s.r[c]).collect();
                        let st = gen_helpers::run(p.id, &mut hel.s, &p.f);
                        let e = ran.entry(form).or_default();
                        e.0 += 1;
                        if st != 0 {
                            for c in 0..NUREG {
                                assert_eq!(hel.s.r[c], before[c], "trap changed r{c}: {why}");
                            }
                            continue;
                        }
                        e.1 += 1;
                        assert_eq!(plain.step(1), 1, "{why}: {:?}", plain.halt);
                        for c in 0..NUREG {
                            // The sequencer's registers (loop and PC stack
                            // mirrors) are the kernel's, not the body's.
                            if (96..112).contains(&c) {
                                continue;
                            }
                            assert_eq!(plain.s.r[c], hel.s.r[c], "r{c}: {why}");
                        }
                        let mut diffs = Vec::new();
                        for b in 0..DATA_WORDS * 4 {
                            let a = plain.s.mem.canonical_of(DATA + b);
                            if plain.s.mem.byte(a) != hel.s.mem.byte(a) {
                                diffs.push(b);
                            }
                        }
                        assert!(diffs.is_empty(), "data {diffs:x?}: {why}");
                    }
                    _ => continue,
                }
            }
        }
    }
    for (form, (n, ok)) in &ran {
        let sk = skipped.get(form).copied().unwrap_or(0);
        eprintln!("helper {form:8} planned {n:5} completed {ok:5} no plan {sk:5}");
    }
    eprintln!("no plan at all: {skipped:?}");
    assert!(ran.values().any(|v| v.1 > 0), "no helper body ran");
}

/// ns per helper call, the generated body against the interpreter's step on
/// the same instruction (`cargo test --release --lib helper_ns -- --ignored --nocapture`).
#[test]
#[ignore]
fn helper_ns() {
    use crate::fast::gen_helpers;
    use crate::fast::helper;
    use std::time::Instant;
    // 3c: R0 = DM(I0, M0) (post-modify), no compute; 4a-like store variants are
    // measured through the vectors below.
    let mut pick = Vec::new();
    for (vi, (form, parcels, words)) in VECTORS.iter().enumerate() {
        if !["3c", "4a", "15b", "4b"].contains(form) {
            continue;
        }
        let code = &words[..*parcels as usize];
        let e = engine_cfg(code, 0, 0x77 ^ vi as u64, false, 0);
        if let Ok(dec) = crate::fast::region::decode_one(&e.s, 0)
            && dec.form == *form
            && let Some(p) = helper::plan(&dec, &*e.s)
        {
            let mut t = e;
            let before: Vec<_> = (0..NUREG).map(|c| t.s.r[c]).collect();
            if gen_helpers::run(p.id, &mut t.s, &p.f) == 0 {
                pick.push((form, code.to_vec(), p, before));
            }
        }
        if pick.len() >= 12 {
            break;
        }
    }
    assert!(!pick.is_empty());
    let n = 200_000u32;
    let mut tot_gen = 0.0;
    let mut tot_int = 0.0;
    for (form, code, p, _) in &pick {
        let mut e = engine_cfg(code, 0, 0x77, false, 0);
        let t0 = Instant::now();
        for _ in 0..n {
            std::hint::black_box(gen_helpers::run(p.id, &mut e.s, &p.f));
        }
        let g = t0.elapsed().as_nanos() as f64 / n as f64;
        let rec = helper::HelperRec::leak_gen(0, None);
        let mut e2 = engine_cfg(code, 0, 0x77, false, 0);
        let t1 = Instant::now();
        for _ in 0..n {
            // The interpreter's step at PC 0 (what the stub does).
            e2.s.pc_sw = 0;
            std::hint::black_box(helper::helper_exec(&mut *e2.s as *mut _ as *mut u8, rec));
        }
        let i = t1.elapsed().as_nanos() as f64 / n as f64;
        eprintln!("helper_ns {form:4} gen {g:6.1} ns, interpreter step {i:6.1} ns");
        tot_gen += g;
        tot_int += i;
    }
    let k = pick.len() as f64;
    eprintln!(
        "helper_ns mean gen {:.1} ns, interpreter {:.1} ns",
        tot_gen / k,
        tot_int / k
    );
}
