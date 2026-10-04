//! Running a region: the checks that stand for the architectural state the
//! region was lowered for, window resolution, the kernel call, and the exact
//! state the interpreter would have after the same number of instructions.
//!
//! What the kernel does not do is done here with the interpreter's own code
//! where that exists: the loop-end bookkeeping of the last executed
//! instruction is `_advance` of the generated core, so a loop that ends (or
//! loops back after a budget-limited run of iterations) behaves exactly as
//! the interpreter's.

use super::cfg::{CfgMeta, LoopDef, Site};
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
pub const AZ: u32 = 1 << 0;
pub const AV: u32 = 1 << 1;
pub const AN: u32 = 1 << 2;
pub const AC: u32 = 1 << 3;
pub const AS: u32 = 1 << 4;
pub const AI: u32 = 1 << 5;
pub const AF: u32 = 1 << 10;
pub const ALU_MASK: u32 = AZ | AV | AN | AC | AS | AI | AF;
pub const MULT_MASK: u32 = 0x3c0;
pub const MN: u32 = 1 << 6;
pub const SV: u32 = 1 << 11;
pub const SZ: u32 = 1 << 12;
pub const SS: u32 = 1 << 13;

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
        FlagKind::FaluOr => {
            let mut bits = AF;
            for r in src {
                if r & 0x7fff_ffff == 0 {
                    bits |= AZ;
                }
                if r >> 31 != 0 {
                    bits |= AN;
                }
            }
            define(v, ALU_MASK, bits)
        }
        FlagKind::IaddSubOr => define(
            v,
            ALU_MASK,
            arith_bits(src[0], src[1], false) | arith_bits(src[0], src[1], true),
        ),
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

/// Ranges where a normal-word access is a plain byte address scaled by 4:
/// not mapped by `addressing::normal_word_to_byte` (the mapped ranges all lie
/// in [0x90000, 0x18000000)), and clear of the core MMRs (normal-word
/// 0x30000..0x32000) and the peripheral space (0x30000000..0x40000000), which
/// the memory windows refuse anyway.
const NW_PLAIN: [(i64, i64); 5] = [
    (0, 0x3_0000),
    (0x3_2000, 0x9_0000),
    (0xE_8000, 0x400_0000),
    (0x1800_0000, 0x3000_0000),
    (0x4000_0000, 1 << 32),
];

fn nw_plain(l: i64, h: i64) -> bool {
    NW_PLAIN.iter().any(|&(lo, hi)| l >= lo && h < hi)
}

