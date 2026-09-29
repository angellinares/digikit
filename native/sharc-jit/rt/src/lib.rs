//! The SHARC+ JIT's runtime, built for wasm32: one machine per instance.
//!
//! It holds the machine state (`native/sharc`'s `St`), runs the
//! one-instruction interpreter over the transpiled core for code that has
//! no translation, dispatches to translated regions (functions in this
//! module's table, installed by the host), counts where interpreted runs
//! start and asks the host (`host.jit`) to translate a PC once it is hot.
//! Region code calls the `jit_*` helpers below for what stays in `St`
//! (special registers, stacks, memory, the undo log).
//!
//! Instructions are decoded from the image at run time (`sharc-decode`);
//! nothing here depends on a particular firmware.

use sharc_decode::{Decoder, SegmentImage, ShortWords};
use sharc_native::frames::{self, Pack};
use sharc_native::rt::*;
use sharc_native::{Engine, trap_name};
use std::collections::HashMap;

#[link(wasm_import_module = "host")]
unsafe extern "C" {
    /// Translate the region at PC; its table slot, or -1.
    fn jit(pc: u32) -> i32;
    fn now_ns() -> f64;
}

pub const EXIT_NEXT: i32 = 0;
pub const EXIT_BUDGET: i32 = 1;
pub const EXIT_TRAP: i32 = 2;
pub const EXIT_BAIL: i32 = 3;

/// Decoded instructions, built on first use (Insn records the core reads).
struct Insns {
    img: SegmentImage,
    dec: Decoder,
    syms: HashMap<&'static str, Sym>,
    fresh: HashMap<String, Sym>,
    /// pc -> 0 not yet decoded, u32::MAX nothing there, else index + 1.
    pages: Vec<Option<Box<[u32; 4096]>>>,
    table: Vec<&'static Insn>,
}

impl Insns {
    fn sym(&mut self, name: &str) -> Sym {
        if let Some(&s) = self.syms.get(name) {
            return s;
        }
        let n = self.fresh.len() as u16;
        *self.fresh.entry(name.to_string()).or_insert(60000u16.wrapping_add(n))
    }

    fn get(&mut self, pc: Int) -> Option<&'static Insn> {
        if !(0..(1 << 24)).contains(&pc) {
            return None;
        }
        let pc = pc as u32;
        let page = self.pages[(pc >> 12) as usize].get_or_insert_with(|| Box::new([0; 4096]));
        let slot = page[(pc & 0xFFF) as usize];
        if slot == u32::MAX {
            return None;
        }
        if slot != 0 {
            return Some(self.table[slot as usize - 1]);
        }
        let d = self.dec.decode_at(&self.img, pc);
        let r = if d.length_bytes.is_none() {
            None
        } else {
            let mut kv = Vec::new();
            for (key, value) in &d.fields {
                let (stem, range) = match key.find('[') {
                    Some(i) => (&key[..i], &key[i..]),
                    None => (key.as_str(), ""),
                };
                let (mut hi, mut lo) = (-1i8, -1i8);
                if let Some(inner) = range.strip_prefix('[').and_then(|x| x.strip_suffix(']'))
                    && let Some((h, l)) = inner.split_once(':')
                {
                    hi = h.parse().unwrap_or(-1);
                    lo = l.parse().unwrap_or(-1);
                }
                let k = self.sym(key);
                let s = self.sym(stem);
                kv.push(FieldEntry(k, s, hi, lo, *value as Int));
            }
            let fields: &'static Fields = Box::leak(Box::new(Fields {
                kv: Box::leak(kv.into_boxed_slice()),
            }));
            let tn = self.sym(&d.type_name);
            let kind = self.sym(d.kind);
            Some(&*Box::leak(Box::new(Insn {
                type_name: tn,
                fields,
                length_bytes: d.length_bytes.map(|n| n as Int),
                kind,
                offset: 0,
            })))
        };
        let page = self.pages[(pc >> 12) as usize].as_mut().unwrap();
        match r {
            None => page[(pc & 0xFFF) as usize] = u32::MAX,
            Some(i) => {
                self.table.push(i);
                page[(pc & 0xFFF) as usize] = self.table.len() as u32;
            }
        }
        r
    }
}

/// Per-machine JIT state.
struct Jit {
    /// pc -> table slot + 1.
    slots: Vec<Option<Box<[u32; 4096]>>>,
    /// Interpreted runs started at a PC.
    hot: HashMap<u32, u32>,
    /// PCs the host could not translate (not asked again).
    refused: HashMap<u32, ()>,
    threshold: u32,
    enabled: bool,
    /// One bit per short-word PC the interpreter executed.
    seen: Vec<u8>,
    last_interp_next: Int,
    regions_called: u64,
    requests: u64,
    /// Profiling: entry pc -> (calls, instructions); (entry, exit code,
    /// pc after) -> count.
    profile: bool,
    prof_entries: HashMap<u32, (u64, u64)>,
    prof_exits: HashMap<(u32, i32, u32), u64>,
    /// Replay every region call through the interpreter and compare.
    verify: bool,
    /// Verify only regions entered in [lo, hi) (all when empty).
    verify_range: (i64, i64),
    /// Verify every Nth region call.
    verify_every: u64,
    report: String,
}

