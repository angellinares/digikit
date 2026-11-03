//! Running a region: the checks that stand for the architectural state the
//! region was lowered for, window resolution, the kernel call, and the exact
//! state the interpreter would have after the same number of instructions.
//!
//! What the kernel does not do is done here with the interpreter's own code
//! where that exists: the loop-end bookkeeping of the last executed
//! instruction is `_advance` of the generated core, so a loop that ends (or
//! loops back after a budget-limited run of iterations) behaves exactly as
//! the interpreter's.

use super::cfg::{CfgMeta, DYN_PC, LoopDef, Site, Tail};
use super::ir::*;
use super::region::{RegMeta, Region};
use super::{Base, FlagKind, FlagWriter, Req};
use crate::rt::*;

/// The kernel context buffer.
pub struct Ctx {
    buf: Box<[u64]>,
    /// `CTX_WIN_CHECK` as last written (the constant pool is written once).
    win_check: bool,
    /// The address of the return instruction the last cluster kernel run left
    /// through (0: it left elsewhere); read by the coverage report.
    pub ret_at: u32,
}

impl Default for Ctx {
    fn default() -> Ctx {
        let mut c = Ctx {
            buf: vec![0u64; CTX_SIZE as usize / 8].into_boxed_slice(),
            win_check: false,
            ret_at: 0,
        };
        for (i, k) in CONST_POOL.iter().enumerate() {
            c.set32(CTX_CONSTS + 4 * i as u32, *k);
        }
        c
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
/// ASTATX.MU, and STKYX.MUS (the same bit number): a float multiply result below
/// the smallest normal.
pub const MU: u32 = 1 << 8;
pub const MUS: u32 = 1 << 8;
pub const STKYX_CODE: usize = 120;
pub const SV: u32 = 1 << 11;
pub const SZ: u32 = 1 << 12;
pub const SS: u32 = 1 << 13;
pub const CACC: u32 = 0xff00_0000;

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

/// The ASTATX bits a flag writer defines or forgets.
pub fn flag_mask(kind: FlagKind) -> u32 {
    match kind {
        FlagKind::Falu
        | FlagKind::FaluOr
        | FlagKind::IaddSubOr
        | FlagKind::Iadd
        | FlagKind::Isub
        | FlagKind::Logical
        | FlagKind::Fabs
        | FlagKind::Iabs
        | FlagKind::Compare { .. } => ALU_MASK,
        FlagKind::Fmul | FlagKind::FmulForget => MULT_MASK,
        FlagKind::Shift { .. }
        | FlagKind::Fext { .. }
        | FlagKind::Btst
        | FlagKind::Leftz
        | FlagKind::ShiftDyn => SV | SZ | SS,
    }
}

/// The writers whose effect survives a replay of all of them in order: a
/// writer whose bits are all defined (or forgotten) again by later ones is
/// dropped (`define`/`forget` overwrite `b` and `m` on their whole mask).
pub fn reduce_flags(all: &[FlagWriter]) -> Vec<FlagWriter> {
    let mut covered = [0u32; 2];
    let mut keep = Vec::new();
    for w in all.iter().rev() {
        let m = flag_mask(w.kind);
        let c = &mut covered[w.pe as usize & 1];
        if m & !*c != 0 {
            keep.push(*w);
        }
        *c |= m;
    }
    keep.reverse();
    keep
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
        FlagKind::Fmul => {
            // MV, MU and MI are clear: a product that is not finite normal
            // or zero (from a zero operand) leaves the kernel.
            let r = src[0];
            let bits = if r >> 31 != 0 { MN } else { 0 };
            define(v, MULT_MASK, bits)
        }
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
            // AZ and AN only; the CACC shift register is `apply_cacc`'s.
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
        FlagKind::Iabs => {
            let neg = src[0] >> 31 != 0;
            let mut bits = arith_bits(0, src[0], neg);
            if neg {
                bits |= AS;
            }
            define(v, ALU_MASK, bits)
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
pub(super) const NW_PLAIN: [(i64, i64); 5] = [
    (0xE_8000, 0x400_0000),
    (0, 0x3_0000),
    (0x3_2000, 0x9_0000),
    (0x1800_0000, 0x3000_0000),
    (0x4000_0000, 1 << 32),
];

fn nw_plain(l: i64, h: i64) -> bool {
    NW_PLAIN.iter().any(|&(lo, hi)| l >= lo && h < hi)
}

/// The `NwPlain` requirements of one (base, stride) group as the hull of their
/// ranges: when the hull lies in one plain range, every member does.
#[derive(Clone, Copy, Debug)]
pub struct NwHull {
    pub base: Base,
    pub lo: i64,
    pub hi: i64,
    pub ts: i64,
    pub nf: i64,
}

pub fn nw_hulls(reqs: &[Req]) -> Vec<NwHull> {
    let mut out: Vec<NwHull> = Vec::new();
    for q in reqs {
        if let Req::NwPlain {
            base,
            lo,
            hi,
            ts,
            nf,
        } = *q
        {
            match out
                .iter_mut()
                .find(|h| h.base == base && h.ts == ts && h.nf == nf)
            {
                Some(h) => {
                    h.lo = h.lo.min(lo);
                    h.hi = h.hi.max(hi);
                }
                None => out.push(NwHull {
                    base,
                    lo,
                    hi,
                    ts,
                    nf,
                }),
            }
        }
    }
    out
}

/// A register requirement as one masked compare of the register's 8 bytes
/// (`V { b, m }` read as one word): `word & mask == want`. Known, Eq, MODE1
/// and ASTATX-known requirements are all of this kind.
#[derive(Clone, Copy, Debug)]
pub struct RegChk {
    /// Byte offset of the register in the engine state.
    pub off: u32,
    pub mask: u64,
    pub want: u64,
}

const _: () = {
    assert!(cfg!(target_endian = "little"));
    assert!(std::mem::size_of::<V>() == 8);
};

/// The word a register with value B and known-mask M reads as (the
/// two 32-bit fields of `V` in memory order, little endian).
pub(super) const fn v_word(b: u32, m: u32) -> u64 {
    if std::mem::offset_of!(crate::rt::V, b) == 0 {
        (m as u64) << 32 | b as u64
    } else {
        (b as u64) << 32 | m as u64
    }
}

/// The register requirements of REQS (`Known`, `Eq`, `Mode1`, `FlagsKnown`)
/// as masked compares, without duplicates.
pub fn reg_checks(reqs: &[Req]) -> Vec<RegChk> {
    let all = u32::MAX;
    let off = |c: usize| (std::mem::offset_of!(St, r) + std::mem::size_of::<V>() * c) as u32;
    let mut out: Vec<RegChk> = Vec::new();
    for q in reqs {
        let chk = match *q {
            Req::Known(c) => RegChk {
                off: off(c as usize),
                mask: v_word(0, all),
                want: v_word(0, all),
            },
            Req::Eq(c, k) => RegChk {
                off: off(c as usize),
                mask: v_word(all, all),
                want: v_word(k, all),
            },
            Req::Mode1 { mask, value } => RegChk {
                off: off(MODE1),
                mask: v_word(mask, all),
                want: v_word(value, all),
            },
            Req::FlagsKnown(mask) => RegChk {
                off: off(118),
                mask: v_word(0, mask),
                want: v_word(0, mask),
            },
            // CaccUnknown is checked on the engine state by `astat_ok`, not a masked compare.
            Req::NwPlain { .. } | Req::Within { .. } | Req::BankSync | Req::CaccUnknown => continue,
        };
        if !out
            .iter()
            .any(|o| o.off == chk.off && o.mask == chk.mask && o.want == chk.want)
        {
            out.push(chk);
        }
    }
    out
}

/// `Req::BankSync`: the applied bank selection is MODE1's.
#[inline]
pub fn bank_synced(s: &St) -> bool {
    let m = s.r[MODE1];
    m.is_c() && s.bank_active_mask == (m.b & 0x4f8) as Int
}

/// Is every circular index register of R inside its buffer?
#[inline]
fn within_ok(s: &St, r: &Region) -> bool {
    r.within.iter().all(|&(c, lo, hi)| {
        let v = s.r[c as usize];
        v.is_c() && (lo..=hi).contains(&v.b)
    })
}

/// `Req::CaccUnknown`: a kernel made for unknown CACC bits runs only while
/// they are not all known (else the generic core would shift the history).
#[inline(always)]
fn cacc_ok(s: &St, r: &Region) -> bool {
    !r.cacc_unknown || s.r[118].m & 0xff00_0000 != 0xff00_0000
}

/// Do all of R's register requirements hold in S?
#[inline]
fn regs_ok(s: &St, chk: &[RegChk]) -> bool {
    let mut bad = 0u64;
    let base = std::ptr::from_ref(s).cast::<u8>();
    for c in chk {
        // SAFETY: `off` is the offset of a register of `St::r` (built by
        // `reg_checks`); V is 8 bytes of two u32 fields.
        let w = unsafe { base.add(c.off as usize).cast::<u64>().read_unaligned() };
        bad |= w & c.mask ^ c.want;
    }
    bad == 0
}

/// A resolved memory window (`Mem::window`) kept between calls: the host
/// pointer of `[lo, hi)` is good while `Mem::layout_gen` is the same (no page
/// was allocated or dropped and no present/dirty bit was cleared; writes only
/// set bits), so a call whose window is the same range skips the resolution.
#[derive(Clone, Copy)]
pub struct WinCache {
    pub(super) gen_: u64,
    pub(super) lo: u32,
    pub(super) hi: u64,
    pub(super) ptr: *mut u8,
}

impl WinCache {
    pub const NONE: WinCache = WinCache {
        gen_: 0,
        lo: 0,
        hi: 0,
        ptr: std::ptr::null_mut(),
    };
}

/// The host pointer of window SPEC over `[lo, hi)`, from the cache when the
/// memory layout is unchanged since it was resolved, else from `Mem::window`
/// (and cached).
#[inline]
fn window_ptr(
    mem: &mut crate::mem::Mem,
    cache: &std::cell::Cell<WinCache>,
    spec: &super::WinSpec,
    lo: u32,
    hi: u64,
) -> Option<*mut u8> {
    let c = cache.get();
    if c.gen_ == mem.layout_gen && c.lo == lo && c.hi == hi && !c.ptr.is_null() {
        if spec.write {
            mem.note_window_write(lo);
        }
        return Some(c.ptr);
    }
    let ptr = mem.window(lo, (hi - lo as u64) as u32, spec.read, spec.write)?;
    cache.set(WinCache {
        gen_: mem.layout_gen,
        lo,
        hi,
        ptr,
    });
    Some(ptr)
}

/// Bytes after the anchor a cached span must still hold to be reused by a
/// window with a store, and the (bytes before, bytes from the anchor) spans
/// tried for it, larger first (see `dynamic_window_ptr`).
const DYN_ROOM: u64 = 16;
const DYN_LADDER: [(u32, u32); 8] = [
    (256, 1024),
    (64, 256),
    (16, 64),
    (0, 64),
    (0, 32),
    (0, 16),
    (0, 8),
    (0, 4),
];

/// Resolve a guarded window (`Lower::dynamic_window`: loads, or loads and
/// stores) from an independently known dynamic anchor.
/// The fast memory page may be the short-word alias of the architectural
/// page.  We try the largest aligned power-of-two span containing the anchor
/// that is wholly normal-word plain and present; the kernel guards every
/// actual load against the selected span.  A cached selected span is reused
/// while the memory layout is unchanged, avoiding repeated failed wider
/// probes on ordinary calls.
fn dynamic_window_ptr(
    s: &mut St,
    cache: &std::cell::Cell<WinCache>,
    spec: &super::WinSpec,
    anchor: u8,
) -> Option<(*mut u8, u32, u64)> {
    let v = s.r[anchor as usize];
    if !v.is_c() {
        return None;
    }
    let a = v.b;
    let cached = cache.get();
    // A window with a store anchors on the index register's own words (the
    // modifiers reach forward from it), see below: a cached span is reused
    // only with that room after the anchor.
    let room = if spec.write { DYN_ROOM } else { 1 };
    if cached.gen_ == s.mem.layout_gen
        && !cached.ptr.is_null()
        && a >= cached.lo
        && (a as u64) + room <= cached.hi
    {
        if spec.write {
            // As `window_ptr` on a hit: the write counts for the code watch.
            s.mem.note_window_write(cached.lo);
        }
        return Some((cached.ptr, cached.lo, cached.hi));
    }
    if spec.write {
        // A store needs every byte of its span overlay-written (dirty), which
        // the bytes around an aligned power-of-two span rarely are: try spans
        // that reach forward from the anchor (and a little back), larger
        // first, inside the anchor's page.
        let page_lo = a & !(crate::mem::PAGE_SIZE as u32 - 1);
        let page_hi = page_lo as u64 + crate::mem::PAGE_SIZE as u64;
        for (back, fwd) in DYN_LADDER {
            let lo = a.saturating_sub(back).max(page_lo);
            let hi = (a as u64 + fwd as u64).min(page_hi);
            if hi < a as u64 + 4 || !nw_plain(lo as i64, hi as i64 - 1) {
                continue;
            }
            if let Some(ptr) = window_ptr(&mut s.mem, cache, spec, lo, hi) {
                return Some((ptr, lo, hi));
            }
        }
    }
    let mut len = crate::mem::PAGE_SIZE as u32;
    loop {
        let lo = a & !(len - 1);
        let hi = lo as u64 + len as u64;
        if nw_plain(lo as i64, hi as i64 - 1)
            && let Some(ptr) = window_ptr(&mut s.mem, cache, spec, lo, hi)
        {
            return Some((ptr, lo, hi));
        }
        if len == 4 {
            return None;
        }
        len /= 2;
    }
}

/// A guarded dynamic window that could not be resolved (the anchor is not
/// plain, present or written memory): an empty span, so every access
/// through it fails its guard and leaves before the instruction. The kernel
/// still runs up to there, instead of the whole call being declined.
fn empty_dyn_window(ctx: &mut Ctx, w: usize) {
    ctx.set64(win_ptr(w as u32), 0);
    note_window(ctx, w, 0, 0);
    ctx.set32(CTX_WIN_DYN_LEN + 4 * w as u32, 0);
}

/// Resolve either a statically proven window or a guarded read window.  A
/// dynamic window has no whole-loop address proof: it is safe because every
/// load carries its own unsigned bounds guard before the instruction commits.
fn resolve_window(
    s: &mut St,
    cache: &std::cell::Cell<WinCache>,
    spec: &super::WinSpec,
    iters: i64,
) -> Option<(*mut u8, u32, u64)> {
    if let Some(anchor) = spec.dynamic_anchor {
        return dynamic_window_ptr(s, cache, spec, anchor);
    }
    let b = match spec.base {
        Base::None => 0,
        Base::Reg(c) => s.r[c as usize].b as i64,
    };
    let d = spec.ts * (iters - 1);
    let f = spec.nf * iters;
    let (lo, hi) = (b + spec.lo + d.min(0) + f, b + spec.hi + d.max(0) + f);
    if lo < 0 || hi > (1i64 << 32) {
        return None;
    }
    let lo = lo as u32;
    let hi = hi as u64;
    window_ptr(&mut s.mem, cache, spec, lo, hi).map(|ptr| (ptr, lo, hi))
}

/// Do all `nw` requirements of R hold for N iterations? The hulls are tried
/// first; when one is not inside a single plain range, each requirement is
/// evaluated on its own (the hull is only a sufficient test).
#[inline]
fn nw_ok(s: &St, r: &Region, n: i64) -> bool {
    let hulls = r.nw_hull.iter().all(|h| {
        let b = match h.base {
            Base::None => 0,
            Base::Reg(c) => s.r[c as usize].b as i64,
        };
        let d = h.ts * (n - 1);
        let f = h.nf * n;
        nw_plain(b + h.lo + d.min(0) + f, b + h.hi + d.max(0) + f)
    });
    hulls || r.nw.iter().all(|q| eval_req(s, q, n))
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
        Req::BankSync => !s.cfg.bank_model || bank_synced(s),
        Req::CaccUnknown => s.r[118].m & 0xff00_0000 != 0xff00_0000,
        Req::Within { c, lo, hi } => {
            let v = s.r[c as usize];
            v.is_c() && (lo..=hi).contains(&v.b)
        }
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
    if cfg.bank_model && r.bank_sync && !bank_synced(s) {
        return Err(Decline::Shape);
    }
    if cfg.stack_model && (s.pc_stack_pending >= 0 || s.pc_stack_requested >= 0) {
        return Err(Decline::Shape);
    }
    let mode1 = s.r[MODE1];
    if !mode1.is_c() || mode1.b & (3 << 22) != 0 {
        // Unknown MODE1, or a broadcast load (BDCST9/BDCST1: a second
        // register is loaded). SIMD is part of every region's MODE1
        // requirement (`Req::Mode1`, bit 21).
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
    if !regs_ok(s, &r.chk) || !cacc_ok(s, r) || !within_ok(s, r) {
        return Err(Decline::Req);
    }
    if !nw_ok(s, r, iters) {
        return Err(Decline::Req);
    }
    // Windows.
    let check = WIN_CHECK.load(std::sync::atomic::Ordering::Relaxed);
    super::dump::begin();
    for (w, spec) in r.wins.iter().enumerate() {
        let Some((ptr, lo, hi)) = resolve_window(s, &r.wcache[w], spec, iters) else {
            if spec.dynamic_anchor.is_some() {
                empty_dyn_window(ctx, w);
                continue;
            }
            return Err(Decline::Window);
        };
        ctx.set64(win_ptr(w as u32), (ptr as u64).wrapping_sub(lo as u64));
        super::dump::win(w as u32, ptr, lo, hi);
        if check || spec.dynamic_anchor.is_some() {
            note_window(ctx, w, lo, hi.min(u32::MAX as u64) as u32);
        }
        ctx.set32(
            CTX_WIN_DYN_LEN + 4 * w as u32,
            spec.dynamic_anchor
                .is_some()
                .then_some((hi - lo as u64) as u32)
                .unwrap_or(0),
        );
    }
    if check {
        for w in r.wins.len()..MAX_WIN as usize {
            ctx.set64(win_ptr(w as u32), 0);
            note_window(ctx, w, 0, 0);
        }
    }
    if check != ctx.win_check {
        ctx.set32(CTX_WIN_CHECK, check as u32);
        ctx.win_check = check;
    }
    ctx.set32(CTX_ITERS, iters as u32);
    set_flat(ctx, s);
    // SAFETY: static windows cover every access of `iters` iterations by the
    // lowering's range analysis. Guarded read windows are resolved here and
    // each dynamic address is checked before its load and instruction commit.
    // The kernel was lowered for this engine state layout.
    let cap = super::dump::before(&r.kernel, ctx.ptr(), std::ptr::from_mut(s).cast());
    let code = unsafe { kernel.run(ctx.ptr(), std::ptr::from_mut(s).cast()) };
    super::dump::after(cap, code, ctx.ptr(), std::ptr::from_mut(s).cast());
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
    apply_cacc(s, r, ctx);
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

/// The flag groups' last writers (kernels that track flags, `lower::cond`)
/// applied to ASTATX with the replay's own effect.
fn apply_groups(s: &mut St, r: &Region, ctx: &Ctx) {
    if !r.flag_groups.iter().any(|g| *g) {
        return;
    }
    let mut v = [s.r[118], s.r[119]];
    let mut any = [false; 2];
    for g in 0..NGROUPS {
        if !r.flag_groups[g as usize] {
            continue;
        }
        let id = ctx.get32(ctx_reg_out(pseudo_flag(g, 0)));
        // Groups 0..2 are PEx's, 3..5 PEy's (ASTATY).
        if let Some(kind) = FlagKind::from_group_id(g % 3, id) {
            let src = [
                ctx.get32(ctx_reg_out(pseudo_flag(g, 1))),
                ctx.get32(ctx_reg_out(pseudo_flag(g, 2))),
            ];
            let pe = (g / 3) as usize;
            v[pe] = apply_flag(v[pe], kind, src);
            any[pe] = true;
        }
    }
    if any[0] {
        s.r[118] = v[0];
    }
    if any[1] {
        s.r[119] = v[1];
    }
}

fn apply_flags(s: &mut St, r: &Region, ctx: &Ctx, full: bool, done: u64, kk: u64) {
    if r.flags.is_empty() {
        return;
    }
    // ASTATX (PEx) and ASTATY (PEy, SIMD).
    let mut v = [s.r[118], s.r[119]];
    let mut any = [false; 2];
    let one = |w: &super::FlagWriter, v: &mut [V; 2], any: &mut [bool; 2]| {
        let mut src = [0u32; 2];
        for (j, slot) in w.fsrc.iter().enumerate() {
            if *slot != 255 {
                src[j] = ctx.get32(CTX_FSRC + 4 * *slot as u32);
            }
        }
        let pe = w.pe as usize & 1;
        v[pe] = apply_flag(v[pe], w.kind, src);
        any[pe] = true;
    };
    if full {
        for w in &r.full_flags {
            one(w, &mut v, &mut any);
        }
    } else {
        if done > 0 {
            // Writers after the failing instruction last ran in the
            // previous iteration.
            for w in r.flags.iter().filter(|w| w.insn as u64 >= kk) {
                one(w, &mut v, &mut any);
            }
        }
        for w in r.flags.iter().filter(|w| (w.insn as u64) < kk) {
            one(w, &mut v, &mut any);
        }
    }
    if any[0] {
        s.r[118] = v[0];
    }
    if any[1] {
        s.r[119] = v[1];
    }
}

/// The CACC compare history (ASTATX bits 24-31) after the compares that ran:
/// the kernel shifts each result in (`Lower::compare_flag`) starting from the
/// entry value, and the region requires ASTATX to be fully known at entry, so
/// the value is exact.
fn apply_cacc(s: &mut St, r: &Region, ctx: &Ctx) {
    if !r.cacc {
        return;
    }
    if r.cacc_unknown {
        // The CACC bits were not all known at entry: the generic core's compare
        // defines the ALU bits (`apply_flag`) and forgets CACC. The kernel's
        // pseudo register starts as 1 and a compare clears that bit, so a
        // clear bit says a compare ran before this exit.
        if ctx.get32(ctx_reg_out(PSEUDO_CACC)) & 1 == 0 {
            let v = s.r[118];
            s.r[118] = V {
                b: v.b & !CACC,
                m: v.m & !CACC,
            };
        }
        return;
    }
    let h = ctx.get32(ctx_reg_out(PSEUDO_CACC)) & CACC;
    let v = s.r[118];
    s.r[118] = V {
        b: (v.b & !CACC) | h,
        m: v.m | CACC,
    };
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

    /// The bookkeeping of a taken call or return at the exit (the generic
    /// core's `_transfer`, `_advance`, `_take_return`).
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum Fx {
        None,
        /// Inside the delay slots of a call: the return address is pushed.
        CallPush(u32),
        /// The call is over: pushed, followed, at its target.
        CallDone(u32),
        /// An RTS: the PC stack entry (the target) is popped.
        Rts,
    }

    pub fn apply_fx(s: &mut St, fx: Fx) -> R<()> {
        match fx {
            Fx::None => {}
            Fx::CallPush(ret) | Fx::CallDone(ret) => {
                state::_push_pc_stack(s, 0x0100_0000 | ret as Int)?;
                if matches!(fx, Fx::CallDone(_)) {
                    s.call_stack.push_raw(ret as Int)?;
                    s.at_loaded_entry = true;
                }
            }
            Fx::Rts => {
                let target = match s.pc_stack.items().last() {
                    Some(&t) => t & 0xff_ffff,
                    None => return Err(TRAP_INDEX),
                };
                sequencer::_pop_pc_stack(s)?;
                match s.call_stack.items().last() {
                    Some(&c) if c & 0xff_ffff == target => s.call_stack.n -= 1,
                    _ => s.call_stack.clear(),
                }
            }
        }
        Ok(())
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
            // The stack empties only when no slot reserved by PUSH LOOP lies
            // below the loops (the stack model's depth is then not zero).
            2 => Some(!(s.cfg.stack_model && s.loop_depth > 0)),
            _ => None,
        };
        set_bit26(s, ev, entry_stkyx);
        let stk26 = ctx.get32(CTX_STK26);
        if (stk26 == 1 || (stk26 == 2 && s.cfg.stack_model))
            && entry_stkyx.m & BIT26 == 0
            && s.r[STKYX].m & BIT26 != 0
        {
            // A DO alone (no loop end after it) clears bit 26 where it was
            // known, and leaves it unknown where it was not (the generic
            // core's `_push` keeps the known mask). The stack model's loop
            // end (`_pop_loop_resource`) keeps the mask too.
            let cur = s.r[STKYX];
            let b = if stk26 == 2 {
                (cur.b & !BIT26) | (entry_stkyx.b & BIT26)
            } else {
                cur.b
            };
            s.r[STKYX] = V { b, m: cur.m & !BIT26 };
        }
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
/// The engine memory's page table and code-watch bytes, for flat accesses.
#[inline(always)]
fn set_flat(ctx: &mut Ctx, s: &St) {
    let (eff, watch) = s.mem.flat_tables();
    ctx.set64(CTX_FLAT_EFF, eff as u64);
    ctx.set64(CTX_FLAT_WATCH, watch as u64);
}

fn note_window(ctx: &mut Ctx, w: usize, lo: u32, hi: u32) {
    ctx.set32(CTX_WIN_EXT + 8 * w as u32, lo);
    ctx.set32(CTX_WIN_EXT + 8 * w as u32 + 4, hi);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mem::Mem;

    #[test]
    fn ddr_monitor_invalidates_cached_static_and_dynamic_windows() {
        let mut s = St::new(Mem::new());
        let a = 0x8001_0000;
        for k in 0..64 {
            s.mem.write_byte(a + k, 0);
        }
        s.r[18] = V::c(a as Int);
        let mut spec = super::super::WinSpec {
            base: Base::None,
            lo: 0,
            hi: 0,
            ts: 0,
            nf: 0,
            read: true,
            write: true,
            dynamic_anchor: None,
        };
        let cache = std::cell::Cell::new(WinCache::NONE);
        assert!(window_ptr(&mut s.mem, &cache, &spec, a, a as u64 + 4).is_some());
        s.mem.set_ddr_monitor(Some((a, a + 4))).unwrap();
        assert!(window_ptr(&mut s.mem, &cache, &spec, a, a as u64 + 4).is_none());
        assert_eq!(s.mem.first_ddr_access(), None);
        s.mem.set_ddr_monitor(None).unwrap();
        spec.dynamic_anchor = Some(18);
        assert!(dynamic_window_ptr(&mut s, &cache, &spec, 18).is_some());
        s.mem.set_ddr_monitor(Some((a, a + 4))).unwrap();
        assert!(dynamic_window_ptr(&mut s, &cache, &spec, 18).is_none());
        assert_eq!(s.mem.first_ddr_access(), None);
    }

    #[test]
    fn dynamic_window_uses_alias_subspan_and_invalidates_cache() {
        let mut s = St::new(Mem::new());
        s.r[18] = V::c(0x24_ebb0);
        // Populate only the high 32 KiB of the short-word alias page.  The
        // 64 KiB probe must fail, while the aligned 32 KiB probe succeeds.
        for a in 0x2824_8000..0x2825_0000 {
            s.mem.write_byte(a, 0);
        }
        let spec = super::super::WinSpec {
            base: Base::None,
            lo: 0,
            hi: 0,
            ts: 0,
            nf: 0,
            read: true,
            write: false,
            dynamic_anchor: Some(18),
        };
        let cache = std::cell::Cell::new(WinCache::NONE);
        let (_, lo, hi) = dynamic_window_ptr(&mut s, &cache, &spec, 18).unwrap();
        assert_eq!((lo, hi), (0x24_8000, 0x25_0000));
        // A repeated entry call uses the selected span without retrying the
        // failed page-wide probe; reset changes layout and removes it.
        assert!(dynamic_window_ptr(&mut s, &cache, &spec, 18).is_some());
        s.mem.reset();
        assert!(dynamic_window_ptr(&mut s, &cache, &spec, 18).is_none());
    }
}

static SHAPE_LOG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `SHARC_FAST_LOG`: say why a call was declined as the wrong shape.
pub fn set_shape_log(on: bool) {
    SHAPE_LOG.store(on, std::sync::atomic::Ordering::Relaxed);
}

static COV_ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

thread_local! {
    static COV_WHY: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

/// `SHARC_FAST_COV`: remember why the last call was declined.
/// Coverage reporting is on (`SHARC_FAST_COV`).
pub fn cov_on() -> bool {
    COV_ON.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn set_cov(on: bool) {
    COV_ON.store(on, std::sync::atomic::Ordering::Relaxed);
}

#[cold]
fn cov_why(f: impl FnOnce() -> String) {
    if COV_ON.load(std::sync::atomic::Ordering::Relaxed) {
        COV_WHY.with(|w| *w.borrow_mut() = f());
    }
}

/// The reason noted by the last declined call (`SHARC_FAST_COV` only).
pub fn take_cov_why() -> String {
    COV_WHY.with(|w| std::mem::take(&mut *w.borrow_mut()))
}

#[cold]
fn shape(why: &str) -> Decline {
    if SHAPE_LOG.load(std::sync::atomic::Ordering::Relaxed) {
        eprintln!("fast: declined, shape: {why}");
    }
    cov_why(|| format!("shape: {why}"));
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
    if cfg.bank_model && r.bank_sync && !bank_synced(s) {
        return Err(shape("bank selection differs from MODE1"));
    }
    if cfg.stack_model && (s.pc_stack_pending >= 0 || s.pc_stack_requested >= 0) {
        return Err(shape("PC stack change in flight"));
    }
    let mode1 = s.r[MODE1];
    if !mode1.is_c() || mode1.b & (3 << 22) != 0 {
        // Unknown MODE1 or a broadcast load (BDCST9/BDCST1). SIMD is part of
        // every region's MODE1 requirement (`Req::Mode1`, bit 21).
        return Err(shape("MODE1 unknown or a broadcast load"));
    }
    // The loops active at entry that the region models.
    let n = meta.n_entry;
    let nloops = s.loops.n;
    if nloops < n {
        return Err(shape("fewer loops than the region models"));
    }
    // Stack model: slot k of the loop stack belongs to loops[k - slot_base]
    // (slots reserved by PUSH LOOP lie below the DO loops).
    let cfg_stack = cfg.stack_model;
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
    if meta.has_rts {
        // A return takes its target from the PC stack (`_take_return`): the
        // top entry. No loop of the region ends before the return (an entry
        // loop would pop its own entry first: `cfg` stops a return inside a
        // loop, and the region models no entry loop).
        let top = pc_stack_items(s).last().copied();
        match top {
            Some(t) if n == 0 && t != 0xFFFF_FFFF => {
                ctx.set32(CTX_DYNPC, (t & 0xff_ffff) as u32);
            }
            _ => {
                if COV_ON.load(std::sync::atomic::Ordering::Relaxed) {
                    cov_why(|| format!("shape: return without a PC stack entry (loops {nloops} depth {} top {top:x?})", s.loop_depth));
                }
                return Err(Decline::Shape);
            }
        }
    }
    if meta.has_call && (s.call_stack.len() as Int >= s.cfg.max_call_depth || s.call_stack.len() + 2 >= MAX_CALLS)
    {
        return Err(shape("call stack too deep"));
    }
    let iters = match meta.reg_n {
        // The region's symbolic trip count is `R[c] + k` (a register-count
        // loop with no entry loop): windows and requirements are checked
        // for that many iterations.
        Some((c, k)) => {
            let v = s.r[c as usize];
            let n = v.b as i64 + k;
            if !v.is_c() || !(1..=u32::MAX as i64).contains(&n) {
                return Err(shape("register loop count unknown or out of range"));
            }
            n
        }
        None => rem_top.max(1),
    };
    let check = WIN_CHECK.load(std::sync::atomic::Ordering::Relaxed);
    if !regs_ok(s, &r.chk) || !cacc_ok(s, r) || !within_ok(s, r) || !nw_ok(s, r, iters) {
        return Err(Decline::Req);
    }
    let avail = s.limit.saturating_sub(s.icount);
    if avail == 0 {
        return Err(Decline::Budget);
    }
    super::dump::begin();
    for (w, spec) in r.wins.iter().enumerate() {
        let Some((ptr, lo, hi)) = resolve_window(s, &r.wcache[w], spec, iters) else {
            if spec.dynamic_anchor.is_some() {
                empty_dyn_window(ctx, w);
                continue;
            }
            cov_why(|| {
                let dynamic = spec.dynamic_anchor.is_some();
                let rw = if spec.write { "write" } else { "read" };
                format!("window {w}: not plain/present RAM (dynamic={dynamic} {rw})")
            });
            return Err(Decline::Window);
        };
        if let Some(a) = spec.dynamic_anchor
            && SHAPE_LOG.load(std::sync::atomic::Ordering::Relaxed)
        {
            eprintln!(
                "fast: dynamic window {w}: anchor r{a} = {:#x}: {lo:#x}..{hi:#x}",
                s.r[a as usize].b
            );
        }
        ctx.set64(win_ptr(w as u32), (ptr as u64).wrapping_sub(lo as u64));
        super::dump::win(w as u32, ptr, lo, hi);
        if check || spec.dynamic_anchor.is_some() {
            note_window(ctx, w, lo, hi.min(u32::MAX as u64) as u32);
        }
        ctx.set32(
            CTX_WIN_DYN_LEN + 4 * w as u32,
            spec.dynamic_anchor
                .is_some()
                .then_some((hi - lo as u64) as u32)
                .unwrap_or(0),
        );
    }
    if check {
        for w in r.wins.len()..MAX_WIN as usize {
            ctx.set64(win_ptr(w as u32), 0);
            note_window(ctx, w, 0, 0);
        }
    }
    if check != ctx.win_check {
        ctx.set32(CTX_WIN_CHECK, check as u32);
        ctx.win_check = check;
    }
    ctx.set32(CTX_BUDGET, avail.min(1 << 31) as u32);
    set_flat(ctx, s);
    // SAFETY: static windows cover every access by the lowering's range
    // analysis. Guarded read windows are resolved here and each dynamic
    // address is checked before its load and instruction commit. The kernel
    // was lowered for this engine state layout.
    let cap = super::dump::before(&r.kernel, ctx.ptr(), std::ptr::from_mut(s).cast());
    let code = unsafe { kernel.run(ctx.ptr(), std::ptr::from_mut(s).cast()) };
    super::dump::after(cap, code, ctx.ptr(), std::ptr::from_mut(s).cast());
    if code == RUN_ENTRY_FAILED {
        // The kernel's own entry checks failed before it did anything.
        return Err(Decline::Req);
    }
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
    apply_cacc(s, r, ctx);
    ctx.ret_at = site.ret;
    s.pc_sw = if site.pc == DYN_PC {
        // The target of the return the kernel ran (24 bits).
        ctx.get32(CTX_DYNPC) as Int
    } else {
        site.pc as Int
    };
    s.icount += completed;
    s.steps += completed as Int;
    // What `exec_insn` does at the start of every instruction.
    s.at_loaded_entry = false;
    if s.cfg.core_timer {
        s.timer_written = false;
    }
    // The call or return this exit completes (or, inside its delay slots,
    // has begun): the glue does what the interpreter does at the branch.
    let mut fx = match site.tail {
        Tail::None => sites::Fx::None,
        Tail::Call(ret) => sites::Fx::CallDone(ret),
        Tail::Rts => sites::Fx::Rts,
    };
    if let Some((j, slots)) = site.pending
        && ctx.get32(CTX_TAKEN + 4 * j as u32) != 0
    {
        let target = match meta.jumps[j as usize].0 {
            DYN_PC => ctx.get32(CTX_DYNPC),
            t => t,
        };
        let tail = meta.jump_tail[j as usize];
        s.pending = Some(Pending {
            target: Some(target as Int),
            call: matches!(tail, Tail::Call(_)),
            slots: slots as Int,
            return_from_call: tail == Tail::Rts,
            return_sw: match tail {
                Tail::Call(ret) => Some(ret as Int),
                _ => None,
            },
        });
        fx = match tail {
            Tail::None => sites::Fx::None,
            Tail::Call(ret) => sites::Fx::CallPush(ret),
            Tail::Rts => sites::Fx::Rts,
        };
    }
    // The common exit: no loop started, ended or was entered by the kernel, and
    // the loops active are the entry loops: only their counts (and, in the stack
    // model, the registers that mirror the top slot) change.
    // A slot the kernel does not store holds what an earlier call left there.
    let didpc = if meta.st_didpc {
        ctx.get32(CTX_DIDPC)
    } else {
        0
    };
    let lset = if meta.st_lcntr {
        ctx.get32(CTX_LCNTR + 4)
    } else {
        0
    };
    let stk26 = if meta.st_stk26 {
        ctx.get32(CTX_STK26)
    } else {
        0
    };
    if site.active.len() == n && didpc == 0 && lset == 0 && stk26 == 0 && fx == sites::Fx::None {
        let base = s.loops.n - n;
        for i in 0..n {
            let rem = ctx.get32(CTX_LOOP_REM_OUT + 4 * i as u32);
            s.loops.a[base + i].remaining = rem as i64;
            if cfg_stack {
                s.loop_slots.a[slot_base + base + i].1 = V::c(rem as Int);
            }
        }
        if !cfg_stack {
            if n > 0 {
                let rem = ctx.get32(CTX_LOOP_REM_OUT + 4 * (n - 1) as u32);
                s.r[103] = V::c(rem as Int);
            }
        } else if s.loop_depth != 0 {
            let slot = s.loop_slots.a[s.loop_depth as usize - 1];
            s.r[102] = slot.0;
            s.r[103] = slot.1;
        } else {
            s.r[102] = V::c(0xFFFF_FFFFu32 as Int);
            s.r[103] = V::c(0xFFFF_FFFFu32 as Int);
        }
        return Ok(crate::EXIT_NEXT);
    }
    // The slots the kernel did not store read as zero.
    if !meta.st_didpc {
        ctx.set32(CTX_DIDPC, 0);
    }
    if !meta.st_lcntr {
        ctx.set32(CTX_LCNTR, 0);
        ctx.set32(CTX_LCNTR + 4, 0);
    }
    if !meta.st_stk26 {
        ctx.set32(CTX_STK26, 0);
    }
    for d in 0..MAX_SLOTS {
        if meta.st_slots >> d & 1 == 0 {
            ctx.set32(CTX_SLOT_LAST + 4 * d, 0);
        }
    }
    s.begin();
    let res = sites::apply(s, meta, site, ctx).and_then(|()| sites::apply_fx(s, fx));
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


