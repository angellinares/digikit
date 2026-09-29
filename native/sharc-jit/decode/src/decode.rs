//! The single-PC SHARC+ VISA decoder itself: a firmware-free port of
//! `tools/sharc_core/sequencer.py::decode_at()`'s `LoadedMemory` path, i.e.
//! `tools/sharc_disasm.py::decode_confident_loaded()` --
//! `decode_loaded_at()` (try the largest of a 6/4/2-byte window that is
//! fully mapped, decode it with `InstructionSet.decode_words()`) followed by
//! `resolve_confident_width()`'s successor-confidence width correction
//! (`WIDTH_LOOKAHEAD = 8`, `NEVER_ALIGNED_FORMS` excluded from a successor
//! chain, `_narrow_candidates_from_words()` for the narrower-form
//! candidates). See `tools/sharc_disasm.py`'s module docstring and its
//! `resolve_confident_width` docstring for the rationale this file mirrors
//! line for line; every public item below cites the Python function it
//! ports.
//!
//! Pure and deterministic: no I/O, no threads, no time, and every ordered
//! collection here is a `Vec` walked in a fixed order (never a hash map), so
//! the exact-match behaviour does not depend on this crate's own iteration
//! order the way `tools/sharc_isa.py`'s dict-based Python does not either
//! (Python dicts already preserve insertion order; this is the same
//! property, not a coincidence).

use crate::isa::Form;

/// The successor-confidence lookahead depth
/// (`tools/sharc_disasm.py::WIDTH_LOOKAHEAD`).
const WIDTH_LOOKAHEAD: u32 = 8;

/// Loader-backed memory, read by short-word PC: the same contract as
/// `sharcldr.LoadedMemory.read_sw(pc_sw, size)` (see `tools/sharcldr.py`).
/// `read_sw` returns `Some` only when *every* one of `size_bytes` bytes
/// starting at short-word address `pc_sw` is mapped in the final loaded
/// image (last loader-stream write wins); otherwise `None`, exactly like
/// `LoadedMemory.read_sw`'s own `None` for "any gap".
///
/// `size_bytes` is always 2, 4 or 6 (a VISA instruction's three possible
/// widths). The returned array holds `size_bytes / 2` little-endian 16-bit
/// words in elements `[0, size_bytes/2)`; elements beyond that are
/// unspecified (the decoder never reads them for that call).
///
/// `sharcldr.LoadedMemory.read_sw`'s own mapping, which any real
/// implementation (e.g. [`crate::image::SegmentImage`]) must reproduce
/// exactly:
///
/// 1. Primary: `byte_addr = 2 * pc_sw + SW_ALIAS_BASE` (`SW_ALIAS_BASE =
///    0x2800_0000`). If every byte of `[byte_addr, byte_addr+size_bytes)` is
///    mapped, that is the answer.
/// 2. Otherwise, only when `pc_sw >= L2_SW_BASE` (`0x00B8_0000`): try the L2
///    boot-ROM alias, `fallback = L2_BYTE_BASE + 2*(pc_sw - L2_SW_BASE)`
///    (`L2_BYTE_BASE = 0x2000_0000`), and only when
///    `fallback + size_bytes <= L2_BYTE_LIMIT` (`0x2010_0000`) -- the
///    documented 1 MiB L2 SRAM window, not one 128 KiB bank (see
///    `tools/sharcldr.py`'s `L2_BYTE_LIMIT` comment). If every byte there is
///    mapped, that is the answer; otherwise `None`.
pub trait ShortWords {
    fn read_sw(&self, pc_sw: u32, size_bytes: u32) -> Option<[u16; 3]>;
}

