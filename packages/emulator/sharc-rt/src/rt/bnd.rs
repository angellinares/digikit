//! The boundary of tools/sharc_dev/sharc_core, hand-written (SUBSET.md section 2).
//!
//! One function per Python function, same name, same parameters (after
//! the state `s`), concrete specialisation only: `Const` is a known
//! [`V`], `Unknown` an unknown one, an `Affine` never occurs natively (the
//! concrete driver never builds one), and the observability strings
//! (`expression`, reasons) are ignored.

use super::*;

// ---------------------------------------------------------------------------
// values.py
// ---------------------------------------------------------------------------

/// values._signed
#[inline(always)]
pub fn _signed(_s: &St, value: Int, bits: Int) -> Int {
    if bits <= 0 || bits > 126 {
        return value;
    }
    if value & (1 << (bits - 1)) != 0 {
        value - (1 << bits)
    } else {
        value
    }
}

/// values._signed32
#[inline(always)]
pub fn _signed32(_s: &St, value: Int) -> Int {
    (value as u32 as i32) as Int
}

/// values._add
#[inline(always)]
pub fn _add(_s: &St, left: V, right: V, _expression: Sym) -> V {
    if left.is_c() && right.is_c() {
        V::c(left.val() + right.val())
    } else {
        V::UNK
    }
}

/// values._negate
#[inline(always)]
pub fn _negate(_s: &St, value: V, _expression: Sym) -> V {
    if value.is_c() {
        V::c(-value.val())
    } else {
        V::UNK
    }
}

/// values._subtract
#[inline(always)]
pub fn _subtract(s: &St, left: V, right: V, expression: Sym, same_source: bool) -> V {
    if same_source {
        return V::c(0);
    }
    _add(s, left, _negate(s, right, expression), expression)
}

/// values._multiply
#[inline(always)]
pub fn _multiply(_s: &St, left: V, right: V, _expression: Sym) -> V {
    if left.is_c() && right.is_c() {
        V::c(left.val() * right.val())
    } else {
        V::UNK
    }
}

/// values._multiply_fractional
#[inline(always)]
pub fn _multiply_fractional(
    s: &St,
    left: V,
    right: V,
    signed_x: bool,
    signed_y: bool,
    _expression: Sym,
) -> V {
    if !(left.is_c() && right.is_c()) {
        return V::UNK;
    }
    let x = if signed_x {
        _signed32(s, left.val())
    } else {
        left.val()
    };
    let y = if signed_y {
        _signed32(s, right.val())
    } else {
        right.val()
    };
    let shift = if signed_x && signed_y { 31 } else { 32 };
    V::c((x * y) >> shift)
}

/// values._bitwise with one of the named _op_* operations.
#[inline(always)]
pub fn _bitwise(_s: &St, left: V, right: V, _expression: Sym, operation: FnId) -> V {
    if !(left.is_c() && right.is_c()) {
        return V::UNK;
    }
    let (a, b) = (left.val(), right.val());
    V::c(match operation {
        FN_OP_AND => a & b,
        FN_OP_OR => a | b,
        FN_OP_XOR => a ^ b,
        FN_OP_ANDNOT => a & !b,
        _ => unreachable!("_bitwise operation {operation}"),
    })
}

/// values._not
#[inline(always)]
pub fn _not(_s: &St, value: V, _expression: Sym) -> V {
    if value.is_c() {
        V::c(!value.val())
    } else {
        V::UNK
    }
}

/// values._is_unknown: Unknown or PartialConst.
#[inline(always)]
pub fn _is_unknown(_s: &St, value: V) -> bool {
    !value.is_c()
}

/// addressing.normal_word_to_byte
#[inline(always)]
pub fn normal_word_to_byte(_s: &St, address: Int) -> Option<Int> {
    crate::addressing::normal_word_to_byte(address)
}

/// values._astatx_known_bit
#[inline(always)]
pub fn _astatx_known_bit(_s: &St, value: V, bit: Int) -> Option<bool> {
    if !(0..32).contains(&bit) {
        // Python: 1 << bit beyond 32 bits is never set in a 32-bit value.
        return if value.is_c() { Some(false) } else { None };
    }
    let m = 1u32 << bit;
    if value.m & m != 0 {
        Some(value.b & m != 0)
    } else {
        None
    }
}

/// values._astatx_define
#[inline(always)]
pub fn _astatx_define(_s: &St, old: V, mask: Int, bits: Int) -> V {
    let mask = mask as u32;
    let bits = bits as u32 & mask;
    let new_mask = old.m | mask;
    if new_mask == 0 {
        return old;
    }
    V {
        m: new_mask,
        b: ((old.b & !mask) | bits) & new_mask,
    }
}

/// values._astatx_forget
#[inline(always)]
pub fn _astatx_forget(_s: &St, old: V, mask: Int) -> V {
    let mask = mask as u32;
    let new_mask = if old.is_c() || old.is_partial() {
        old.m & !mask
    } else {
        return old;
    };
    V {
        b: old.b & new_mask,
        m: new_mask,
    }
}

const CACC_MASK: u32 = 0xFF00_0000;
const COMPARE_PRESERVE: u32 = 0x00FF_FFC0 & !(1 << 10);

/// values._apply_flag_update
#[inline(always)]
pub fn _apply_flag_update(s: &St, old: V, update: FlagUpdate) -> V {
    if update.cacc >= 0 && old.is_c() {
        let value = old.b;
        let shifted = (value & COMPARE_PRESERVE)
            | ((value >> 1) & 0x7F00_0000)
            | ((update.cacc as u32) << 31);
        let result =
            V::c(((shifted & !(update.define_mask as u32)) | update.define_bits as u32) as Int);
        let forget = update.forget_mask & !(CACC_MASK as Int);
        return if forget != 0 {
            _astatx_forget(s, result, forget)
        } else {
            result
        };
    }
    let mut result = old;
    if update.define_mask != 0 {
        result = _astatx_define(s, result, update.define_mask, update.define_bits);
    }
    if update.forget_mask != 0 {
        result = _astatx_forget(s, result, update.forget_mask);
    }
    result
}

/// values._flags_define
#[inline(always)]
pub fn _flags_define(_s: &St, mask: Int, bits: Int) -> FlagUpdate {
    FlagUpdate {
        define_mask: mask,
        define_bits: bits & mask,
        forget_mask: 0,
        cacc: -1,
    }
}

/// values._flags_forget
#[inline(always)]
pub fn _flags_forget(_s: &St, mask: Int) -> FlagUpdate {
    FlagUpdate {
        define_mask: 0,
        define_bits: 0,
        forget_mask: mask,
        cacc: -1,
    }
}

/// values._flags_put
#[inline(always)]
pub fn _flags_put(_s: &St, update: FlagUpdate, bit: Int, known: Option<bool>) -> FlagUpdate {
    let m: Int = 1 << bit;
    match known {
        None => FlagUpdate {
            define_mask: update.define_mask & !m,
            define_bits: update.define_bits & !m,
            forget_mask: update.forget_mask | m,
            cacc: update.cacc,
        },
        Some(k) => FlagUpdate {
            define_mask: update.define_mask | m,
            define_bits: (update.define_bits & !m) | if k { m } else { 0 },
            forget_mask: update.forget_mask & !m,
            cacc: update.cacc,
        },
    }
}

pub const FLAGS_NONE: FlagUpdate = FlagUpdate {
    define_mask: 0,
    define_bits: 0,
    forget_mask: 0,
    cacc: -1,
};

/// values._flags_from_pairs
#[inline(always)]
pub fn _flags_from_pairs(s: &St, pairs: Tup<(Int, Option<bool>)>) -> FlagUpdate {
    let mut update = FLAGS_NONE;
    for i in 0..pairs.len() {
        let (bit, known) = pairs.get(i);
        update = _flags_put(s, update, bit, known);
    }
    update
}

/// values._flags_then
#[inline(always)]
pub fn _flags_then(_s: &St, first: FlagUpdate, second: FlagUpdate) -> FlagUpdate {
    let forget = (first.forget_mask & !second.define_mask) | second.forget_mask;
    let define = (second.define_mask | (first.define_mask & !first.forget_mask)) & !forget;
    let bits = (second.define_bits | (first.define_bits & !second.define_mask)) & define;
    FlagUpdate {
        define_mask: define,
        define_bits: bits,
        forget_mask: forget,
        cacc: first.cacc,
    }
}

/// values._flags_or
#[inline(always)]
pub fn _flags_or(_s: &St, a: FlagUpdate, b: FlagUpdate) -> FlagUpdate {
    let t = a.define_bits | b.define_bits;
    let unknown = (a.forget_mask | b.forget_mask) & !t;
    let f = (a.define_mask | b.define_mask) & !t & !unknown;
    FlagUpdate {
        define_mask: t | f,
        define_bits: t,
        forget_mask: unknown,
        cacc: -1,
    }
}

/// values._stack_bounded_symbol: needs an Affine value, so never natively.
#[inline(always)]
pub fn _stack_bounded_symbol(_s: &St, _value: V) -> Option<(Sym, Int)> {
    None
}

