//! Running a region: the checks that stand for the architectural state the
//! region was lowered for, window resolution, the kernel call, and the exact
//! state the interpreter would have after the same number of instructions.
//!
//! What the kernel does not do is done here with the interpreter's own code
//! where that exists: the loop-end bookkeeping of the last executed
//! instruction is `_advance` of the generated core, so a loop that ends (or
//! loops back after a budget-limited run of iterations) behaves exactly as
//! the interpreter's.

use super::ir::*;
use super::region::{RegMeta, Region};
use super::{Base, FlagKind, Req};
use crate::rt::*;

/// The kernel context buffer.
pub struct Ctx {
    buf: Box<[u64]>,
}

impl Default for Ctx {
    fn default() -> Ctx {
        Ctx {
            buf: vec![0u64; CTX_SIZE as usize / 8].into_boxed_slice(),
        }
    }
}

impl Ctx {
    #[inline(always)]
    pub fn ptr(&mut self) -> *mut u8 {
        self.buf.as_mut_ptr() as *mut u8
    }
    #[inline(always)]
    pub fn get32(&self, off: u32) -> u32 {
        debug_assert!(off + 4 <= CTX_SIZE && off % 4 == 0);
        // SAFETY: in bounds of the buffer, 4-byte aligned (checked above).
        unsafe {
            (self.buf.as_ptr() as *const u8)
                .add(off as usize)
                .cast::<u32>()
                .read()
        }
    }
    #[inline(always)]
    pub fn set32(&mut self, off: u32, v: u32) {
        debug_assert!(off + 4 <= CTX_SIZE && off % 4 == 0);
        // SAFETY: as get32.
        unsafe {
            (self.buf.as_mut_ptr() as *mut u8)
                .add(off as usize)
                .cast::<u32>()
                .write(v)
        }
    }
    #[inline(always)]
    pub fn set64(&mut self, off: u32, v: u64) {
        debug_assert!(off + 8 <= CTX_SIZE && off % 8 == 0);
        // SAFETY: as get32.
        unsafe {
            (self.buf.as_mut_ptr() as *mut u8)
                .add(off as usize)
                .cast::<u64>()
                .write(v)
        }
    }
}

/// Why a call did nothing (state untouched).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decline {
    /// Engine state outside the region's shape (pending transfer, pc,
    /// models in flight, MODE1, loop stack, counters).
    Shape,
    /// A register requirement (unknown value, changed constant, range).
    Req,
    /// A memory window is not plain, present and writable RAM.
    Window,
    /// Not even one iteration fits the budget.
    Budget,
    /// The kernel left before completing its first instruction.
    Exit0,
}

/// The ASTATX bits the flag writers define or forget.
const AZ: u32 = 1 << 0;
const AV: u32 = 1 << 1;
const AN: u32 = 1 << 2;
const AC: u32 = 1 << 3;
const AS: u32 = 1 << 4;
const AI: u32 = 1 << 5;
const AF: u32 = 1 << 10;
const ALU_MASK: u32 = AZ | AV | AN | AC | AS | AI | AF;
const MULT_MASK: u32 = 0x3c0;
const MN: u32 = 1 << 6;
const SV: u32 = 1 << 11;
const SZ: u32 = 1 << 12;
const SS: u32 = 1 << 13;
const CACC: u32 = 0xff00_0000;

fn define(v: V, mask: u32, bits: u32) -> V {
    V {
        b: (v.b & !mask) | (bits & mask),
        m: v.m | mask,
    }
}

fn forget(v: V, mask: u32) -> V {
    let m = v.m & !mask;
    V { b: v.b & m, m }
}