impl Jit {
    fn slot(&self, pc: Int) -> Option<u32> {
        if !(0..(1 << 24)).contains(&pc) {
            return None;
        }
        let pc = pc as u32;
        let s = self.slots[(pc >> 12) as usize].as_ref()?[(pc & 0xFFF) as usize];
        (s != 0).then(|| s - 1)
    }
    fn install(&mut self, pc: u32, slot: u32) {
        if pc >= (1 << 24) {
            return;
        }
        let page = self.slots[(pc >> 12) as usize].get_or_insert_with(|| Box::new([0; 4096]));
        page[(pc & 0xFFF) as usize] = slot + 1;
    }
}

struct Rt {
    e: Engine,
    jit: Jit,
    pack: Option<Pack<'static>>,
    last_ns: f64,
    err: String,
}

static mut INSNS: *mut Insns = std::ptr::null_mut();
static mut RT: *mut Rt = std::ptr::null_mut();
#[repr(align(16))]
struct Scratch(#[allow(dead_code)] [u8; 64]);
static mut SCRATCH: Scratch = Scratch([0; 64]);

fn insn_at(pc: Int) -> Option<&'static Insn> {
    // SAFETY: single-threaded wasm; INSNS is set by jit_create before any
    // instruction runs and never freed.
    unsafe { INSNS.as_mut()?.get(pc) }
}

fn rt() -> &'static mut Rt {
    // SAFETY: single-threaded; set by jit_create.
    unsafe { RT.as_mut().expect("jit_create first") }
}

fn scratch() -> *mut u8 {
    (&raw mut SCRATCH).cast::<u8>()
}

fn copy_out(text: &[u8], out: *mut u8, cap: usize) -> i32 {
    if text.len() > cap {
        return -(text.len() as i32);
    }
    // SAFETY: the caller passes CAP writable bytes at OUT.
    unsafe { std::ptr::copy_nonoverlapping(text.as_ptr(), out, text.len()) };
    text.len() as i32
}

fn bytes<'a>(p: *const u8, n: usize) -> &'a [u8] {
    if n == 0 {
        return &[];
    }
    // SAFETY: the host passes N readable bytes at P.
    unsafe { std::slice::from_raw_parts(p, n) }
}

// ----------------------------------------------------------------- memory

#[unsafe(no_mangle)]
pub extern "C" fn jit_alloc(len: usize) -> *mut u8 {
    let mut v = vec![0u8; len.max(1)];
    let p = v.as_mut_ptr();
    std::mem::forget(v);
    p
}

/// # Safety
/// P/LEN from one jit_alloc(LEN).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jit_free(p: *mut u8, len: usize) {
    // SAFETY: allocated by jit_alloc as a Vec<u8> of that length.
    drop(unsafe { Vec::from_raw_parts(p, len.max(1), len.max(1)) });
}

// ----------------------------------------------------------------- layout

/// St offsets for region code (sharc-translate mach::Layout order).
///
/// # Safety
/// OUT points to 20 writable u32s.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jit_layout(out: *mut u32) {
    use std::mem::{offset_of, size_of};
    let w: [u32; 20] = [
        offset_of!(St, r) as u32,
        offset_of!(V, b) as u32,
        offset_of!(V, m) as u32,
        size_of::<V>() as u32,
        offset_of!(St, pc_sw) as u32,
        offset_of!(St, icount) as u32,
        offset_of!(St, limit) as u32,
        offset_of!(St, un) as u32,
        (offset_of!(St, loops) + offset_of!(Stk<Loop, MAX_LOOPS>, n)) as u32,
        (offset_of!(St, loops) + offset_of!(Stk<Loop, MAX_LOOPS>, a)) as u32,
        size_of::<Loop>() as u32,
        offset_of!(Loop, start_sw) as u32,
        offset_of!(Loop, end_sw) as u32,
        offset_of!(Loop, remaining) as u32,
        offset_of!(Loop, mode) as u32,
        (offset_of!(St, call_stack) + offset_of!(Stk<Int, MAX_CALLS>, n)) as u32,
        (offset_of!(St, call_stack) + offset_of!(Stk<Int, MAX_CALLS>, a)) as u32,
        size_of::<Int>() as u32,
        (offset_of!(St, status_stack) + offset_of!(Stk<(V, V, V), MAX_STATUS>, n)) as u32,
        scratch() as u32,
    ];
    // SAFETY: OUT has 20 u32s.
    unsafe { std::ptr::copy_nonoverlapping(w.as_ptr(), out, 20) };
}

// ---------------------------------------------------------------- machine