/// values._aconv: a Const shifts; anything else is Unknown (the symbolic
/// branches need an Affine).
#[inline(always)]
pub fn _aconv(_s: &St, value: V, w2b: bool, _source_code: Int, _pc_sw: Int) -> R<V> {
    if value.is_c() {
        let mapped = if w2b {
            crate::addressing::normal_word_to_architectural_byte(value.val())
        } else {
            crate::addressing::byte_to_normal_word(value.val())
        };
        if let Some(mapped) = mapped {
            return Ok(V::c(mapped));
        }
        let already_in_space = if w2b {
            crate::addressing::byte_to_normal_word(value.val())
        } else {
            crate::addressing::normal_word_to_architectural_byte(value.val())
        };
        if already_in_space.is_some() {
            return Ok(value);
        }
        return Ok(V::c(if w2b {
            value.val() << 2
        } else {
            value.val() >> 2
        }));
    }
    Ok(V::UNK)
}

// ---------------------------------------------------------------------------
// state.py
// ---------------------------------------------------------------------------

/// state._ureg_raw
#[inline(always)]
pub fn _ureg_raw(s: &St, values: RegView, code: Int) -> V {
    rv_get(s, values, code)
}

/// state._ureg: a PartialConst reads as Const when fully known (never, by
/// construction) and Unknown otherwise.
#[inline(always)]
pub fn _ureg(s: &St, values: RegView, code: Int) -> V {
    let v = rv_get(s, values, code);
    if v.is_partial() { V::UNK } else { v }
}

/// state._ureg_raw over block code's register file.
#[inline(always)]
pub fn _ureg_raw_rf(rf: &Rf, values: RegView, code: Int) -> V {
    rf_get(rf, values, code)
}

/// state._ureg over block code's register file.
#[inline(always)]
pub fn _ureg_rf(rf: &Rf, values: RegView, code: Int) -> V {
    let v = rf_get(rf, values, code);
    if v.is_partial() { V::UNK } else { v }
}

/// state._snapshot_uregs: the register file as it is now.
#[inline(always)]
pub fn _snapshot_uregs(s: &mut St, uregs: RegView) -> RegView {
    if uregs.0 & 1 == 0 {
        s.snapshot();
    }
    RegView(uregs.0 | 1)
}

/// state._bank_request. MODE1's alternate-bank bits are latched before its
/// visible assignment; St::commit supplies the one-cycle delay.
pub fn _bank_request(s: &mut St, value: V) -> R<()> {
    if s.cfg.bank_model {
        if !value.is_c() || s.bank_requested_mask >= 0 {
            return Err(TRAP_SYMBOLIC);
        }
        s.bank_requested_mask = value.val() & BANK_MASK;
    }
    Ok(())
}

/// Python calls this at each source completion site.  Native completion is
/// centralised in St::commit/commit_blk so a trap can never expose a swap.
#[inline(always)]
pub fn _bank_complete(_s: &mut St) {}

/// state._bank_hold_request: a delayed RTI keeps its popped bank selection
/// requested through the first delay slot (consumed by St::bank_complete).
pub fn _bank_hold_request(s: &mut St) {
    if s.cfg.bank_model && s.bank_requested_mask >= 0 {
        s.bank_requested_mask |= BANK_HOLD;
    }
}

/// PCSTKP truncation is delayed until the following completed instruction.
pub fn _pc_stack_request(s: &mut St, value: V) -> R<()> {
    if !value.is_c()
        || value.b > 30
        || value.b as usize > s.pc_stack.len()
        || (s.pc_stack_pending >= 0 && value.val() > s.pc_stack_pending)
    {
        return Err(TRAP_SYMBOLIC);
    }
    s.pc_stack_requested = value.val();
    Ok(())
}

#[inline(always)]
pub fn _pc_stack_complete(_s: &mut St) {}

/// state._pey_view
#[inline(always)]
pub fn _pey_view(_s: &St, values: RegView) -> RegView {
    RegView(values.0 | RegView::PEY)
}

/// state._pey_special
#[inline(always)]
pub fn _pey_special(_s: &St, special: SpecView) -> SpecView {
    if special == SpecView::CUR {
        SpecView::PEY_CUR
    } else {
        SpecView::PEY_EMPTY
    }
}

/// state._mr_from_signed
#[inline(always)]
pub fn _mr_from_signed(_s: &St, value: Int) -> MR {
    MR::new(MR_MASK, value)
}

const MR_WORD_SLICE: [(u32, u32); 3] = [(0, 32), (32, 32), (64, 16)];

/// state._mr_read_word
#[inline(always)]
pub fn _mr_read_word(_s: &St, mr: Spec, word: Int) -> V {
    let (shift, width) = MR_WORD_SLICE[word as usize];
    let Spec::M(mr) = mr else {
        return V::UNK;
    };
    let word_mask: Int = ((1 << width) - 1) << shift;
    if mr.mask & word_mask != word_mask {
        return V::UNK;
    }
    let mut raw = (mr.bits >> shift) & ((1 << width) - 1);
    if word == 2 && raw & (1 << 15) != 0 {
        raw |= 0xFFFF_0000;
    }
    V::c(raw)
}

/// state._mr_write_word
#[inline(always)]
pub fn _mr_write_word(_s: &St, mr: Spec, word: Int, value: V) -> Spec {
    let (shift, width) = MR_WORD_SLICE[word as usize];
    let (old_mask, old_bits) = match mr {
        Spec::M(m) => (m.mask, m.bits),
        Spec::V(_) => (0, 0),
    };
    let word_mask: Int = ((1 << width) - 1) << shift;
    let mr2_mask: Int = 0xFFFF << 64;
    let touched = word_mask | if word == 1 { mr2_mask } else { 0 };
    if !value.is_c() {
        let new_mask = old_mask & !touched;
        let new_bits = old_bits & !touched;
        return if new_mask != 0 {
            Spec::M(MR::new(new_mask, new_bits))
        } else {
            Spec::V(V::UNK)
        };
    }
    let bits = (value.val() & ((1 << width) - 1)) << shift;
    let mut new_mask = (old_mask & !word_mask) | word_mask;
    let mut new_bits = (old_bits & !word_mask) | bits;
    if word == 1 {
        let sign: Int = if (value.val() >> 31) & 1 != 0 {
            0xFFFF
        } else {
            0
        };
        new_mask |= mr2_mask;
        new_bits = (new_bits & !mr2_mask) | (sign << 64);
    }
    Spec::M(MR::new(new_mask, new_bits))
}

// ---------------------------------------------------------------------------
// encoding.py
// ---------------------------------------------------------------------------

/// encoding._field: the exact key, else the first key STEM[...].
#[inline(always)]
pub fn _field(_s: &St, f: Fields, stem: Sym) -> R<Int> {
    match f.lookup(stem) {
        Some(e) => Ok(e.4 as Int),
        None => Err(TRAP_KEY),
    }
}

/// The field named exactly STEM[HI:LO].
#[inline(always)]
fn range_field(f: Fields, stem: Sym, hi: i8, lo: i8) -> R<Int> {
    for e in f.entries() {
        if e.1 == stem && e.2 == hi && e.3 == lo {
            return Ok(e.4 as Int);
        }
    }
    Err(TRAP_KEY)
}

/// encoding._wide: STEM[31:16] << 16 | STEM[15:0].
#[inline(always)]
pub fn _wide(_s: &St, f: Fields, stem: Sym) -> R<Int> {
    Ok((range_field(f, stem, 31, 16)? << 16) | range_field(f, stem, 15, 0)?)
}

/// encoding._split_compute_fields: the generator already added
/// compute[22:16] and compute[15:0] wherever an instruction has compute.
#[inline(always)]
pub fn _split_compute_fields(_s: &St, f: Fields) -> Fields {
    f
}

// ---------------------------------------------------------------------------
// sequencer.decode_at: the generated table of decoded instructions.
// ---------------------------------------------------------------------------

/// The generated crate wraps this with its string table (`sym_of`): the
/// runtime holds no generated data. Always inline so the wrapper is as direct
/// as when both lived in one crate.
#[inline(always)]
pub fn decode_at_with(s: &mut St, pc_sw: Int, sym_of: impl Fn(&str) -> Option<Sym>) -> R<Insn> {
    if (0x90000..0x90080).contains(&pc_sw) {
        let address = 0x2824_0000 + ((pc_sw - 0x90000) * 6) as u32;
        let low = s.mem.read_present(address, 4).ok_or(TRAP_NO_INSN)? as u64;
        let high = s.mem.read_present(address + 4, 2).ok_or(TRAP_NO_INSN)? as u64;
        return decoded_insn(crate::decode::decode_isa(low | (high << 32)), sym_of);
    }
    if !s.runtime_decode {
        return (s.insn_at)(pc_sw).ok_or(TRAP_NO_INSN);
    }
    let pc = u32::try_from(pc_sw).map_err(|_| TRAP_NO_INSN)?;
    let read_sw = s.read_sw;
    let dec_gen = s.mem.dec_gen;
    if let Some(cached) = s.decode_cache.get_mut(pc) {
        if s.dec_watch && cached.seen == dec_gen {
            // No write reached a page holding one of its words since they
            // last matched (Mem::watch_sw).
            return Ok(cached.insn);
        }
        if cached
            .words
            .iter()
            .all(|&(at, word)| read_sw(&s.mem, at) == word)
        {
            cached.seen = dec_gen;
            return Ok(cached.insn);
        }
    }
    let mut words = Vec::new();
    let decoded = crate::decode::decode_at(
        |at| {
            let word = read_sw(&s.mem, at);
            if !words.iter().any(|&(seen, _)| seen == at) {
                words.push((at, word));
            }
            word
        },
        pc,
    );
    let insn = decoded_insn(decoded, sym_of)?;
    if s.dec_watch {
        for &(at, _) in &words {
            s.mem.watch_sw(at);
        }
    }
    let dec_gen = s.mem.dec_gen;
    s.decode_cache.insert(
        pc,
        CachedInsn {
            insn,
            words,
            seen: dec_gen,
        },
    );
    Ok(insn)
}

