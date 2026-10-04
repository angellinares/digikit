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
    for u in &mut v {
        u.check = Check::None;
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
        _ => r.u32(),
    }
}

fn randomise(s: &mut St, r: &mut Rng, ms: &[u32; 16]) {
    for c in 0..16 {
        let v = if r.chance(2) {
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
    // ASTATX: random value with a random known mask.
    let m = if r.chance(3) { u32::MAX } else { r.u32() };
    s.r[118] = V { b: r.u32() & m, m };
    // Data window contents: floats and ints.
    for k in 0..DATA_LEN / 4 {
        let w = if r.chance(2) {
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
            let (raw, check) = (unit.make)(&mut rng);
            let mut ms = [0u32; 16];
            for m in ms.iter_mut() {
                *m = (rng.below(9) as i32 - 4) as u32;
            }
            let s = &mut e.s;
            randomise(s, &mut rng, &ms);
            let nwords = match unit.form {
                "2c" | "3c" => 1,
                "2a_short" | "3b" | "5b_move" => 2,
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
                let special = expect_special(check, s);
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
                        Req::Known(c) => Some(c) == unknown,
                        Req::Eq(c, _) => Some(c) == unknown || Some(c) == flip,
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
            total_mismatch += 1;
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
        _ => {
            eprintln!("usage: fast_diff forms|region ...");
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
        match r.below(3) {
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

fn region(args: &[String]) {
    let image = std::fs::read(&args[0]).expect("image");
    let dir = &args[1];
    let pc = u32::from_str_radix(args[2].trim_start_matches("0x"), 16).expect("pc");
    let backend_name = flag(args, "--backend").unwrap_or("cl").to_string();
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
    let top = *probe.s.loops.items().last().expect("loop at entry");
    assert_eq!(probe.s.pc_sw, pc as Int, "state is at the entry");
    let rem = top.remaining as u32;
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
        let r = fe.region(pc).expect("region built");
        kk = r.len() as u32;
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