/// One decoded instruction, in the shape a run-time JIT wants: enough to
/// dispatch on `type_name`/`kind` and read operand fields by label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decoded {
    /// The form id (`tools/sharc_isa.py::form_id`, e.g. `"5b_move"`), or
    /// `"unknown"` when nothing decodes here.
    pub type_name: String,
    /// `"confident"` (every fixed bit of the winning form is documented),
    /// `"uncertain"` (the table marks some fixed bits unconfirmed), or
    /// `"unknown"` (no form matched, several tied, or the buffer was too
    /// short -- see `tools/sharc_disasm.py::disassemble`'s module
    /// docstring).
    pub kind: &'static str,
    /// The instruction's width in bytes (2, 4 or 6), or `None` for
    /// `"unknown"`.
    pub length_bytes: Option<u32>,
    /// `{label: value}` in the winning form's own field order, exactly as
    /// Python's `dict` preserves insertion order -- with
    /// `tools/sharc_rsgen.py::split_compute()` applied: a form with a bare
    /// `"compute"` field (only `Type3a`/`"3a"` in the current table) gets two
    /// extra entries appended at the end, `"compute[22:16]"` (the field
    /// shifted right 16 bits) and `"compute[15:0]"` (the field's low 16
    /// bits); the original `"compute"` entry is kept too, exactly as the
    /// Python dict-copy-then-assign leaves it.
    pub fields: Vec<(String, i64)>,
}

const KIND_CONFIDENT: &str = "confident";
const KIND_UNCERTAIN: &str = "uncertain";
const KIND_UNKNOWN: &str = "unknown";

/// A decoded instruction at one position, before `type_name`/`kind` are
/// rendered into [`Decoded`] -- mirrors `tools/sharc_disasm.py::Instruction`
/// restricted to the fields `resolve_confident_width` actually reads
/// (`length_bytes`, `kind`) plus what the caller ultimately wants
/// (`form_idx`, `fields`). `form_idx.is_none()` is Python's `kind ==
/// "unknown"`.
#[derive(Clone)]
struct Insn {
    form_idx: Option<usize>,
    length_bytes: Option<u32>,
    fields: Vec<(String, i64)>,
    /// Meaningful only when `form_idx.is_some()`: true for `"confident"`,
    /// false for `"uncertain"`.
    confident: bool,
}

impl Insn {
    fn unknown() -> Insn {
        Insn {
            form_idx: None,
            length_bytes: None,
            fields: Vec::new(),
            confident: false,
        }
    }

    #[inline]
    fn is_unknown(&self) -> bool {
        self.form_idx.is_none()
    }

    #[inline]
    fn probe(&self) -> Probe {
        Probe {
            form_idx: self.form_idx,
            length_bytes: self.length_bytes,
            confident: self.confident,
        }
    }
}

/// [`Insn`] without its `fields`: what the successor-confidence lookahead
/// (`confident_reach`) actually needs at each position it walks through.
/// Skipping field extraction there (each field label would otherwise be
/// cloned into a fresh `String`, up to `WIDTH_LOOKAHEAD` times per decode
/// for positions whose fields are never read) is the difference between a
/// probe and a real decode; the two width-correction candidates
/// (`narrow_candidates_from_words`) and the final answer still compute
/// fields in full, since either may become the returned [`Decoded`].
#[derive(Clone, Copy)]
struct Probe {
    form_idx: Option<usize>,
    length_bytes: Option<u32>,
    confident: bool,
}

impl Probe {
    fn unknown() -> Probe {
        Probe {
            form_idx: None,
            length_bytes: None,
            confident: false,
        }
    }
}

/// The 48-bit MSB-aligned frame of up to three short words, most
/// significant first, zero-padded (`tools/sharc_isa.py::frame_of`).
fn frame_of(words: &[u16]) -> u64 {
    let mut frame: u64 = 0;
    for (index, &word) in words.iter().take(3).enumerate() {
        frame |= (word as u64) << (32 - 16 * index as u32);
    }
    frame
}

/// The immutable, VISA-mode instruction table plus the decode algorithm
/// (`tools/sharc_isa.py::InstructionSet` + `tools/sharc_disasm.py`'s
/// decode-at-a-PC functions). Stateless and `Sync`: build one and share it
/// across threads or calls.
pub struct Decoder {
    forms: Vec<Form>,
    /// Every distinct `extent_bits` among `forms`, ascending (in the shipped
    /// table: `[16, 32, 48]`). Precomputed once so
    /// `narrow_candidates_from_words` does not re-collect and sort this on
    /// every call.
    distinct_widths: Vec<u32>,
}

