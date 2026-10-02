//! Firmware-independent SHARC+ instruction decoding.
//!
//! The generated table is derived solely from the public ISA table.  A caller
//! provides a short-word reader, so the same decoder can run over any loaded
//! firmware image without embedding its blocks or symbols.

use core::fmt;

const MAX_FIELDS: usize = 12;
const WIDTH_LOOKAHEAD: usize = 8;
const WIDTH_REJOIN_LOOKAHEAD: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeKind {
    Confident,
    Uncertain,
    Unknown,
}

impl DecodeKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Confident => "confident",
            Self::Uncertain => "uncertain",
            Self::Unknown => "unknown",
        }
    }
}

/// Why a decode has no instruction.  This retains the Python decoder's
/// fail-closed distinction without coupling the runtime to Python strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeNote {
    None,
    Unmapped,
    NoForm,
    Ambiguous,
    Truncated {
        type_name: &'static str,
        expected_bytes: u8,
        available_bytes: u8,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldValue {
    pub key: &'static str,
    pub stem: &'static str,
    pub hi: i8,
    pub lo: i8,
    pub value: i64,
}

const EMPTY_FIELD: FieldValue = FieldValue {
    key: "",
    stem: "",
    hi: -1,
    lo: -1,
    value: 0,
};

/// A decoded instruction.  `type_name`, fields, kind and length map directly
/// to `rt::Insn` once the host has interned the string identifiers.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Decoded {
    pub type_name: &'static str,
    pub length_bytes: Option<u8>,
    pub kind: DecodeKind,
    pub note: DecodeNote,
    fields: [FieldValue; MAX_FIELDS],
    field_len: u8,
}

impl fmt::Debug for Decoded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Decoded")
            .field("type_name", &self.type_name)
            .field("length_bytes", &self.length_bytes)
            .field("kind", &self.kind)
            .field("fields", &self.fields())
            .finish()
    }
}

impl Decoded {
    #[inline]
    pub const fn unknown() -> Self {
        Self {
            type_name: "unknown",
            length_bytes: None,
            kind: DecodeKind::Unknown,
            note: DecodeNote::NoForm,
            fields: [EMPTY_FIELD; MAX_FIELDS],
            field_len: 0,
        }
    }

    #[inline]
    pub fn fields(&self) -> &[FieldValue] {
        &self.fields[..self.field_len as usize]
    }
}

#[derive(Clone, Copy)]
pub(super) struct FieldSpec {
    key: &'static str,
    stem: &'static str,
    hi: i8,
    lo: i8,
}

#[derive(Clone, Copy)]
pub(super) struct Form {
    name: &'static str,
    width_bytes: u8,
    mask: u64,
    value: u64,
    leading_fixed_bits: u8,
    fixed_bits: u8,
    kind: DecodeKind,
    fields: &'static [FieldSpec],
}

include!("decode_table.rs");

#[inline]
fn frame(words: &[u16]) -> u64 {
    words
        .iter()
        .take(3)
        .enumerate()
        .fold(0, |out, (index, word)| {
            out | ((*word as u64) << (32 - 16 * index))
        })
}

