//! Differential tests of the fast tier against the interpreter.
//!
//!     fast_diff forms IMAGE STATE [--trials N] [--backend cl|interp]
//!                                 [--seed S] [--filter SUBSTR]
//!     fast_diff region IMAGE STATE_DIR PC_HEX [--backend ...]
//!
//! `forms`: for every lowered form and compute/transfer kind, instances with
//! random fields are lowered as one-instruction regions and run through the
//! backend on random engine states; each result is compared with `exec_insn`
//! on the same state (all registers with their masks, flags, loop and PC
//! stacks, counters, the data window bytes and guard bands). A decline must
//! leave the state untouched and happen exactly when the float result is not
//! a finite normal number or zero (or a requirement fails).
//!
//! `region`: the captured entry state of a DO-loop region against the
//! interpreter for the whole loop, a budget that ends mid-loop, and values
//! injected so the loop exits mid-iteration.

use sharc_native::Dispatch;
use sharc_native::fast::glue::{Ctx, Decline, run as fast_run};
use sharc_native::fast::interp::InterpBackend;
use sharc_native::fast::ir::{CompiledKernel, KernelBackend};
use sharc_native::fast::region::{Region, build_straight};
use sharc_native::fast::{FastEngine, FastTier};
use sharc_native::mem::Mem;
use sharc_native::rt::{Int, Loop, NUREG, Pending, Spec, St, V};
use sharc_native::{Engine, exec_insn};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

// -- plumbing -----------------------------------------------------------------

const CLOCK_BASE: u64 = 573_627_620;
const CODE_PC: u32 = 0x1f_f000;
const DATA_LO: u32 = 0x25_f000;
const DATA_LEN: u32 = 0x1000;
const BAND: u32 = 64;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn u32(&mut self) -> u32 {
        (self.next() >> 32) as u32
    }
    fn below(&mut self, n: u32) -> u32 {
        (self.next() % n as u64) as u32
    }
    fn chance(&mut self, one_in: u32) -> bool {
        self.below(one_in) == 0
    }
}

fn open(image: &[u8], state: &[u8], clock_base: u64) -> Engine {
    let mut e = Engine::from_image(image).expect("image blob");
    e.import(state).expect("state blob");
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

fn backend(name: &str) -> Box<dyn KernelBackend> {
    match name {
        "interp" => Box::new(InterpBackend),
        // SHARC_FAST_BACKEND=path of the shared library.
        "plugin" => sharc_native::fast::default_backend(),
        _ => Box::<sharc_fast_cl::ClBackend>::default(),
    }
}

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .map(|i| args[i + 1].as_str())
}

// -- state snapshots ------------------------------------------------------------

#[derive(Clone, PartialEq, Debug)]
struct Snap {
    r: Vec<V>,
    bank_alt: Vec<V>,
    loops: Vec<Loop>,
    loop_depth: Int,
    loop_slots: Vec<(V, V)>,
    call_stack: Vec<Int>,
    pc_stack: Vec<Int>,
    status_stack: Vec<(V, V, V)>,
    special: Vec<Spec>,
    steps: Int,
    at_loaded_entry: bool,
    timer_written: bool,
    pc_sw: Int,
    pending: Option<Pending>,
    icount: u64,
    mmrs: Vec<(u32, V)>,
    masks: [Int; 3],
    mem: Vec<u8>,
    /// Every overlay byte (region tests only).
    dirty: Vec<(u32, u8)>,
}

fn mem_lo() -> u32 {
    DATA_LO - BAND
}
fn mem_len() -> u32 {
    DATA_LEN + 2 * BAND
}

impl Snap {
    fn take(s: &St) -> Snap {
        Snap::take_with(s, false)
    }

    fn take_with(s: &St, full: bool) -> Snap {
        Snap {
            dirty: if full {
                s.mem.dirty_bytes()
            } else {
                Vec::new()
            },
            r: s.r.to_vec(),
            bank_alt: s.bank_alt.to_vec(),
            loops: s.loops.items().to_vec(),
            loop_depth: s.loop_depth,
            loop_slots: s.loop_slots.items().to_vec(),
            call_stack: s.call_stack.items().to_vec(),
            pc_stack: s.pc_stack.items().to_vec(),
            status_stack: s.status_stack.items().to_vec(),
            special: s.special.to_vec(),
            steps: s.steps,
            at_loaded_entry: s.at_loaded_entry,
            timer_written: s.timer_written,
            pc_sw: s.pc_sw,
            pending: s.pending,
            icount: s.icount,
            mmrs: s.mmrs.clone(),
            masks: [
                s.bank_active_mask,
                s.bank_pending_mask,
                s.bank_requested_mask,
            ],
            mem: if full {
                Vec::new()
            } else {
                (0..mem_len())
                    .map(|k| {
                        let a = s.mem.canonical_of(mem_lo() + k);
                        s.mem.byte(a)
                    })
                    .collect()
            },
        }
    }

    fn restore(&self, s: &mut St) {
        s.r.copy_from_slice(&self.r);
        s.bank_alt.copy_from_slice(&self.bank_alt);
        s.loops.n = self.loops.len();
        s.loops.a[..self.loops.len()].copy_from_slice(&self.loops);
        s.loop_depth = self.loop_depth;
        s.loop_slots.n = self.loop_slots.len();
        s.loop_slots.a[..self.loop_slots.len()].copy_from_slice(&self.loop_slots);
        s.call_stack.n = self.call_stack.len();
        s.call_stack.a[..self.call_stack.len()].copy_from_slice(&self.call_stack);
        s.pc_stack.n = self.pc_stack.len();
        s.pc_stack.a[..self.pc_stack.len()].copy_from_slice(&self.pc_stack);
        s.status_stack.n = self.status_stack.len();
        s.status_stack.a[..self.status_stack.len()].copy_from_slice(&self.status_stack);
        s.special.copy_from_slice(&self.special);
        s.steps = self.steps;
        s.at_loaded_entry = self.at_loaded_entry;
        s.timer_written = self.timer_written;
        s.pc_sw = self.pc_sw;
        s.pending = self.pending;
        s.icount = self.icount;
        s.mmrs.clone_from(&self.mmrs);
        s.bank_active_mask = self.masks[0];
        s.bank_pending_mask = self.masks[1];
        s.bank_requested_mask = self.masks[2];
        for (k, b) in self.mem.iter().enumerate() {
            let a = s.mem.canonical_of(mem_lo() + k as u32);
            if s.mem.byte(a) != *b {
                s.mem.write_byte(a, *b);
            }
        }
        s.trap = None;
    }
}

fn diff(a: &Snap, b: &Snap) -> Vec<String> {
    diff_with(a, b, true)
}

/// `steps` is counted by the interpreter and the fast tier, not by the
/// generated blocks: compare it only when no block ran.
fn diff_with(a: &Snap, b: &Snap, steps: bool) -> Vec<String> {
    let mut out = Vec::new();
    for i in 0..a.r.len() {
        if a.r[i] != b.r[i] {
            out.push(format!(
                "r[{i}] interp {:08x}/{:08x} fast {:08x}/{:08x}",
                a.r[i].b, a.r[i].m, b.r[i].b, b.r[i].m
            ));
        }
    }
    macro_rules! cmp {
        ($f:ident) => {
            if a.$f != b.$f {
                out.push(format!(
                    "{} interp {:?} fast {:?}",
                    stringify!($f),
                    a.$f,
                    b.$f
                ));
            }
        };
    }
    cmp!(bank_alt);
    cmp!(loops);
    cmp!(loop_depth);
    cmp!(loop_slots);
    cmp!(call_stack);
    cmp!(pc_stack);
    cmp!(status_stack);
    cmp!(special);
    if steps {
        cmp!(steps);
    }
    cmp!(at_loaded_entry);
    cmp!(timer_written);
    cmp!(pc_sw);
    cmp!(pending);
    cmp!(icount);
    cmp!(mmrs);
    cmp!(masks);
    if a.dirty != b.dirty {
        let am: BTreeMap<_, _> = a.dirty.iter().cloned().collect();
        let bm: BTreeMap<_, _> = b.dirty.iter().cloned().collect();
        let mut keys: Vec<_> = am.keys().chain(bm.keys()).collect();
        keys.sort();
        keys.dedup();
        let bad: Vec<_> = keys
            .into_iter()
            .filter(|k| am.get(k) != bm.get(k))
            .collect();
        out.push(format!(
            "dirty memory: {} bytes differ, first at {:#x} (interp {:?} fast {:?})",
            bad.len(),
            bad[0],
            am.get(bad[0]),
            bm.get(bad[0])
        ));
    }
    if a.mem != b.mem {
        let bad: Vec<_> = (0..a.mem.len()).filter(|&k| a.mem[k] != b.mem[k]).collect();
        out.push(format!(
            "memory: {} bytes differ, first at +{:#x} (interp {:02x} fast {:02x})",
            bad.len(),
            bad[0] as u32 + mem_lo(),
            a.mem[bad[0]],
            b.mem[bad[0]]
        ));
    }
    out
}

// -- instruction assembly ---------------------------------------------------------

/// A 48-bit instruction frame being assembled.
#[derive(Clone, Copy)]
struct Raw(u64);

impl Raw {
    fn set(&mut self, hi: u32, lo: u32, v: u64) {
        let w = hi - lo + 1;
        let m = ((1u64 << w) - 1) << lo;
        self.0 = (self.0 & !m) | ((v << lo) & m);
    }
}

/// (name, mask, value) of the forms we assemble (public ISA facts).
fn form_base(form: &str) -> (u64, u64) {
    match form {
        "2a" => (0xff0000000000, 0x010000000000),
        "2a_short" => (0xff8000000000, 0x018000000000),
        "2c" => (0xf00000000000, 0xc00000000000),
        "3a" => (0xe00000000000, 0x400000000000),
        "3b" => (0xe000007c0000, 0x4000003c0000),
        "3c" => (0xf01000000000, 0x901000000000),
        "5a_move" => (0xf80000000000, 0x700000000000),
        "5b_move" => (0xf800007f0000, 0x7000003f0000),
        "6a_mem" => (0xf00000000000, 0x800000000000),
        "6b_shiftimm" => (0xffc187800000, 0x020000000000),
        "7a" => (0xff0000000000, 0x040000000000),
        "17a" => (0xff8000000000, 0x0f0000000000),
        "4a" => (0xf00000000000, 0x600000000000),
        "4b" => (0xf00000780000, 0x600000380000),
        "4d" => (0xf00000780000, 0x600000300000),
        "15a" => (0xe00000000000, 0xa00000000000),
        "15b" => (0xf01c00000000, 0x900800000000),
        "14a" => (0xfc0000000000, 0x100000000000),
        "19a" => (0xff8000000000, 0x160000000000),
        "19a_scaled" => (0xff0000000000, 0x150000000000),
        "7b" => (0xff00007f0000, 0x0400003f0000),
        _ => panic!("form {form}"),
    }
}

fn write_code(s: &mut St, raw: u64, nwords: u32) {
    // The instruction words, then plenty of valid successors (a 2c nop:
    // fixed bits 1100 and an all-zero short compute "R0 = R0 + R0").
    let words = [(raw >> 32) as u16, (raw >> 16) as u16, raw as u16];
    let mut all: Vec<u16> = words[..nwords as usize].to_vec();
    for _ in 0..24 {
        all.push(0xc000);
    }
    for (k, w) in all.iter().enumerate() {
        let a = 0x2800_0000 + (CODE_PC + k as u32) * 2;
        s.mem.write_byte(a, *w as u8);
        s.mem.write_byte(a + 1, (*w >> 8) as u8);
    }
}

#[derive(Clone, Copy, Debug)]
enum Check {
    None,
    FAdd(u32, u32),
    FSub(u32, u32),
    FNeg(u32),
    FMul(u32, u32),
    Trunc(u32),
    /// MUL/ALU add or subtract: the four operand registers, true for subtract.
    MfAdd([u32; 4], bool),
    /// MUL dual add/subtract operand registers.
    MfDual([u32; 4]),
    DualF(u32, u32),
    FAbs(u32),
    FPass(u32),
    FClip(u32, u32),
    FComp(u32, u32),
    Logb(u32),
    Scalb(u32, u32),
    Recips(u32),
    /// fix / trunc, optionally by a scale register.
    Fix(u32, Option<u32>),
    FloatBy(u32, u32),
    MulSsi(u32, u32),
}

struct Unit {
    form: &'static str,
    name: String,
    check: Check,
    /// Build one instance (raw frame) with random fields.
    make: Box<dyn Fn(&mut Rng) -> (u64, Check)>,
}