impl Decoder {
    /// Parse the embedded copy of `tools/sharcspec/decode_table.json` once,
    /// keeping only its VISA forms (`InstructionSet.from_json(mode="visa")`).
    ///
    /// # Panics
    /// If the embedded table fails to parse -- it is compiled into this
    /// crate from the repository's own `tools/sharcspec/decode_table.json`,
    /// so this can only happen if that file is malformed at build time.
    pub fn new() -> Decoder {
        let forms = crate::isa::load_visa_forms(crate::isa::DECODE_TABLE_JSON)
            .expect("embedded tools/sharcspec/decode_table.json failed to parse");
        let mut distinct_widths: Vec<u32> = forms.iter().map(|f| f.extent_bits).collect();
        distinct_widths.sort_unstable();
        distinct_widths.dedup();
        Decoder {
            forms,
            distinct_widths,
        }
    }

    /// `tools/sharc_core/sequencer.py::decode_at(mem, None, pc_sw)`'s
    /// `LoadedMemory` path, i.e.
    /// `tools/sharc_disasm.py::decode_confident_loaded(mem, pc_sw)`.
    pub fn decode_at(&self, mem: &dyn ShortWords, pc_sw: u32) -> Decoded {
        let insn = self.decode_loaded_at(mem, pc_sw);
        let insn = if insn.is_unknown() {
            insn
        } else {
            // decode_confident_loaded: pos = 2 * pc_sw (a byte offset).
            self.resolve_confident_width(mem, pc_sw.wrapping_mul(2), insn)
        };
        self.render(insn)
    }

    /// Whether a form named in IDS matches the words at PC_SW (a cheap
    /// superset test before `decode_at`: the decoded form at PC_SW is one of
    /// IDS only if this is true).
    pub fn may_be(&self, mem: &dyn ShortWords, pc_sw: u32, ids: &[&str]) -> bool {
        for size in [6u32, 4, 2] {
            if let Some(words) = mem.read_sw(pc_sw, size) {
                let n = (size / 2) as usize;
                let frame = frame_of(&words[..n]);
                return self
                    .forms
                    .iter()
                    .any(|f| ids.contains(&f.id.as_str()) && f.extent_words() as usize <= n && f.matches(frame));
            }
        }
        false
    }

    fn render(&self, insn: Insn) -> Decoded {
        match insn.form_idx {
            None => Decoded {
                type_name: KIND_UNKNOWN.to_string(),
                kind: KIND_UNKNOWN,
                length_bytes: None,
                fields: Vec::new(),
            },
            Some(idx) => {
                let form = &self.forms[idx];
                Decoded {
                    type_name: form.id.clone(),
                    kind: if insn.confident {
                        KIND_CONFIDENT
                    } else {
                        KIND_UNCERTAIN
                    },
                    length_bytes: insn.length_bytes,
                    fields: split_compute(insn.fields),
                }
            }
        }
    }

    /// `tools/sharc_disasm.py::decode_loaded_at`: the largest of a 6/4/2-byte
    /// window at `pc_sw` that is fully mapped, decoded as one instruction;
    /// `Insn::unknown()` ("PC unmapped in loader memory") if none is.
    fn decode_loaded_at(&self, mem: &dyn ShortWords, pc_sw: u32) -> Insn {
        for size in [6u32, 4, 2] {
            if let Some(words) = mem.read_sw(pc_sw, size) {
                let n = (size / 2) as usize;
                return self.decode_words(&words[..n]);
            }
        }
        Insn::unknown()
    }

    /// `sharc_isa.InstructionSet.decode_words` + `sharc_disasm.disassemble`'s
    /// success/failure split: match `words` (1-3 of them) against every
    /// form, and only accept a unique winner that `words` is long enough to
    /// cover. Shared by [`Decoder::decode_words`] (which also extracts
    /// fields) and [`Decoder::decode_words_probe`] (which does not).
    fn match_words(&self, words: &[u16]) -> Option<(usize, u32, bool)> {
        let frame = frame_of(words);
        let idx = self.select_frame(frame)?;
        let form = &self.forms[idx];
        if (words.len() as u32) < form.extent_words() {
            return None;
        }
        Some((idx, form.extent_bits / 8, !form.uncertain))
    }