fn decoded_insn(decoded: crate::decode::Decoded, sym_of: impl Fn(&str) -> Option<Sym>) -> R<Insn> {
    if decoded.kind == crate::decode::DecodeKind::Unknown {
        return Err(TRAP_NO_INSN);
    }
    let type_name = sym_of(decoded.type_name).ok_or(TRAP_NO_INSN)?;
    let kind = sym_of(decoded.kind.as_str()).ok_or(TRAP_NO_INSN)?;
    let mut entries = [FieldEntry(S_EMPTY, S_EMPTY, -1, -1, 0); MAX_INSN_FIELDS];
    let fields = decoded.fields();
    if fields.len() > entries.len() {
        return Err(TRAP_NO_INSN);
    }
    for (dst, field) in entries.iter_mut().zip(fields) {
        *dst = FieldEntry(
            sym_of(field.key).ok_or(TRAP_NO_INSN)?,
            sym_of(field.stem).ok_or(TRAP_NO_INSN)?,
            field.hi,
            field.lo,
            field.value,
        );
    }
    let insn = Insn {
        type_name,
        fields: Fields::from_entries(entries, fields.len() as u8),
        length_bytes: decoded.length_bytes.map(Int::from),
        kind,
        offset: 0,
    };
    Ok(insn)
}

// ---------------------------------------------------------------------------
// floats.py primitives (SUBSET.md's table of exact equivalents)
// ---------------------------------------------------------------------------

/// struct.unpack('<f'): a float32 widened to double. A NaN is converted
/// the way the host's fcvt does (sign kept, quiet bit set, payload moved
/// to the top), explicitly, so constant folding cannot differ.
#[inline(always)]
pub fn _f32_from_bits(_s: &St, bits: Int) -> f64 {
    let b = bits as u32;
    let f = f32::from_bits(b);
    if f.is_nan() {
        let sign = ((b >> 31) as u64) << 63;
        let payload = ((b & 0x003F_FFFF) as u64) << 29;
        return f64::from_bits(sign | 0x7FF8_0000_0000_0000 | payload);
    }
    f as f64
}

#[inline(always)]
pub fn _f64_from_words(_s: &St, hi: Int, lo: Int) -> f64 {
    f64::from_bits(((hi as u64 & 0xFFFF_FFFF) << 32) | (lo as u64 & 0xFFFF_FFFF))
}

#[inline(always)]
pub fn _ldexp(_s: &St, value: f64, exponent: Int) -> f64 {
    scalbn(value, exponent)
}

#[inline(always)]
pub fn _trunc_int(_s: &St, value: f64) -> Int {
    value.trunc() as Int
}

#[inline(always)]
pub fn _round_even_int(_s: &St, value: f64) -> Int {
    value.round_ties_even() as Int
}

// Denormals (floats.py, "Denormal flush"): a computational unit reads a
// denormal operand as a zero of the same sign and returns a post-rounded
// denormal result as a zero of the same sign.

/// floats._flush_denormal_bits
#[inline(always)]
fn ftz32(b: u32) -> u32 {
    if b & 0x7F80_0000 == 0 { b & 0x8000_0000 } else { b }
}

/// floats._F32_FLUSH_BELOW: 2**-126 - 2**-151 (exact in a double).
const F32_FLUSH_BELOW: f64 = f32::MIN_POSITIVE as f64 - f64::from_bits((1023u64 - 151) << 52);

/// floats._float32_result: `_float32_bits` plus the denormal rule; returns
/// (bits, overflowed, underflowed).
#[inline(always)]
pub fn _float32_result(s: &St, value: f64) -> (Int, bool, bool) {
    let m = value.abs();
    if m > 0.0 && m < F32_FLUSH_BELOW {
        let sign = if value.is_sign_negative() { 0x8000_0000 } else { 0 };
        return (sign, false, true);
    }
    let (bits, overflowed) = _float32_bits(s, value);
    (bits, overflowed, false)
}

// Fast paths of floats._float_binary (+ -), floats._float_multiply and the
// multiplier half of the multifunction ops. For add, subtract and multiply
// a NaN operand always gives a NaN result, so a result that is not NaN
// proves both operands are numbers and the NaN handling of the original
// sequence (operand checks, `nan_order`, the NaN case of `_float32_bits`)
// can be skipped. The operands are flushed first. A native f32 result with
// a zero exponent field is a denormal or zero, below the flush cut, so it
// returns the zero of its sign; the smallest normal is the one result that
// can come from either side of the cut, so it (and a NaN) runs the
// original sequence unchanged (cold).

/// The original `_float_binary` body once the fast path saw a NaN result or
/// the smallest normal (inlined and marked cold: a call here would cost the
/// enclosing block function its register allocation).
#[inline(always)]
fn float_binary_slow(
    s: &St,
    left: V,
    right: V,
    op: fn(f64, f64) -> f64,
) -> (V, Option<bool>, Option<bool>) {
    std::hint::cold_path();
    let a = _f32_from_bits(s, ftz32(left.b) as Int);
    let b = _f32_from_bits(s, ftz32(right.b) as Int);
    if a.is_nan() || b.is_nan() {
        return (V::c(0xFFFF_FFFF), Some(false), Some(true));
    }
    let raw = op(a, b);
    if raw.is_nan() {
        return (V::c(0xFFFF_FFFF), Some(false), Some(true));
    }
    let (bits, overflowed, _underflowed) = _float32_result(s, raw);
    (V::c(bits), Some(overflowed), Some(false))
}

macro_rules! float_binary_fast {
    ($name:ident, $op:tt, $slow:path) => {
        /// floats._float_binary with the operation fixed.
        #[inline(always)]
        pub fn $name(s: &St, left: V, right: V) -> (V, Option<bool>, Option<bool>) {
            if !left.is_c() || !right.is_c() {
                return (V::UNK, None, None);
            }
            // Native single precision: the sum or difference of two float32
            // in double and rounded once to float32 equals the float32
            // operation (double has more than 2p+2 bits), denormals included.
            // Overflow to infinity needs finite operands (an infinite operand
            // gives an infinite double, which `_float32_bits` does not count).
            let (a, b) = (f32::from_bits(ftz32(left.b)), f32::from_bits(ftz32(right.b)));
            let r = a $op b;
            let rb = r.to_bits();
            // A normal result, then a zero or denormal one (flushed to a zero
            // of its sign), neither overflowed nor NaN.
            let mag = rb & 0x7FFF_FFFF;
            if mag.wrapping_sub(0x0080_0001) < 0x7EFF_FFFF {
                return (V::c(rb as Int), Some(false), Some(false));
            }
            if mag < 0x0080_0000 {
                return (V::c((rb & 0x8000_0000) as Int), Some(false), Some(false));
            }
            if !r.is_nan() && rb & 0x7FFF_FFFF != 0x0080_0000 {
                let overflowed = r.is_infinite() && a.is_finite() && b.is_finite();
                return (V::c(ftz32(rb) as Int), Some(overflowed), Some(false));
            }
            float_binary_slow(s, left, right, $slow)
        }
    };
}
float_binary_fast!(_float_binary_add, +, fadd);
float_binary_fast!(_float_binary_sub, -, fsub);