fn compute_check(cu: u32, opcode: u32, rn: u32, rx: u32, ry: u32) -> Check {
    let _ = rn;
    match (cu, opcode) {
        (0, 0x81) => Check::FAdd(rx, ry),
        (0, 0x82) => Check::FSub(rx, ry),
        (0, 0xa2) => Check::FNeg(rx),
        (0, 0xcd) => Check::Trunc(rx),
        (1, 0x30) => Check::FMul(rx, ry),
        (0, 0xb0) => Check::FAbs(rx),
        (0, 0xa1) => Check::FPass(rx),
        (0, 0xe3) => Check::FClip(rx, ry),
        (0, 0x8a) => Check::FComp(rx, ry),
        (0, 0xc1) => Check::Logb(rx),
        (0, 0xbd) => Check::Scalb(rx, ry),
        (0, 0xc4) => Check::Recips(rx),
        (0, 0xc9) => Check::Fix(rx, None),
        (0, 0xd9 | 0xdd) => Check::Fix(rx, Some(ry)),
        (0, 0xda) => Check::FloatBy(rx, ry),
        (1, 0x70) => Check::MulSsi(rx, ry),
        _ => Check::None,
    }
}

const FULL_OPS: &[(&str, u32, u32)] = &[
    ("add", 0, 0x01),
    ("sub", 0, 0x02),
    ("neg", 0, 0x22),
    ("pass", 0, 0x21),
    ("and", 0, 0x40),
    ("or", 0, 0x41),
    ("xor", 0, 0x42),
    ("not", 0, 0x43),
    ("fadd", 0, 0x81),
    ("fsub", 0, 0x82),
    ("fneg", 0, 0xa2),
    ("float", 0, 0xca),
    ("trunc", 0, 0xcd),
    ("fmul", 1, 0x30),
    ("inc", 0, 0x29),
    ("dec", 0, 0x2a),
    ("comp", 0, 0x0a),
    ("compu", 0, 0x0b),
    ("min", 0, 0x61),
    ("max", 0, 0x62),
    ("fpass", 0, 0xa1),
    ("fabs", 0, 0xb0),
    ("fclip", 0, 0xe3),
    ("fcomp", 0, 0x8a),
    ("logb", 0, 0xc1),
    ("scalb", 0, 0xbd),
    ("recips", 0, 0xc4),
    ("fix", 0, 0xc9),
    ("fix_by", 0, 0xd9),
    ("trunc_by", 0, 0xdd),
    ("float_by", 0, 0xda),
    ("mul_ssi", 1, 0x70),
    ("lshift_r", 2, 0x00),
    ("ashift_r", 2, 0x04),
    ("lshift_or_r", 2, 0x20),
    ("leftz", 2, 0x88),
    ("btst", 2, 0xcc),
];

const SHORT_OPS: &[(&str, u32)] = &[
    ("add", 0x0),
    ("sub", 0x1),
    ("pass", 0x2),
    ("fadd", 0x8),
    ("fsub", 0x9),
    ("float", 0xa),
    ("and", 0xc),
    ("or", 0xd),
    ("xor", 0xe),
    ("fmul", 0xf),
    ("comp", 0x3),
    ("not", 0x4),
    ("inc", 0x5),
    ("dec", 0x6),
    ("mul", 0x7),
    ("fcomp", 0xb),
];

const SHIFT_OPS: &[(&str, u32)] = &[
    ("lshift", 0x00),
    ("ashift", 0x01),
    ("lshift-or", 0x08),
    ("fext", 0x10),
    ("fext-se", 0x12),
];

fn units() -> Vec<Unit> {
    let mut v: Vec<Unit> = Vec::new();
    // Full computes in every hosting form. Each entry builds a 23-bit compute
    // field (and what the result domain check needs).
    type Gen = Rc<dyn Fn(&mut Rng) -> (u32, Check)>;
    let mut computes: Vec<(String, Gen)> = Vec::new();
    for &(name, cu, opcode) in FULL_OPS {
        computes.push((
            name.to_string(),
            Rc::new(move |r: &mut Rng| {
                let (rn, rx, ry) = (r.below(16), r.below(16), r.below(16));
                (
                    (cu << 20) | (opcode << 12) | (rn << 8) | (rx << 4) | ry,
                    compute_check(cu, opcode, rn, rx, ry),
                )
            }),
        ));
    }
    // Multifunction: MUL/ALU add and subtract, MUL dual add/subtract, and the
    // single-function dual add/subtract (fixed and float).
    for (name, category) in [("mf-add", 0x18u32), ("mf-sub", 0x19), ("mf-dual", 0x30)] {
        computes.push((
            name.to_string(),
            Rc::new(move |r: &mut Rng| {
                let (rm, ra) = (r.below(16), r.below(16));
                let (a, b, c, d) = (r.below(4), r.below(4), r.below(4), r.below(4));
                let cat = if category == 0x30 {
                    0x30 | r.below(16)
                } else {
                    category
                };
                let regs = [a, 4 + b, 8 + c, 12 + d];
                let check = if category == 0x30 {
                    Check::MfDual(regs)
                } else {
                    Check::MfAdd(regs, category == 0x19)
                };
                let f = (1 << 22) | (cat << 16) | (rm << 12) | (ra << 8);
                (f | (a << 6) | (b << 4) | (c << 2) | d, check)
            }),
        ));
    }
    for (name, top) in [("dual-fixed", 0x7u32), ("dual-float", 0xf)] {
        computes.push((
            name.to_string(),
            Rc::new(move |r: &mut Rng| {
                let (rn, rx, ry, rs) = (r.below(16), r.below(16), r.below(16), r.below(16));
                let check = if top == 0xf {
                    Check::DualF(rx, ry)
                } else {
                    Check::None
                };
                (
                    (((top << 4) | rs) << 12) | (rn << 8) | (rx << 4) | ry,
                    check,
                )
            }),
        ));
    }
    for (name, mkc) in computes {
        for form in [
            "2a",
            "2a_short",
            "3a_load_post",
            "3a_store_pre",
            "3a_nomem",
            "5a_move",
            "7a",
        ] {
            let (hform, kind): (&'static str, &str) = match form {
                "3a_load_post" => ("3a", "load-post"),
                "3a_store_pre" => ("3a", "store-pre"),
                "3a_nomem" => ("3a", "load-pre"),
                "2a" => ("2a", ""),
                "2a_short" => ("2a_short", ""),
                "5a_move" => ("5a_move", ""),
                _ => ("7a", ""),
            };
            let kind = kind.to_string();
            let mkc = mkc.clone();
            v.push(Unit {
                form: hform,
                name: format!("{hform}/{kind}/{name}"),
                check: Check::None,
                make: Box::new(move |r| {
                    let (compute, check) = mkc(r);
                    let (mask, base) = form_base(hform);
                    let mut raw = Raw((r.next() & 0xffff_ffff_ffff & !mask) | base);
                    raw.set(22, 0, compute as u64);
                    match hform {
                        "2a" => raw.set(37, 33, 0x1f),
                        "2a_short" => {
                            raw = Raw((r.next() & 0xffff_ffff_ffff & !mask) | base);
                            raw.set(38, 16, compute as u64);
                        }
                        "3a" => {
                            let (u, d) = match kind.as_str() {
                                "load-post" => (1, 0),
                                "store-pre" => (0, 1),
                                _ => (0, 0),
                            };
                            raw.set(44, 44, u);
                            raw.set(37, 33, 0x1f);
                            raw.set(32, 32, r.below(2) as u64);
                            raw.set(31, 31, d);
                            raw.set(30, 30, 0);
                            raw.set(29, 23, r.below(48) as u64);
                        }
                        "5a_move" => {
                            raw.set(37, 33, 0x1f);
                            let src = r.below(48);
                            raw.set(42, 38, (src >> 2) as u64);
                            raw.set(32, 32, ((src >> 1) & 1) as u64);
                            raw.set(31, 31, (src & 1) as u64);
                            raw.set(29, 23, r.below(48) as u64);
                        }
                        _ => {
                            // 7a: plain modify, (sw), (nw).
                            raw.set(37, 33, 0x1f);
                            raw.set(39, 39, (r.below(3) == 2) as u64);
                            raw.set(23, 23, (r.below(3) == 1) as u64);
                            raw.set(38, 38, r.below(2) as u64);
                        }
                    }
                    (raw.0, check)
                }),
            });
        }
    }
    // Short computes (2c).
    for &(name, opc) in SHORT_OPS {
        v.push(Unit {
            form: "2c",
            name: format!("2c/{name}"),
            check: Check::None,
            make: Box::new(move |r| {
                let (rn, rx) = (r.below(16), r.below(16));
                let (mask, base) = form_base("2c");
                let mut raw = Raw((r.next() & 0xffff_ffff_ffff & !mask) | base);
                raw.set(43, 32, ((opc << 8) | (rn << 4) | rx) as u64);
                let check = match opc {
                    0x8 => Check::FAdd(rn, rx),
                    0x9 => Check::FSub(rn, rx),
                    0xf => Check::FMul(rn, rx),
                    0xb => Check::FComp(rn, rx),
                    _ => Check::None,
                };
                (raw.0, check)
            }),
        });
    }
    // Shifter immediates in 6b and 6a_mem.
    for &(name, opc) in SHIFT_OPS {
        for hform in ["6b_shiftimm", "6a_mem"] {
            v.push(Unit {
                form: hform,
                name: format!("{hform}/{name}"),
                check: Check::None,
                make: Box::new(move |r| {
                    let (mask, base) = form_base(hform);
                    let mut raw = Raw((r.next() & 0xffff_ffff_ffff & !mask) | base);
                    let (rn, rx) = (r.below(16), r.below(16));
                    let data8 = match opc {
                        0x10 | 0x12 => r.below(256),
                        _ => {
                            if r.chance(4) {
                                [0u32, 31, 32, 0xe0, 0xe1, 0xff, 1][r.below(7) as usize]
                            } else {
                                r.below(256)
                            }
                        }
                    };
                    let f = (opc << 16) | (data8 << 8) | (rn << 4) | rx;
                    raw.set(22, 0, f as u64);
                    raw.set(37, 33, 0x1f);
                    raw.set(30, 27, r.below(16) as u64);
                    if hform == "6a_mem" {
                        raw.set(43, 41, r.below(8) as u64);
                        raw.set(40, 38, r.below(8) as u64);
                        raw.set(32, 32, r.below(2) as u64);
                        raw.set(31, 31, r.below(2) as u64);
                        raw.set(26, 23, r.below(16) as u64);
                    }
                    (raw.0, Check::None)
                }),
            });
        }
    }
    // Transfers.
    for (hform, kind) in [
        ("3a", "load-post"),
        ("3a", "load-pre"),
        ("3a", "store-post"),
        ("3a", "store-pre"),
        ("3b", "load-post"),
        ("3b", "load-pre"),
        ("3b", "store-post"),
        ("3b", "store-pre"),
        ("3c", "load"),
        ("3c", "store"),
    ] {
        v.push(Unit {
            form: hform,
            name: format!("{hform}/xfer/{kind}"),
            check: Check::None,
            make: Box::new(move |r| {
                let (mask, base) = form_base(hform);
                let mut raw = Raw((r.next() & 0xffff_ffff_ffff & !mask) | base);
                let store = kind.starts_with("store");
                match hform {
                    "3a" | "3b" => {
                        raw.set(44, 44, kind.ends_with("post") as u64);
                        raw.set(43, 41, r.below(8) as u64);
                        raw.set(40, 38, r.below(8) as u64);
                        raw.set(37, 33, 0x1f);
                        raw.set(32, 32, r.below(2) as u64);
                        raw.set(31, 31, store as u64);
                        raw.set(30, 30, 0);
                        raw.set(29, 23, r.below(48) as u64);
                        if hform == "3a" {
                            raw.set(22, 0, 0);
                        } else {
                            raw.set(17, 17, 1);
                            raw.set(16, 16, 1);
                        }
                    }
                    _ => {
                        raw.set(43, 41, r.below(8) as u64);
                        raw.set(40, 38, r.below(8) as u64);
                        raw.set(37, 37, store as u64);
                        raw.set(35, 32, r.below(16) as u64);
                    }
                }
                (raw.0, Check::None)
            }),
        });
    }
    // Moves and immediates.
    v.push(Unit {
        form: "5b_move",
        name: "5b_move".into(),
        check: Check::None,
        make: Box::new(|r| {
            let (mask, base) = form_base("5b_move");
            let mut raw = Raw((r.next() & 0xffff_ffff_ffff & !mask) | base);
            raw.set(37, 33, 0x1f);
            let src = r.below(48);
            raw.set(42, 38, (src >> 2) as u64);
            raw.set(32, 32, ((src >> 1) & 1) as u64);
            raw.set(31, 31, (src & 1) as u64);
            raw.set(29, 23, r.below(48) as u64);
            (raw.0, Check::None)
        }),
    });
    v.push(Unit {
        form: "17a",
        name: "17a".into(),
        check: Check::None,
        make: Box::new(|r| {
            let (mask, base) = form_base("17a");
            let mut raw = Raw((r.next() & 0xffff_ffff_ffff & !mask) | base);
            raw.set(38, 32, r.below(48) as u64);
            (raw.0, Check::None)
        }),
    });
    v.extend(units_mem());
    for u in &mut v {
        u.check = Check::None;
    }
    v
}