/// AC AV AN AZ of `a + b` (or `a - b`) as `flags._arith_flag_bits`.
fn arith_bits(a: u32, b: u32, sub: bool) -> u32 {
    let (b_eff, cin) = if sub { (!b, 1u64) } else { (b, 0u64) };
    let low31 = (a & 0x7fff_ffff) as u64 + (b_eff & 0x7fff_ffff) as u64 + cin;
    let into_msb = (low31 >> 31) & 1;
    let full = a as u64 + b_eff as u64 + cin;
    let cout = (full >> 32) & 1;
    let res = full as u32;
    let mut bits = 0;
    if cout == 1 {
        bits |= AC;
    }
    if into_msb ^ cout == 1 {
        bits |= AV;
    }
    if res & 0x8000_0000 != 0 {
        bits |= AN;
    }
    if res == 0 {
        bits |= AZ;
    }
    bits
}

/// ASTATX after one flag writer, given its remembered sources.
pub fn apply_flag(v: V, kind: FlagKind, src: [u32; 2]) -> V {
    match kind {
        FlagKind::Falu => {
            let r = src[0];
            let mut bits = AF;
            if r & 0x7fff_ffff == 0 {
                bits |= AZ;
            }
            if r >> 31 != 0 {
                bits |= AN;
            }
            define(v, ALU_MASK, bits)
        }
        FlagKind::Fmul => define(v, MULT_MASK, if src[0] >> 31 != 0 { MN } else { 0 }),
        FlagKind::FmulForget => forget(v, MULT_MASK),
        FlagKind::Iadd => define(v, ALU_MASK, arith_bits(src[0], src[1], false)),
        FlagKind::Isub => define(v, ALU_MASK, arith_bits(src[0], src[1], true)),
        FlagKind::Logical => {
            let r = src[0];
            let mut bits = 0;
            if r >> 31 != 0 {
                bits |= AN;
            }
            if r == 0 {
                bits |= AZ;
            }
            define(v, ALU_MASK, bits)
        }
        FlagKind::Shift { sv } | FlagKind::Fext { sv } => {
            let mut bits = 0;
            if sv {
                bits |= SV;
            }
            if src[0] == 0 {
                bits |= SZ;
            }
            define(v, SV | SZ | SS, bits)
        }
        FlagKind::Fabs => {
            let mut bits = AF;
            if src[0] & 0x7fff_ffff == 0 {
                bits |= AZ;
            }
            if src[1] >> 31 != 0 {
                bits |= AS;
            }
            define(v, ALU_MASK, bits)
        }
        FlagKind::Compare { float } => {
            // AZ and AN only; the CACC shift register is `apply_flags`'.
            let mut bits = if float { AF } else { 0 };
            if src[0] & 1 != 0 {
                bits |= AZ;
            }
            if src[0] & 4 != 0 {
                bits |= AN;
            }
            define(v, ALU_MASK, bits)
        }
        FlagKind::Btst => {
            let mut bits = 0;
            if src[0] & 1 != 0 {
                bits |= SZ;
            }
            if src[0] & 2 != 0 {
                bits |= SV;
            }
            define(v, SV | SZ | SS, bits)
        }
        FlagKind::Leftz => {
            let mut bits = 0;
            if src[0] >> 31 != 0 {
                bits |= SZ;
            }
            if src[0] == 0 {
                bits |= SV;
            }
            define(v, SV | SZ | SS, bits)
        }
        FlagKind::ShiftDyn => {
            let mut bits = 0;
            if src[1] != 0 {
                bits |= SV;
            }
            if src[0] == 0 {
                bits |= SZ;
            }
            define(v, SV | SZ | SS, bits)
        }
        FlagKind::Recips => {
            let bits = if src[0] >> 31 != 0 { AN } else { 0 };
            define(v, AC | AS | AI | AN | AV | AZ, bits)
        }
    }
}