/// floats._float_multiply: (result, overflowed, underflowed, invalid).
#[inline(always)]
pub fn _float_multiply(
    s: &St,
    left: V,
    right: V,
) -> (V, Option<bool>, Option<bool>, Option<bool>) {
    if !left.is_c() || !right.is_c() {
        return (V::UNK, None, None, None);
    }
    let (a, b) = (f32::from_bits(ftz32(left.b)), f32::from_bits(ftz32(right.b)));
    let r = a * b;
    let rb = r.to_bits();
    let mag = rb & 0x7FFF_FFFF;
    if mag.wrapping_sub(0x0080_0001) < 0x7EFF_FFFF {
        // A normal result: not overflowed, not underflowed, not NaN.
        return (V::c(rb as Int), Some(false), Some(false), Some(false));
    }
    if mag < 0x0080_0000 {
        // A zero or denormal result is a zero of its sign; of two nonzero
        // numbers it is an underflow.
        let underflowed = a != 0.0 && b != 0.0;
        return (
            V::c((rb & 0x8000_0000) as Int),
            Some(false),
            Some(underflowed),
            Some(false),
        );
    }
    if !r.is_nan() && rb & 0x7FFF_FFFF != 0x0080_0000 {
        let overflowed = r.is_infinite() && a.is_finite() && b.is_finite();
        // A zero-exponent result of two nonzero numbers is an underflow
        // (a product of finite nonzero floats is never an exact zero).
        let underflowed = rb & 0x7F80_0000 == 0 && a != 0.0 && b != 0.0;
        return (
            V::c(ftz32(rb) as Int),
            Some(overflowed),
            Some(underflowed),
            Some(false),
        );
    }
    float_multiply_slow(s, left, right)
}

#[inline(always)]
fn float_multiply_slow(
    s: &St,
    left: V,
    right: V,
) -> (V, Option<bool>, Option<bool>, Option<bool>) {
    std::hint::cold_path();
    let a = _f32_from_bits(s, ftz32(left.b) as Int);
    let b = _f32_from_bits(s, ftz32(right.b) as Int);
    // The invalid result of the multiplier is 0x7FFFFFFF, not all ones (finding 103).
    if a.is_nan() || b.is_nan() {
        return (V::c(0x7FFF_FFFF), Some(false), Some(false), Some(true));
    }
    let raw = fmul(a, b);
    if raw.is_nan() {
        return (V::c(0x7FFF_FFFF), Some(false), Some(false), Some(true));
    }
    let (bits, overflowed, underflowed) = _float32_result(s, raw);
    (V::c(bits), Some(overflowed), Some(underflowed), Some(false))
}

/// floats._float32: a fully known value read as a computational unit reads
/// it (a denormal is a zero of the same sign), widened to a double; None
/// for anything not fully known.
#[inline(always)]
pub fn _float32(s: &St, value: V) -> Option<f64> {
    if !value.is_c() {
        return None;
    }
    Some(_f32_from_bits(s, ftz32(value.b) as Int))
}

/// compute_multi._float_multiply_parts: a float multiply's result and its
/// MN/MV/MU/MI update.
#[inline(always)]
pub fn _float_multiply_parts(s: &St, left: V, right: V, _label: Sym) -> (V, FlagUpdate) {
    if !left.is_c() || !right.is_c() {
        return (
            V::UNK,
            FlagUpdate {
                define_mask: 0,
                define_bits: 0,
                forget_mask: 960,
                cacc: -1,
            },
        );
    }
    // The product of the flushed operands in single precision: a normal
    // result, or one that is a (flushed) zero, is built directly. The
    // smallest normal, an infinity and a NaN take the original sequence.
    let (a, b) = (f32::from_bits(ftz32(left.b)), f32::from_bits(ftz32(right.b)));
    let rb = (a * b).to_bits();
    let mag = rb & 0x7FFF_FFFF;
    if mag.wrapping_sub(0x0080_0001) < 0x7EFF_FFFF {
        return (
            V::c(rb as Int),
            FlagUpdate {
                define_mask: 0x3C0,
                define_bits: ((rb >> 31) as Int) << 6,
                forget_mask: 0,
                cacc: -1,
            },
        );
    }
    if mag < 0x0080_0000 {
        let value = V::c((rb & 0x8000_0000) as Int);
        let underflowed = a != 0.0 && b != 0.0;
        return (
            value,
            _astatx_mult_float(s, value, Some(false), Some(underflowed), Some(false)),
        );
    }
    float_multiply_parts_general(s, left, right)
}

#[inline(always)]
fn float_multiply_parts_general(s: &St, left: V, right: V) -> (V, FlagUpdate) {
    let a = _f32_from_bits(s, ftz32(left.b) as Int);
    let b = _f32_from_bits(s, ftz32(right.b) as Int);
    let raw = fmul(a, b);
    let (bits, overflowed, underflowed) = _float32_result(s, raw);
    let value = V::c(bits);
    if raw.is_nan() {
        return (
            value,
            FlagUpdate {
                define_mask: 512,
                define_bits: 512,
                forget_mask: 448,
                cacc: -1,
            },
        );
    }
    (
        value,
        _astatx_mult_float(s, value, Some(overflowed), Some(underflowed), Some(false)),
    )
}

/// compute_mult._astatx_mult_float: the multiplier flags MN/MV/MU/MI
/// (bits 6..9) of a float multiply result. With no overflow, no underflow
/// and no invalid operation (all known false) the update is MN = sign and
/// the other three cleared, built directly. Everything else takes the
/// original `_flags_put` sequence.
#[inline(always)]
pub fn _astatx_mult_float(
    s: &St,
    result: V,
    overflowed: Option<bool>,
    underflowed: Option<bool>,
    invalid: Option<bool>,
) -> FlagUpdate {
    if result.is_c()
        && overflowed == Some(false)
        && underflowed == Some(false)
        && invalid == Some(false)
    {
        return FlagUpdate {
            define_mask: 0x3C0,
            define_bits: ((result.b >> 31) as Int) << 6,
            forget_mask: 0,
            cacc: -1,
        };
    }
    astatx_mult_float_general(s, result, overflowed, underflowed, invalid)
}

#[inline(always)]
fn astatx_mult_float_general(
    s: &St,
    result: V,
    overflowed: Option<bool>,
    underflowed: Option<bool>,
    invalid: Option<bool>,
) -> FlagUpdate {
    let (mut mn, mut mv, mut mu) = (None, None, None);
    if result.is_c() {
        let bits = result.val();
        mn = Some(bits & 0x8000_0000 != 0);
        mv = Some(overflowed.unwrap_or(false));
        mu = underflowed;
    }
    let mut update = FLAGS_NONE;
    update = _flags_put(s, update, 6, mn);
    update = _flags_put(s, update, 7, mv);
    update = _flags_put(s, update, 8, mu);
    _flags_put(s, update, 9, invalid)
}

/// floats._float32_bits: struct.pack('<f') rounds to nearest even and
/// raises OverflowError when a finite value rounds to infinity.
#[inline(always)]
pub fn _float32_bits(_s: &St, value: f64) -> (Int, bool) {
    if value.is_nan() {
        // The host's fcvt: sign kept, quiet bit set, top payload bits kept.
        let b = value.to_bits();
        let sign = ((b >> 63) as u32) << 31;
        let payload = ((b >> 29) & 0x003F_FFFF) as u32;
        return ((sign | 0x7FC0_0000 | payload) as Int, false);
    }
    let y = value as f32;
    let overflowed = y.is_infinite() && !value.is_infinite();
    (y.to_bits() as Int, overflowed)
}

#[inline(always)]
pub fn _double_pair_bits(_s: &St, value: f64) -> (Int, Int) {
    let bits = value.to_bits();
    ((bits >> 32) as Int, (bits & 0xFFFF_FFFF) as Int)
}

// ---------------------------------------------------------------------------
// memory.py: the byte store (loader image + overlay) and its access rules.
// ---------------------------------------------------------------------------

pub const SW_ALIAS_BASE: Int = 0x2800_0000;
const CORE_MMR_RANGE: (Int, Int) = (0x30000, 0x32000);
const SYSTEM_MMR_RANGE: (Int, Int) = (0x3100_0000, 0x310F_FFFF);
const L1_BLOCK3_NW_BASE: Int = 0x000E_0000;
const L1_BLOCK3_NW_LIMIT: Int = 0x000E_8000;
const L1_BLOCK3_SW_BASE: Int = 0x001C_0000;

/// memory._concrete_address
#[inline(always)]
pub fn _concrete_address(_s: &St, value: VI) -> Option<Int> {
    match value {
        VI::V(v) if v.is_c() => Some(v.val()),
        VI::V(_) => None,
        VI::I(i) => Some(i),
    }
}

#[inline(always)]
fn in_core_mmr_range(a: Int) -> bool {
    CORE_MMR_RANGE.0 <= a && a < CORE_MMR_RANGE.1
}

#[inline(always)]
fn in_system_mmr_range(a: Int) -> bool {
    SYSTEM_MMR_RANGE.0 <= a && a <= SYSTEM_MMR_RANGE.1
}

/// memory._byte_present (loader image or overlay).
#[inline(always)]
pub fn _byte_present(s: &St, here: Int) -> bool {
    (0..=u32::MAX as Int).contains(&here) && s.mem.present(here as u32)
}

/// memory._canonical_dm_address
#[inline(always)]
pub fn _canonical_dm_address(s: &St, address: Int, width: Int, for_write: bool) -> R<Option<Int>> {
    if !s.cfg.has_concrete {
        return Ok(None);
    }
    let mut address = address;
    if !_byte_present(s, address) && (0..SW_ALIAS_BASE).contains(&address) {
        let alias = SW_ALIAS_BASE + address;
        if for_write || _byte_present(s, alias) {
            address = alias;
        }
    }
    if for_write {
        return Ok(Some(address));
    }
    if width > 0 && s.mem.all_present(address, width) {
        return Ok(Some(address));
    }
    let mut here = address;
    while here < address + width {
        if !_byte_present(s, here) {
            return Ok(None);
        }
        here += 1;
    }
    Ok(Some(address))
}