    fn decode_words(&self, words: &[u16]) -> Insn {
        match self.match_words(words) {
            None => Insn::unknown(),
            Some((idx, length_bytes, confident)) => Insn {
                form_idx: Some(idx),
                length_bytes: Some(length_bytes),
                fields: self.forms[idx].extract_fields(frame_of(words)),
                confident,
            },
        }
    }

    /// Same match as [`Decoder::decode_words`], without extracting fields
    /// (see [`Probe`]).
    fn decode_words_probe(&self, words: &[u16]) -> Probe {
        match self.match_words(words) {
            None => Probe::unknown(),
            Some((idx, length_bytes, confident)) => Probe {
                form_idx: Some(idx),
                length_bytes: Some(length_bytes),
                confident,
            },
        }
    }

    /// `sharc_isa.InstructionSet.select_frame`: among the forms whose mask
    /// matches `frame`, the longest run of fixed bits from bit 47 wins, then
    /// the most fixed bits overall; a unique winner is `Some`, a tie or no
    /// match is `None`.
    ///
    /// This is `decode_at`'s hottest call (up to `WIDTH_LOOKAHEAD` times per
    /// successor-confidence probe, on top of the initial decode), so unlike
    /// the two-pass filter-then-filter Python reads as, it finds the winner
    /// in one pass with no allocation: maximizing `(leading_fixed_bits,
    /// fixed_bits)` lexicographically over the matching forms is exactly
    /// Python's "most leading fixed bits, then most fixed bits" rule, and
    /// counting how many forms tie for that lexicographic maximum is exactly
    /// Python's `len(candidates) == 1` uniqueness check -- `Selection`'s
    /// full candidate list is never a caller's contract here, only whether a
    /// winner exists.
    fn select_frame(&self, frame: u64) -> Option<usize> {
        let mut best: Option<(u32, u32)> = None;
        let mut best_idx = usize::MAX;
        let mut ties = 0u32;
        for (i, form) in self.forms.iter().enumerate() {
            if !form.matches(frame) {
                continue;
            }
            let key = (form.leading_fixed_bits, form.fixed_bits);
            match best {
                Some(b) if key < b => {}
                Some(b) if key == b => ties += 1,
                _ => {
                    best = Some(key);
                    best_idx = i;
                    ties = 1;
                }
            }
        }
        if ties == 1 { Some(best_idx) } else { None }
    }

    /// `tools/sharc_disasm.py::_raw_at_loaded`, for the successor-confidence
    /// lookahead only: the uncorrected single-form decode at byte position
    /// `pos` (must be even), or `None` where nothing decodes there at all,
    /// or only a `NEVER_ALIGNED_FORMS` decode trap does. Fields are never
    /// read along this path (see [`Probe`]), so this decodes at the largest
    /// mapped window without extracting them.
    fn raw_at_loaded_probe(&self, mem: &dyn ShortWords, pos: u32) -> Option<Probe> {
        if !pos.is_multiple_of(2) {
            return None;
        }
        let pc_sw = pos / 2;
        for size in [6u32, 4, 2] {
            if let Some(words) = mem.read_sw(pc_sw, size) {
                let n = (size / 2) as usize;
                let probe = self.decode_words_probe(&words[..n]);
                return match probe.form_idx {
                    None => None,
                    Some(idx) if self.forms[idx].never_aligned => None,
                    Some(_) => Some(probe),
                };
            }
        }
        None
    }

    /// `tools/sharc_disasm.py::_loaded_words`: up to three 16-bit words at
    /// `pc_sw`, from the largest contiguous mapped window (6, 4, then 2
    /// bytes); empty when none of those is fully mapped.
    fn loaded_words(&self, mem: &dyn ShortWords, pc_sw: u32) -> Vec<u16> {
        for size in [6u32, 4, 2] {
            if let Some(words) = mem.read_sw(pc_sw, size) {
                let n = (size / 2) as usize;
                return words[..n].to_vec();
            }
        }
        Vec::new()
    }