/// The interpreter's `_advance` for the region's last instruction (loop
/// bookkeeping and the PC), counted as one completed instruction.
#[cfg(sharc_gen)]
fn advance_last(s: &mut St, len_bytes: u8) -> R<()> {
    let insn = Insn {
        length_bytes: Some(len_bytes as Int),
        ..INSN_NONE
    };
    s.begin();
    match crate::generated::core_g::sequencer::_advance(s, insn) {
        Ok(()) => {
            s.commit();
            Ok(())
        }
        Err(t) => {
            s.rollback();
            Err(t)
        }
    }
}

#[cfg(not(sharc_gen))]
fn advance_last(_s: &mut St, _len_bytes: u8) -> R<()> {
    Err(TRAP_NO_INSN)
}

const NW_LO: i64 = 0xE8000;
const NW_HI: i64 = 0x400_0000;

pub fn eval_req(s: &St, req: &Req, n: i64) -> bool {
    match *req {
        Req::Known(c) => s.r[c as usize].is_c(),
        Req::Eq(c, k) => s.r[c as usize].is_c() && s.r[c as usize].b == k,
        Req::NwPlain { base, lo, hi, ts } => {
            let b = match base {
                Base::None => 0,
                Base::Reg(c) => s.r[c as usize].b as i64,
            };
            let d = ts * (n - 1);
            let (l, h) = (b + lo + d.min(0), b + hi + d.max(0));
            l >= NW_LO && h < NW_HI
        }
        Req::Mode1Bit { bit, set } => {
            let m = s.r[MODE1];
            m.is_c() && ((m.b >> bit) & 1 != 0) == set
        }
    }
}