enum Selection {
    Form(&'static Form),
    NoForm,
    Ambiguous,
}

fn select(words: &[u16]) -> Selection {
    let frame = frame(words);
    let mut leading = 0;
    let mut fixed = 0;
    let mut winner = None;
    let mut tied = false;
    for form in FORMS {
        if frame & form.mask != form.value {
            continue;
        }
        if winner.is_none()
            || form.leading_fixed_bits > leading
            || (form.leading_fixed_bits == leading && form.fixed_bits > fixed)
        {
            leading = form.leading_fixed_bits;
            fixed = form.fixed_bits;
            winner = Some(form);
            tied = false;
        } else if form.leading_fixed_bits == leading && form.fixed_bits == fixed {
            tied = true;
        }
    }
    if tied {
        Selection::Ambiguous
    } else if let Some(form) = winner {
        Selection::Form(form)
    } else {
        Selection::NoForm
    }
}

fn decode_words(words: &[u16]) -> Decoded {
    let form = match select(words) {
        Selection::Form(form) => form,
        Selection::NoForm => return Decoded::unknown(),
        Selection::Ambiguous => {
            let mut decoded = Decoded::unknown();
            decoded.note = DecodeNote::Ambiguous;
            return decoded;
        }
    };
    if words.len() < (form.width_bytes / 2) as usize {
        let mut decoded = Decoded::unknown();
        decoded.note = DecodeNote::Truncated {
            type_name: form.name,
            expected_bytes: form.width_bytes,
            available_bytes: (words.len() * 2) as u8,
        };
        return decoded;
    }
    let raw = frame(words);
    let mut fields = [EMPTY_FIELD; MAX_FIELDS];
    for (index, spec) in form.fields.iter().enumerate() {
        debug_assert!(index < MAX_FIELDS);
        let width = (spec.hi - spec.lo + 1) as u32;
        fields[index] = FieldValue {
            key: spec.key,
            stem: spec.stem,
            hi: ranged_hi(spec.key),
            lo: ranged_lo(spec.key),
            value: ((raw >> spec.lo) & ((1_u64 << width) - 1)) as i64,
        };
    }
    Decoded {
        type_name: form.name,
        length_bytes: Some(form.width_bytes),
        kind: form.kind,
        note: DecodeNote::None,
        fields,
        field_len: form.fields.len() as u8,
    }
}

fn ranged_hi(key: &str) -> i8 {
    let Some((_, range)) = key.split_once('[') else {
        return -1;
    };
    range
        .split_once(':')
        .and_then(|(hi, _)| hi.parse().ok())
        .unwrap_or(-1)
}

fn ranged_lo(key: &str) -> i8 {
    let Some((_, range)) = key.split_once(':') else {
        return -1;
    };
    range
        .strip_suffix(']')
        .and_then(|lo| lo.parse().ok())
        .unwrap_or(-1)
}

fn loaded_words(read_word: &mut impl FnMut(u32) -> Option<u16>, pc_sw: u32) -> [Option<u16>; 3] {
    let first = read_word(pc_sw);
    let second = first.and_then(|_| read_word(pc_sw + 1));
    let third = second.and_then(|_| read_word(pc_sw + 2));
    [first, second, third]
}

fn decode_raw(read_word: &mut impl FnMut(u32) -> Option<u16>, pc_sw: u32) -> Decoded {
    let loaded = loaded_words(read_word, pc_sw);
    let count = loaded.iter().take_while(|word| word.is_some()).count();
    let mut words = [0_u16; 3];
    for (dst, src) in words.iter_mut().zip(loaded) {
        *dst = src.unwrap_or(0);
    }
    if count == 0 {
        let mut decoded = Decoded::unknown();
        decoded.note = DecodeNote::Unmapped;
        decoded
    } else {
        decode_words(&words[..count])
    }
}

// Match sharc_disasm.NEVER_ALIGNED_FORMS: a direct landing may report this
// legacy form, but it cannot supply confidence to another form's successor
// chain.  `10a_abs` is not a VISA row and therefore is absent from FORMS.
#[inline]
fn successor_raw(read_word: &mut impl FnMut(u32) -> Option<u16>, pc_sw: u32) -> Decoded {
    let decoded = decode_raw(read_word, pc_sw);
    if decoded.type_name == "10a_rel" {
        Decoded::unknown()
    } else {
        decoded
    }
}

fn narrow_candidates(words: &[u16], max_bytes: u8, out: &mut [Option<&'static Form>; 2]) -> usize {
    let raw = frame(words);
    let mut count = 0;
    for width in [2_u8, 4] {
        if width >= max_bytes || words.len() < (width / 2) as usize {
            continue;
        }
        let mut fixed = 0;
        let mut candidate = None;
        let mut tied = false;
        for form in FORMS {
            if form.width_bytes != width || raw & form.mask != form.value {
                continue;
            }
            if candidate.is_none() || form.fixed_bits > fixed {
                fixed = form.fixed_bits;
                candidate = Some(form);
                tied = false;
            } else if form.fixed_bits == fixed {
                tied = true;
            }
        }
        if !tied && candidate.is_some() {
            out[count] = candidate;
            count += 1;
        }
    }
    count
}

fn decode_form(words: &[u16], form: &'static Form) -> Decoded {
    let raw = frame(words);
    let mut fields = [EMPTY_FIELD; MAX_FIELDS];
    for (index, spec) in form.fields.iter().enumerate() {
        let width = (spec.hi - spec.lo + 1) as u32;
        fields[index] = FieldValue {
            key: spec.key,
            stem: spec.stem,
            hi: ranged_hi(spec.key),
            lo: ranged_lo(spec.key),
            value: ((raw >> spec.lo) & ((1_u64 << width) - 1)) as i64,
        };
    }
    Decoded {
        type_name: form.name,
        length_bytes: Some(form.width_bytes),
        kind: form.kind,
        note: DecodeNote::None,
        fields,
        field_len: form.fields.len() as u8,
    }
}

fn successor_confidence(
    read_word: &mut impl FnMut(u32) -> Option<u16>,
    pc_sw: u32,
    first: Decoded,
    lookahead: usize,
) -> ([u32; WIDTH_REJOIN_LOOKAHEAD + 1], usize, bool) {
    let mut boundaries = [0; WIDTH_REJOIN_LOOKAHEAD + 1];
    boundaries[0] = pc_sw;
    let mut len = 1;
    let mut current = first;
    let mut pc = pc_sw;
    for _ in 0..lookahead {
        if current.kind != DecodeKind::Confident {
            return (boundaries, len, false);
        }
        pc += (current.length_bytes.expect("non-unknown confident") / 2) as u32;
        boundaries[len] = pc;
        len += 1;
        current = successor_raw(read_word, pc);
    }
    (boundaries, len, true)
}

/// Decode one instruction at a short-word PC.  The reader must return `None`
/// at an unmapped word; a missing word never becomes an invented instruction.
pub fn decode_at(mut read_word: impl FnMut(u32) -> Option<u16>, pc_sw: u32) -> Decoded {
    let wide = decode_raw(&mut read_word, pc_sw);
    if wide.kind == DecodeKind::Unknown || wide.length_bytes.unwrap() <= 2 {
        return wide;
    }
    let loaded = loaded_words(&mut read_word, pc_sw);
    let count = loaded.iter().take_while(|word| word.is_some()).count();
    let mut words = [0_u16; 3];
    for (dst, src) in words.iter_mut().zip(loaded) {
        *dst = src.unwrap_or(0);
    }
    let mut candidates = [None; 2];
    let candidate_count =
        narrow_candidates(&words[..count], wide.length_bytes.unwrap(), &mut candidates);
    if candidate_count == 0 {
        return wide;
    }
    let (wide_bounds, wide_len, wide_clean) =
        successor_confidence(&mut read_word, pc_sw, wide, WIDTH_LOOKAHEAD);
    for form in candidates.into_iter().take(candidate_count).flatten() {
        if form.kind != DecodeKind::Confident {
            continue;
        }
        let candidate = decode_form(&words[..count], form);
        let (candidate_bounds, candidate_len, candidate_clean) =
            successor_confidence(&mut read_word, pc_sw, candidate, WIDTH_LOOKAHEAD);
        if !candidate_clean {
            continue;
        }
        let rejoins = candidate_bounds[..candidate_len]
            .iter()
            .skip(1)
            .any(|boundary| wide_bounds[1..wide_len].contains(boundary));
        if !wide_clean || rejoins {
            return candidate;
        }
        if wide.type_name == "2b" && candidate.type_name == "2c" {
            let (wide_bounds, wide_len, _) =
                successor_confidence(&mut read_word, pc_sw, wide, WIDTH_REJOIN_LOOKAHEAD);
            let (candidate_bounds, candidate_len, _) =
                successor_confidence(&mut read_word, pc_sw, candidate, WIDTH_REJOIN_LOOKAHEAD);
            if candidate_bounds[..candidate_len]
                .iter()
                .skip(1)
                .any(|boundary| wide_bounds[1..wide_len].contains(boundary))
            {
                return candidate;
            }
        }
    }
    wide
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words_at(words: &[u16], pc: u32) -> Option<u16> {
        words.get(pc as usize).copied()
    }

    #[test]
    fn decodes_and_preserves_field_layout() {
        let words = [0x0100, 0x0042, 0x1234];
        let decoded = decode_at(|pc| words_at(&words, pc), 0);
        assert_eq!(decoded.type_name, "2a");
        assert_eq!(decoded.length_bytes, Some(6));
        assert_eq!(decoded.kind, DecodeKind::Confident);
        assert_eq!(decoded.fields()[0].key, "cond[4:0]");
        assert_eq!(decoded.fields()[0].hi, 4);
        assert_eq!(decoded.fields()[0].lo, 0);
    }

    #[test]
    fn missing_second_word_fails_closed() {
        let decoded = decode_at(|pc| (pc == 0).then_some(0x0100), 0);
        assert_eq!(decoded.kind, DecodeKind::Unknown);
        assert_eq!(decoded.length_bytes, None);
        assert!(matches!(decoded.note, DecodeNote::Truncated { .. }));
    }

    #[test]
    fn unmapped_pc_has_python_unknown_metadata() {
        let decoded = decode_at(|_| None, 0);
        assert_eq!(decoded.type_name, "unknown");
        assert_eq!(decoded.kind, DecodeKind::Unknown);
        assert_eq!(decoded.length_bytes, None);
        assert_eq!(decoded.note, DecodeNote::Unmapped);
    }

    #[test]
    fn uncertainty_is_reported() {
        let words = [0x8000, 0, 0];
        let decoded = decode_at(|pc| words_at(&words, pc), 0);
        assert_eq!(decoded.type_name, "6a_nomem");
        assert_eq!(decoded.kind, DecodeKind::Uncertain);
    }

    #[test]
    fn whole_compute_keeps_the_rsgen_runtime_field_shape() {
        let words = [0x4000, 0x0000, 0x1234];
        let decoded = decode_at(|pc| words_at(&words, pc), 0);
        assert_eq!(decoded.type_name, "3a");
        assert_eq!(decoded.fields()[8].key, "compute");
        assert_eq!(decoded.fields()[8].value, 0x1234);
        assert_eq!(decoded.fields()[9].key, "compute[22:16]");
        assert_eq!(decoded.fields()[9].value, 0);
        assert_eq!(decoded.fields()[10].key, "compute[15:0]");
        assert_eq!(decoded.fields()[10].value, 0x1234);
    }

    #[test]
    fn successor_confidence_corrects_a_wide_narrow_collision() {
        // Public-format synthetic short ADD followed by immediate loads.
        // The wider interpretation consumes the first load's opcode.
        let mut words = vec![0xc000];
        for i in 0..12_u16 {
            words.extend([0x0f04 + i % 8, 0, 0x1000 + i]);
        }
        let decoded = decode_at(|pc| words_at(&words, pc), 0);
        assert_eq!(decoded.type_name, "2c");
        assert_eq!(decoded.length_bytes, Some(2));
        assert_eq!(decoded.kind, DecodeKind::Confident);
    }

    #[test]
    fn short_compute_rejoins_after_ten_loads_despite_uncertain_tail() {
        // Synthetic public-format ADD, ten immediate loads and NOP.
        // The alternative stream reads the constants as BIT instructions.
        let mut words = vec![0xc020];
        for i in 0..10_u16 {
            words.extend([0x0f00 + i, 0x1400, 0x0100]);
        }
        words.extend([0x0001, 0x8000, 0, 0]);
        assert_eq!(
            decode_raw(&mut |pc| words_at(&words, pc), 0).type_name,
            "2b"
        );
        let decoded = decode_at(|pc| words_at(&words, pc), 0);
        assert_eq!(decoded.type_name, "2c");
        assert_eq!(decoded.length_bytes, Some(2));
    }
}