/// Immediate-offset transfers and immediate modifies (`lower::mem_imm`).
fn units_mem() -> Vec<Unit> {
    let mut v: Vec<Unit> = Vec::new();
    // A small signed word offset that keeps I +- 4*off inside the data window.
    fn small(r: &mut Rng, bits: u32) -> u64 {
        let half = 1u32 << (bits - 1);
        let k = if r.chance(8) {
            [0, 1, half - 1, half][r.below(4) as usize]
        } else {
            r.below(1 << bits)
        };
        (k as u64) & ((1u64 << bits) - 1)
    }
    // 4a, 4b, 4d: index plus a signed 6-bit offset.
    for (hform, kind) in [
        ("4a", "load-post"),
        ("4a", "load-pre"),
        ("4a", "store-post"),
        ("4a", "store-pre"),
        ("4b", "load-post"),
        ("4b", "load-pre"),
        ("4b", "store-post"),
        ("4b", "store-pre"),
        ("4d", "load-post"),
        ("4d", "load-pre"),
        ("4d", "store-post"),
        ("4d", "store-pre"),
    ] {
        v.push(Unit {
            form: hform,
            name: format!("{hform}/xfer/{kind}"),
            check: Check::None,
            make: Box::new(move |r| {
                let (mask, base) = form_base(hform);
                let mut raw = Raw((r.next() & 0xffff_ffff_ffff & !mask) | base);
                raw.set(43, 41, r.below(8) as u64);
                raw.set(40, 40, r.below(2) as u64);
                raw.set(39, 39, kind.starts_with("store") as u64);
                raw.set(38, 38, kind.ends_with("post") as u64);
                raw.set(37, 33, 0x1f);
                let off = small(r, 6);
                raw.set(32, 32, off >> 5);
                raw.set(31, 27, off & 31);
                raw.set(26, 23, r.below(16) as u64);
                match hform {
                    "4a" => raw.set(22, 0, 0),
                    "4b" => {
                        raw.set(18, 16, 0b111);
                    }
                    _ => {
                        raw.set(18, 16, 0b011);
                    }
                }
                (raw.0, Check::None)
            }),
        });
    }
    // 4a with a compute next to the transfer.
    for &(name, cu, opcode) in FULL_OPS {
        v.push(Unit {
            form: "4a",
            name: format!("4a/compute/{name}"),
            check: Check::None,
            make: Box::new(move |r| {
                let (rn, rx, ry) = (r.below(16), r.below(16), r.below(16));
                let compute = (cu << 20) | (opcode << 12) | (rn << 8) | (rx << 4) | ry;
                let (mask, base) = form_base("4a");
                let mut raw = Raw((r.next() & 0xffff_ffff_ffff & !mask) | base);
                raw.set(43, 41, r.below(8) as u64);
                raw.set(40, 40, r.below(2) as u64);
                raw.set(39, 39, r.below(2) as u64);
                raw.set(38, 38, r.below(2) as u64);
                raw.set(37, 33, 0x1f);
                let off = small(r, 6);
                raw.set(32, 32, off >> 5);
                raw.set(31, 27, off & 31);
                raw.set(26, 23, r.below(16) as u64);
                raw.set(22, 0, compute as u64);
                (raw.0, compute_check(cu, opcode, rn, rx, ry))
            }),
        });
    }
    // 15b and 15a: pre-modify by an immediate, any register in R, I, M.
    for (hform, kind) in [
        ("15b", "load"),
        ("15b", "store"),
        ("15a", "load"),
        ("15a", "store"),
    ] {
        v.push(Unit {
            form: hform,
            name: format!("{hform}/xfer/{kind}"),
            check: Check::None,
            make: Box::new(move |r| {
                let (mask, base) = form_base(hform);
                let mut raw = Raw((r.next() & 0xffff_ffff_ffff & !mask) | base);
                raw.set(43, 41, r.below(8) as u64);
                raw.set(40, 40, (kind == "store") as u64);
                raw.set(39, 39, 0);
                if hform == "15b" {
                    raw.set(37, 37, r.below(2) as u64);
                    raw.set(29, 23, r.below(48) as u64);
                    raw.set(22, 16, small(r, 7));
                } else {
                    raw.set(44, 44, r.below(2) as u64);
                    raw.set(38, 32, r.below(48) as u64);
                    // A signed word offset; negative values are the
                    // 0xfffffffN style of the firmware.
                    let w = small(r, 8) as u32;
                    let w = (((w ^ 0x80) as i32) - 0x80) as u32;
                    raw.set(31, 0, w as u64);
                }
                (raw.0, Check::None)
            }),
        });
    }
    // 14a: an absolute address (a few outside the plain range: refused).
    for kind in ["load", "store"] {
        v.push(Unit {
            form: "14a",
            name: format!("14a/xfer/{kind}"),
            check: Check::None,
            make: Box::new(move |r| {
                let (mask, base) = form_base("14a");
                let mut raw = Raw((r.next() & 0xffff_ffff_ffff & !mask) | base);
                raw.set(41, 41, r.below(2) as u64);
                raw.set(40, 40, (kind == "store") as u64);
                raw.set(39, 39, 0);
                raw.set(38, 32, r.below(48) as u64);
                let mut a = DATA_LO + 4 * r.below(0x400);
                if r.chance(8) {
                    a += r.below(4);
                }
                if r.chance(32) {
                    a = r.u32();
                }
                raw.set(31, 0, a as u64);
                (raw.0, Check::None)
            }),
        });
    }
    // Immediate modifies.
    for hform in ["19a", "19a_scaled", "7b"] {
        v.push(Unit {
            form: hform,
            name: format!("{hform}/modify"),
            check: Check::None,
            make: Box::new(move |r| {
                let (mask, base) = form_base(hform);
                let mut raw = Raw((r.next() & 0xffff_ffff_ffff & !mask) | base);
                if hform == "7b" {
                    raw.set(38, 38, r.below(2) as u64);
                    raw.set(37, 33, 0x1f);
                } else {
                    raw.set(38, 38, r.below(2) as u64);
                    if hform == "19a_scaled" {
                        raw.set(39, 39, r.below(2) as u64);
                    }
                    let d = if r.chance(4) {
                        r.u32()
                    } else {
                        (r.below(512) as i32 - 256) as u32
                    };
                    raw.set(31, 0, d as u64);
                }
                (raw.0, Check::None)
            }),
        });
    }
    v
}

// -- random states ------------------------------------------------------------------

fn rand_f32(r: &mut Rng) -> u32 {
    match r.below(16) {
        0 => 0,
        1 => 0x8000_0000,
        2 => 0x7f80_0000 | (r.below(2) << 31),
        3 => 0x7fc0_0000 | r.below(0x40_0000),
        4 => r.below(0x80_0000) | (r.below(2) << 31),
        5 => f32::to_bits((r.below(2000) as f32 - 1000.0) * 0.25),
        6 => f32::to_bits(f32::from_bits(r.u32() & 0x4effffff) * 1.0),
        7 => 0x4f00_0000 + r.below(8) - 4,
        8 => f32::to_bits((r.below(1 << 24) as f32) * 0.5),
        9 => 0x7f7f_ffff - r.below(4),
        10 => 0x0080_0000 + r.below(4),
        _ => {
            // Random normal, moderate exponent.
            let e = 100 + r.below(60);
            (r.below(2) << 31) | (e << 23) | r.below(1 << 23)
        }
    }
}

fn rand_int(r: &mut Rng) -> u32 {
    match r.below(10) {
        0 => 0,
        1 => 1,
        2 => 0xffff_ffff,
        3 => 0x8000_0000,
        4 => 0x7fff_ffff,
        5 => r.below(64),
        6 => r.u32() >> r.below(32),
        7 => (r.below(700) as i32 - 350) as u32,
        _ => r.u32(),
    }
}

static KNOWN_FLAGS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Mostly ordinary float values (long kernel runs), a few special ones.
static CALM: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn randomise(s: &mut St, r: &mut Rng, ms: &[u32; 16]) {
    let calm = CALM.load(std::sync::atomic::Ordering::Relaxed);
    for c in 0..16 {
        let v = if calm && !r.chance(24) {
            calm_value(r)
        } else if r.chance(2) {
            rand_f32(r)
        } else {
            rand_int(r)
        };
        s.r[c] = V::c(v as Int);
    }
    for c in 16..32 {
        // Index registers: word aligned inside the data window (mostly).
        let mut a = DATA_LO + 0x400 + 4 * r.below(0x100);
        if r.chance(16) {
            a += r.below(4);
        }
        s.r[c] = V::c(a as Int);
    }
    for c in 32..48 {
        s.r[c] = V::c(ms[c - 32] as Int);
    }
    // Length registers: no circular buffer (a kernel that updates an index
    // register requires it).
    for c in 48..64 {
        s.r[c] = V::c(0);
    }
    // ASTATX: random value with a random known mask.
    let known = KNOWN_FLAGS.load(std::sync::atomic::Ordering::Relaxed);
    let m = if known {
        if r.chance(12) { r.u32() } else { u32::MAX }
    } else if r.chance(3) {
        u32::MAX
    } else {
        r.u32()
    };
    s.r[118] = V { b: r.u32() & m, m };
    // Data window contents: floats and ints.
    for k in 0..DATA_LEN / 4 {
        let w = if calm && !r.chance(24) {
            calm_value(r)
        } else if r.chance(2) {
            rand_f32(r)
        } else {
            rand_int(r)
        };
        let a = s.mem.canonical_of(DATA_LO + 4 * k);
        for b in 0..4 {
            s.mem.write_byte(a + b, (w >> (8 * b)) as u8);
        }
    }
}

/// Multiply: a denormal result sets MU, so it is outside the fast domain.
fn is_special(bits: u32) -> bool {
    let f = f32::from_bits(bits);
    !(f == 0.0 || f.is_normal())
}

/// Float ALU: a denormal result is exact and unflagged; NaN and infinity
/// are outside.
fn not_finite(bits: u32) -> bool {
    !f32::from_bits(bits).is_finite()
}

fn expect_special(c: Check, s: &St) -> bool {
    let f = |code: u32| f32::from_bits(s.r[code as usize].b);
    match c {
        Check::None => false,
        Check::FAdd(x, y) => not_finite((f(x) + f(y)).to_bits()),
        Check::FSub(x, y) => not_finite((f(x) - f(y)).to_bits()),
        Check::FMul(x, y) => is_special((f(x) * f(y)).to_bits()),
        Check::FNeg(x) => not_finite((-f(x)).to_bits()),
        Check::Trunc(x) => s.r[x as usize].b & 0x7fff_ffff >= 0x4f00_0000,
        Check::MfAdd([xm, ym, xa, ya], sub) => {
            let a = if sub { f(xa) - f(ya) } else { f(xa) + f(ya) };
            not_finite((f(xm) * f(ym)).to_bits()) || not_finite(a.to_bits())
        }
        Check::MfDual([xm, ym, xa, ya]) => {
            not_finite((f(xm) * f(ym)).to_bits())
                || not_finite((f(xa) + f(ya)).to_bits())
                || not_finite((f(xa) - f(ya)).to_bits())
        }
        Check::DualF(x, y) => {
            not_finite((f(x) + f(y)).to_bits()) || not_finite((f(x) - f(y)).to_bits())
        }
        Check::FAbs(x) => f(x).is_nan(),
        Check::FPass(x) => not_finite(s.r[x as usize].b),
        Check::FClip(x, y) => {
            let (a, b) = (f(x), f(y));
            if a.is_nan() || b.is_nan() {
                true
            } else {
                let r = if a.abs() < b.abs() {
                    a
                } else {
                    b.abs().copysign(a)
                };
                !r.is_finite()
            }
        }
        Check::FComp(x, y) => f(x).is_nan() || f(y).is_nan(),
        Check::Logb(x) => !(1..=254).contains(&((s.r[x as usize].b >> 23) & 0xff)),
        Check::Scalb(x, y) => {
            let sh = s.r[y as usize].b as i32 as i64;
            let bits = s.r[x as usize].b;
            let e = ((bits >> 23) & 0xff) as i64;
            if !(-1000..1000).contains(&sh) {
                true
            } else if bits & 0x7fff_ffff == 0 {
                false
            } else {
                !(1..=254).contains(&e) || e + sh > 254
            }
        }
        Check::Recips(x) => !(1..=252).contains(&((s.r[x as usize].b >> 23) & 0xff)),
        Check::Fix(x, by) => {
            let sh = by.map_or(0, |y| s.r[y as usize].b as i32 as i64);
            let bits = s.r[x as usize].b;
            let e = ((bits >> 23) & 0xff) as i64;
            if !(-1000..1000).contains(&sh) {
                true
            } else if bits & 0x7fff_ffff == 0 {
                false
            } else {
                !(1..=254).contains(&e) || e + sh >= 158
            }
        }
        Check::FloatBy(x, y) => {
            let sh = s.r[y as usize].b as i32 as i64;
            let v = s.r[x as usize].b as i32 as f32;
            let e = ((v.to_bits() >> 23) & 0xff) as i64;
            if !(-1000..1000).contains(&sh) {
                true
            } else if v == 0.0 {
                false
            } else {
                !(2..=254).contains(&(e + sh))
            }
        }
        Check::MulSsi(x, y) => {
            let p = (s.r[x as usize].b as i32 as i64) * (s.r[y as usize].b as i32 as i64);
            p != p as i32 as i64
        }
    }
}