/// `SHARC_UNINIT=<hex seed>` (opt-in, default off): a read of bytes that nothing
/// loaded or wrote returns a non-zero pattern, a function of the address and the
/// seed, instead of zero (real SRAM holds whatever it powered up with; the model
/// holds zeros). Little-endian, per byte, so any width and alignment agrees.
fn uninit_seed() -> u32 {
    static SEED: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *SEED.get_or_init(|| {
        std::env::var("SHARC_UNINIT")
            .ok()
            .and_then(|v| u32::from_str_radix(v.trim_start_matches("0x"), 16).ok())
            .unwrap_or(0)
    })
}

/// `SHARC_UNINIT_LOG=<file>`: append one line per logged pattern read (diagnostic output only).
#[cold]
fn uninit_log(line: &str) {
    use std::io::Write;
    if let Some(path) = std::env::var_os("SHARC_UNINIT_LOG")
        && let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path)
    {
        let _ = writeln!(f, "pid {} {line}", std::process::id());
    }
}

/// `SHARC_UNINIT_TRAP=1` (opt-in, default off): list data reads of bytes that nothing
/// loaded or wrote, once per word-aligned address, to `SHARC_UNINIT_LOG` (stderr if
/// unset). The value read does not change. Silicon parity (MODE1 bits 27-29) may fault
/// on such reads.
fn uninit_trap() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SHARC_UNINIT_TRAP").is_some_and(|v| v != "0"))
}

#[cold]
fn uninit_trap_hit(s: &St, address: Int, width: Int) {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static SEEN: OnceLock<Mutex<HashMap<u32, u64>>> = OnceLock::new();
    let word = (address as u32) & !3;
    let n = {
        let mut m = SEEN.get_or_init(Default::default).lock().unwrap();
        let c = m.entry(word).or_insert(0);
        *c += 1;
        *c
    };
    if n == 1 || (n >= 64 && n.is_power_of_two()) {
        let line = format!(
            "uninit-read addr={:#x} w={width} pc={:#x} n={n}",
            address as u32, s.pc_sw as u32
        );
        if std::env::var_os("SHARC_UNINIT_LOG").is_some() {
            uninit_log(&line);
        } else {
            eprintln!("{line}");
        }
    }
}

#[cold]
fn uninit_value(address: Int, width: Int) -> Int {
    let seed = uninit_seed();
    if seed == 0 {
        return 0;
    }
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if n < 24 || n.is_power_of_two() {
        uninit_log(&format!("read #{n} of {width} byte(s) at {:#x}", address as u32));
    }
    let mut v: Int = 0;
    for k in 0..width {
        let a = (address as u32).wrapping_add(k as u32);
        let mut x = (a ^ seed).wrapping_mul(0x9E37_79B1);
        x ^= x >> 15;
        x = x.wrapping_mul(0x85EB_CA6B);
        x ^= x >> 13;
        let b = (x & 0xff) as u8 | 0x01;
        v |= (b as Int) << (8 * k);
    }
    v
}

#[inline(always)]
fn fixed_width_mmr(s: &St, c: Int) -> bool {
    s.in_mmr_windows(c) && (s.core_mmr_reset.contains(&(c as u32)) || s.is_named_mmr(c))
}

/// The address of a plain-RAM access: a known address on a page with no
/// MMR (St::mmr_page) under the default memory configuration. Only such
/// accesses take the inline fast paths below; the rest go through the
/// full rules.
#[inline(always)]
fn plain_address(s: &St, address: VI) -> Option<u32> {
    let a = match address {
        VI::V(v) if v.is_c() => v.b,
        VI::I(i) if (0..=u32::MAX as Int).contains(&i) => i as u32,
        _ => return None,
    };
    if !s.cfg.fast_mem {
        return None;
    }
    Some(a)
}

/// A plain-RAM read at A: the bytes there, or (the first byte absent
/// below the short-word alias base) the aliased bytes, when all present
/// (_canonical_dm_address).
#[inline(always)]
fn plain_read(s: &St, a: u32, width: u32) -> Option<u32> {
    blk_read(&s.mem, a, width)
}

/// `Mem::fast_read` for block code: an aligned word takes the shorter path
/// (it declines an unaligned one to the full rules).
#[inline(always)]
fn blk_read(m: &crate::mem::Mem, a: u32, width: u32) -> Option<u32> {
    if width == 4 {
        m.fast_read_word(a)
    } else {
        m.fast_read(a, width)
    }
}

/// `Mem::fast_write` for block code (see `blk_read`).
#[inline(always)]
fn blk_write(m: &mut crate::mem::Mem, a: u32, width: u32, v: u32) -> Option<u32> {
    if width == 4 {
        m.fast_write_word(a, v)
    } else {
        m.fast_write(a, width, v)
    }
}

/// The plain address of a block-code access before the normal-word mapping.
#[inline(always)]
fn raw_address_b(address: VI) -> Option<u32> {
    match address {
        VI::V(v) if v.is_c() => Some(v.b),
        VI::I(i) if (0..=u32::MAX as Int).contains(&i) => Some(i as u32),
        _ => None,
    }
}

#[inline(always)]
fn blk_read_at(m: &crate::mem::Mem, a: u32, width: Int, nw: bool) -> Option<u32> {
    let a = if nw {
        crate::addressing::normal_word_or_same(a)
    } else {
        a
    };
    blk_read(m, a, width as u32)
}

#[inline(always)]
fn blk_write_nolog_at(m: &mut crate::mem::Mem, a: u32, width: Int, v: u32, nw: bool) -> bool {
    let a = if nw {
        crate::addressing::normal_word_or_same(a)
    } else {
        a
    };
    blk_write(m, a, width as u32, v).is_some()
}

#[inline(always)]
fn blk_write_log_at(s: &mut St, a: u32, width: Int, v: u32, nw: bool) -> R<bool> {
    if s.un >= UNDO_CAP {
        return Ok(false);
    }
    let a = if nw {
        crate::addressing::normal_word_or_same(a)
    } else {
        a
    };
    match blk_write(&mut s.mem, a, width as u32, v) {
        Some(old) => {
            let c = s.mem.canonical_of(a);
            s.log(Undo::MemWord(c, width as u8, old))?;
            Ok(true)
        }
        None => Ok(false),
    }
}

#[inline(always)]
fn access_address(address: VI, normal_word: bool) -> VI {
    if normal_word {
        let concrete = match address {
            VI::I(a) => Some(a),
            VI::V(v) if v.is_c() => Some(v.val()),
            _ => None,
        };
        if let Some(a) = concrete.and_then(crate::addressing::normal_word_to_byte) {
            return VI::I(a);
        }
    }
    address
}

/// A block-code access's plain address, after the normal-word mapping, as a
/// 32-bit value; None when the address is not a plain known one (then the
/// general path below decides, from the original address).
#[inline(always)]
fn fast_address_b(address: VI, normal_word: bool) -> Option<u32> {
    let a = match address {
        VI::V(v) if v.is_c() => v.b,
        VI::I(i) if (0..=u32::MAX as Int).contains(&i) => i as u32,
        _ => return None,
    };
    Some(if normal_word {
        crate::addressing::normal_word_or_same(a)
    } else {
        a
    })
}

/// _dm_read for block code.
#[inline(always)]
pub fn _dm_read_b(
    s: &St,
    address: VI,
    width: Int,
    signed: bool,
    normal_word: bool,
) -> R<Option<V>> {
    if let Some(a) = raw_address_b(address)
        && matches!(width, 1 | 2 | 4)
        && let Some(raw) = blk_read_at(&s.mem, a, width, normal_word)
    {
        let raw = raw as Int;
        let value = if signed {
            let bits = 8 * width;
            if raw & (1 << (bits - 1)) != 0 {
                raw - (1 << bits)
            } else {
                raw
            }
        } else {
            raw
        };
        return Ok(Some(V::c(value)));
    }
    _dm_read_full(s, address, width, signed, normal_word)
}

/// _dm_write for block code.
#[inline(always)]
pub fn _dm_write_b(s: &mut St, address: VI, width: Int, value: V, normal_word: bool) -> R<bool> {
    if value.is_c()
        && matches!(width, 1 | 2 | 4)
        && let Some(a) = raw_address_b(address)
        && blk_write_log_at(s, a, width, value.b, normal_word)?
    {
        return Ok(true);
    }
    _dm_write_full(s, address, width, value, normal_word)
}

/// _dm_write_nolog for block code.
#[inline(always)]
pub fn _dm_write_nolog_b(
    s: &mut St,
    address: VI,
    width: Int,
    value: V,
    normal_word: bool,
) -> R<bool> {
    if value.is_c()
        && matches!(width, 1 | 2 | 4)
        && let Some(a) = raw_address_b(address)
        && blk_write_nolog_at(&mut s.mem, a, width, value.b, normal_word)
    {
        return Ok(true);
    }
    _dm_write_nolog_full(s, address, width, value, normal_word)
}