/// Create the machine over IMAGE (tools/sharc_transpile_run.py pack_image).
/// 0, or <0 on a malformed image.
///
/// # Safety
/// IMAGE points to LEN readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jit_create(image: *const u8, len: usize) -> i32 {
    let b = bytes(image, len);
    let Ok(img) = sharc_decode::parse_pack_image(b) else {
        return -10;
    };
    let mut syms = HashMap::new();
    for (k, &n) in sharc_native::generated::syms::SYM_NAMES.iter().enumerate() {
        syms.entry(n).or_insert(k as Sym);
    }
    let mut pages = Vec::new();
    pages.resize_with(1 << 12, || None);
    let insns = Box::new(Insns {
        img,
        dec: Decoder::new(),
        syms,
        fresh: HashMap::new(),
        pages,
        table: Vec::new(),
    });
    let mut e = match Engine::from_image(b) {
        Ok(e) => e,
        Err(c) => return c,
    };
    e.s.insn_at = insn_at;
    let mut slots = Vec::new();
    slots.resize_with(1 << 12, || None);
    let r = Box::new(Rt {
        e,
        jit: Jit {
            slots,
            hot: HashMap::new(),
            refused: HashMap::new(),
            threshold: 2,
            enabled: true,
            seen: vec![0; 1 << 21],
            last_interp_next: -1,
            regions_called: 0,
            requests: 0,
            profile: false,
            prof_entries: HashMap::new(),
            prof_exits: HashMap::new(),
            verify: false,
            verify_range: (0, i64::MAX),
            verify_every: 1,
            report: String::new(),
        },
        pack: None,
        last_ns: 0.0,
        err: String::new(),
    });
    // SAFETY: single-threaded; the previous machine (if any) is dropped.
    unsafe {
        if !RT.is_null() {
            drop(Box::from_raw(RT));
        }
        if INSNS.is_null() {
            INSNS = Box::into_raw(insns);
        } else {
            drop(Box::from_raw(INSNS));
            INSNS = Box::into_raw(insns);
        }
        RT = Box::into_raw(r);
    }
    0
}

/// The image's DO loop ends (sorted), which region code assumes the loop
/// stack holds.
///
/// # Safety
/// P points to N i64s.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jit_set_loop_ends(p: *const i64, n: usize) {
    // SAFETY: caller contract.
    let v: Vec<i64> = unsafe { std::slice::from_raw_parts(p, n) }.to_vec();
    let r = rt();
    r.e.s.loop_ends = Box::leak(v.into_boxed_slice());
    r.e.s.check_loops();
}

#[unsafe(no_mangle)]
pub extern "C" fn jit_st() -> *mut St {
    &mut *rt().e.s
}

#[unsafe(no_mangle)]
pub extern "C" fn jit_set_verify(on: i32, lo: i64, hi: i64, every: u32) {
    let j = &mut rt().jit;
    j.verify = on != 0;
    j.verify_every = every.max(1) as u64;
    j.verify_range = (lo, if hi <= lo { i64::MAX } else { hi });
}

/// # Safety
/// OUT points to CAP writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jit_verify_report(out: *mut u8, cap: usize) -> i32 {
    let r = rt().jit.report.clone();
    let n = r.len().min(cap);
    copy_out(&r.as_bytes()[..n], out, cap)
}

/// The kind symbol of the instruction at PC, or -1 (none there).
#[unsafe(no_mangle)]
pub extern "C" fn jit_insn_kind(_s: *mut St, pc: i64) -> i32 {
    match insn_at(pc as Int) {
        Some(i) => i.kind as i32,
        None => -1,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn jit_set_profile(on: i32) {
    let j = &mut rt().jit;
    j.profile = on != 0;
    j.prof_entries.clear();
    j.prof_exits.clear();
}

/// The profile as text: regions by entries, then the commonest exits.
///
/// # Safety
/// OUT points to CAP writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jit_profile(out: *mut u8, cap: usize) -> i32 {
    let j = &rt().jit;
    let mut v: Vec<(&u32, &(u64, u64))> = j.prof_entries.iter().collect();
    v.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.0.cmp(b.0)));
    let total: u64 = v.iter().map(|x| x.1.0).sum();
    let mut s = format!("region entries {total}\n");
    for (pc, (n, i)) in v.iter().take(40) {
        s.push_str(&format!("  {pc:#x}: {n} entries, {:.1} instructions each\n", *i as f64 / *n as f64));
    }
    let mut x: Vec<(&(u32, i32, u32), &u64)> = j.prof_exits.iter().collect();
    x.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    s.push_str("exits (entry, code, pc after)\n");
    for ((e, c, p), n) in x.iter().take(40) {
        s.push_str(&format!("  {e:#x} -> {c} at {p:#x}: {n}\n"));
    }
    let n = s.len().min(cap);
    copy_out(&s.as_bytes()[..n], out, cap)
}

#[unsafe(no_mangle)]
pub extern "C" fn jit_pending_none() -> i32 {
    rt().e.s.pending.is_none() as i32
}

#[unsafe(no_mangle)]
pub extern "C" fn jit_pc() -> i64 {
    rt().e.s.pc_sw as i64
}

#[unsafe(no_mangle)]
pub extern "C" fn jit_icount() -> i64 {
    rt().e.s.icount as i64
}