// -- conditions ---------------------------------------------------------------------

/// Forms whose bits 37:33 are an execution condition.
fn has_cond(form: &str) -> bool {
    matches!(
        form,
        "2a" | "3a" | "3b" | "5a_move" | "5b_move" | "6a_mem" | "6b_shiftimm" | "7a"
    )
}

/// The condition codes the fast tier lowers, plus a few it must refuse.
const COND_CODES: &[u32] = &[
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x0d, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15,
    0x16, 0x17, 0x18, 0x1d, 0x00, 0x01, 0x02, 0x10, 0x11, 0x12, 0x09, 0x0e,
];

/// The predicate of the Python core (`sequencer._predicate`) for SISD:
/// None when the flags it needs are unknown.
fn pred(cond: u32, a: V, mode1: u32) -> Option<bool> {
    if cond == 0x1f {
        return Some(true);
    }
    let bit = |n: u32| -> Option<bool> { (a.m >> n & 1 == 1).then_some(a.b >> n & 1 == 1) };
    match cond {
        0x00 => bit(0),
        0x10 => bit(0).map(|x| !x),
        0x01 | 0x02 | 0x11 | 0x12 => {
            let (af, an, az) = (bit(10)?, bit(2)?, bit(0)?);
            let (x, y);
            if af {
                x = an || az;
                y = an && !az;
            } else {
                let av = bit(1)?;
                let term = if av {
                    let alusat = mode1 & (1 << 13) != 0;
                    an != !alusat
                } else {
                    an
                };
                x = term || az;
                y = term;
            }
            Some(match cond {
                0x02 => x,
                0x12 => !x,
                0x01 => y,
                _ => !y,
            })
        }
        0x03 => bit(3),
        0x13 => bit(3).map(|x| !x),
        0x04 => bit(1),
        0x14 => bit(1).map(|x| !x),
        0x05 => bit(7),
        0x15 => bit(7).map(|x| !x),
        0x06 => bit(6),
        0x16 => bit(6).map(|x| !x),
        0x07 => bit(11),
        0x17 => bit(11).map(|x| !x),
        0x08 => bit(12),
        0x18 => bit(12).map(|x| !x),
        0x0d => bit(18),
        0x1d => bit(18).map(|x| !x),
        _ => None,
    }
}

// -- the forms test -------------------------------------------------------------------

#[derive(Default)]
struct Tally {
    ran: u64,
    declined_special: u64,
    declined_req: u64,
    interp_trap: u64,
    refused: u64,
    built: u64,
    mismatches: u64,
    specials: u64,
}

fn forms(args: &[String]) {
    let image = std::fs::read(&args[0]).expect("image");
    let state = std::fs::read(&args[1]).expect("state");
    let trials: u64 = flag(args, "--trials").map_or(20000, |x| x.parse().unwrap());
    let seed: u64 = flag(args, "--seed").map_or(0x5eed, |x| x.parse().unwrap());
    let filter = flag(args, "--filter").unwrap_or("");
    let instances: u64 = flag(args, "--instances").map_or(20, |x| x.parse().unwrap());
    // --cond: conditional instructions (random condition codes, mostly
    // known flags) instead of unconditional ones.
    let cond_mode = args.iter().any(|a| a == "--cond");
    KNOWN_FLAGS.store(cond_mode, std::sync::atomic::Ordering::Relaxed);
    let mut be = backend(flag(args, "--backend").unwrap_or("cl"));
    let mut e = open(&image, &state, CLOCK_BASE);
    e.enable_runtime_decode(Mem::read_sw);
    {
        let s = &mut e.s;
        s.loops.n = 0;
        s.pending = None;
        s.cfg.core_timer = false;
        s.bank_pending_mask = -1;
        s.bank_requested_mask = -1;
        s.pc_stack_pending = -1;
        s.pc_stack_requested = -1;
        s.r[114] = V::c(0x3900_1cf8);
        s.pc_sw = CODE_PC as Int;
        // The data window and its guard bands exist and are overlay bytes.
        for k in 0..mem_len() {
            let a = s.mem.canonical_of(mem_lo() + k);
            s.mem.write_byte(a, 0xa5);
        }
    }
    let mut rng = Rng(seed);
    let mut ctx = Ctx::default();
    let all = units();
    let min_form: u64 = flag(args, "--min-form").map_or(0, |x| x.parse().unwrap());
    let mut per_form_units: BTreeMap<&str, u64> = BTreeMap::new();
    for u in all.iter().filter(|u| u.name.contains(filter)) {
        *per_form_units.entry(u.form).or_default() += 1;
    }
    let mut per_form: BTreeMap<&str, (u64, u64)> = BTreeMap::new();
    let mut total_mismatch = 0u64;
    let t0 = std::time::Instant::now();
    for unit in all.iter().filter(|u| u.name.contains(filter)) {
        let mut tally = Tally::default();
        let unit_trials = trials.max(min_form.div_ceil(per_form_units[unit.form]));
        let per_instance = (unit_trials / instances).max(1);
        let mut inst_done = 0;
        let mut attempts = 0;
        while inst_done < instances && attempts < instances * 200 {
            attempts += 1;
            let (mut raw, check) = (unit.make)(&mut rng);
            let mut cond = 0x1f;
            if cond_mode && (raw >> 33) & 0x1f == 0x1f && has_cond(unit.form) {
                cond = COND_CODES[rng.below(COND_CODES.len() as u32) as usize];
                raw = (raw & !(0x1fu64 << 33)) | ((cond as u64) << 33);
            }
            let mut ms = [0u32; 16];
            for m in ms.iter_mut() {
                *m = (rng.below(9) as i32 - 4) as u32;
            }
            let s = &mut e.s;
            randomise(s, &mut rng, &ms);
            // MODE1 at build time: every region requires TRUNCATE (bit 15)
            // clear.
            let mode_build = 0x3900_1cf8u32;
            s.r[114] = V::c(mode_build as Int);
            let nwords = match unit.form {
                "2c" | "3c" => 1,
                "2a_short" | "3b" | "5b_move" | "4b" | "15b" | "7b" => 2,
                _ => 3,
            };
            write_code(s, raw, nwords);
            s.pc_sw = CODE_PC as Int;
            let region: Region = match build_straight(s, CODE_PC) {
                Ok(r) => r,
                Err(_) => {
                    tally.refused += 1;
                    continue;
                }
            };
            if region.forms.first() != Some(&unit.form) {
                tally.refused += 1;
                continue;
            }
            let kernel: Box<dyn CompiledKernel> = match be.compile(&region.kernel) {
                Ok(k) => k,
                Err(err) => {
                    eprintln!("compile failed for {} {:012x}: {err}", unit.name, raw);
                    std::process::exit(1);
                }
            };
            tally.built += 1;
            inst_done += 1;
            let declined_before = tally.declined_req;
            for trial in 0..per_instance {
                let s = &mut e.s;
                let mut st_ms = ms;
                let mut flip = None;
                if trial % 50 == 49 {
                    let k = rng.below(16) as usize;
                    st_ms[k] ^= 1; // a baked constant changes
                    flip = Some(32 + k as u8);
                }
                randomise(s, &mut rng, &st_ms);
                let mut lflip = None;
                if trial % 50 == 24 {
                    // A circular buffer is set up on a random index register.
                    let k = rng.below(16) as usize;
                    s.r[48 + k] = V::c(1 + rng.below(64) as Int);
                    lflip = Some(48 + k as u8);
                }
                // TRUNCATE flips now and then: the MODE1 requirement fails.
                let mode_flip = trial % 37 == 36;
                s.r[114] = V::c((mode_build ^ ((mode_flip as u32) << 15)) as Int);
                // TRUNCATE flips now and then: a requirement on MODE1 fails.
                let mode_flip = trial % 37 == 36;
                s.r[114] = V::c((mode_build ^ ((mode_flip as u32) << 15)) as Int);
                let mut unknown = None;
                if rng.chance(12) {
                    // A register becomes unknown.
                    let c = rng.below(48) as usize;
                    s.r[c] = V::UNK;
                    unknown = Some(c as u8);
                }
                s.pc_sw = CODE_PC as Int;
                s.pending = None;
                s.limit = s.icount + 1000;
                let s0 = Snap::take(s);
                let executes = pred(cond, s.r[118], s.r[114].b);
                let special = expect_special(check, s) && executes != Some(false);
                let insn = sharc_native::rt::bnd::decode_at(s, (), None, CODE_PC as Int)
                    .expect("decode of the assembled instruction");
                let res = exec_insn(s, insn);
                let interp = Snap::take(s);
                s0.restore(s);
                if res.is_err() {
                    tally.interp_trap += 1;
                    s0.restore(s);
                    continue;
                }
                let req_fail = region
                    .reqs
                    .iter()
                    .any(|q| !sharc_native::fast::glue::eval_req(s, q, 1));
                // Which requirements the injected change may break.
                let expect_fail = region.reqs.iter().any(|q| {
                    use sharc_native::fast::{Base, Req};
                    match *q {
                        Req::FlagsKnown(mask) => s.r[118].m & mask != mask,
                        Req::Known(c) => Some(c) == unknown,
                        Req::Eq(c, _) => Some(c) == unknown || Some(c) == flip || Some(c) == lflip,
                        Req::Mode1 { .. } => mode_flip,
                        Req::NwPlain {
                            base: Base::Reg(c), ..
                        } => Some(c) == unknown,
                        _ => false,
                    }
                });
                let out = fast_run(s, &region, &*kernel, &mut ctx);
                let fastsnap = Snap::take(s);
                let mut bad = Vec::new();
                if req_fail != expect_fail {
                    bad.push(format!(
                        "requirements: failed {req_fail}, injected-change expectation {expect_fail}"
                    ));
                }
                match out {
                    Ok(_) => {
                        if req_fail {
                            bad.push("ran although a requirement fails".to_string());
                        }
                        if special {
                            bad.push("ran although the result is special".to_string());
                            tally.specials += 1;
                        }
                        bad.extend(diff(&interp, &fastsnap));
                        tally.ran += 1;
                    }
                    Err(Decline::Exit0) => {
                        if !special {
                            bad.push("exit before the instruction on a normal result".into());
                        }
                        bad.extend(diff(&s0, &fastsnap));
                        tally.declined_special += 1;
                    }
                    Err(d) => {
                        if std::env::var_os("FAST_DIFF_WHY").is_some()
                            && tally.declined_req - declined_before < 1
                        {
                            let why: Vec<_> = region
                                .reqs
                                .iter()
                                .filter(|q| !sharc_native::fast::glue::eval_req(s, q, 1))
                                .collect();
                            eprintln!(
                                "decline {d:?} {} {:012x}: failing {why:?}; all {:?} wins {:?}",
                                unit.name, raw, region.reqs, region.wins
                            );
                        }
                        if !req_fail {
                            bad.push(format!("declined {d:?} with the requirements met"));
                        }
                        bad.extend(diff(&s0, &fastsnap));
                        tally.declined_req += 1;
                    }
                }
                if !bad.is_empty() {
                    tally.mismatches += 1;
                    if tally.mismatches <= 3 {
                        eprintln!(
                            "MISMATCH {} raw {:012x} forms {:?}\n  {}",
                            unit.name,
                            raw,
                            region.forms,
                            bad.join("\n  ")
                        );
                        let _ = &s0;
                    }
                }
                s0.restore(s);
            }
            if std::env::var_os("FAST_DIFF_WHY").is_some() {
                eprintln!(
                    "  instance {:012x}: declined {} of {}",
                    raw,
                    tally.declined_req - declined_before,
                    per_instance
                );
            }
        }
        let total = tally.ran + tally.declined_special + tally.declined_req;
        let pf = per_form.entry(unit.form).or_default();
        pf.0 += total;
        pf.1 += tally.mismatches;
        total_mismatch += tally.mismatches;
        println!(
            "{:<28} instances {:>3} trials {:>7} ran {:>7} special-exit {:>6} req-decline {:>5} interp-trap {:>4} refused {:>4} mismatches {}",
            unit.name,
            tally.built,
            total + tally.interp_trap,
            tally.ran,
            tally.declined_special,
            tally.declined_req,
            tally.interp_trap,
            tally.refused,
            tally.mismatches
        );
        if tally.built == 0 {
            println!("  NO instance lowered for {}", unit.name);
            // Conditional index updates (post-modify) have no address range.
            if !cond_mode {
                total_mismatch += 1;
            }
        }
    }
    println!(
        "--- per form ({} backend, {:.1}s) ---",
        be.name(),
        t0.elapsed().as_secs_f64()
    );
    for (f, (n, m)) in &per_form {
        println!("{f:<12} states {n:>8} mismatches {m}");
    }
    if total_mismatch > 0 {
        println!("FAIL: {total_mismatch} mismatches");
        std::process::exit(1);
    }
    println!("PASS");
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("forms") => forms(&args[1..]),
        Some("region") => region(&args[1..]),
        Some("cfg") => cfgprog(&args[1..]),
        Some("capture") => capture(&args[1..]),
        Some("loops") => loops(&args[1..]),
        _ => {
            eprintln!("usage: fast_diff forms|region|cfg ...");
            std::process::exit(2);
        }
    }
}