    /// `tools/sharc_disasm.py::_narrow_candidates_from_words`: every
    /// confident/uncertain instruction decodable at byte position `offset`
    /// (an even number; short-word PC `offset/2`) from forms narrower than
    /// `max_bits`, picking -- independently at each width -- the form with
    /// the most fixed bits among those of exactly that width whose mask
    /// matches, skipping a width with no match or a tie. This does not use
    /// `select_frame`'s cross-width "most leading fixed bits" ranking: each
    /// width is judged only against other forms of the same width.
    fn narrow_candidates_from_words(
        &self,
        mem: &dyn ShortWords,
        offset: u32,
        max_bits: u32,
    ) -> Vec<Insn> {
        let words = self.loaded_words(mem, offset / 2);
        if words.is_empty() {
            return Vec::new();
        }
        let frame = frame_of(&words);
        let mut out = Vec::new();
        for &width in self.distinct_widths.iter().filter(|&&w| w < max_bits) {
            let need_words = (width / 16) as usize;
            if words.len() < need_words {
                continue;
            }
            let matching: Vec<usize> = (0..self.forms.len())
                .filter(|&i| self.forms[i].extent_bits == width && self.forms[i].matches(frame))
                .collect();
            if matching.is_empty() {
                continue;
            }
            let best = matching
                .iter()
                .map(|&i| self.forms[i].fixed_bits)
                .max()
                .unwrap();
            let ties: Vec<usize> = matching
                .into_iter()
                .filter(|&i| self.forms[i].fixed_bits == best)
                .collect();
            if ties.len() != 1 {
                continue;
            }
            let idx = ties[0];
            let form = &self.forms[idx];
            out.push(Insn {
                form_idx: Some(idx),
                length_bytes: Some(width / 8),
                fields: form.extract_fields(frame),
                confident: !form.uncertain,
            });
        }
        out
    }

    /// `tools/sharc_disasm.py::confident_reach` (the nested closure in
    /// `resolve_confident_width`): the set of byte positions reached by
    /// walking up to `WIDTH_LOOKAHEAD` further confident instructions from
    /// `(pos0, insn0)`, and whether all of them stayed confident (`false` as
    /// soon as one does not, or is itself unknown).
    fn confident_reach(&self, mem: &dyn ShortWords, pos0: u32, start: Probe) -> (Vec<u32>, bool) {
        let mut boundaries = vec![pos0];
        let mut cur = Some(start);
        let mut p = pos0;
        for _ in 0..WIDTH_LOOKAHEAD {
            let Some(c) = cur else {
                return (boundaries, false);
            };
            if c.form_idx.is_none() || !c.confident {
                return (boundaries, false);
            }
            p = p.wrapping_add(c.length_bytes.expect("confident insn has a length"));
            boundaries.push(p);
            cur = self.raw_at_loaded_probe(mem, p);
        }
        (boundaries, true)
    }

    /// `tools/sharc_disasm.py::resolve_confident_width`: prefer a narrower
    /// form over a wider one at the same position when the narrower form's
    /// own successor chain decodes confidently for `WIDTH_LOOKAHEAD`
    /// instructions where the wider form's does not, or independently
    /// rejoins one of the wider form's own successor boundaries.
    fn resolve_confident_width(&self, mem: &dyn ShortWords, pos: u32, insn: Insn) -> Insn {
        let Some(length_bytes) = insn.length_bytes else {
            return insn;
        };
        if length_bytes <= 2 {
            return insn;
        }
        let max_bits = length_bytes * 8;
        let (wide_boundaries, wide_clean) = self.confident_reach(mem, pos, insn.probe());
        let candidates = self.narrow_candidates_from_words(mem, pos, max_bits);
        if candidates.is_empty() {
            return insn;
        }
        let wide_targets: Vec<u32> = wide_boundaries.into_iter().filter(|&b| b != pos).collect();
        for cand in candidates {
            if cand.form_idx.is_none() || !cand.confident {
                continue;
            }
            let (cand_boundaries, cand_clean) = self.confident_reach(mem, pos, cand.probe());
            if !cand_clean {
                continue;
            }
            if !wide_clean || cand_boundaries.iter().any(|b| wide_targets.contains(b)) {
                return cand;
            }
        }
        insn
    }
}

impl Default for Decoder {
    fn default() -> Decoder {
        Decoder::new()
    }
}