#[unsafe(no_mangle)]
pub extern "C" fn jit_seen() -> *const u8 {
    rt().jit.seen.as_ptr()
}

#[unsafe(no_mangle)]
pub extern "C" fn jit_install(pc: u32, slot: u32) {
    rt().jit.install(pc, slot)
}

#[unsafe(no_mangle)]
pub extern "C" fn jit_configure(threshold: u32, enabled: i32) {
    let j = &mut rt().jit;
    j.threshold = threshold.max(1);
    j.enabled = enabled != 0;
}

/// Is the image's instruction at PC one the core can run (the decoded
/// length), for the host's region formation? 0 when not.
#[unsafe(no_mangle)]
pub extern "C" fn jit_decodes(pc: u32) -> i32 {
    insn_at(pc as Int).is_some() as i32
}

fn before_n(_e: &Engine, _b: &[u8]) -> u64 {
    0
}

/// After a region call from BEFORE: replay the same instructions through
/// the interpreter from BEFORE and compare. False (and a report, and a
/// halt) on a difference; the machine continues from the interpreter's
/// state either way.
fn verify_region(e: &mut Engine, j: &mut Jit, before: &[u8], pc0: Int, _n0: u64) -> bool {
    let n = e.s.icount - e.s.limit.min(e.s.icount).min(e.s.icount);
    let _ = n;
    let jit_state = sharc_native::canon::export_state(&e.s, false);
    let jit_r = e.s.r;
    let jit_pc = e.s.pc_sw;
    let jit_ic = e.s.icount;
    let (cfg, sp, ic) = (e.s.cfg.clone(), e.s.special_present, e.s.icount);
    let _ = ic;
    // Back to BEFORE (import resets cfg and special presence: keep ours).
    let _ = e.import(before);
    e.s.cfg = cfg;
    e.s.special_present = sp;
    e.s.icount = VERIFY_IC0.with(|c| c.get());
    let trace = TRACE.with(|t| std::mem::take(&mut *t.borrow_mut()));
    let entry_r = e.s.r;
    let mut k = 0usize;
    let mut first_bad: Option<String> = None;
    while e.s.icount < jit_ic {
        let Some(insn) = (e.s.insn_at)(e.s.pc_sw) else { break };
        let ipc = e.s.pc_sw;
        if sharc_native::exec_insn(&mut e.s, insn).is_err() {
            break;
        }
        if first_bad.is_none()
            && let Some((tpc, regs)) = trace.get(k)
        {
            if *tpc as Int != ipc {
                first_bad = Some(format!("  trace: instruction {k} at {ipc:#x} but the trace has {tpc:#x}\n"));
            }
            let mut want = entry_r;
            for &(c, b, m) in regs {
                want[c as usize] = V { b: b & m, m };
            }
            for c in 0..NUREG {
                let v = e.s.r[c];
                if first_bad.is_none() && v != want[c] {
                    first_bad = Some(format!(
                        "  trace: after instruction {k} at {ipc:#x}: r[{c}] jit {:#x}/{:#x}{} interp {:#x}/{:#x}\n",
                        want[c].b,
                        want[c].m,
                        if regs.iter().any(|r| r.0 as usize == c) { "" } else { " (untouched)" },
                        v.b,
                        v.m
                    ));
                }
            }
        }
        k += 1;
    }
    let int_state = sharc_native::canon::export_state(&e.s, false);
    if int_state == jit_state {
        return true;
    }
    let mut rep = format!(
        "region at {pc0:#x}: {} instructions; jit pc {jit_pc:#x} interp pc {:#x} (icount {} vs {})\n",
        jit_ic - VERIFY_IC0.with(|c| c.get()),
        e.s.pc_sw,
        jit_ic,
        e.s.icount
    );
    for c in 0..NUREG {
        if jit_r[c] != e.s.r[c] {
            rep.push_str(&format!("  r[{c}]: jit {:#x}/{:#x} interp {:#x}/{:#x}\n", jit_r[c].b, jit_r[c].m, e.s.r[c].b, e.s.r[c].m));
        }
    }
    if let Some(b) = first_bad {
        rep.push_str(&b);
    }
    if jit_state.len() != int_state.len() {
        rep.push_str(&format!("  state sizes {} vs {}\n", jit_state.len(), int_state.len()));
    } else if let Some(k) = (0..jit_state.len()).find(|&k| jit_state[k] != int_state[k]) {
        rep.push_str(&format!("  first differing byte of the state blob: {k}\n"));
    }
    j.report.push_str(&rep);
    e.halt = Some(format!("jit verify: region at {pc0:#x} differs"));
    false
}