struct Shared(Rc<RefCell<FastEngine>>);

// SAFETY: the test drives one engine on one thread.
unsafe impl Send for Shared {}

impl FastTier for Shared {
    fn wants(&self, pc: u32) -> bool {
        self.0.borrow().wants(pc)
    }
    fn run(&mut self, s: &mut St, pc: u32) -> Option<u32> {
        self.0.borrow_mut().run(s, pc)
    }
    fn report(&self) -> String {
        self.0.borrow().report()
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Interp,
    /// The fast tier alone (the interpreter does the rest).
    Fast,
    /// The generated blocks alone.
    Aot,
    /// The fast tier in front of the generated blocks.
    Both,
}

fn meta_clock(path: &str) -> u64 {
    let text = std::fs::read_to_string(path).expect("meta");
    text.lines()
        .find_map(|l| l.strip_prefix("clock="))
        .expect("clock")
        .parse()
        .expect("clock number")
}

fn rand_word(r: &mut Rng) -> u32 {
    if r.chance(2) {
        rand_f32(r)
    } else {
        rand_int(r)
    }
}

/// A deterministic perturbation of the engine's inputs (registers and the
/// memory the loop streams read).
fn inject(e: &mut Engine, seed: u64, what: &mut String) {
    let mut r = Rng(seed | 1);
    for _ in 0..1 + r.below(3) {
        match r.below(4) {
            3 => {
                // Flip flag bits, so conditions and branches go the other
                // way (the known mask stays as it is).
                let a = e.s.r[118];
                let flip = r.u32() & 0x0000_47ff & a.m;
                e.s.r[118] = V {
                    b: a.b ^ flip,
                    m: a.m,
                };
                what.push_str(&format!(" astatx^={flip:x}"));
            }
            0 => {
                let c = r.below(16) as usize;
                let v = rand_word(&mut r);
                e.s.r[c] = V::c(v as Int);
                what.push_str(&format!(" r{c}={v:08x}"));
            }
            1 => {
                // A word one of the index registers will reach.
                let ic = [17usize, 18, 19, 20, 21, 22, 28][r.below(7) as usize];
                let base = e.s.r[ic].b;
                let a = base.wrapping_add(4 * r.below(80));
                let v = rand_word(&mut r);
                let ca = e.s.mem.canonical_of(a);
                for b in 0..4 {
                    e.s.mem.write_byte(ca + b, (v >> (8 * b)) as u8);
                }
                what.push_str(&format!(" mem[{a:#x}]={v:08x}"));
            }
            _ => {
                // Moderate float perturbation of a data register.
                let c = r.below(16) as usize;
                let f = f32::from_bits(e.s.r[c].b) * (1.0 + (r.below(1000) as f32) * 0.001);
                e.s.r[c] = V::c(f.to_bits() as Int);
                what.push_str(&format!(" r{c}*={f}"));
            }
        }
    }
}

/// `loops IMAGE STATE_DIR PC_HEX [--seeds N] [--ops a,b,..] [--words W]`: the
/// body of a captured DO loop is replaced with random one-word short computes
/// (the first W words from the listed opcodes, the rest passes) and the loop
/// is run for the whole count and for budgets that end inside an iteration,
/// against the interpreter (`exec_insn` instruction by instruction; the fast
/// run is followed by the interpreter up to the same instruction count).
/// Registers and ASTATX are randomised, so special values exit mid-iteration.
/// This is what checks the compare ops' carry-history over many iterations.
fn loops(args: &[String]) {
    let image = std::fs::read(&args[0]).expect("image");
    let dir = &args[1];
    let pc = u32::from_str_radix(args[2].trim_start_matches("0x"), 16).expect("pc");
    let seeds: u64 = flag(args, "--seeds").map_or(200, |x| x.parse().unwrap());
    let ops: Vec<u32> = flag(args, "--ops")
        .unwrap_or("3,b,0,2,8,5,6,4")
        .split(',')
        .map(|x| u32::from_str_radix(x, 16).unwrap())
        .collect();
    let active: u32 = flag(args, "--words").map_or(12, |x| x.parse().unwrap());
    let state = std::fs::read(format!("{dir}/r_{pc:X}.state")).expect("state");
    let clock = meta_clock(&format!("{dir}/r_{pc:X}.meta"));
    let mut be = backend(flag(args, "--backend").unwrap_or("cl"));
    let make = |seed: u64| -> Engine {
        let mut e = open(&image, &state, clock);
        e.enable_runtime_decode(Mem::read_sw);
        e.s.cfg.core_timer = false;
        // The captured loop may run in SIMD mode; the fast tier handles SISD
        // only (and TRUNCATE clear), so test it with the plain mode.
        e.s.r[114] = V::c(0x3900_1cf8);
        let top = *e.s.loops.items().last().expect("loop at entry");
        let mut r = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        let words = (top.end_sw - top.start_sw + 1) as u32;
        for k in 0..words {
            let opc = if k == 0 && r.chance(2) {
                [0x3, 0xb][r.below(2) as usize]
            } else if k >= active {
                0x2
            } else {
                ops[r.below(ops.len() as u32) as usize]
            };
            let w = 0xc000u32 | (opc << 8) | (r.below(16) << 4) | r.below(16);
            let a = 0x2800_0000 + (top.start_sw as u32 + k) * 2;
            e.s.mem.write_byte(a, w as u8);
            e.s.mem.write_byte(a + 1, (w >> 8) as u8);
        }
        for c in 0..16 {
            let v = if r.chance(5) {
                0x7fc0_0000
            } else {
                rand_word(&mut r)
            };
            e.s.r[c] = V::c(v as Int);
        }
        let m = if r.chance(4) { r.u32() } else { u32::MAX };
        e.s.r[118] = V { b: r.u32() & m, m };
        e
    };
    let interp_to = |s: &mut St, target: u64| {
        while s.icount < target {
            let pc = s.pc_sw;
            let insn = sharc_native::rt::bnd::decode_at(s, (), None, pc).expect("decode");
            if exec_insn(s, insn).is_err() {
                break;
            }
        }
    };
    let probe = make(0);
    let top = *probe.s.loops.items().last().unwrap();
    let k = (top.end_sw - top.start_sw + 1) as u64;
    let rem = top.remaining as u64;
    drop(probe);
    let mut rng = Rng(0x5eed_1234);
    let mut ctx = Ctx::default();
    let (mut bad, mut ran_fast, mut total, mut refused) = (0u64, 0u64, 0u64, 0u64);
    let mut declines: BTreeMap<String, u64> = BTreeMap::new();
    for seed in 0..seeds {
        let full = k * rem;
        let budgets = [
            full,
            full + 26,
            1 + rng.below(full as u32) as u64,
            1 + rng.below(full as u32) as u64,
            k + 1 + rng.below(3 * k as u32) as u64,
        ];
        let probe = make(seed);
        let region = match sharc_native::fast::region::build_loop(&probe.s, pc) {
            Ok(r) => r,
            Err(err) => {
                refused += 1;
                if refused <= 3 {
                    println!("seed {seed}: refused: {}", err.0);
                }
                continue;
            }
        };
        let kernel = be.compile(&region.kernel).expect("compile");
        drop(probe);
        for n in budgets {
            let mut ea = make(seed);
            ea.s.limit = ea.s.icount + n;
            let target = ea.s.icount + n;
            interp_to(&mut ea.s, target);
            let sa = Snap::take_with(&ea.s, true);
            let mut eb = make(seed);
            eb.s.limit = eb.s.icount + n;
            let target = eb.s.icount + n;
            let out = fast_run(&mut eb.s, &region, &*kernel, &mut ctx);
            match &out {
                Ok(_) => ran_fast += 1,
                Err(d) => *declines.entry(format!("{d:?}")).or_default() += 1,
            }
            interp_to(&mut eb.s, target);
            let sb = Snap::take_with(&eb.s, true);
            let msgs = diff(&sa, &sb);
            total += 1;
            if !msgs.is_empty() {
                bad += 1;
                if bad <= 5 {
                    println!("FAIL seed {seed} budget {n} ({out:?})");
                    for m in msgs.iter().take(6) {
                        println!("      {m}");
                    }
                }
            }
        }
    }
    println!(
        "loops {pc:X}: {total} runs, {ran_fast} used the fast tier, {refused} seeds refused, {bad} differ"
    );
    println!("declines: {declines:?}");
    if bad > 0 {
        std::process::exit(1);
    }
    println!("PASS");
}

fn region(args: &[String]) {
    let image = std::fs::read(&args[0]).expect("image");
    let dir = &args[1];
    let pc = u32::from_str_radix(args[2].trim_start_matches("0x"), 16).expect("pc");
    let check_windows = args.iter().any(|a| a == "--check-windows");
    sharc_native::fast::glue::set_window_check(check_windows);
    let backend_name = flag(args, "--backend")
        .unwrap_or(if check_windows { "interp" } else { "cl" })
        .to_string();
    let injections: u64 = flag(args, "--inject").map_or(100, |x| x.parse().unwrap());
    // `--from HEX`: the captured state is at another entry of the region; run
    // the interpreter from it to the loop entry PC and use that state.
    let from = flag(args, "--from").map_or(pc, |x| {
        u32::from_str_radix(x.trim_start_matches("0x"), 16).expect("from")
    });
    let mut state = std::fs::read(format!("{dir}/r_{from:X}.state")).expect("state");
    let clock = meta_clock(&format!("{dir}/r_{from:X}.meta"));
    if from != pc {
        let mut e = open(&image, &state, clock);
        e.use_blocks = false;
        let mut n = 0;
        while e.s.pc_sw != pc as Int || e.s.loops.items().is_empty() {
            e.step(1);
            n += 1;
            assert!(n < 100_000, "no loop entry {pc:X} reached from {from:X}");
        }
        println!("advanced {n} steps from {from:X} to {pc:X}");
        state = e.export();
    }
    // `--relocate`: point every index register that lies outside the plain
    // normal-word range at a fresh window of plain RAM filled with random
    // finite floats, so a region captured on other memory still runs.
    if args.iter().any(|a| a == "--relocate") {
        let mut e = open(&image, &state, clock);
        let mut r = Rng(0x1234_5678_9abc);
        for (k, c) in (16..24usize).enumerate() {
            let v = e.s.r[c].b;
            if (0xe8000..0x400_0000 - 0x10000).contains(&(v as i64)) {
                continue;
            }
            let base = 0x30_0000 + 0x8000 * k as u32;
            e.s.r[c] = V::c((base + 0x4000) as Int);
            for w in 0..0x2000u32 {
                let bits = 0x3f00_0000 | (r.u32() & 0x7f_ffff) | (r.below(2) << 31);
                let a = e.s.mem.canonical_of(base + 4 * w);
                for b in 0..4 {
                    e.s.mem.write_byte(a + b, (bits >> (8 * b)) as u8);
                }
            }
            println!("relocated I{} {v:#x} -> {:#x}", c - 16, base + 0x4000);
        }
        state = e.export();
    }
    // `--dump DIR`: write the (advanced, relocated) entry state and a meta file
    // so that `region_bench` can time the region from it.
    if let Some(out) = flag(args, "--dump") {
        std::fs::create_dir_all(out).expect("dump dir");
        std::fs::write(format!("{out}/r_{pc:X}.state"), &state).expect("dump state");
        let meta = std::fs::read_to_string(format!("{dir}/r_{from:X}.meta")).expect("meta");
        let meta: String = meta
            .lines()
            .map(|l| {
                if l.starts_with("pc=") {
                    format!("pc={pc:X}\n")
                } else if l.starts_with("total_insns_in_window=") {
                    "total_insns_in_window=1000000\n".to_string()
                } else {
                    format!("{l}\n")
                }
            })
            .collect();
        std::fs::write(format!("{out}/r_{pc:X}.meta"), meta).expect("dump meta");
    }
    let want_aot = flag(args, "--no-aot").is_none();
    let make = |mode: Mode| -> (Engine, Option<Rc<RefCell<FastEngine>>>) {
        let mut e = open(&image, &state, clock);
        // Test-only entry-state adjustments, the same for every mode: make
        // unknown R/I/M registers concrete (FILL_UNKNOWN) and clear MODE1
        // bits (MODE1_CLEAR, hex), so a loop captured in an unusual state
        // still exercises the kernel.
        if std::env::var_os("FILL_UNKNOWN").is_some() {
            for c in 0..48usize {
                if !e.s.r[c].is_c() {
                    e.s.r[c] = V::c(0x3f80_0000 + 0x1234 * c as Int);
                }
            }
        }
        if let Some(list) = std::env::var_os("SET_REG") {
            // SET_REG=code:hexvalue,... (decimal register code).
            for item in list.to_str().unwrap().split(',') {
                let (c, v) = item.split_once(':').unwrap();
                e.s.r[c.parse::<usize>().unwrap()] =
                    V::c(u32::from_str_radix(v, 16).unwrap() as Int);
            }
        }
        if let Some(m) = std::env::var_os("MODE1_CLEAR") {
            let m = u32::from_str_radix(m.to_str().unwrap(), 16).unwrap();
            let v = e.s.r[114];
            e.s.r[114] = V::c((v.b & !m) as Int);
        }
        match mode {
            Mode::Interp => {
                e.use_blocks = false;
                (e, None)
            }
            Mode::Aot => (e, None),
            Mode::Fast | Mode::Both => {
                if mode == Mode::Fast {
                    e.dispatch = Dispatch::default();
                }
                let fe = Rc::new(RefCell::new(FastEngine::new(backend(&backend_name), &[pc])));
                e.set_fast(Some(Box::new(Shared(fe.clone()))));
                (e, Some(fe))
            }
        }
    };
    // Shape of the entry.
    let (probe, _) = make(Mode::Interp);
    let top = probe.s.loops.items().last().copied();
    assert_eq!(probe.s.pc_sw, pc as Int, "state is at the entry");
    let rem = top.map_or(1, |t| t.remaining as u32);
    drop(probe);
    let mut total = 0;
    let mut bad_total = 0;
    let mut cases: Vec<(String, u32, Option<u64>)> = Vec::new();
    let (mut kk, mut full_n);
    {
        let (mut e, fe) = make(Mode::Fast);
        e.step(1_000_000);
        let fe = fe.unwrap();
        let fe = fe.borrow();
        let Some(r) = fe.region(pc) else {
            println!("region {pc:X}: not built: {:?}", fe.refusals());
            return;
        };
        kk = r.len() as u32;
        if let Some(c) = &r.cfg {
            println!(
                "region {pc:X}: cfg shape, {} loops ({} active at entry), {} sites, {} stop(s): {:?}",
                c.loops.len(),
                c.n_entry,
                c.sites.len(),
                c.stops.len(),
                c.stops.iter().take(6).collect::<Vec<_>>()
            );
        }
        {
            // Which requirements fail at the captured entry state.
            let (entry, _) = make(Mode::Interp);
            let bad: Vec<_> = r
                .reqs
                .iter()
                .filter(|q| !sharc_native::fast::glue::eval_req(&entry.s, q, rem as i64))
                .collect();
            println!("entry requirements failing: {bad:?}");
            for q in &bad {
                if let sharc_native::fast::Req::Known(c) | sharc_native::fast::Req::Eq(c, _) = q {
                    let v = entry.s.r[*c as usize];
                    println!("  r[{c}] = {:08x}/{:08x}", v.b, v.m);
                }
            }
        }
        println!(
            "region {pc:X}: {} instructions ({:?}), kernel {} ops, {} windows, compile {} us [{}]",
            r.len(),
            r.forms,
            r.kernel.inst_count(),
            r.wins.len(),
            fe.stats.compile_ns / 1000,
            fe.backend_name()
        );
        full_n = kk * rem;
    }
    let _ = &mut kk;
    let _ = &mut full_n;
    cases.push(("full loop".into(), kk * rem, None));
    cases.push(("loop + 26".into(), kk * rem + 26, None));
    for n in [
        1,
        kk - 1,
        kk,
        kk + 1,
        2 * kk,
        3 * kk + 7,
        kk * rem / 2 + 3,
        kk * rem - 1,
        kk * rem + 1,
        777,
        5000,
        40000,
    ] {
        cases.push((format!("budget {n}"), n, None));
    }
    for j in 0..injections {
        cases.push((
            format!("inject #{j}"),
            kk * rem + 26,
            Some(0x9e37_79b9_7f4a_7c15u64.wrapping_mul(j + 1)),
        ));
    }
    let mut exit_hist: BTreeMap<String, u64> = BTreeMap::new();
    for (name, n, inj) in &cases {
        let mut what = String::new();
        let mut run = |mode: Mode| {
            let (mut e, fe) = make(mode);
            if let Some(seed) = inj {
                what.clear();
                inject(&mut e, *seed, &mut what);
            }
            let ran = e.step(*n);
            let sn = Snap::take_with(&e.s, true);
            let st = fe.map(|f| f.borrow().stats.clone());
            (sn, ran, st, e.halt.clone())
        };
        let (sa, ra, _, ha) = run(Mode::Interp);
        let (sb, rb, stb, hb) = run(Mode::Fast);
        let mut msgs = diff(&sa, &sb);
        if ra != rb || ha != hb {
            msgs.push(format!("ran interp {ra} fast {rb}; halt {ha:?} {hb:?}"));
        }
        if want_aot {
            // The fast tier in front of the generated blocks, as in a build.
            let (sd, rd, _, hd) = run(Mode::Both);
            for m in diff_with(&sa, &sd, false) {
                msgs.push(format!("fast+AOT: {m}"));
            }
            if ra != rd || ha != hd {
                msgs.push(format!("fast+AOT ran {rd} vs interp {ra}; halt {hd:?}"));
            }
        }
        let stb = stb.unwrap();
        let fast_insns = stb.insns;
        let how = if stb.runs == 0 {
            "declined".to_string()
        } else {
            format!("{} insns in {} runs", fast_insns, stb.runs)
        };
        *exit_hist.entry(how.clone()).or_default() += 1;
        let mut aot_note = String::new();
        if want_aot && inj.is_none() {
            let (sc, rc, _, _) = run(Mode::Aot);
            let d = diff_with(&sa, &sc, false);
            if !d.is_empty() || ra != rc {
                aot_note = format!(" [AOT differs from interpreter: {} fields]", d.len());
                for m in d.iter().take(3) {
                    aot_note.push_str(&format!("\n      AOT: {m}"));
                }
            }
        }
        total += 1;
        if msgs.is_empty() {
            if inj.is_none() || stb.runs == 0 {
                println!("ok   {name:<18} n={n:<5} fast: {how}{what}{aot_note}");
            }
        } else {
            bad_total += 1;
            println!("FAIL {name:<18} n={n:<5} fast: {how}{what}{aot_note}");
            for m in msgs.iter().take(8) {
                println!("      {m}");
            }
        }
    }
    println!("--- fast-tier use over {total} cases ---");
    for (k, v) in &exit_hist {
        println!("{v:>5} x {k}");
    }
    if bad_total > 0 {
        println!("FAIL: {bad_total} of {total} cases differ");
        std::process::exit(1);
    }
    println!("PASS: {total} cases");
}

#[allow(dead_code)]
fn _unused(_: usize) -> usize {
    NUREG
}

// -- random CFG programs ---------------------------------------------------------------
//
//     fast_diff cfg IMAGE STATE [--cases N] [--runs R] [--seed S] [--backend ..]
//                               [--stack-model 0|1] [--shape cfg|auto] [--case K]
//
// Random structured programs (conditional instructions, forward and backward
// branches, delayed branches, DO loops with literal and register counts,
// nested) are written to memory; the fast tier is given random instruction
// starts as entry points, so it takes over in the middle of a program, inside
// loops and with the loops the interpreter has pushed. Each program runs
// from several random states for several budgets against the interpreter
// alone, comparing the whole state.

const R_COUNT: u32 = 15; // DO counts
const R_TRIPS: u32 = 13; // back-loop counter
const R_MINUS1: u32 = 14; // constant -1

fn words_of(raw: u64, n: usize) -> Vec<u16> {
    [(raw >> 32) as u16, (raw >> 16) as u16, raw as u16][..n].to_vec()
}

fn compute_field(cu: u32, opcode: u32, rn: u32, rx: u32, ry: u32) -> u32 {
    (cu << 20) | (opcode << 12) | (rn << 8) | (rx << 4) | ry
}

fn frame(form: &str, r: &mut Rng) -> Raw {
    let (mask, base) = form_base(form);
    Raw((r.next() & 0xffff_ffff_ffff & !mask) | base)
}

fn i_17a(ureg: u32, value: u32) -> Vec<u16> {
    let mut raw = Raw(form_base("17a").1);
    raw.set(38, 32, ureg as u64);
    raw.set(31, 0, value as u64);
    words_of(raw.0, 3)
}

fn i_2a(cond: u32, compute: u32) -> Vec<u16> {
    let mut raw = Raw(form_base("2a").1);
    raw.set(37, 33, cond as u64);
    raw.set(22, 0, compute as u64);
    words_of(raw.0, 3)
}

/// JUMP (cond) with a relative offset in short words from the jump.
fn i_jump(cond: u32, delayed: bool, rel: i32) -> Vec<u16> {
    let mut raw = Raw(0x07 << 40);
    raw.set(37, 33, cond as u64);
    raw.set(26, 26, delayed as u64);
    raw.set(23, 0, (rel as u32 & 0xff_ffff) as u64);
    words_of(raw.0, 3)
}

fn i_do(reg: Option<u32>, count: u32, rel: i32) -> Vec<u16> {
    let raw = match reg {
        None => {
            let mut raw = Raw(0x0c << 40);
            raw.set(39, 32, (count >> 8) as u64);
            raw.set(31, 24, (count & 0xff) as u64);
            raw.set(22, 0, (rel as u32 & 0x7f_ffff) as u64);
            raw
        }
        Some(code) => {
            let mut raw = Raw(0x0d << 40);
            raw.set(38, 32, code as u64);
            raw.set(22, 0, (rel as u32 & 0x7f_ffff) as u64);
            raw
        }
    };
    words_of(raw.0, 3)
}

/// A real condition (never "always"), for branches.
fn real_cond(r: &mut Rng) -> u32 {
    // The last two codes the fast tier refuses: rarely.
    let n = COND_CODES.len() as u32;
    if r.chance(60) {
        COND_CODES[(n - 1 - r.below(2)) as usize]
    } else {
        COND_CODES[r.below(n - 2) as usize]
    }
}

fn pick_cond(r: &mut Rng, allow: bool) -> u32 {
    if allow && r.chance(2) {
        COND_CODES[r.below(COND_CODES.len() as u32) as usize]
    } else {
        0x1f
    }
}

fn calm_value(r: &mut Rng) -> u32 {
    match r.below(6) {
        0 => rand_int(r),
        1 => r.below(16),
        2 => (-(r.below(8) as i32)) as u32,
        _ => f32::to_bits((r.below(2000) as f32 - 1000.0) * 0.125),
    }
}

/// A random multifunction or dual add/subtract compute field (R0-R12
/// destinations): their flag writers must be right when a condition reads
/// them.
fn extra_compute(r: &mut Rng) -> u32 {
    let (rm, ra) = (r.below(13), r.below(13));
    match r.below(5) {
        0..=2 => {
            let cat = [0x18, 0x19, 0x30 | r.below(13)][r.below(3) as usize];
            let (a, b, c, d) = (r.below(4), r.below(4), r.below(4), r.below(4));
            let f = (1 << 22) | (cat << 16) | (rm << 12) | (ra << 8);
            f | (a << 6) | (b << 4) | (c << 2) | d
        }
        n => {
            let top = if n == 3 { 0x7 } else { 0xf };
            let rs = r.below(13);
            (((top << 4) | rs) << 12) | (rm << 8) | (r.below(16) << 4) | r.below(16)
        }
    }
}

/// A random immediate-offset memory form or immediate modify (`units_mem`),
/// confined to I0-I5 and R0-R12.
fn mem_imm_insn(r: &mut Rng) -> Vec<u16> {
    let units = units_mem();
    let u = &units[r.below(units.len() as u32) as usize];
    let (mut raw, _) = (u.make)(r);
    let mut r64 = Raw(raw);
    match u.form {
        "4a" | "4b" | "4d" => {
            r64.set(43, 41, r.below(6) as u64);
            r64.set(26, 23, r.below(13) as u64);
        }
        "15b" => {
            r64.set(43, 41, r.below(6) as u64);
            r64.set(29, 23, r.below(13) as u64);
        }
        "15a" => {
            r64.set(43, 41, r.below(6) as u64);
            r64.set(38, 32, r.below(13) as u64);
        }
        "14a" => r64.set(38, 32, r.below(13) as u64),
        _ => {}
    }
    raw = r64.0;
    let n = match u.form {
        "4b" | "15b" | "7b" => 2,
        _ => 3,
    };
    words_of(raw, n)
}

/// One random instruction (R0-R12 as destinations).
fn gen_insn(r: &mut Rng, allow_cond: bool) -> Vec<u16> {
    if r.chance(10) {
        return mem_imm_insn(r);
    }
    let cond = pick_cond(r, allow_cond);
    let rn = r.below(13);
    match r.below(13) {
        12 => {
            // Ia = MODIFY(Ib, Mc): a register step (plain, (sw) or (nw)).
            let mut raw = frame("7a", r);
            raw.set(39, 39, (r.below(3) == 2) as u64);
            raw.set(23, 23, (r.below(3) == 1) as u64);
            raw.set(38, 38, 0);
            raw.set(37, 33, 0x1f);
            raw.set(22, 0, 0);
            // Keep both indices in I0-I5.
            let src = r.below(6);
            raw.set(32, 32, (src >> 2) as u64);
            raw.set(31, 30, (src & 3) as u64);
            raw.set(29, 27, r.below(8) as u64);
            let dst = r.below(6);
            raw.set(26, 24, (src ^ dst) as u64);
            words_of(raw.0, 3)
        }
        0..=3 => {
            if r.chance(4) {
                return i_2a(cond, extra_compute(r));
            }
            let (_, cu, opc) = FULL_OPS[r.below(FULL_OPS.len() as u32) as usize];
            i_2a(cond, compute_field(cu, opc, rn, r.below(16), r.below(16)))
        }
        4 => {
            let (_, opc) = SHORT_OPS[r.below(SHORT_OPS.len() as u32) as usize];
            let mut raw = frame("2c", r);
            raw.set(43, 32, ((opc << 8) | (rn << 4) | r.below(16)) as u64);
            words_of(raw.0, 1)
        }
        5 => {
            let (_, cu, opc) = FULL_OPS[r.below(FULL_OPS.len() as u32) as usize];
            let mut raw = frame("2a_short", r);
            raw.set(
                38,
                16,
                compute_field(cu, opc, rn, r.below(16), r.below(16)) as u64,
            );
            words_of(raw.0, 2)
        }
        6 | 7 => {
            let (_, opc) = SHIFT_OPS[r.below(SHIFT_OPS.len() as u32) as usize];
            let mut raw = frame("6b_shiftimm", r);
            let data8 = r.below(256);
            raw.set(
                22,
                0,
                ((opc << 16) | (data8 << 8) | (rn << 4) | r.below(16)) as u64,
            );
            raw.set(37, 33, cond as u64);
            raw.set(30, 27, r.below(16) as u64);
            words_of(raw.0, 3)
        }
        8 => i_17a(rn, calm_value(r)),
        9 | 10 => {
            // R = DM(I, M) / DM(I, M) = R with M = 0 (the address is the
            // index register's value); post-modify, pre-modify, either way.
            let mut raw = frame("3a", r);
            raw.set(44, 44, r.below(2) as u64);
            raw.set(43, 41, r.below(6) as u64);
            raw.set(40, 38, r.below(8) as u64);
            raw.set(37, 33, cond as u64);
            raw.set(32, 32, 0);
            raw.set(31, 31, r.below(2) as u64);
            raw.set(30, 30, 0);
            raw.set(29, 23, rn as u64);
            let compute = if r.chance(3) {
                let (_, cu, opc) = FULL_OPS[r.below(FULL_OPS.len() as u32) as usize];
                compute_field(cu, opc, r.below(13), r.below(16), r.below(16))
            } else {
                0
            };
            raw.set(22, 0, compute as u64);
            words_of(raw.0, 3)
        }
        _ => {
            let mut raw = frame("5a_move", r);
            let src = r.below(16);
            raw.set(42, 38, (src >> 2) as u64);
            raw.set(37, 33, cond as u64);
            raw.set(32, 32, ((src >> 1) & 1) as u64);
            raw.set(31, 31, (src & 1) as u64);
            raw.set(29, 23, rn as u64);
            raw.set(22, 0, 0);
            words_of(raw.0, 3)
        }
    }
}

#[derive(Clone)]
enum Item {
    Insn(Vec<u16>),
    /// JUMP IF NOT cond over the body.
    IfSkip {
        cond: u32,
        body: Vec<Item>,
    },
    /// The same with a delayed jump and two slots.
    DelayIf {
        cond: u32,
        slots: [Vec<u16>; 2],
        body: Vec<Item>,
    },
    Do {
        by_reg: bool,
        count: u32,
        body: Vec<Item>,
    },
    /// A counted loop closed by a backward JUMP IF NE.
    Back {
        trips: u32,
        delayed: bool,
        body: Vec<Item>,
    },
    Exit {
        cond: u32,
    },
}

fn gen_items(r: &mut Rng, depth: u32, in_back: bool, n: u32) -> Vec<Item> {
    let mut v = Vec::new();
    for _ in 0..n {
        let roll = if depth >= 3 { r.below(8) } else { r.below(18) };
        v.push(match roll {
            0..=7 => Item::Insn(gen_insn(r, true)),
            8 | 9 => {
                let cond = real_cond(r);
                let n = 1 + r.below(4);
                Item::IfSkip {
                    cond,
                    body: gen_items(r, depth + 1, in_back, n),
                }
            }
            10 | 11 => {
                let cond = real_cond(r);
                let slots = [gen_insn(r, true), gen_insn(r, true)];
                let n = 1 + r.below(3);
                Item::DelayIf {
                    cond,
                    slots,
                    body: gen_items(r, depth + 1, in_back, n),
                }
            }
            12..=14 => {
                let n = 1 + r.below(4);
                let mut body = gen_items(r, depth + 1, in_back, n);
                body.push(Item::Insn(gen_insn(r, true)));
                Item::Do {
                    by_reg: r.chance(3),
                    count: 1 + r.below(4),
                    body,
                }
            }
            15 if !in_back => {
                let trips = 1 + r.below(4);
                let delayed = r.chance(3);
                let n = 1 + r.below(4);
                Item::Back {
                    trips,
                    delayed,
                    body: gen_items(r, depth + 1, true, n),
                }
            }
            16 => Item::Exit { cond: real_cond(r) },
            _ => Item::Insn(gen_insn(r, true)),
        });
    }
    v
}

struct Asm {
    words: Vec<u16>,
    starts: Vec<u32>,
}

impl Asm {
    fn insn(&mut self, w: Vec<u16>) {
        self.starts.push(self.words.len() as u32);
        self.words.extend(w);
    }

