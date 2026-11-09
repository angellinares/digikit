//! `SHARC_FAST_DUMP=DIR`: write the fast tier's real kernels and a few real
//! calls of each (context, register words, windows, before and after) so a
//! backend in another process (WebAssembly in V8) can be checked and timed on
//! the exact work the engine does.
//!
//! Files in DIR:
//!  * `kernel-<kid>.bin`: `Kernel::encode()` words (little endian u32);
//!  * `call-<kid>-<n>.bin`: one captured call (layout in `Capture::write`);
//!    (windows: slot, DM start, length, host address lo/hi words, then the bytes before and after);
//!  * `calls.tsv`: `kid entry_pc insns code` per captured call;
//!  * `totals.tsv`: `entry_pc calls insns` per region at exit.
//!
//! Capture is limited to `PER_KERNEL` calls of at least `MIN_INSNS`
//! instructions per kernel. Compiled in only with the `fast-dump` cargo
//! feature (without it every hook is a constant false); with the feature, off
//! unless `SHARC_FAST_DUMP` is set (default `$DIGI_DIAG_DIR/fastdump`).

use super::ir::*;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

const PER_KERNEL: u32 = 2;
/// Calls of one kernel looked at before giving up on finding big ones.
const ATTEMPTS: u32 = 96;
const MIN_INSNS: u64 = 48;

fn dir() -> Option<&'static std::path::PathBuf> {
    static D: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();
    D.get_or_init(|| {
        let p = std::path::PathBuf::from(crate::diag::out_path("SHARC_FAST_DUMP", "fastdump")?);
        std::fs::create_dir_all(&p).ok()?;
        Some(p)
    })
    .as_ref()
}

static ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Reads `SHARC_FAST_DUMP` once (FastEngine::new calls it); `enabled` is then
/// one relaxed load, inlined into the glue's call path.
pub fn init() {
    if !cfg!(feature = "fast-dump") {
        if std::env::var_os("SHARC_FAST_DUMP").is_some() {
            eprintln!("SHARC_FAST_DUMP ignored: build with the fast-dump feature");
        }
        return;
    }
    ON.store(dir().is_some(), std::sync::atomic::Ordering::Relaxed);
}

/// Constant false without the `fast-dump` feature, so every hook below
/// compiles away and the call path pays nothing.
#[inline(always)]
pub fn enabled() -> bool {
    cfg!(feature = "fast-dump") && ON.load(std::sync::atomic::Ordering::Relaxed)
}

struct Win {
    w: u32,
    ptr: *const u8,
    lo: u32,
    len: u32,
}

pub struct Capture {
    kid: u64,
    key: usize,
    ctx_pre: Vec<u8>,
    st_off: u32,
    st_pre: Vec<u8>,
    /// (slot, DM start, bytes before, host address, bytes after)
    wins: Vec<(u32, u32, Vec<u8>, *const u8, Vec<u8>)>,
    ctx_post: Vec<u8>,
    st_post: Vec<u8>,
    code: u32,
}

thread_local! {
    static WINS: RefCell<Vec<Win>> = const { RefCell::new(Vec::new()) };
    static PENDING: RefCell<Option<Capture>> = const { RefCell::new(None) };
    /// Kernel address -> (kid, attempts, captured).
    static SEEN: RefCell<HashMap<usize, (u64, u32, u32)>> = RefCell::new(HashMap::new());
    static WRITTEN: RefCell<HashSet<u64>> = RefCell::new(HashSet::new());
}

#[inline(always)]
pub fn begin() {
    if enabled() {
        WINS.with(|w| w.borrow_mut().clear());
    }
}

#[inline(always)]
pub fn win(w: u32, ptr: *const u8, lo: u32, hi: u64) {
    if enabled() {
        let len = hi.saturating_sub(lo as u64).min(1 << 20) as u32;
        WINS.with(|v| v.borrow_mut().push(Win { w, ptr, lo, len }));
    }
}

fn st_range(k: &Kernel) -> Option<(u32, u32)> {
    let (mut lo, mut hi) = (u32::MAX, 0u32);
    for i in k.pre.iter().chain(&k.body).chain(&k.post) {
        let off = match *i {
            Inst::Def(_, Op::St32(o)) | Inst::StSt32 { off: o, .. } => o,
            _ => continue,
        };
        lo = lo.min(off);
        hi = hi.max(off + 4);
    }
    (lo != u32::MAX).then_some((lo, hi - lo))
}

fn fnv(words: &[u32]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for w in words {
        for b in w.to_le_bytes() {
            h = (h ^ b as u64).wrapping_mul(0x100_0000_01b3);
        }
    }
    h
}

/// Called just before a kernel runs, after the windows are in the context.
#[inline(always)]
pub fn before(k: &Kernel, ctx: *const u8, st: *const u8) -> Option<Capture> {
    if !enabled() {
        return None;
    }
    before_slow(k, ctx, st)
}