/// Run REGION's kernel from the engine state S. `Ok(code)` after at least
/// one completed instruction (the engine's exit code); `Err` with the state
/// untouched.
pub fn run(
    s: &mut St,
    r: &Region,
    kernel: &dyn CompiledKernel,
    ctx: &mut Ctx,
) -> Result<u32, Decline> {
    if s.pending.is_some() || s.pc_sw != r.entry_pc as Int {
        return Err(Decline::Shape);
    }
    let cfg = &s.cfg;
    if !(cfg.fast_mem && cfg.assume_nw32 && cfg.has_concrete && !cfg.data_memory_tainted) {
        return Err(Decline::Shape);
    }
    if cfg.bank_model && (s.bank_pending_mask >= 0 || s.bank_requested_mask >= 0) {
        return Err(Decline::Shape);
    }
    if cfg.stack_model && (s.pc_stack_pending >= 0 || s.pc_stack_requested >= 0) {
        return Err(Decline::Shape);
    }
    let mode1 = s.r[MODE1];
    if !mode1.is_c() || mode1.b & (1 << 21) != 0 {
        // Unknown MODE1, or SIMD (PEx and PEy both execute).
        return Err(Decline::Shape);
    }
    let k = r.insns.len() as u64;
    let (rem, iters) = match r.lp {
        Some(lp) => {
            let Some(top) = s.loops.items().last().copied() else {
                return Err(Decline::Shape);
            };
            if top.start_sw != lp.start || top.end_sw != lp.end || top.mode != lp.mode {
                return Err(Decline::Shape);
            }
            let rem = top.remaining;
            if rem < 1 || rem > u32::MAX as i64 {
                return Err(Decline::Shape);
            }
            let c = s.r[103];
            if !c.is_c() || c.b as i64 != rem {
                return Err(Decline::Shape);
            }
            if cfg.stack_model {
                let d = s.loop_depth;
                if d < 1 || d as usize > s.loop_slots.items().len() {
                    return Err(Decline::Shape);
                }
                let slot = s.loop_slots.items()[d as usize - 1];
                if slot.1 != V::c(rem as Int) {
                    return Err(Decline::Shape);
                }
            }
            let avail = s.limit.saturating_sub(s.icount);
            let fit = (avail / k).min(u32::MAX as u64) as i64;
            let iters = rem.min(fit);
            if iters == 0 {
                return Err(Decline::Budget);
            }
            if iters == rem {
                // The loop will end: `_advance` pops the PC stack entry the
                // DO pushed; it must be there.
                let stack = if cfg.stack_model {
                    s.pc_stack.items()
                } else {
                    s.call_stack.items()
                };
                match stack.last() {
                    Some(&top_pc) if (top_pc & 0xff_ffff) == lp.start as Int => {}
                    _ => return Err(Decline::Shape),
                }
            }
            (rem, iters)
        }
        None => {
            if s.limit.saturating_sub(s.icount) < k {
                return Err(Decline::Budget);
            }
            (1, 1)
        }
    };
    for &c in &r.known {
        if s.r[c as usize].m != u32::MAX {
            return Err(Decline::Req);
        }
    }
    for &(c, k) in &r.eqs {
        let v = s.r[c as usize];
        if v.m != u32::MAX || v.b != k {
            return Err(Decline::Req);
        }
    }
    for req in &r.nw {
        if !eval_req(s, req, iters) {
            return Err(Decline::Req);
        }
    }
    // Windows.
    for (w, spec) in r.wins.iter().enumerate() {
        let b = match spec.base {
            Base::None => 0,
            Base::Reg(c) => s.r[c as usize].b as i64,
        };
        let d = spec.ts * (iters - 1);
        let (lo, hi) = (b + spec.lo + d.min(0), b + spec.hi + d.max(0));
        if lo < 0 || hi > (1i64 << 32) {
            return Err(Decline::Window);
        }
        let Some(ptr) = s
            .mem
            .window(lo as u32, (hi - lo) as u32, spec.read, spec.write)
        else {
            return Err(Decline::Window);
        };
        ctx.set64(win_ptr(w as u32), (ptr as u64).wrapping_sub(lo as u64));
    }
    ctx.set32(CTX_ITERS, iters as u32);
    for (i, c) in CONST_POOL.iter().enumerate() {
        ctx.set32(CTX_CONSTS + 4 * i as u32, *c);
    }
    for &c in &r.inputs {
        ctx.set32(CTX_REGS_IN + 4 * c as u32, s.r[c as usize].b);
    }
    // SAFETY: the windows were resolved for this call and cover every
    // access of `iters` iterations (the lowering's range analysis).
    let code = unsafe { kernel.run(ctx.ptr()) };
    let done = ctx.get32(CTX_DONE) as u64;
    let kk = if code == 0 {
        k
    } else {
        ctx.get32(CTX_EXIT_K) as u64
    };
    let completed = if code == 0 {
        iters as u64 * k
    } else {
        done * k + kk
    };
    if completed == 0 {
        return Err(Decline::Exit0);
    }
    // Registers: those the instructions that ran have written.
    let full = code == 0;
    for &(code, first) in &r.outputs {
        let written = full || done > 0 || (first as u64) < kk;
        if written {
            s.r[code as usize] = V {
                b: ctx.get32(CTX_REGS_OUT + 4 * code as u32),
                m: u32::MAX,
            };
        }
    }
    apply_flags(s, r, ctx, full, done, kk, iters as u64);
    let ran = if full { iters as u64 } else { done };
    // Loop-backs that happened before the end of the executed part. A full
    // run's last iteration ends inside `advance_last` below.
    let backs = if full { ran.saturating_sub(1) } else { ran };
    if backs > 0 && r.lp.is_some() {
        collapse_loop_backs(s, rem, backs as i64);
    }
    if full {
        let last = r.insns.last().unwrap();
        s.pc_sw = last.pc as Int;
        s.icount += completed - 1;
        s.steps += completed as Int - 1;
        if let Err(t) = advance_last(s, r.last_len_bytes) {
            s.trap = Some(t);
            return Ok(crate::EXIT_TRAP);
        }
        let ended = r.lp.is_none() || iters == rem;
        Ok(if ended {
            crate::EXIT_NEXT
        } else {
            crate::EXIT_BUDGET
        })
    } else {
        s.pc_sw = r.insns[kk as usize].pc as Int;
        s.icount += completed;
        s.steps += completed as Int;
        Ok(crate::EXIT_BUDGET)
    }
}