    fn patch(&mut self, at: usize, w: Vec<u16>) {
        self.words[at..at + w.len()].copy_from_slice(&w);
    }

    fn items(&mut self, items: &[Item]) {
        for it in items {
            match it {
                Item::Insn(w) => self.insn(w.clone()),
                Item::IfSkip { cond, body } => {
                    let j = self.words.len();
                    self.insn(i_jump(cond ^ 0x10, false, 0));
                    self.items(body);
                    let rel = (self.words.len() - j) as i32;
                    self.patch(j, i_jump(cond ^ 0x10, false, rel));
                }
                Item::DelayIf { cond, slots, body } => {
                    let j = self.words.len();
                    self.insn(i_jump(cond ^ 0x10, true, 0));
                    self.insn(slots[0].clone());
                    self.insn(slots[1].clone());
                    self.items(body);
                    let rel = (self.words.len() - j) as i32;
                    self.patch(j, i_jump(cond ^ 0x10, true, rel));
                }
                Item::Do {
                    by_reg,
                    count,
                    body,
                } => {
                    if *by_reg {
                        self.insn(i_17a(R_COUNT, *count));
                    }
                    let d = self.words.len();
                    self.insn(i_do(by_reg.then_some(R_COUNT), *count, 0));
                    self.items(body);
                    let end = *self.starts.last().unwrap() as usize;
                    let rel = (end - d) as i32;
                    self.patch(d, i_do(by_reg.then_some(R_COUNT), *count, rel));
                }
                Item::Back {
                    trips,
                    delayed,
                    body,
                } => {
                    self.insn(i_17a(R_TRIPS, *trips));
                    let head = self.words.len();
                    self.items(body);
                    self.insn(i_2a(
                        0x1f,
                        compute_field(0, 0x01, R_TRIPS, R_TRIPS, R_MINUS1),
                    ));
                    let j = self.words.len();
                    let rel = head as i32 - j as i32;
                    self.insn(i_jump(0x10, *delayed, rel));
                    if *delayed {
                        // Slots that leave the counter and the flags alone.
                        self.insn(i_17a(0, 1));
                        self.insn(i_17a(1, 2));
                    }
                }
                Item::Exit { cond } => {
                    self.insn(i_jump(*cond, false, 0x2000));
                }
            }
        }
    }
}

fn write_words(s: &mut St, words: &[u16]) {
    for (k, w) in words.iter().enumerate() {
        let a = 0x2800_0000 + (CODE_PC + k as u32) * 2;
        s.mem.write_byte(a, *w as u8);
        s.mem.write_byte(a + 1, (*w >> 8) as u8);
    }
}

fn prep_engine(e: &mut Engine, stack_model: Option<bool>) {
    e.enable_runtime_decode(Mem::read_sw);
    if let Some(v) = stack_model {
        assert_eq!(e.set_option(20, v as i64), 0);
    }
    let s = &mut e.s;
    s.loops.n = 0;
    s.loop_depth = 0;
    s.call_stack.n = 0;
    s.pc_stack.n = 0;
    s.pending = None;
    s.cfg.core_timer = false;
    s.bank_pending_mask = -1;
    s.bank_requested_mask = -1;
    s.pc_stack_pending = -1;
    s.pc_stack_requested = -1;
    s.r[114] = V::c(0x3900_1cf8);
    s.pc_sw = CODE_PC as Int;
    for k in 0..mem_len() {
        let a = s.mem.canonical_of(mem_lo() + k);
        s.mem.write_byte(a, 0xa5);
    }
}

fn cfgprog(args: &[String]) {
    let image = std::fs::read(&args[0]).expect("image");
    let state = std::fs::read(&args[1]).expect("state");
    let cases: u64 = flag(args, "--cases").map_or(100, |x| x.parse().unwrap());
    let runs: u64 = flag(args, "--runs").map_or(6, |x| x.parse().unwrap());
    let seed: u64 = flag(args, "--seed").map_or(0xc0ffee, |x| x.parse().unwrap());
    let only: Option<u64> = flag(args, "--case").map(|x| x.parse().unwrap());
    // The state's own setting (the DN2 audio state has the stack model on)
    // unless --stack-model 0|1 says otherwise.
    let stack_model: Option<bool> = flag(args, "--stack-model").map(|x| x != "0");
    let force_cfg = flag(args, "--shape") == Some("cfg");
    let dump = args.iter().any(|a| a == "--dump");
    // --stride: nonzero modifier registers, so address registers step.
    let stride = args.iter().any(|a| a == "--stride");
    // --simple: a few instructions, then a DO loop with a straight body
    // (the loop shape), entered at every instruction.
    let simple = args.iter().any(|a| a == "--simple");
    KNOWN_FLAGS.store(true, std::sync::atomic::Ordering::Relaxed);
    CALM.store(true, std::sync::atomic::Ordering::Relaxed);
    // --check-windows: the reference interpreter asserts that every window
    // access lies inside its window (and is the backend unless one is named).
    let check_windows = args.iter().any(|a| a == "--check-windows");
    sharc_native::fast::glue::set_window_check(check_windows);
    let backend_name = flag(args, "--backend")
        .unwrap_or(if check_windows { "interp" } else { "cl" })
        .to_string();
    let mut bad_total = 0u64;
    let mut tier_runs = 0u64;
    let mut tier_insns = 0u64;
    let mut runs_total = 0u64;
    let mut built = 0u64;
    let mut declined = [0u64; 5];
    let mut refusals: BTreeMap<String, u64> = BTreeMap::new();
    let t0 = std::time::Instant::now();
    for case in 0..cases {
        if only.is_some_and(|k| k != case) {
            continue;
        }
        let mut rng = Rng(seed ^ (case + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15));
        for _ in 0..4 {
            rng.next();
        }
        let n_items = 3 + rng.below(8);
        let items = if simple {
            let mut v: Vec<Item> = (0..rng.below(3))
                .map(|_| Item::Insn(gen_insn(&mut rng, true)))
                .collect();
            let n = 3 + rng.below(8);
            let body: Vec<Item> = (0..n)
                .map(|_| Item::Insn(gen_insn(&mut rng, true)))
                .collect();
            v.push(Item::Do {
                by_reg: rng.chance(3),
                count: 2 + rng.below(4),
                body,
            });
            v
        } else {
            gen_items(&mut rng, 0, false, n_items)
        };
        let mut asm = Asm {
            words: Vec::new(),
            starts: Vec::new(),
        };
        asm.insn(i_17a(R_MINUS1, 0xffff_ffff));
        asm.items(&items);
        asm.insn(i_jump(0x1f, false, 0));
        // Debugging aid: CFG_NOP=hexpc,.. replaces three-word instructions
        // with `R15 = 0`.
        if let Ok(list) = std::env::var("CFG_NOP") {
            for pcs in list.split(',') {
                let off = u32::from_str_radix(pcs, 16).unwrap() - CODE_PC;
                let end = asm
                    .starts
                    .iter()
                    .find(|&&x| x > off)
                    .copied()
                    .unwrap_or(asm.words.len() as u32);
                assert_eq!(
                    end - off,
                    3,
                    "CFG_NOP: {pcs} is not a three-word instruction"
                );
                let w = i_17a(15, 0);
                asm.words[off as usize..end as usize].copy_from_slice(&w);
            }
        }
        let mut entries: Vec<u32> = if simple {
            asm.starts.iter().skip(1).map(|st| CODE_PC + st).collect()
        } else {
            (0..1 + rng.below(3))
                .map(|_| CODE_PC + asm.starts[rng.below(asm.starts.len() as u32) as usize])
                .collect()
        };
        if rng.chance(2) {
            entries.push(CODE_PC);
        }
        entries.sort_unstable();
        entries.dedup();
        if dump {
            println!(
                "case {case}: {} words, entries {:x?}",
                asm.words.len(),
                entries
            );
            for st in &asm.starts {
                print!("{:06x}:", CODE_PC + st);
                let n = asm
                    .starts
                    .iter()
                    .find(|&&x| x > *st)
                    .copied()
                    .unwrap_or(asm.words.len() as u32)
                    - st;
                for k in 0..n {
                    print!(" {:04x}", asm.words[(st + k) as usize]);
                }
                println!();
            }
        }
        let mut ei = open(&image, &state, CLOCK_BASE);
        let mut ef = open(&image, &state, CLOCK_BASE);
        if case == 0 {
            let c = &ei.s.cfg;
            println!(
                "engine config from the state: stack_model {} bank_model {} core_timer {} peripheral {}; testing stack_model {stack_model:?}",
                c.stack_model, c.bank_model, c.core_timer, c.peripheral_model
            );
        }
        prep_engine(&mut ei, stack_model);
        prep_engine(&mut ef, stack_model);
        write_words(&mut ei.s, &asm.words);
        write_words(&mut ef.s, &asm.words);
        ei.use_blocks = false;
        ef.dispatch = Dispatch::default();
        let fe = Rc::new(RefCell::new(FastEngine::new(
            backend(&backend_name),
            &entries,
        )));
        fe.borrow_mut().force_cfg(force_cfg);
        ef.set_fast(Some(Box::new(Shared(fe.clone()))));
        let s0i = Snap::take(&ei.s);
        let s0f = Snap::take(&ef.s);
        let mut case_bad = 0;
        let mut ms_case = [0u32; 16];
        if stride {
            for m in ms_case.iter_mut() {
                *m = (rng.below(6) as i32 - 2) as u32;
            }
        }
        // Both engines from the random state STATE_SEED for N instructions.
        let pair = |ei: &mut Engine, ef: &mut Engine, st_seed: u64, mode1: u32, n: u32| {
            for (e, s0) in [(&mut *ei, &s0i), (&mut *ef, &s0f)] {
                s0.restore(&mut e.s);
                randomise(&mut e.s, &mut Rng(st_seed), &ms_case);
                e.s.r[114] = V::c(mode1 as Int);
                e.s.pc_sw = CODE_PC as Int;
                e.halt = None;
            }
            let ran_i = ei.step(n);
            let ran_f = ef.step(n);
            let sa = Snap::take_with(&ei.s, true);
            let sb = Snap::take_with(&ef.s, true);
            let mut msgs = diff(&sa, &sb);
            if ran_i != ran_f || ei.halt != ef.halt {
                msgs.push(format!(
                    "ran interp {ran_i} fast {ran_f}; halt {:?} {:?}",
                    ei.halt, ef.halt
                ));
            }
            (msgs, sa.pc_sw)
        };
        for run in 0..runs {
            let st_seed = rng.next();
            let n = if rng.chance(3) {
                3000 + rng.below(3000)
            } else {
                let cap = if rng.chance(2) { 40 } else { 400 };
                1 + rng.below(cap)
            };
            let mode1 =
                [0x3900_1cf8u32, 0x3901_1cf8, 0x3900_3cf8, 0x3901_3cf8][rng.below(4) as usize];
            let before = fe.borrow().stats.clone();
            let (msgs, _) = pair(&mut ei, &mut ef, st_seed, mode1, n as u32);
            let after = fe.borrow().stats.clone();
            runs_total += 1;
            tier_runs += after.runs - before.runs;
            tier_insns += after.insns - before.insns;
            if !msgs.is_empty() {
                case_bad += 1;
                bad_total += 1;
                println!(
                    "FAIL case {case} run {run} (state seed {st_seed:#x}, n {n}, mode1 {mode1:#x}, entries {entries:x?}, tier runs {})",
                    after.runs - before.runs
                );
                for m in msgs.iter().take(6) {
                    println!("      {m}");
                }
                // The smallest budget at which they differ: the instruction
                // retired last is where they part.
                let (mut lo, mut hi) = (0u32, n as u32);
                while hi - lo > 1 {
                    let mid = (lo + hi) / 2;
                    if pair(&mut ei, &mut ef, st_seed, mode1, mid).0.is_empty() {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
                let (m_hi, _) = pair(&mut ei, &mut ef, st_seed, mode1, hi);
                let (_, pc_lo) = pair(&mut ei, &mut ef, st_seed, mode1, lo);
                println!(
                    "      at budget {lo}: pc interp {:#x} fast {:#x}, icount {} {}",
                    ei.s.pc_sw, ef.s.pc_sw, ei.s.icount, ef.s.icount
                );
                let _ = pair(&mut ei, &mut ef, st_seed, mode1, hi);
                println!(
                    "      at budget {hi}: pc interp {:#x} fast {:#x}, icount {} {}, fast pending {:?} loops {:?}",
                    ei.s.pc_sw,
                    ef.s.pc_sw,
                    ei.s.icount,
                    ef.s.icount,
                    ef.s.pending,
                    ef.s.loops.items()
                );
                println!(
                    "      first difference at budget {hi}: instruction at {pc_lo:#x} (state before it matched)"
                );
                for m in m_hi.iter().take(8) {
                    println!("        {m}");
                }
            }
        }
        let fb = fe.borrow();
        built += fb.stats.built;
        for (i, d) in fb.stats.declined.iter().enumerate() {
            declined[i] += d;
        }
        for (_, why) in fb.refusals() {
            if why.contains("neither") || why.contains("Verifier") || why.contains("invalid") {
                println!("case {case}: refused: {why}");
            }
            let key: String = why
                .chars()
                .take(if why.contains("Verifier") { 900 } else { 70 })
                .collect();
            *refusals.entry(key).or_default() += 1;
        }
        if case_bad > 0 {
            println!(
                "  case {case}: {case_bad} of {runs} runs differ (rerun with --case {case} --dump)"
            );
        }
    }
    println!(
        "--- {} runs in {:.1}s: tier ran {} times, {} instructions, {} regions built ---",
        runs_total,
        t0.elapsed().as_secs_f64(),
        tier_runs,
        tier_insns,
        built
    );
    println!("declined shape/req/window/budget/exit0 = {declined:?}");
    for (why, n) in &refusals {
        println!("{n:>5} x refused: {why}");
    }
    if bad_total > 0 {
        println!("FAIL: {bad_total} runs differ");
        std::process::exit(1);
    }
    println!("PASS");
}

// -- capturing region entry states ---------------------------------------------------
//
//     fast_diff capture IMAGE STATE FRAMES OUTDIR PC[,PC...] [--min-frame 4620]
//
// Replays the DN2 note frames with the interpreter alone and writes the engine
// state at the first arrival at each PC after MIN-FRAME (OUTDIR/r_<PC>.state
// and .meta, as `region_bench capture` does, but for any PC, not only the
// entries of generated blocks).

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

fn capture(args: &[String]) {
    const GAP: u32 = 667_000;
    let image = std::fs::read(&args[0]).expect("image");
    let state = std::fs::read(&args[1]).expect("state");
    let frames = read_frames(&args[2]);
    let outdir = &args[3];
    let pcs: Vec<u32> = args[4]
        .split(',')
        .filter(|x| !x.is_empty())
        .map(|x| u32::from_str_radix(x.trim_start_matches("0x"), 16).expect("hex pc"))
        .collect();
    let min_frame: usize = flag(args, "--min-frame").map_or(4620, |x| x.parse().unwrap());
    let start: usize = flag(args, "--start").map_or(4600, |x| x.parse().unwrap());
    let end: usize = flag(args, "--end").map_or(min_frame + 80, |x| x.parse().unwrap());
    std::fs::create_dir_all(outdir).expect("outdir");
    for pc in pcs {
        let mut e = open(&image, &state, CLOCK_BASE);
        e.use_blocks = false;
        let mut found = false;
        'frames: for (i, frame) in frames.iter().enumerate().take(end + 1).skip(start) {
            e.spi2_exchange(frame).expect("spi2 exchange");
            let mut done = 0u32;
            while done < GAP {
                let ran = e.step_until_in(GAP - done, pc, pc + 1);
                done += ran;
                if i >= min_frame && e.s.pc_sw == pc as Int {
                    e.export_ranges = true;
                    let blob = e.export();
                    e.export_ranges = false;
                    let clock = CLOCK_BASE + e.s.icount;
                    std::fs::write(format!("{outdir}/r_{pc:X}.state"), blob).expect("write");
                    std::fs::write(
                        format!("{outdir}/r_{pc:X}.meta"),
                        format!("pc={pc:X}\nframe={i}\nclock={clock}\n"),
                    )
                    .expect("write");
                    println!("r_{pc:X}: captured in frame {i}");
                    found = true;
                    break 'frames;
                }
                if e.halt.is_some() {
                    println!("halt at frame {i}: {:?}", e.halt);
                    break 'frames;
                }
                if ran == 0 || e.s.pc_sw == pc as Int {
                    // At the PC before the capture window: move on.
                    done += e.step(1);
                }
            }
            let _ = e.sport_block(None).expect("sport block");
        }
        if !found {
            println!("r_{pc:X}: not reached in frames {min_frame}..{end}");
        }
    }
}