#[inline(never)]
fn before_slow(k: &Kernel, ctx: *const u8, st: *const u8) -> Option<Capture> {
    let d = dir()?;
    let key = k as *const Kernel as usize;
    let kid = SEEN.with(|s| {
        let mut s = s.borrow_mut();
        let e = s.entry(key).or_insert((0, 0, 0));
        if e.1 >= ATTEMPTS || e.2 >= PER_KERNEL {
            return None;
        }
        e.1 += 1;
        if e.0 == 0 {
            let words = k.encode();
            e.0 = fnv(&words);
            WRITTEN.with(|w| {
                if w.borrow_mut().insert(e.0) {
                    let bytes: Vec<u8> = words.iter().flat_map(|x| x.to_le_bytes()).collect();
                    let _ = std::fs::write(d.join(format!("kernel-{:016x}.bin", e.0)), bytes);
                    let _ =
                        std::fs::write(d.join(format!("kernel-{:016x}.txt", e.0)), format!("{k}"));
                }
            });
        }
        Some(e.0)
    })?;
    let (st_off, st_len) = st_range(k).unwrap_or((0, 0));
    // SAFETY: the engine passes its context buffer and its `St`; the register
    // range comes from the offsets the kernel names.
    let (ctx_pre, st_pre) = unsafe {
        (
            std::slice::from_raw_parts(ctx, CTX_SIZE as usize).to_vec(),
            std::slice::from_raw_parts(st.add(st_off as usize), st_len as usize).to_vec(),
        )
    };
    let wins = WINS.with(|w| {
        w.borrow()
            .iter()
            .map(|x| {
                // SAFETY: the window was resolved for this call and covers `len` bytes.
                let data = unsafe { std::slice::from_raw_parts(x.ptr, x.len as usize) }.to_vec();
                (x.w, x.lo, data, x.ptr, Vec::new())
            })
            .collect()
    });
    Some(Capture {
        kid,
        key,
        ctx_pre,
        st_off,
        st_pre,
        wins,
        ctx_post: Vec::new(),
        st_post: Vec::new(),
        code: 0,
    })
}

/// Called just after the kernel ran.
#[inline(always)]
pub fn after(cap: Option<Capture>, code: u32, ctx: *const u8, st: *const u8) {
    let Some(mut c) = cap else { return };
    // SAFETY: as in `before`.
    unsafe {
        c.ctx_post = std::slice::from_raw_parts(ctx, CTX_SIZE as usize).to_vec();
        c.st_post = std::slice::from_raw_parts(st.add(c.st_off as usize), c.st_pre.len()).to_vec();
    }
    for w in &mut c.wins {
        // SAFETY: the window memory is valid for the call that just ended.
        w.4 = unsafe { std::slice::from_raw_parts(w.3, w.2.len()) }.to_vec();
    }
    c.code = code;
    PENDING.with(|p| *p.borrow_mut() = Some(c));
}

/// Called by the engine once the call's instruction count is known.
#[inline(always)]
pub fn finish(entry_pc: u32, insns: u64) {
    if enabled() {
        finish_slow(entry_pc, insns);
    }
}

#[inline(never)]
fn finish_slow(entry_pc: u32, insns: u64) {
    let Some(d) = dir() else { return };
    let Some(c) = PENDING.with(|p| p.borrow_mut().take()) else {
        return;
    };
    if insns < MIN_INSNS && std::env::var_os("SHARC_FAST_DUMP_ALL").is_none() {
        return;
    }
    let n = SEEN.with(|s| {
        let mut s = s.borrow_mut();
        let e = s.entry(c.key).or_insert((0, 0, 0));
        e.2 += 1;
        e.2
    });
    let mut w: Vec<u8> = Vec::new();
    let push = |w: &mut Vec<u8>, x: u32| w.extend(x.to_le_bytes());
    for x in [
        0x3150_4143, // "CAP1"
        c.kid as u32,
        (c.kid >> 32) as u32,
        insns as u32,
        (insns >> 32) as u32,
        c.code,
        c.ctx_pre.len() as u32,
        c.st_off,
        c.st_pre.len() as u32,
        c.wins.len() as u32,
    ] {
        push(&mut w, x);
    }
    w.extend(&c.ctx_pre);
    w.extend(&c.ctx_post);
    w.extend(&c.st_pre);
    w.extend(&c.st_post);
    for (idx, lo, pre, ptr, post) in &c.wins {
        for x in [
            *idx,
            *lo,
            pre.len() as u32,
            *ptr as usize as u32,
            (*ptr as usize as u64 >> 32) as u32,
        ] {
            push(&mut w, x);
        }
        w.extend(pre);
        w.extend(post);
    }
    let _ = std::fs::write(d.join(format!("call-{:016x}-{n}.bin", c.kid)), w);
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(d.join("calls.tsv"))
    {
        let _ = writeln!(f, "{:016x}\t{entry_pc:#x}\t{insns}\t{}", c.kid, c.code);
    }
}

/// At exit: calls and instructions per region.
pub fn totals(rows: &[(u32, u64, u64)]) {
    let Some(d) = dir() else { return };
    let text: String = rows
        .iter()
        .map(|(pc, calls, insns)| format!("{pc:#x}\t{calls}\t{insns}\n"))
        .collect();
    let _ = std::fs::write(d.join("totals.tsv"), text);
}