thread_local! {
    static VERIFY_IC0: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// Per instruction of the last region call: (pc, registers it held).
    static TRACE: std::cell::RefCell<Vec<(i64, Vec<(u32, u32, u32)>)>> = const { std::cell::RefCell::new(Vec::new()) };
    static TRACE_CUR: std::cell::RefCell<Vec<(u32, u32, u32)>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[unsafe(no_mangle)]
pub extern "C" fn jit_trace(_s: *mut St, c: i32, b: i32, m: i32) {
    TRACE_CUR.with(|t| t.borrow_mut().push((c as u32, b as u32, m as u32)));
}

#[unsafe(no_mangle)]
pub extern "C" fn jit_trace_end(_s: *mut St, pc: i64) {
    let regs = TRACE_CUR.with(|t| std::mem::take(&mut *t.borrow_mut()));
    TRACE.with(|t| t.borrow_mut().push((pc, regs)));
}

fn trapped(e: &mut Engine, t: Trap) {
    e.stats.traps += 1;
    e.last_trap = Some(t);
    e.halt = Some(format!("native-trap: {} at {:#x}", trap_name(t), e.s.pc_sw));
}

/// Run up to N instructions: translated regions where there are, the
/// interpreter elsewhere. Returns how many completed; fewer means a trap
/// (see halt).
fn step(e: &mut Engine, j: &mut Jit, n: u32) -> u32 {
    if e.halt.is_some() {
        return 0;
    }
    let start = e.s.icount;
    let limit = start + n as u64;
    while e.s.icount < limit {
        let pc = e.s.pc_sw;
        if j.enabled && e.s.cfg.block_ok && e.s.loops_ok && e.s.pending.is_none() {
            if let Some(slot) = j.slot(pc) {
                e.s.limit = limit;
                let before = e.s.icount;
                // SAFETY: SLOT is a table index the host installed a region
                // function of type (i32) -> i32 at; St is live.
                let f: extern "C" fn(*mut St) -> i32 = unsafe { std::mem::transmute(slot as usize) };
                let in_range = (j.verify_range.0..j.verify_range.1).contains(&(e.s.icount as i64))
                    && j.regions_called % j.verify_every == 0;
                let snap = (j.verify && in_range).then(|| {
                    VERIFY_IC0.with(|c| c.set(e.s.icount));
                    TRACE.with(|t| t.borrow_mut().clear());
                    sharc_native::canon::export_state(&e.s, true)
                });
                let code = f(&mut *e.s);
                if let Some(snap) = snap
                    && !verify_region(e, j, &snap, pc, before_n(e, &snap))
                {
                    break;
                }
                j.regions_called += 1;
                if j.profile {
                    let p = pc as u32;
                    let x = j.prof_entries.entry(p).or_default();
                    x.0 += 1;
                    x.1 += e.s.icount - before;
                    *j.prof_exits.entry((p, code, e.s.pc_sw as u32)).or_default() += 1;
                }
                e.stats.block_entries += 1;
                e.stats.block_instructions += e.s.icount - before;
                j.last_interp_next = -1;
                // Anything but a clean exit: the interpreter runs the
                // instruction at pc next.
                match code {
                    EXIT_NEXT => continue,
                    EXIT_TRAP => e.stats.block_traps += 1,
                    _ => {}
                }
                if e.s.icount >= limit {
                    break;
                }
            } else if pc != j.last_interp_next && (0..(1 << 24)).contains(&pc) {
                // A jump target reached in the interpreter (or where it
                // resumed after a region): count it toward translation.
                let p = pc as u32;
                let c = j.hot.entry(p).or_insert(0);
                *c += 1;
                if *c >= j.threshold && !j.refused.contains_key(&p) {
                    j.requests += 1;
                    // SAFETY: a host import; it only reads this instance's
                    // memory and installs a table entry.
                    let slot = unsafe { jit(p) };
                    if slot >= 0 {
                        j.install(p, slot as u32);
                        continue;
                    }
                    j.refused.insert(p, ());
                }
            }
        }
        let Some(insn) = (e.s.insn_at)(e.s.pc_sw) else {
            trapped(e, TRAP_NO_INSN);
            break;
        };
        e.stats.single_steps += 1;
        let p = e.s.pc_sw;
        if (0..(1 << 24)).contains(&p) {
            j.seen[(p >> 3) as usize] |= 1 << (p & 7);
        }
        if let Err(t) = sharc_native::exec_insn(&mut e.s, insn) {
            trapped(e, t);
            break;
        }
        // The fall-through PC: a next PC other than it is a jump target.
        j.last_interp_next = p + insn.length_bytes.unwrap_or(0) / 2;
    }
    (e.s.icount - start) as u32
}

#[unsafe(no_mangle)]
pub extern "C" fn jit_step(n: u32) -> i32 {
    let r = rt();
    step(&mut r.e, &mut r.jit, n) as i32
}

/// Run the region at SLOT once with a budget of N instructions (tests):
/// its exit code.
#[unsafe(no_mangle)]
pub extern "C" fn jit_run_slot(slot: u32, n: u32) -> i32 {
    let e = &mut rt().e;
    e.s.limit = e.s.icount + n as u64;
    // SAFETY: as in step().
    let f: extern "C" fn(*mut St) -> i32 = unsafe { std::mem::transmute(slot as usize) };
    f(&mut *e.s)
}

// ----------------------------------------------------- the C ABI, wrapped

/// # Safety
/// BLOB points to LEN bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jit_import_state(blob: *const u8, len: usize) -> i32 {
    let r = rt();
    match r.e.import(bytes(blob, len)) {
        Ok(()) => {
            r.jit.last_interp_next = -1;
            0
        }
        Err(c) => c,
    }
}