/// memory._dm_read. Inline: a plain-RAM read of present bytes; the rest
/// out of line.
#[inline(always)]
pub fn _dm_read(s: &St, address: VI, width: Int, signed: bool, normal_word: bool) -> R<Option<V>> {
    if s.cfg.fast_mem
        && let Some(a) = fast_address_b(address, normal_word)
        && matches!(width, 1 | 2 | 4)
        && let Some(raw) = plain_read(s, a, width as u32)
    {
        let raw = raw as Int;
        let value = if signed {
            let bits = 8 * width;
            if raw & (1 << (bits - 1)) != 0 {
                raw - (1 << bits)
            } else {
                raw
            }
        } else {
            raw
        };
        return Ok(Some(V::c(value)));
    }
    _dm_read_full(s, address, width, signed, normal_word)
}

/// memory._dm_read, every rule.
#[inline(never)]
pub fn _dm_read_full(
    s: &St,
    address: VI,
    width: Int,
    signed: bool,
    normal_word: bool,
) -> R<Option<V>> {
    let address = access_address(address, normal_word);
    let Some(concrete) = _concrete_address(s, address) else {
        return Ok(None);
    };
    if matches!(width, 1 | 2 | 4 | 8) && (0..=u32::MAX as Int).contains(&concrete) {
        s.mem.note_ddr_access(
            concrete as u32,
            width as u32,
            crate::mem::DdrAccessKind::CpuRead,
        );
    }
    if !s.cfg.has_concrete || !matches!(width, 1 | 2 | 4 | 8) {
        return Ok(None);
    }
    if s.cfg.peripheral_model
        && width == 4
        && (0..=u32::MAX as Int).contains(&concrete)
        && let Some(v) = super::periph::periph_read(s, concrete as u32)?
    {
        return Ok(Some(v));
    }
    // Plain internal/external RAM, the common case: no MMR rules apply.
    let mmr_candidate = in_core_mmr_range(concrete)
        || in_system_mmr_range(concrete)
        || (width == 4 && fixed_width_mmr(s, concrete));
    if width == 4 && mmr_candidate {
        let fixed = fixed_width_mmr(s, concrete);
        if fixed {
            if let Some(v) = s.mmr_get(concrete as u32) {
                return Ok(if v.is_c() { Some(v) } else { None });
            }
            if s.cfg.data_memory_tainted {
                return Ok(None);
            }
        }
        if s.cfg.explicit_memory_model {
            if let Some(v) = s.mmr_get(concrete as u32) {
                return Ok(if v.is_c() { Some(v) } else { None });
            }
            return Err(TRAP_UNMODELED_MMR);
        }
    }
    let fixed = width == 4 && mmr_candidate && fixed_width_mmr(s, concrete);
    if width == 4 && !s.cfg.assume_nw32 && !fixed && !(0x3000_0000..0x4000_0000).contains(&concrete)
    {
        return Ok(None);
    }
    let canonical = _canonical_dm_address(s, concrete, width, false)?;
    let Some(canonical) = canonical else {
        let non_mmr = !(fixed_width_mmr(s, concrete)
            || in_core_mmr_range(concrete)
            || in_system_mmr_range(concrete));
        if non_mmr && uninit_trap() {
            uninit_trap_hit(s, concrete, width);
        }
        let plain = matches!(width, 1 | 2 | 4) && non_mmr;
        if plain && (s.cfg.explicit_memory_model || uninit_seed() != 0) {
            // bytes nothing loaded or wrote: zero, or (SHARC_UNINIT) the power-up pattern
            return Ok(Some(V::c(uninit_value(concrete, width))));
        }
        return Ok(None);
    };
    if s.cfg.data_memory_tainted && !s.mem.all_dirty(canonical, width) {
        return Ok(None);
    }
    if width > 4 {
        return Ok(None);
    }
    let raw = s.mem.read_le(canonical as u32, width as u32) as Int;
    let value = if signed {
        let bits = 8 * width;
        if raw & (1 << (bits - 1)) != 0 {
            raw - (1 << bits)
        } else {
            raw
        }
    } else {
        raw
    };
    Ok(Some(V::c(value)))
}

/// memory._read_px48: the loader image only (not the overlay).
#[inline(always)]
pub fn _read_px48(s: &St, address: VI) -> R<Option<(V, V)>> {
    let Some(concrete) = _concrete_address(s, address) else {
        return Ok(None);
    };
    if !s.cfg.has_concrete || !(L1_BLOCK3_NW_BASE..L1_BLOCK3_NW_LIMIT).contains(&concrete) {
        return Ok(None);
    }
    let offset = concrete - L1_BLOCK3_NW_BASE;
    let byte_address = (2 * L1_BLOCK3_SW_BASE + SW_ALIAS_BASE) + 6 * offset;
    let mut raw = [0u8; 6];
    for (k, b) in raw.iter_mut().enumerate() {
        match s.mem.loader_byte(byte_address as u32 + k as u32) {
            Some(x) => *b = x,
            None => return Ok(None),
        }
    }
    let high = u16::from_le_bytes([raw[0], raw[1]]) as Int;
    let middle = u16::from_le_bytes([raw[2], raw[3]]) as Int;
    let low = u16::from_le_bytes([raw[4], raw[5]]) as Int;
    Ok(Some((V::c(low << 16), V::c((high << 16) | middle))))
}

/// memory._load_normal_ureg. Returns the loaded Const, or None (the
/// combined-PX summary dict is observability-only). WRITE_UREG is the
/// generated core's `state._write_ureg` (or the runtime's own register
/// write when there is none): the generated crate supplies it.
#[inline(always)]
pub fn _load_normal_ureg_with(
    s: &mut St,
    space: Sym,
    address: VI,
    code: Int,
    write_ureg: impl FnOnce(&mut St, Int, V) -> R<()>,
) -> R<Option<V>> {
    if code == UREG_PX as Int {
        if let Some((px1, px2)) = _read_px48(s, address)? {
            s.set_r(UREG_PX, V::UNK)?;
            s.set_r(UREG_PX1, px1)?;
            s.set_r(UREG_PX2, px2)?;
            return Ok(None);
        }
        s.set_r(UREG_PX1, V::UNK)?;
        s.set_r(UREG_PX2, V::UNK)?;
    } else if space == S_DM || space == S_PM {
        let loaded = _dm_read(s, address, 4, false, true)?;
        write_ureg(s, code, loaded.unwrap_or(V::UNK))?;
        return Ok(loaded);
    }
    s_set_r(s, code, V::UNK)?;
    Ok(None)
}

/// memory._load_normal_ureg over block code's register file. A DM load
/// that reads nothing known leaves block code (the result would be an
/// Unknown register).
#[inline(always)]
pub fn _load_normal_ureg_rf(
    s: &mut St,
    rf: &mut Rf,
    space: Sym,
    address: VI,
    code: Int,
) -> R<Option<V>> {
    if matches!(code, 100 | 101) {
        return Err(TRAP_BLOCK_UNKNOWN);
    }
    if code == UREG_PX as Int {
        if let Some((px1, px2)) = _read_px48(s, address)? {
            rf_put(rf, UREG_PX as Int, V::UNK)?;
            rf_set(rf, UREG_PX1 as Int, px1)?;
            rf_set(rf, UREG_PX2 as Int, px2)?;
            return Ok(None);
        }
        rf_put(rf, UREG_PX1 as Int, V::UNK)?;
        rf_put(rf, UREG_PX2 as Int, V::UNK)?;
    } else if space == S_DM || space == S_PM {
        // Block code only: the run configuration has the plain-RAM paths.
        let loaded = _dm_read_b(s, address, 4, false, true)?;
        match loaded {
            Some(v) => rf_set(rf, code, v)?,
            None if rf.allow_unknown => rf_set(rf, code, V::UNK)?,
            None => return Err(TRAP_BLOCK_UNKNOWN),
        }
        return Ok(loaded);
    }
    rf_put(rf, code, V::UNK)?;
    Ok(None)
}

/// memory._dm_write. Inline: a plain-RAM write over overlay bytes; the
/// rest out of line.
#[inline(always)]
pub fn _dm_write(s: &mut St, address: VI, width: Int, value: V, normal_word: bool) -> R<bool> {
    let address = access_address(address, normal_word);
    if value.is_c()
        && matches!(width, 1 | 2 | 4)
        && s.un < UNDO_CAP
        && let Some(a) = plain_address(s, address)
        && let Some(old) = blk_write(&mut s.mem, a, width as u32, value.b)
    {
        let c = s.mem.canonical_of(a);
        s.log(Undo::MemWord(c, width as u8, old))?;
        return Ok(true);
    }
    _dm_write_full(s, address, width, value, false)
}