pub fn eval_req(s: &St, req: &Req, n: i64) -> bool {
    match *req {
        Req::Known(c) => s.r[c as usize].is_c(),
        Req::Eq(c, k) => s.r[c as usize].is_c() && s.r[c as usize].b == k,
        Req::Mode1 { mask, value } => {
            let m = s.r[MODE1];
            m.is_c() && m.b & mask == value
        }
        Req::FlagsKnown(mask) => s.r[118].m & mask == mask,
        Req::NwPlain {
            base,
            lo,
            hi,
            ts,
            nf,
        } => {
            let b = match base {
                Base::None => 0,
                Base::Reg(c) => s.r[c as usize].b as i64,
            };
            let d = ts * (n - 1);
            let f = nf * n;
            let (l, h) = (b + lo + d.min(0) + f, b + hi + d.max(0) + f);
            nw_plain(l, h)
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
    if !mode1.is_c() || mode1.b & (7 << 21) != 0 {
        // Unknown MODE1, or SIMD (PEx and PEy both execute), or a broadcast
        // load (BDCST9/BDCST1: a second register is loaded).
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
    for req in &r.misc {
        if !eval_req(s, req, iters) {
            return Err(Decline::Req);
        }
    }
    // Windows.
    let check = WIN_CHECK.load(std::sync::atomic::Ordering::Relaxed);
    for (w, spec) in r.wins.iter().enumerate() {
        let b = match spec.base {
            Base::None => 0,
            Base::Reg(c) => s.r[c as usize].b as i64,
        };
        let d = spec.ts * (iters - 1);
        let f = spec.nf * iters;
        let (lo, hi) = (b + spec.lo + d.min(0) + f, b + spec.hi + d.max(0) + f);
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
        if check {
            note_window(ctx, w, lo as u32, hi.min(u32::MAX as i64) as u32);
        }
    }
    if check {
        for w in r.wins.len()..MAX_WIN as usize {
            ctx.set64(win_ptr(w as u32), 0);
            note_window(ctx, w, 0, 0);
        }
        ctx.set32(CTX_WIN_CHECK, 1);
    } else {
        ctx.set32(CTX_WIN_CHECK, 0);
    }
    ctx.set32(CTX_ITERS, iters as u32);
    for (i, c) in CONST_POOL.iter().enumerate() {
        ctx.set32(CTX_CONSTS + 4 * i as u32, *c);
    }
    for &c in &r.inputs {
        ctx.set32(ctx_reg_in(c as u32), input_of(s, c));
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
                b: ctx.get32(ctx_reg_out(code as u32)),
                m: u32::MAX,
            };
        }
    }
    apply_flags(s, r, ctx, full, done, kk);
    apply_groups(s, r, ctx);
    s.at_loaded_entry = false;
    if s.cfg.core_timer {
        s.timer_written = false;
    }
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

/// A kernel input: a register's value; the flag state starts as the entry
/// ASTATX bits and "no writer yet" for each flag group.
fn input_of(s: &St, c: u8) -> u32 {
    match c as u32 {
        PSEUDO_FB0 => s.r[118].b,
        c if c >= NREGS => 0,
        c => s.r[c as usize].b,
    }
}

/// The flag groups' last writers (kernels that track flags, `lower::cond`)
/// applied to ASTATX with the replay's own effect.
fn apply_groups(s: &mut St, r: &Region, ctx: &Ctx) {
    if !r.flag_groups.iter().any(|g| *g) {
        return;
    }
    let mut v = s.r[118];
    for g in 0..NGROUPS {
        if !r.flag_groups[g as usize] {
            continue;
        }
        let id = ctx.get32(ctx_reg_out(pseudo_flag(g, 0)));
        if let Some(kind) = FlagKind::from_group_id(g, id) {
            let src = [
                ctx.get32(ctx_reg_out(pseudo_flag(g, 1))),
                ctx.get32(ctx_reg_out(pseudo_flag(g, 2))),
            ];
            v = apply_flag(v, kind, src);
        }
    }
    s.r[118] = v;
}

fn apply_flags(s: &mut St, r: &Region, ctx: &Ctx, full: bool, done: u64, kk: u64) {
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
        s.r[118] = v;
    }
}

#[allow(dead_code)]
fn _unused(_: &RegMeta) {}

// -- CFG regions ---------------------------------------------------------------

/// The loop PC-stack entry the interpreter pushes for a DO (its start).
fn pc_stack_items(s: &St) -> &[Int] {
    if s.cfg.stack_model {
        s.pc_stack.items()
    } else {
        s.call_stack.items()
    }
}

#[cfg(sharc_gen)]
mod sites {
    use super::*;
    use crate::generated::core_g::{sequencer, state};

    const STKYX: usize = 120;
    const LCNTR: usize = 104;
    const CURLCNTR: usize = 103;
    const BIT26: u32 = 1 << 26;

    fn set_bit26(s: &mut St, v: Option<bool>, entry: V) {
        let cur = s.r[STKYX];
        let new = match v {
            Some(true) => V {
                b: cur.b | BIT26,
                m: cur.m | BIT26,
            },
            Some(false) => V {
                b: cur.b & !BIT26,
                m: cur.m | BIT26,
            },
            // No loop event happened: bit 26 is what it was at entry.
            None => V {
                b: (cur.b & !BIT26) | (entry.b & BIT26),
                m: (cur.m & !BIT26) | (entry.m & BIT26),
            },
        };
        s.r[STKYX] = new;
    }

    /// A loop ends: the interpreter's `_advance` loop-exit path.
    fn pop_loop(s: &mut St, start: u32) -> R<()> {
        s.loops.n -= 1;
        if s.cfg.stack_model {
            state::_pop_loop_resource(s)?;
        }
        let depth = state::_pc_stack_depth(s)?;
        if depth == 0 || (state::_pc_stack_top(s)? & 0xff_ffff) != start as Int {
            return Err(TRAP_INDEX);
        }
        sequencer::_pop_pc_stack(s)?;
        if !s.cfg.stack_model {
            let top = s.loops.items().last().map(|l| l.remaining);
            let v = match top {
                Some(r) => V::c(r as Int),
                None => V::c(0xFFFF_FFFF),
            };
            s.r[CURLCNTR] = v;
            if s.loops.n == 0 {
                let c = s.r[STKYX];
                s.r[STKYX] = V {
                    b: c.b | BIT26,
                    m: c.m | BIT26,
                };
            }
        }
        Ok(())
    }

    /// The state a DO leaves, with the loop's remaining count REM.
    fn push_loop(s: &mut St, l: &LoopDef, rem: u32) -> R<()> {
        s.r[CURLCNTR] = V::c(rem as Int);
        let c = s.r[STKYX];
        s.r[STKYX] = V {
            b: c.b & !BIT26,
            m: c.m | BIT26,
        };
        if s.cfg.stack_model {
            state::_push_loop_resource(s)?;
            let slot = (
                state::_packed_counter_laddr(s, l.end as Int)?,
                V::c(rem as Int),
            );
            let at = (s.loop_depth - 1) as usize;
            s.loop_slots.a[at] = slot;
            state::_sync_empty_loop_registers(s)?;
        }
        s.loops.push_raw(Loop {
            start_sw: l.start as i64,
            end_sw: l.end as i64,
            remaining: rem as i64,
            mode: l.mode,
        })?;
        state::_push_pc_stack(s, l.start as Int)
    }

    /// Rebuild the loop stack, PC stack and the registers that mirror them as
    /// the interpreter has them at EXIT.
    pub fn apply(s: &mut St, meta: &CfgMeta, site: &Site, ctx: &Ctx) -> R<()> {
        let n = meta.n_entry;
        let entry_stkyx = s.r[STKYX];
        let active_entry = site.active.iter().filter(|&&i| (i as usize) < n).count();
        let rem = |i: usize| ctx.get32(CTX_LOOP_REM_OUT + 4 * i as u32);
        let any_inregion = site.active.iter().any(|&i| (i as usize) >= n);
        let did = ctx.get32(CTX_DIDPC) != 0;
        let loops_before = s.loops.n;
        // Entry loops that ended. The slot of a loop that finished keeps its
        // last counter, 1.
        for i in (active_entry..n).rev() {
            if s.cfg.stack_model {
                let at = s.loop_depth as usize - 1;
                s.loop_slots.a[at].1 = V::c(1);
            }
            pop_loop(s, meta.loops[i].start)?;
        }
        // Entry loops still running: their counts.
        let base = s.loops.n - active_entry;
        let slot_base = if s.cfg.stack_model {
            s.loop_depth as usize - s.loops.n
        } else {
            0
        };
        for i in 0..active_entry {
            s.loops.a[base + i].remaining = rem(i) as i64;
            if s.cfg.stack_model {
                s.loop_slots.a[slot_base + base + i].1 = V::c(rem(i) as Int);
            }
        }
        if !s.cfg.stack_model {
            if active_entry > 0 && !did {
                s.r[CURLCNTR] = V::c(rem(active_entry - 1) as Int);
            } else if did {
                // The last loop event set the mirror.
                let top = s.loops.items().last().map(|l| l.remaining);
                s.r[CURLCNTR] = match top {
                    Some(r) => V::c(r as Int),
                    None => V::c(0xFFFF_FFFF),
                };
            }
        }
        // Loops started inside the region.
        for &i in site.active.iter().filter(|&&i| (i as usize) >= n) {
            push_loop(s, &meta.loops[i as usize], rem(i as usize))?;
        }
        if s.cfg.stack_model {
            // Loops started and finished in the region leave their slots
            // behind (address word, counter 1) unless a later loop reused
            // the depth.
            let depth = s.loop_depth as usize;
            let reserved = depth - s.loops.n;
            for d in 0..MAX_SLOTS as usize {
                let li = ctx.get32(CTX_SLOT_LAST + 4 * d as u32) as usize;
                let at = reserved + d;
                if li != 0 && at >= depth && at < s.loop_slots.items().len() {
                    let l = &meta.loops[li - 1];
                    let laddr = state::_packed_counter_laddr(s, l.end as Int)?;
                    s.loop_slots.a[at] = (laddr, V::c(1));
                }
            }
            // `_execute` starts every instruction with this: LADDR and
            // CURLCNTR follow the top loop slot.
            state::_sync_empty_loop_registers(s)?;
        }
        if did || any_inregion {
            state::_sync_pc_stack(s)?;
        }
        if ctx.get32(CTX_LCNTR + 4) != 0 {
            s.r[LCNTR] = V::c(ctx.get32(CTX_LCNTR) as Int);
        }
        let ev = match ctx.get32(CTX_STK26) {
            1 => Some(false),
            2 => Some(true),
            _ => None,
        };
        set_bit26(s, ev, entry_stkyx);
        if s.loops.n != loops_before || did {
            s.check_loops();
        }
        Ok(())
    }
}

static WIN_CHECK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Tests: have the reference interpreter check every window access
/// (`CTX_WIN_CHECK`).
pub fn set_window_check(on: bool) {
    WIN_CHECK.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Record window W's extent for the check (and clear the unused ones).
fn note_window(ctx: &mut Ctx, w: usize, lo: u32, hi: u32) {
    ctx.set32(CTX_WIN_EXT + 8 * w as u32, lo);
    ctx.set32(CTX_WIN_EXT + 8 * w as u32 + 4, hi);
}

static SHAPE_LOG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `SHARC_FAST_LOG`: say why a call was declined as the wrong shape.
pub fn set_shape_log(on: bool) {
    SHAPE_LOG.store(on, std::sync::atomic::Ordering::Relaxed);
}

#[cold]
fn shape(why: &str) -> Decline {
    if SHAPE_LOG.load(std::sync::atomic::Ordering::Relaxed) {
        eprintln!("fast: declined, shape: {why}");
    }
    Decline::Shape
}

/// Run a CFG region's kernel (see `cfg`). As `run`: `Ok(code)` after at least
/// one instruction completed, `Err` with the state untouched.
#[cfg(sharc_gen)]
pub fn run_cfg(
    s: &mut St,
    r: &Region,
    kernel: &dyn CompiledKernel,
    ctx: &mut Ctx,
) -> Result<u32, Decline> {
    let meta = r.cfg.as_ref().expect("a cfg region");
    if s.pending.is_some() || s.pc_sw != r.entry_pc as Int {
        return Err(shape("pending transfer or pc"));
    }
    let cfg = &s.cfg;
    if !(cfg.fast_mem && cfg.assume_nw32 && cfg.has_concrete && !cfg.data_memory_tainted) {
        return Err(shape("memory configuration"));
    }
    if cfg.bank_model && (s.bank_pending_mask >= 0 || s.bank_requested_mask >= 0) {
        return Err(shape("bank change in flight"));
    }
    if cfg.stack_model && (s.pc_stack_pending >= 0 || s.pc_stack_requested >= 0) {
        return Err(shape("PC stack change in flight"));
    }
    let mode1 = s.r[MODE1];
    if !mode1.is_c() || mode1.b & (7 << 21) != 0 {
        // Unknown MODE1, SIMD, or a broadcast load (BDCST9/BDCST1).
        return Err(shape("MODE1 unknown, SIMD or broadcast load"));
    }
    // The loops active at entry that the region models.
    let n = meta.n_entry;
    let nloops = s.loops.n;
    if nloops < n {
        return Err(shape("fewer loops than the region models"));
    }
    // Stack model: slot k of the loop stack belongs to loops[k - slot_base]
    // (slots reserved by PUSH LOOP lie below the DO loops).
    let slot_base = if cfg.stack_model {
        let depth = s.loop_depth as usize;
        if depth < nloops
            || depth > s.loop_slots.items().len()
            || s.loops.items().iter().any(|l| l.start_sw == 0xFFFF_FFFF)
        {
            return Err(shape("loop slots do not match the loop stack"));
        }
        depth - nloops
    } else {
        0
    };
    let mut rem_top = 1i64;
    for i in 0..n {
        let l = s.loops.a[nloops - n + i];
        let d = &meta.loops[i];
        if l.start_sw != d.start as i64 || l.end_sw != d.end as i64 || l.mode != d.mode {
            return Err(shape("entry loop differs from the region's"));
        }
        if l.remaining < 1 || l.remaining > u32::MAX as i64 {
            return Err(shape("entry loop count"));
        }
        ctx.set32(CTX_LOOP_REM_IN + 4 * i as u32, l.remaining as u32);
        rem_top = l.remaining;
        if cfg.stack_model {
            let slot = s.loop_slots.items().get(slot_base + nloops - n + i);
            if slot.is_none_or(|x| x.1 != V::c(l.remaining as Int)) {
                return Err(shape("entry loop slot counter"));
            }
        }
        // The loop's PC-stack entry.
        let st = pc_stack_items(s);
        if st.len() < n || (st[st.len() - n + i] & 0xff_ffff) != l.start_sw as Int {
            return Err(shape("entry loop PC-stack entry"));
        }
    }
    if n > 0 {
        let c = s.r[103];
        if !c.is_c() || c.b as i64 != rem_top {
            return Err(shape("CURLCNTR"));
        }
    }
    // Room for the loops the region starts.
    let deep = meta.deepest_in_region as usize;
    if nloops + deep > MAX_LOOPS - 1
        || pc_stack_items(s).len() + deep >= 29
        || (cfg.stack_model && s.loop_depth as usize + deep > 5)
    {
        return Err(shape("loop or PC stack too deep"));
    }
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
    let iters = rem_top.max(1);
    for req in r.nw.iter().chain(&r.misc) {
        if !eval_req(s, req, iters) {
            return Err(Decline::Req);
        }
    }
    let avail = s.limit.saturating_sub(s.icount);
    if avail == 0 {
        return Err(Decline::Budget);
    }
    let check = WIN_CHECK.load(std::sync::atomic::Ordering::Relaxed);
    for (w, spec) in r.wins.iter().enumerate() {
        let b = match spec.base {
            Base::None => 0,
            Base::Reg(c) => s.r[c as usize].b as i64,
        };
        let d = spec.ts * (iters - 1);
        let f = spec.nf * iters;
        let (lo, hi) = (b + spec.lo + d.min(0) + f, b + spec.hi + d.max(0) + f);
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
        if check {
            note_window(ctx, w, lo as u32, hi.min(u32::MAX as i64) as u32);
        }
    }
    if check {
        for w in r.wins.len()..MAX_WIN as usize {
            ctx.set64(win_ptr(w as u32), 0);
            note_window(ctx, w, 0, 0);
        }
        ctx.set32(CTX_WIN_CHECK, 1);
    } else {
        ctx.set32(CTX_WIN_CHECK, 0);
    }
    ctx.set32(CTX_BUDGET, avail.min(1 << 31) as u32);
    for (i, c) in CONST_POOL.iter().enumerate() {
        ctx.set32(CTX_CONSTS + 4 * i as u32, *c);
    }
    for &c in &r.inputs {
        ctx.set32(ctx_reg_in(c as u32), input_of(s, c));
    }
    // SAFETY: the windows were resolved for this call and cover every access
    // (the lowering's range analysis).
    let code = unsafe { kernel.run(ctx.ptr()) };
    debug_assert_eq!(code, 1, "a cfg kernel leaves through an exit");
    let site = &meta.sites[ctx.get32(CTX_EXIT_K) as usize];
    let completed = ctx.get32(CTX_ICNT) as u64 + site.off as u64;
    if completed == 0 {
        return Err(Decline::Exit0);
    }
    // Registers: every register the region writes, from the kernel's
    // variables (unchanged where the path did not write it).
    for &(c, _) in &r.outputs {
        s.r[c as usize] = V {
            b: ctx.get32(ctx_reg_out(c as u32)),
            m: u32::MAX,
        };
    }
    apply_groups(s, r, ctx);
    s.pc_sw = site.pc as Int;
    s.icount += completed;
    s.steps += completed as Int;
    // What `exec_insn` does at the start of every instruction.
    s.at_loaded_entry = false;
    if s.cfg.core_timer {
        s.timer_written = false;
    }
    if let Some((j, slots)) = site.pending
        && ctx.get32(CTX_TAKEN + 4 * j as u32) != 0
    {
        s.pending = Some(Pending {
            target: Some(meta.jumps[j as usize].0 as Int),
            call: false,
            slots: slots as Int,
            return_from_call: false,
            return_sw: None,
        });
    }
    s.begin();
    let res = sites::apply(s, meta, site, ctx);
    match res {
        Ok(()) => s.commit_host(),
        Err(t) => {
            s.rollback();
            s.trap = Some(t);
            return Ok(crate::EXIT_TRAP);
        }
    }
    Ok(crate::EXIT_NEXT)
}

#[cfg(not(sharc_gen))]
pub fn run_cfg(
    _s: &mut St,
    _r: &Region,
    _kernel: &dyn CompiledKernel,
    _ctx: &mut Ctx,
) -> Result<u32, Decline> {
    Err(Decline::Shape)
}