/// # Safety
/// OUT points to CAP writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jit_export_state(out: *mut u8, cap: usize) -> i32 {
    let e = &rt().e;
    let v = sharc_native::canon::export_state(&e.s, e.export_ranges);
    copy_out(&v, out, cap)
}

/// # Safety
/// OUT points to CAP writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jit_halt_reason(out: *mut u8, cap: usize) -> i32 {
    let h = rt().e.halt.clone().unwrap_or_default();
    let n = h.len().min(cap);
    copy_out(&h.as_bytes()[..n], out, cap)
}

#[unsafe(no_mangle)]
pub extern "C" fn jit_set_option(key: u32, value: i64) -> i32 {
    rt().e.set_option(key, value)
}

/// # Safety
/// NAME/MODE point to their lengths.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jit_set_provisional(name: *const u8, nlen: usize, mode: *const u8, mlen: usize) -> i32 {
    let e = &mut rt().e;
    let (Ok(n), Ok(m)) = (std::str::from_utf8(bytes(name, nlen)), std::str::from_utf8(bytes(mode, mlen))) else {
        return -1;
    };
    let (Some(n), Some(m)) = (sharc_native::sym_of(n), sharc_native::sym_of(m)) else {
        return -2;
    };
    e.s.cfg.provisional_interp.retain(|(k, _)| *k != n);
    e.s.cfg.provisional_interp.push((n, m));
    e.s.cfg.refresh();
    0
}

/// # Safety
/// BLOB points to LEN bytes (canon::parse_insn).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jit_exec_insn(blob: *const u8, len: usize) -> i32 {
    let e = &mut rt().e;
    let insn = match sharc_native::canon::parse_insn(bytes(blob, len)) {
        Ok(i) => i,
        Err(c) => return c,
    };
    match sharc_native::exec_insn(&mut e.s, insn) {
        Ok(()) => 1,
        Err(t) => {
            trapped(e, t);
            0
        }
    }
}

/// [instructions, region entries, region instructions, single steps,
/// traps, regions, special presence bits, region traps, requests].
///
/// # Safety
/// OUT points to CAP u64s.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jit_stats(out: *mut u64, cap: usize) -> i32 {
    let r = rt();
    let e = &r.e;
    let v = [
        e.s.icount,
        e.stats.block_entries,
        e.stats.block_instructions,
        e.stats.single_steps,
        e.stats.traps,
        r.jit.requests,
        (0..7).map(|i| (e.s.special_present[i] as u64) << i).sum(),
        e.stats.block_traps,
        r.jit.regions_called,
    ];
    let n = v.len().min(cap);
    // SAFETY: OUT has CAP >= n u64s.
    unsafe { std::ptr::copy_nonoverlapping(v.as_ptr(), out, n) };
    n as i32
}

#[unsafe(no_mangle)]
pub extern "C" fn jit_set_reg(code: u32, kind: u32, value: u32, mask: u32) -> i32 {
    if code as usize >= NUREG {
        return -1;
    }
    let v = match kind {
        1 => V::c(value as Int),
        2 => V::partial(mask as Int, value as Int),
        _ => V::UNK,
    };
    rt().e.set_reg(code as usize, v);
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn jit_get_reg(code: u32) -> u64 {
    let v = rt().e.s.r[code as usize % NUREG];
    ((v.m as u64) << 32) | v.b as u64
}

/// # Safety
/// DATA points to LEN bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jit_poke(address: u64, data: *const u8, len: usize, width: u32) -> i32 {
    rt().e.poke(address, bytes(data, len), width)
}