/// memory._dm_write, every rule.
#[inline(never)]
pub fn _dm_write_full(s: &mut St, address: VI, width: Int, value: V, normal_word: bool) -> R<bool> {
    let address = access_address(address, normal_word);
    let Some(concrete) = _concrete_address(s, address) else {
        return Ok(false);
    };
    if matches!(width, 1 | 2 | 4) && (0..=u32::MAX as Int).contains(&concrete) {
        s.mem.note_ddr_access(
            concrete as u32,
            width as u32,
            crate::mem::DdrAccessKind::CpuWrite,
        );
    }
    if !s.cfg.has_concrete || !value.is_c() || !matches!(width, 1 | 2 | 4) {
        return Ok(false);
    }
    if s.cfg.peripheral_model && width == 4 && (0..=u32::MAX as Int).contains(&concrete) {
        if s.in_block && super::periph::write_acts(concrete as u32) {
            return Err(TRAP_BLOCK_MODEL);
        }
        if super::periph::periph_write(s, concrete as u32, value.b)? {
            return Ok(true);
        }
    }
    if width == 4 && fixed_width_mmr(s, concrete) {
        s.mmr_set(concrete as u32, value)?;
        return Ok(true);
    }
    if width == 4 && !s.cfg.assume_nw32 && !(0x3000_0000..0x4000_0000).contains(&concrete) {
        return Ok(false);
    }
    let Some(c) = _canonical_dm_address(s, concrete, width, true)? else {
        return Ok(false);
    };
    if !(0..=(u32::MAX as Int - width + 1)).contains(&c) {
        return Err(TRAP_ADDRESS);
    }
    s.mem_write(c as u32, width as u32, value.b)?;
    Ok(true)
}

/// memory._dm_write for block code whose instruction reads no memory: the
/// write is not logged (a trap later in the instruction leaves it; the
/// instruction runs again and stores the same bytes).
#[inline(always)]
pub fn _dm_write_nolog(
    s: &mut St,
    address: VI,
    width: Int,
    value: V,
    normal_word: bool,
) -> R<bool> {
    let address = access_address(address, normal_word);
    if value.is_c()
        && matches!(width, 1 | 2 | 4)
        && let Some(a) = plain_address(s, address)
        && blk_write(&mut s.mem, a, width as u32, value.b).is_some()
    {
        return Ok(true);
    }
    _dm_write_nolog_full(s, address, width, value, false)
}

#[inline(never)]
fn _dm_write_nolog_full(
    s: &mut St,
    address: VI,
    width: Int,
    value: V,
    normal_word: bool,
) -> R<bool> {
    let address = access_address(address, normal_word);
    let Some(concrete) = _concrete_address(s, address) else {
        return Ok(false);
    };
    if matches!(width, 1 | 2 | 4) && (0..=u32::MAX as Int).contains(&concrete) {
        s.mem.note_ddr_access(
            concrete as u32,
            width as u32,
            crate::mem::DdrAccessKind::CpuWrite,
        );
    }
    if !s.cfg.has_concrete || !value.is_c() || !matches!(width, 1 | 2 | 4) {
        return Ok(false);
    }
    if s.cfg.peripheral_model && width == 4 && (0..=u32::MAX as Int).contains(&concrete) {
        if s.in_block && super::periph::write_acts(concrete as u32) {
            return Err(TRAP_BLOCK_MODEL);
        }
        if super::periph::periph_write(s, concrete as u32, value.b)? {
            return Ok(true);
        }
    }
    if width == 4 && fixed_width_mmr(s, concrete) {
        s.mmr_put(concrete as u32, value);
        return Ok(true);
    }
    if width == 4 && !s.cfg.assume_nw32 && !(0x3000_0000..0x4000_0000).contains(&concrete) {
        return Ok(false);
    }
    let Some(c) = _canonical_dm_address(s, concrete, width, true)? else {
        return Ok(false);
    };
    if !(0..=(u32::MAX as Int - width + 1)).contains(&c) {
        return Err(TRAP_ADDRESS);
    }
    if !s.mem.write_dirty(c as u32, width as u32, value.b) {
        for k in 0..width as u32 {
            s.mem.write_byte(c as u32 + k, (value.b >> (8 * k)) as u8);
        }
    }
    Ok(true)
}

impl St {
    /// An unjournaled store of WIDTH bytes at A, for an accelerator's DMA (rt/iir.rs).
    pub(crate) fn mem_store_nolog(&mut self, a: u32, width: u32, v: u32) {
        if !self.mem.write_dirty(a, width, v) {
            for k in 0..width {
                self.mem.write_byte(a + k, (v >> (8 * k)) as u8);
            }
        }
    }

    #[inline(always)]
    fn mem_write(&mut self, a: u32, width: u32, v: u32) -> R<()> {
        if let Some(old) = self.mem.read_dirty(a, width) {
            self.log(Undo::MemWord(a, width as u8, old))?;
            self.mem.write_dirty(a, width, v);
            return Ok(());
        }
        for k in 0..width {
            let addr = a + k;
            let (old, flags) = self.mem.write_byte(addr, (v >> (8 * k)) as u8);
            self.log(Undo::Mem(addr, old, flags))?;
        }
        Ok(())
    }
}

/// Differential tests of the fast paths above against the bodies they
/// replaced (kept here as `*_ref`): result and every flag bit must agree.
#[cfg(test)]
mod fast_diff {
    use super::*;

    /// Deterministic xorshift64*.
    pub(super) struct Rng(pub u64);
    impl Rng {
        pub fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
    }

    // -- float32 operand generators -------------------------------------

    pub(super) fn specials_f32() -> Vec<u32> {
        let mut v = Vec::new();
        for sign in [0u32, 0x8000_0000] {
            for exp in [
                0u32, 1, 2, 3, 64, 126, 127, 128, 150, 151, 152, 253, 254, 255,
            ] {
                for mant in [
                    0u32, 1, 2, 0x3F_FFFF, 0x40_0000, 0x40_0001, 0x7F_FFFE, 0x7F_FFFF,
                ] {
                    v.push(sign | (exp << 23) | mant);
                }
            }
        }
        v
    }

    /// Operand pair biased towards overflow, underflow, cancellation and
    /// denormal results, or fully random.
    pub(super) fn pair(rng: &mut Rng) -> (u32, u32) {
        let r = rng.next();
        let r2 = rng.next();
        let (a, b) = (r as u32, (r >> 32) as u32);
        match r2 % 10 {
            0 => (a, b),
            1 => {
                // exponent sum near the overflow edge (product), small mantissa
                let ea = (r2 >> 8) as u32 % 256;
                let eb =
                    (254i32 + 127 - ea as i32 + ((r2 >> 20) as i32 % 5) - 2).clamp(0, 255) as u32;
                (
                    (a & 0x807F_FFFF) | (ea << 23),
                    (b & 0x807F_FFFF) | (eb << 23),
                )
            }
            2 => {
                // exponent sum near the underflow edge
                let ea = (r2 >> 8) as u32 % 256;
                let eb = (127 - ea as i32 + ((r2 >> 20) as i32 % 5) - 2 + 0).clamp(0, 255) as u32;
                (
                    (a & 0x807F_FFFF) | (ea << 23),
                    (b & 0x807F_FFFF) | (eb << 23),
                )
            }
            3 => {
                // x and -x to within a few ulp: cancellation
                let d = (r2 >> 8) as u32 % 8;
                (a, (a ^ 0x8000_0000).wrapping_add(d).wrapping_sub(4))
            }
            4 => {
                // both denormal or tiny
                (
                    a & 0x80FF_FFFF & !0x7F00_0000,
                    b & 0x80FF_FFFF & !0x7E00_0000,
                )
            }
            5 => {
                // large magnitudes: sum overflow
                (
                    (a & 0x8000_0000) | 0x7F00_0000 | (a & 0x00FF_FFFF),
                    (b & 0x8000_0000) | 0x7F00_0000 | (b & 0x00FF_FFFF),
                )
            }
            6 => {
                // NaN / inf operand mixed in
                let special = [
                    0x7F80_0000u32,
                    0xFF80_0000,
                    0x7FC0_0000,
                    0x7F80_0001,
                    0xFFFF_FFFF,
                    0,
                    0x8000_0000,
                ];
                (special[(r2 >> 8) as usize % 7], b)
            }
            8 => {
                // x and -x to within a few ulp at the smallest normals
                let m = a & 0x007F_FFFF;
                let d = (r2 >> 8) as u32 % 8;
                (
                    0x0080_0000 | m,
                    (0x8080_0000 | m).wrapping_add(d).wrapping_sub(4),
                )
            }
            9 => {
                // a product at the flush cut: (1 + (sqrt2 - 1)) squared is
                // 2 to within a few ulp, with exponents summing to -127
                let ea = 14 + (r2 >> 8) as u32 % 100;
                let eb = 127 - ea;
                let da = (r2 >> 20) as u32 % 7;
                let db = (r2 >> 24) as u32 % 7;
                (
                    ((a & 0x8000_0000) | (ea << 23)) | (0x3504F3 + da - 3),
                    ((b & 0x8000_0000) | (eb << 23)) | (0x3504F3 + db - 3),
                )
            }
            _ => (a, a),
        }
    }