/// `tools/sharc_rsgen.py::split_compute`, applied to every instruction's
/// fields (the native code generator applies it once, at generation time,
/// to instructions with a whole `"compute"` field; this crate's decoder
/// applies the identical rule to every decode, since it has no separate
/// generation step). A bare `"compute"` entry (only form `"3a"` in the
/// current table) gets two more entries appended: `"compute[22:16]"`
/// (`compute >> 16`) and `"compute[15:0]"` (`compute & 0xFFFF`). The
/// original `"compute"` entry is kept, matching Python's `dict(fields)`
/// copy-then-assign (assigning a new key to a dict appends it, preserving
/// every existing entry including `"compute"` itself).
fn split_compute(fields: Vec<(String, i64)>) -> Vec<(String, i64)> {
    let Some(compute) = fields.iter().find(|(k, _)| k == "compute").map(|(_, v)| *v) else {
        return fields;
    };
    let mut out = fields;
    out.push(("compute[22:16]".to_string(), compute >> 16));
    out.push(("compute[15:0]".to_string(), compute & 0xFFFF));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A tiny in-memory [`ShortWords`] for unit tests: exact byte ranges,
    /// no aliasing -- enough to exercise the decode algorithm without
    /// `crate::image::SegmentImage` or any firmware.
    struct FlatMem {
        bytes: BTreeMap<u32, u8>,
    }

    impl FlatMem {
        fn new() -> FlatMem {
            FlatMem {
                bytes: BTreeMap::new(),
            }
        }

        fn put_sw(&mut self, pc_sw: u32, words: &[u16]) {
            let base = 2 * pc_sw + 0x2800_0000;
            for (i, w) in words.iter().enumerate() {
                let [lo, hi] = w.to_le_bytes();
                self.bytes.insert(base + 2 * i as u32, lo);
                self.bytes.insert(base + 2 * i as u32 + 1, hi);
            }
        }
    }

    impl ShortWords for FlatMem {
        fn read_sw(&self, pc_sw: u32, size_bytes: u32) -> Option<[u16; 3]> {
            let base = 2u32.wrapping_mul(pc_sw).wrapping_add(0x2800_0000);
            let mut out = [0u16; 3];
            for i in 0..(size_bytes / 2) {
                let lo = *self.bytes.get(&(base + 2 * i))?;
                let hi = *self.bytes.get(&(base + 2 * i + 1))?;
                out[i as usize] = u16::from_le_bytes([lo, hi]);
            }
            Some(out)
        }
    }

    #[test]
    fn unmapped_pc_decodes_unknown() {
        let mem = FlatMem::new();
        let d = Decoder::new();
        let got = d.decode_at(&mem, 0x1000);
        assert_eq!(got.type_name, "unknown");
        assert_eq!(got.kind, "unknown");
        assert_eq!(got.length_bytes, None);
        assert!(got.fields.is_empty());
    }

    #[test]
    fn type21a_all_zero_word_decodes_confident_48_bit() {
        // Type21a: mask 0xffffffffffff, value 0 -- an all-zero 48-bit frame,
        // with zero fields (matches tools/sharc.py's own dt2-1.16 sweep at
        // sw 0x120000: type_name "21a", kind "confident", length 6, no
        // fields).
        let mut mem = FlatMem::new();
        mem.put_sw(0x1000, &[0, 0, 0]);
        let d = Decoder::new();
        let got = d.decode_at(&mem, 0x1000);
        assert_eq!(got.type_name, "21a");
        assert_eq!(got.kind, "confident");
        assert_eq!(got.length_bytes, Some(6));
        assert!(got.fields.is_empty());
    }

    #[test]
    fn split_compute_appends_after_the_original_entries() {
        let fields = vec![("u".to_string(), 1), ("compute".to_string(), 0x1_2345)];
        let out = split_compute(fields);
        assert_eq!(
            out,
            vec![
                ("u".to_string(), 1),
                ("compute".to_string(), 0x1_2345),
                ("compute[22:16]".to_string(), 0x1),
                ("compute[15:0]".to_string(), 0x2345),
            ]
        );
    }

    #[test]
    fn split_compute_is_identity_without_a_compute_field() {
        let fields = vec![("u".to_string(), 1)];
        assert_eq!(split_compute(fields.clone()), fields);
    }
}