#[unsafe(no_mangle)]
pub extern "C" fn jit_peek(address: u64, width: u32) -> i64 {
    match rt().e.peek(address, width) {
        Ok(Some(v)) => (1i64 << 32) | v as i64,
        Ok(None) => 0,
        Err(_) => -1,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn jit_fresh_call(pc: u32, return_address: i64) -> i32 {
    let r = rt();
    r.e.fresh_call(pc, (return_address >= 0).then_some(return_address as Int));
    r.jit.last_interp_next = -1;
    0
}

/// # Safety
/// OUT points to CAP writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jit_info(out: *mut u8, cap: usize) -> i32 {
    copy_out(sharc_native::build_info().as_bytes(), out, cap)
}

// ------------------------------------------------------------ frame packs

fn clock() -> u64 {
    // SAFETY: a host function without memory access.
    unsafe { now_ns() as u64 }
}

/// Open a frame pack (the buffer is kept); the frame count or -1.
///
/// # Safety
/// P/LEN from jit_alloc(LEN).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jit_pack_open(p: *mut u8, len: usize) -> i32 {
    // SAFETY: allocated by jit_alloc(len).
    let b: &'static [u8] = unsafe { Vec::from_raw_parts(p, len, len) }.leak();
    let r = rt();
    match frames::parse(b) {
        Ok(pack) => {
            let n = pack.frames.len() as i32;
            if let Err(w) = frames::reload(&mut r.e, &pack) {
                r.err = w;
                return -1;
            }
            r.jit.last_interp_next = -1;
            r.pack = Some(pack);
            n
        }
        Err(w) => {
            r.err = w;
            -1
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn jit_pack_first() -> u32 {
    rt().pack.as_ref().map(|p| p.first).unwrap_or(0)
}

#[unsafe(no_mangle)]
pub extern "C" fn jit_pack_reload() -> i32 {
    let r = rt();
    let Some(pack) = r.pack.as_ref() else { return -1 };
    match frames::reload(&mut r.e, pack) {
        Ok(()) => {
            r.jit.last_interp_next = -1;
            0
        }
        Err(w) => {
            r.err = w;
            -1
        }
    }
}

/// Run frame K; its instructions, or -1 (jit_error).
#[unsafe(no_mangle)]
pub extern "C" fn jit_pack_frame(k: u32) -> f64 {
    let r = rt();
    let Some(pack) = r.pack.take() else { return -1.0 };
    let Some(&data) = pack.frames.get(k as usize) else {
        r.pack = Some(pack);
        return -1.0;
    };
    let j = &mut r.jit;
    let mut stepper = |e: &mut Engine, n: u32| -> u32 { step(e, j, n) };
    let res = frames::frame_with(&mut r.e, &pack, data, &clock, &mut stepper);
    r.pack = Some(pack);
    match res {
        Ok(o) => {
            r.last_ns = o.handler_time as f64;
            o.instructions as f64
        }
        Err(w) => {
            r.err = w;
            -1.0
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn jit_last_ns() -> f64 {
    rt().last_ns
}

/// # Safety
/// OUT points to 32 writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jit_hash(out: *mut u8) {
    let h = frames::state_hash(&rt().e);
    // SAFETY: OUT has 32 bytes.
    unsafe { std::ptr::copy_nonoverlapping(h.as_ptr(), out, 32) };
}

/// # Safety
/// OUT points to CAP writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jit_error(out: *mut u8, cap: usize) -> i32 {
    let e = rt().err.clone();
    let n = e.len().min(cap);
    copy_out(&e.as_bytes()[..n], out, cap)
}

// ---------------------------------------------- helpers region code calls

fn vi(akind: i32, a: i64, am: i32) -> VI {
    if akind == 0 {
        let m = am as u32;
        VI::V(V { b: a as u32 & m, m })
    } else {
        VI::I(a as Int)
    }
}

fn put_v(off: usize, v: V) {
    let s = scratch();
    // SAFETY: the scratch area has 64 bytes.
    unsafe {
        (s.add(off) as *mut u32).write_unaligned(v.b);
        (s.add(off + 4) as *mut u32).write_unaligned(v.m);
    }
}

fn get_v(off: usize) -> V {
    let s = scratch();
    // SAFETY: the scratch area has 64 bytes.
    unsafe {
        let b = (s.add(off) as *const u32).read_unaligned();
        let m = (s.add(off + 4) as *const u32).read_unaligned();
        V { b: b & m, m }
    }
}

fn put_i64(off: usize, v: i64) {
    // SAFETY: the scratch area has 64 bytes.
    unsafe { (scratch().add(off) as *mut i64).write_unaligned(v) }
}

fn put_i128(off: usize, v: Int) {
    // SAFETY: the scratch area has 64 bytes.
    unsafe { (scratch().add(off) as *mut i128).write_unaligned(v) }
}

fn st<'a>(p: *mut St) -> &'a mut St {
    // SAFETY: region code passes the St pointer it was called with.
    unsafe { &mut *p }
}

#[unsafe(no_mangle)]
pub extern "C" fn jit_sv_get(s: *mut St, view: i32, key: i32) -> i32 {
    match sv_get(st(s), SpecView(view as u8), key as Sym) {
        None => 0,
        Some(Spec::V(v)) => {
            put_v(0, v);
            1
        }
        Some(Spec::M(m)) => {
            put_i128(0, m.mask);
            put_i128(16, m.bits);
            2
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn jit_set_special(s: *mut St, key: i32, kind: i32) -> i32 {
    let v = if kind == 1 {
        Spec::V(get_v(0))
    } else {
        // SAFETY: the scratch area has 64 bytes.
        let (mask, bits) = unsafe {
            (
                (scratch() as *const i128).read_unaligned(),
                (scratch().add(16) as *const i128).read_unaligned(),
            )
        };
        Spec::M(MR::new(mask, bits))
    };
    match s_set_special(st(s), key as Sym, v) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn jit_dm_read(s: *mut St, akind: i32, a: i64, am: i32, width: i32, signed: i32) -> i64 {
    match bnd::_dm_read(st(s), vi(akind, a, am), width as Int, signed != 0) {
        Ok(None) => 0,
        Ok(Some(v)) if v.is_c() => (1i64 << 32) | v.b as i64,
        // A value not fully known: the interpreter runs the instruction.
        Ok(Some(_)) => -2,
        Err(_) => -1,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn jit_dm_write(s: *mut St, akind: i32, a: i64, am: i32, width: i32, b: i32, m: i32) -> i32 {
    let m = m as u32;
    let v = V { b: b as u32 & m, m };
    match bnd::_dm_write(st(s), vi(akind, a, am), width as Int, v) {
        Ok(true) => 1,
        Ok(false) => 0,
        Err(_) => -1,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn jit_read_px48(s: *mut St, akind: i32, a: i64, am: i32) -> i32 {
    match bnd::_read_px48(st(s), vi(akind, a, am)) {
        Ok(None) => 0,
        Ok(Some((x, y))) => {
            put_v(0, x);
            put_v(8, y);
            1
        }
        Err(_) => -1,
    }
}

fn lp(start: i64, end: i64, rem: i64, mode: i64) -> Loop {
    Loop {
        start_sw: start,
        end_sw: end,
        remaining: rem,
        mode,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn jit_push_loop(s: *mut St, start: i64, end: i64, rem: i64, mode: i64) -> i32 {
    stk_push_loops(st(s), lp(start, end, rem, mode)).map_or(-1, |_| 0)
}

#[unsafe(no_mangle)]
pub extern "C" fn jit_pop_loop(s: *mut St) -> i32 {
    match stk_pop_loops(st(s)) {
        Ok(l) => {
            put_i64(0, l.start_sw);
            put_i64(8, l.end_sw);
            put_i64(16, l.remaining);
            put_i64(24, l.mode);
            0
        }
        Err(_) => -1,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn jit_set_loop(s: *mut St, i: i64, start: i64, end: i64, rem: i64, mode: i64) -> i32 {
    stk_set_loops(st(s), i as Int, lp(start, end, rem, mode)).map_or(-1, |_| 0)
}

#[unsafe(no_mangle)]
pub extern "C" fn jit_push_call(s: *mut St, v: i64) -> i32 {
    stk_push_call_stack(st(s), v as Int).map_or(-1, |_| 0)
}

#[unsafe(no_mangle)]
pub extern "C" fn jit_pop_call(s: *mut St) -> i32 {
    match stk_pop_call_stack(st(s)) {
        Ok(v) => {
            put_i64(0, v as i64);
            0
        }
        Err(_) => -1,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn jit_set_call(s: *mut St, i: i64, v: i64) -> i32 {
    stk_set_call_stack(st(s), i as Int, v as Int).map_or(-1, |_| 0)
}

#[unsafe(no_mangle)]
pub extern "C" fn jit_push_status(s: *mut St) -> i32 {
    let t = (get_v(0), get_v(8), get_v(16));
    stk_push_status_stack(st(s), t).map_or(-1, |_| 0)
}

#[unsafe(no_mangle)]
pub extern "C" fn jit_pop_status(s: *mut St) -> i32 {
    match stk_pop_status_stack(st(s)) {
        Ok((a, b, c)) => {
            put_v(0, a);
            put_v(8, b);
            put_v(16, c);
            0
        }
        Err(_) => -1,
    }
}

#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn jit_set_pending(
    s: *mut St,
    present: i32,
    tpresent: i32,
    target: i64,
    call: i32,
    slots: i64,
    rfc: i32,
    rpresent: i32,
    rsw: i64,
) {
    st(s).pending = (present != 0).then(|| Pending {
        target: (tpresent != 0).then_some(target as Int),
        call: call != 0,
        slots: slots as Int,
        return_from_call: rfc != 0,
        return_sw: (rpresent != 0).then_some(rsw as Int),
    });
}

fn pair(lo: i64, hi: i64) -> i128 {
    ((hi as i128) << 64) | (lo as u64 as i128)
}

#[unsafe(no_mangle)]
pub extern "C" fn jit_mul128(alo: i64, ahi: i64, blo: i64, bhi: i64) {
    put_i128(0, pair(alo, ahi).wrapping_mul(pair(blo, bhi)));
}

#[unsafe(no_mangle)]
pub extern "C" fn jit_shl128(lo: i64, hi: i64, n: i32) {
    put_i128(0, pair(lo, hi).wrapping_shl(n as u32 & 127));
}

#[unsafe(no_mangle)]
pub extern "C" fn jit_shr128(lo: i64, hi: i64, n: i32, signed: i32) {
    let v = pair(lo, hi);
    let r = if signed != 0 { v >> (n as u32 & 127) } else { ((v as u128) >> (n as u32 & 127)) as i128 };
    put_i128(0, r);
}

#[unsafe(no_mangle)]
pub extern "C" fn jit_rollback_log(s: *mut St) {
    st(s).rollback_log();
}

// Keep ShortWords in scope for the decoder's trait object.
#[allow(dead_code)]
fn _uses(_: &dyn ShortWords) {}