    /// A denormal operand as a zero of its sign (PRM p.100), independently of
    /// the production `ftz32`.
    fn operand_ref(s: &St, v: V) -> f64 {
        let f = f32::from_bits(v.b);
        if f.is_subnormal() {
            return _f32_from_bits(s, (0.0f32).copysign(f).to_bits() as Int);
        }
        _f32_from_bits(s, v.val())
    }

    /// A result as a unit returns it, with the cut computed from powers of
    /// two: (bits, overflowed, underflowed).
    fn result_ref(s: &St, raw: f64) -> (Int, bool, bool) {
        let cut = 2f64.powi(-126) - 2f64.powi(-151);
        if raw != 0.0 && raw.abs() < cut {
            return (if raw < 0.0 { 0x8000_0000 } else { 0 }, false, true);
        }
        let (bits, overflowed) = _float32_bits(s, raw);
        (bits, overflowed, false)
    }

    fn float_binary_ref(
        s: &St,
        left: V,
        right: V,
        op: fn(f64, f64) -> f64,
    ) -> (V, Option<bool>, Option<bool>) {
        if !left.is_c() || !right.is_c() {
            return (V::UNK, None, None);
        }
        let a = operand_ref(s, left);
        let b = operand_ref(s, right);
        if a.is_nan() || b.is_nan() {
            return (V::c(0xFFFF_FFFF), Some(false), Some(true));
        }
        let raw = op(a, b);
        if raw.is_nan() {
            return (V::c(0xFFFF_FFFF), Some(false), Some(true));
        }
        let (bits, overflowed, _) = result_ref(s, raw);
        (V::c(bits), Some(overflowed), Some(false))
    }

    fn float_multiply_ref(
        s: &St,
        left: V,
        right: V,
    ) -> (V, Option<bool>, Option<bool>, Option<bool>) {
        if !left.is_c() || !right.is_c() {
            return (V::UNK, None, None, None);
        }
        let a = operand_ref(s, left);
        let b = operand_ref(s, right);
        if a.is_nan() || b.is_nan() {
            return (V::c(0x7FFF_FFFF), Some(false), Some(false), Some(true));
        }
        let raw = a * b;
        if raw.is_nan() {
            return (V::c(0x7FFF_FFFF), Some(false), Some(false), Some(true));
        }
        let (bits, overflowed, underflowed) = result_ref(s, raw);
        (V::c(bits), Some(overflowed), Some(underflowed), Some(false))
    }

    #[test]
    fn float_binary_fast_paths_match_reference() {
        let s = St::new(crate::mem::Mem::new());
        type Fast = fn(&St, V, V) -> (V, Option<bool>, Option<bool>);
        let ops: [(Fast, fn(f64, f64) -> f64); 2] =
            [(_float_binary_add, fadd), (_float_binary_sub, fsub)];
        let mut count = 0u64;
        let mut check = |a: V, b: V| {
            for (fast, slow) in ops {
                let got = fast(&s, a, b);
                let want = float_binary_ref(&s, a, b, slow);
                assert_eq!(got, want, "a={:#x}/{:#x} b={:#x}/{:#x}", a.b, a.m, b.b, b.m);
                count += 1;
            }
            let got = _float_multiply(&s, a, b);
            assert_eq!(
                got,
                float_multiply_ref(&s, a, b),
                "mul a={:#x} b={:#x}",
                a.b,
                b.b
            );
            count += 1;
        };
        let sp = specials_f32();
        for &a in &sp {
            for &b in &sp {
                check(V::c(a as Int), V::c(b as Int));
            }
            check(V::UNK, V::c(a as Int));
            check(V::c(a as Int), V::UNK);
            check(V::partial(0xFFFF_0000, a as Int), V::c(a as Int));
        }
        let mut rng = Rng(0x1234_5678_9ABC_DEF1);
        for _ in 0..10_000_000u64 {
            let (a, b) = pair(&mut rng);
            check(V::c(a as Int), V::c(b as Int));
        }
        assert!(count >= 30_000_000);
        eprintln!("float_binary differential: {count} comparisons, 0 mismatches");
    }

    /// The body of compute_mult._astatx_mult_float.
    fn astatx_mult_float_ref(
        s: &St,
        result: V,
        overflowed: Option<bool>,
        underflowed: Option<bool>,
        invalid: Option<bool>,
    ) -> FlagUpdate {
        let (mut mn, mut mv, mut mu) = (None, None, None);
        if result.is_c() {
            let bits = result.val();
            mn = Some(bits & 2147483648 != 0);
            mv = Some(match overflowed {
                Some(x) => x,
                None => false,
            });
            mu = underflowed;
        }
        let mut update = FlagUpdate {
            define_mask: 0,
            define_bits: 0,
            forget_mask: 0,
            cacc: -1,
        };
        update = _flags_put(s, update, 6, mn);
        update = _flags_put(s, update, 7, mv);
        update = _flags_put(s, update, 8, mu);
        _flags_put(s, update, 9, invalid)
    }

    #[test]
    fn astatx_mult_float_fast_path_matches_reference() {
        let s = St::new(crate::mem::Mem::new());
        let opts = [None, Some(false), Some(true)];
        let mut count = 0u64;
        let mut check = |r: V| {
            for ov in opts {
                for un in opts {
                    for inv in opts {
                        assert_eq!(
                            _astatx_mult_float(&s, r, ov, un, inv),
                            astatx_mult_float_ref(&s, r, ov, un, inv),
                            "bits={:#x}/{:#x} ov={ov:?} un={un:?} inv={inv:?}",
                            r.b,
                            r.m
                        );
                        count += 1;
                    }
                }
            }
        };
        for bits in specials_f32() {
            check(V::c(bits as Int));
            check(V::partial(0xFFFF_FF00, bits as Int));
        }
        check(V::UNK);
        let mut rng = Rng(0x0BAD_5EED_1234_5678);
        for _ in 0..2_000_000u64 {
            let (a, _) = pair(&mut rng);
            check(V::c(a as Int));
        }
        // Every exponent field, both signs, mantissa zero/nonzero.
        for e in 0..256u32 {
            for sign in [0u32, 0x8000_0000] {
                for m in [0u32, 1, 0x7F_FFFF] {
                    check(V::c((sign | e << 23 | m) as Int));
                }
            }
        }
        assert!(count >= 10_000_000);
        eprintln!("astatx_mult_float differential: {count} comparisons, 0 mismatches");
    }

    /// compute_multi._float_multiply_parts as the generated code ran it
    /// before it became a boundary function.
    fn float_multiply_parts_ref(s: &St, left: V, right: V) -> (V, FlagUpdate) {
        if !left.is_c() || !right.is_c() {
            return (
                V::UNK,
                FlagUpdate {
                    define_mask: 0,
                    define_bits: 0,
                    forget_mask: 960,
                    cacc: -1,
                },
            );
        }
        let (a, b) = (operand_ref(s, left), operand_ref(s, right));
        let raw = fmul(a, b);
        let (bits, overflowed, underflowed) = result_ref(s, raw);
        let value = V::c(bits);
        if raw.is_nan() {
            return (
                value,
                FlagUpdate {
                    define_mask: 512,
                    define_bits: 512,
                    forget_mask: 448,
                    cacc: -1,
                },
            );
        }
        (
            value,
            astatx_mult_float_ref(s, value, Some(overflowed), Some(underflowed), Some(false)),
        )
    }

    #[test]
    fn float_multiply_parts_and_float32_match_reference() {
        let s = St::new(crate::mem::Mem::new());
        let mut count = 0u64;
        let mut check = |a: V, b: V| {
            assert_eq!(
                _float_multiply_parts(&s, a, b, S_EMPTY),
                float_multiply_parts_ref(&s, a, b),
                "a={:#x}/{:#x} b={:#x}/{:#x}",
                a.b,
                a.m,
                b.b,
                b.m
            );
            count += 1;
        };
        let sp = specials_f32();
        for &a in &sp {
            for &b in &sp {
                check(V::c(a as Int), V::c(b as Int));
            }
            check(V::UNK, V::c(a as Int));
            check(V::c(a as Int), V::UNK);
            check(V::partial(0xFFFF_0000, a as Int), V::c(a as Int));
            let want = if a & 0x7F80_0000 == 0 {
                f32::from_bits(a & 0x8000_0000) as f64
            } else if f32::from_bits(a).is_nan() {
                _f32_from_bits(&s, a as Int)
            } else {
                f32::from_bits(a) as f64
            };
            let got = _float32(&s, V::c(a as Int)).unwrap();
            assert!(got.to_bits() == want.to_bits() || (got.is_nan() && want.is_nan()));
            assert_eq!(_float32(&s, V::UNK), None);
            assert_eq!(_float32(&s, V::partial(0xFFFF_0000, a as Int)), None);
        }
        let mut rng = Rng(0x7777_1234_5678_9ABC);
        for _ in 0..4_000_000u64 {
            let (a, b) = pair(&mut rng);
            check(V::c(a as Int), V::c(b as Int));
        }
        assert!(count >= 4_000_000);
    }
}