/// N loop-backs of the top loop: remaining, CURLCNTR and (stack model) the
/// loop slot's counter, as `_advance` leaves them.
fn collapse_loop_backs(s: &mut St, rem: i64, n: i64) {
    let left = rem - n;
    let top = s.loops.n - 1;
    s.loops.a[top].remaining = left;
    s.r[103] = V::c(left as Int);
    if s.cfg.stack_model {
        let d = s.loop_depth as usize - 1;
        s.loop_slots.a[d].1 = V::c(left as Int);
    }
}

fn apply_flags(s: &mut St, r: &Region, ctx: &Ctx, full: bool, done: u64, kk: u64, iters: u64) {
    if r.flags.is_empty() {
        return;
    }
    let mut v = s.r[118];
    let mut any = false;
    let one = |w: &super::FlagWriter, v: &mut V| {
        let mut src = [0u32; 2];
        for (j, slot) in w.fsrc.iter().enumerate() {
            if *slot != 255 {
                src[j] = ctx.get32(CTX_FSRC + 4 * *slot as u32);
            }
        }
        *v = apply_flag(*v, w.kind, src);
    };
    if full {
        for w in &r.flags {
            one(w, &mut v);
            any = true;
        }
    } else {
        if done > 0 {
            // Writers after the failing instruction last ran in the
            // previous iteration.
            for w in r.flags.iter().filter(|w| w.insn as u64 >= kk) {
                one(w, &mut v);
                any = true;
            }
        }
        for w in r.flags.iter().filter(|w| (w.insn as u64) < kk) {
            one(w, &mut v);
            any = true;
        }
    }
    if any {
        v = compare_history(s.r[118], v, r, ctx, full, done, kk, iters);
        s.r[118] = v;
    }
}

/// The CACC shift register after the compare writers that ran: each compare
/// shifts its result bit into bit 31 (`values._apply_flag_update`, when the
/// ASTATX before it is fully known; otherwise the register becomes unknown).
/// The kernel carries the shifted-in bits in a history word whose slot each
/// compare writer refreshes (`Lower::compare_flag`). V is ASTATX after the
/// writers' other effects, V0 the value before the region.
#[allow(clippy::too_many_arguments)]
fn compare_history(
    v0: V,
    v: V,
    r: &Region,
    ctx: &Ctx,
    full: bool,
    done: u64,
    kk: u64,
    iters: u64,
) -> V {
    let is_cmp = |w: &&super::FlagWriter| matches!(w.kind, FlagKind::Compare { .. });
    let n = r.flags.iter().filter(is_cmp).count() as u64;
    if n == 0 {
        return v;
    }
    let (shifts, last) = if full {
        (iters * n, r.flags.iter().rfind(is_cmp))
    } else {
        let cur = r
            .flags
            .iter()
            .filter(is_cmp)
            .filter(|w| (w.insn as u64) < kk);
        let in_cur = cur.clone().count() as u64;
        let last_cur = cur.last();
        let last = if last_cur.is_some() {
            last_cur
        } else if done > 0 {
            r.flags.iter().rfind(is_cmp)
        } else {
            None
        };
        (done * n + in_cur, last)
    };
    let Some(last) = last else { return v };
    if shifts == 0 {
        return v;
    }
    if v0.m != u32::MAX {
        return forget(v, CACC);
    }
    let hist = ctx.get32(CTX_FSRC + 4 * last.fsrc[1] as u32);
    let new = if shifts >= 8 {
        hist & CACC
    } else {
        ((v0.b & CACC) >> shifts) & CACC | (hist & CACC)
    };
    V {
        b: (v.b & !CACC) | new,
        m: v.m,
    }
}

#[allow(dead_code)]
fn _unused(_: &RegMeta) {}
