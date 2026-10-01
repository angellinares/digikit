//! The harness's canonical machine-state format (tools/sharc_diff.py,
//! module docstring: `pack_state`/`unpack_state`, STATE_FORMAT_VERSION 1)
//! and the image blob tools/sharc_transpile_run.py `pack_image` writes.

use crate::mem::Mem;
use crate::rt::*;
use crate::sha256::Sha256;

pub const STATE_FORMAT_VERSION: u32 = 1;
const PAGE: u32 = 4096;

struct Rd<'a> {
    b: &'a [u8],
    off: usize,
}

impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], i32> {
        if self.off + n > self.b.len() {
            return Err(-2);
        }
        let s = &self.b[self.off..self.off + n];
        self.off += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, i32> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, i32> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, i32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn i32(&mut self) -> Result<i32, i32> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn i64(&mut self) -> Result<i64, i32> {
        Ok(i64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn u80(&mut self) -> Result<Int, i32> {
        let s = self.take(10)?;
        let mut v: Int = 0;
        for (i, &b) in s.iter().enumerate() {
            v |= (b as Int) << (8 * i);
        }
        Ok(v)
    }
    fn value(&mut self) -> Result<V, i32> {
        let kind = self.u8()?;
        let value = self.u32()?;
        let mask = self.u32()?;
        Ok(match kind {
            1 => V::c(value as Int),
            2 => V::partial(mask as Int, value as Int),
            _ => V::UNK,
        })
    }
    fn str(&mut self) -> Result<&'a str, i32> {
        let n = self.u16()? as usize;
        std::str::from_utf8(self.take(n)?).map_err(|_| -3)
    }
}

fn put_value(out: &mut Vec<u8>, v: V) {
    let (kind, value, mask) = if v.is_c() {
        (1u8, v.b, u32::MAX)
    } else if v.is_unknown() {
        (0u8, 0, 0)
    } else {
        (2u8, v.b, v.m)
    };
    out.push(kind);
    out.extend_from_slice(&value.to_le_bytes());
    out.extend_from_slice(&mask.to_le_bytes());
}

fn put_u80(out: &mut Vec<u8>, v: Int) {
    let v = v & MR_MASK;
    for i in 0..10 {
        out.push((v >> (8 * i)) as u8);
    }
}

/// The image blob: magic "SHIM", version 1, loader segments, the
/// addresses sharcimm.name_address names (exact and ranges), and
/// encoding.CORE_MMR_RESET_VALUES.
pub struct Image {
    pub mem: Mem,
    pub named: Vec<u32>,
    pub ranges: Vec<(u32, u32)>,
    pub core_reset: Vec<u32>,
}

pub fn parse_image(b: &[u8]) -> Result<Image, i32> {
    let mut r = Rd { b, off: 0 };
    if b.is_empty() {
        return Ok(Image {
            mem: Mem::new(),
            named: Vec::new(),
            ranges: Vec::new(),
            core_reset: Vec::new(),
        });
    }
    if r.take(4)? != b"SHIM" || r.u32()? != 1 {
        return Err(-1);
    }
    let mut mem = Mem::new();
    for _ in 0..r.u32()? {
        let a = r.u32()?;
        let n = r.u32()? as usize;
        mem.load(a, r.take(n)?);
    }
    let mut named: Vec<u32> = (0..r.u32()?).map(|_| r.u32()).collect::<Result<_, _>>()?;
    named.sort_unstable();
    let ranges = (0..r.u32()?)
        .map(|_| Ok((r.u32()?, r.u32()?)))
        .collect::<Result<Vec<_>, i32>>()?;
    let core_reset = (0..r.u32()?).map(|_| r.u32()).collect::<Result<_, _>>()?;
    Ok(Image {
        mem,
        named,
        ranges,
        core_reset,
    })
}

/// sharc_diff.import_state: a fresh state (memory back to the loader
/// image, the harness's run configuration) seeded from BLOB.
pub fn import_state(s: &mut St, blob: &[u8]) -> Result<(), i32> {
    let mut r = Rd { b: blob, off: 0 };
    if r.take(4)? != b"SHRD" {
        return Err(-1);
    }
    if r.u32()? != STATE_FORMAT_VERSION {
        return Err(-4);
    }
    s.mem.reset();
    s.pc_sw = r.u32()? as Int;
    if r.u8()? != 0 {
        r.str()?;
    }
    for code in 0..NUREG {
        s.r[code] = r.value()?;
    }
    for i in 0..7 {
        let kind = r.u8()?;
        let value = r.u80()?;
        let mask = r.u80()?;
        s.special[i] = match kind {
            2 => Spec::M(MR::new(mask, value)),
            1 => Spec::V(V::c(value)),
            _ => Spec::V(V::UNK),
        };
        s.special_present[i] = true;
    }
    s.mmrs.clear();
    for _ in 0..r.u32()? {
        let a = r.u32()?;
        let v = r.value()?;
        s.mmr_put(a, v);
    }
    s.pending = None;
    if r.u8()? != 0 {
        let target_present = r.u8()?;
        let target = r.u32()?;
        let call = r.u8()? != 0;
        let slots = r.u8()?;
        let return_from_call = r.u8()? != 0;
        let return_sw_present = r.u8()?;
        let return_sw = r.i32()?;
        s.pending = Some(Pending {
            target: (target_present != 0).then_some(target as Int),
            call,
            slots: slots as Int,
            return_from_call,
            return_sw: (return_sw_present != 0).then_some(return_sw as Int),
        });
    }
    s.loops.clear();
    for _ in 0..r.u16()? {
        let start_sw = r.u32()? as i64;
        let end_sw = r.u32()? as i64;
        let remaining = r.u32()? as i64;
        let mode = r.u32()? as i64;
        s.loops
            .push_raw(Loop {
                start_sw,
                end_sw,
                remaining,
                mode,
            })
            .map_err(|_| -5)?;
    }
    s.call_stack.clear();
    for _ in 0..r.u16()? {
        let v = r.u32()? as Int;
        s.call_stack.push_raw(v).map_err(|_| -5)?;
    }
    s.status_stack.clear();
    for _ in 0..r.u16()? {
        let t = (r.value()?, r.value()?, r.value()?);
        s.status_stack.push_raw(t).map_err(|_| -5)?;
    }
    for _ in 0..r.u32()? {
        let a = r.u32()?;
        let n = r.u32()? as usize;
        let data = r.take(n)?;
        for (k, &byte) in data.iter().enumerate() {
            // import_state writes through _dm_write(address + i, 1, byte).
            let addr = a as Int + k as Int;
            let Ok(Some(c)) = bnd::_canonical_dm_address(s, addr, 1, true) else {
                return Err(-6);
            };
            s.mem.write_byte(c as u32, byte);
        }
    }
    // Page hashes describe the source's overlay; nothing to import.
    s.steps = 0;
    s.at_loaded_entry = false;
    s.cfg = Cfg::default();
    s.sync_snapshot();
    s.check_loops();
    s.trap = None;
    Ok(())
}

/// sharc_diff.export_state + pack_state. With RANGES, every overlay byte
/// also goes out as an explicit memory range (for handing the state to
/// the Python core).
pub fn export_state(s: &St, ranges: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(4096);
    out.extend_from_slice(b"SHRD");
    out.extend_from_slice(&STATE_FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&(s.pc_sw as u32).to_le_bytes());
    out.push(0); // never stopped: a stop traps first
    for code in 0..NUREG {
        put_value(&mut out, s.r[code]);
    }
    for i in 0..7 {
        let (kind, value, mask): (u8, Int, Int) = match (s.special_present[i], s.special[i]) {
            (true, Spec::M(m)) => (2, m.bits, m.mask),
            (true, Spec::V(v)) if v.is_c() => (1, v.val(), 0xFFFF_FFFF),
            _ => (0, 0, 0),
        };
        out.push(kind);
        put_u80(&mut out, value);
        put_u80(&mut out, mask);
    }
    out.extend_from_slice(&(s.mmrs.len() as u32).to_le_bytes());
    for &(a, v) in &s.mmrs {
        out.extend_from_slice(&a.to_le_bytes());
        put_value(&mut out, v);
    }
    match s.pending {
        None => out.push(0),
        Some(p) => {
            out.push(1);
            out.push(p.target.is_some() as u8);
            out.extend_from_slice(&(p.target.unwrap_or(0) as u32).to_le_bytes());
            out.push(p.call as u8);
            out.push(p.slots as u8);
            out.push(p.return_from_call as u8);
            out.push(p.return_sw.is_some() as u8);
            out.extend_from_slice(&(p.return_sw.unwrap_or(0) as i32).to_le_bytes());
        }
    }
    out.extend_from_slice(&(s.loops.len() as u16).to_le_bytes());
    for l in s.loops.items() {
        for v in [l.start_sw, l.end_sw, l.remaining, l.mode] {
            out.extend_from_slice(&(v as u32).to_le_bytes());
        }
    }
    out.extend_from_slice(&(s.call_stack.len() as u16).to_le_bytes());
    for &v in s.call_stack.items() {
        out.extend_from_slice(&(v as u32).to_le_bytes());
    }
    out.extend_from_slice(&(s.status_stack.len() as u16).to_le_bytes());
    for &(a, b, c) in s.status_stack.items() {
        put_value(&mut out, a);
        put_value(&mut out, b);
        put_value(&mut out, c);
    }
    let dirty = s.mem.dirty_bytes();
    if ranges {
        let mut runs: Vec<(u32, Vec<u8>)> = Vec::new();
        for &(a, v) in &dirty {
            match runs.last_mut() {
                Some((start, bytes)) if *start as u64 + bytes.len() as u64 == a as u64 => {
                    bytes.push(v)
                }
                _ => runs.push((a, vec![v])),
            }
        }
        out.extend_from_slice(&(runs.len() as u32).to_le_bytes());
        for (a, bytes) in runs {
            out.extend_from_slice(&a.to_le_bytes());
            out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            out.extend_from_slice(&bytes);
        }
    } else {
        out.extend_from_slice(&0u32.to_le_bytes());
    }
    // Page hashes: sha256 of (address:u64 LE, value:u8) per 4096-byte page.
    let mut hashes: Vec<(u32, [u8; 32])> = Vec::new();
    let mut i = 0;
    while i < dirty.len() {
        let page = dirty[i].0 - dirty[i].0 % PAGE;
        let mut h = Sha256::new();
        while i < dirty.len() && dirty[i].0 - dirty[i].0 % PAGE == page {
            h.update(&(dirty[i].0 as u64).to_le_bytes());
            h.update(&[dirty[i].1]);
            i += 1;
        }
        hashes.push((page, h.finish()));
    }
    out.extend_from_slice(&(hashes.len() as u32).to_le_bytes());
    for (page, digest) in hashes {
        out.extend_from_slice(&page.to_le_bytes());
        out.extend_from_slice(&digest);
    }
    out
}

/// Run configuration: 10 explicit_memory_model, 11 approx_recips, 12
/// assume_nw32, 13 follow_loaded_calls, 14 max_call_depth, 15
/// continue_external_calls, 16 data_memory_tainted, 17 has_concrete (a
/// State with concrete=None), 18 dossier_bytes.
pub fn set_option(s: &mut St, key: u32, value: i64) -> i32 {
    let b = value != 0;
    match key {
        10 => s.cfg.explicit_memory_model = b,
        11 => s.cfg.approx_recips = b,
        12 => s.cfg.assume_nw32 = b,
        13 => s.cfg.follow_loaded_calls = b,
        14 => s.cfg.max_call_depth = value as Int,
        15 => s.cfg.continue_external_calls = b,
        16 => s.cfg.data_memory_tainted = b,
        17 => s.cfg.has_concrete = b,
        18 => s.cfg.dossier_bytes = value as Int,
        _ => return -1,
    }
    s.cfg.refresh();
    0
}

/// One instruction for sharc_native_exec_insn: magic "SHIN", type name,
/// length_bytes (i32, -1 for None), kind, then u16 field count and
/// (key, value:i64) pairs, strings as u16 length + UTF-8. The Insn is
/// leaked (tests only).
pub fn parse_insn(b: &[u8]) -> Result<&'static Insn, i32> {
    let mut r = Rd { b, off: 0 };
    if r.take(4)? != b"SHIN" {
        return Err(-1);
    }
    let type_name = crate::sym_of(r.str()?).ok_or(-7)?;
    let length = r.i32()?;
    let kind = crate::sym_of(r.str()?).ok_or(-7)?;
    let n = r.u16()?;
    let mut entries = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let key = r.str()?;
        let value = r.i64()? as Int;
        entries.push(field_entry(key, value).ok_or(-7)?);
    }
    let fields: &'static Fields = Box::leak(Box::new(Fields {
        kv: Box::leak(entries.into_boxed_slice()),
    }));
    Ok(Box::leak(Box::new(Insn {
        type_name,
        fields,
        length_bytes: (length >= 0).then_some(length as Int),
        kind,
        offset: 0,
    })))
}

/// The decoded instructions of an image (tools/sharc_rsgen.py insn_blob),
/// by PC.
pub struct InsnTable {
    /// Two levels over the 24-bit short-word PC: 4096-PC pages of
    /// instruction indices plus one (0: none).
    pages: Vec<Option<Box<[u32; 4096]>>>,
    insns: &'static [Insn],
}

impl InsnTable {
    #[inline(always)]
    pub fn get(&self, pc: Int) -> Option<&'static Insn> {
        if !(0..(1 << 24)).contains(&pc) {
            return None;
        }
        let pc = pc as u32;
        let page = self.pages[(pc >> 12) as usize].as_deref()?;
        let i = page[(pc & 0xFFF) as usize];
        if i == 0 {
            None
        } else {
            Some(&self.insns[i as usize - 1])
        }
    }
    pub fn len(&self) -> usize {
        self.insns.len()
    }
    pub fn is_empty(&self) -> bool {
        self.insns.is_empty()
    }
}

/// Parse BLOB once (the table lives as long as the process).
pub fn insn_table(blob: &'static [u8]) -> &'static InsnTable {
    static TABLE: std::sync::OnceLock<InsnTable> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| parse_insn_table(blob).expect("malformed instruction table"))
}

fn parse_insn_table(b: &[u8]) -> Result<InsnTable, i32> {
    let mut r = Rd { b, off: 0 };
    if r.take(4)? != b"SHIX" {
        return Err(-1);
    }
    let n = r.u32()? as usize;
    let mut pages: Vec<Option<Box<[u32; 4096]>>> = Vec::new();
    pages.resize_with(1 << 12, || None);
    // Decode into temporary headers and one field arena before publishing
    // process-lifetime references. Per-instruction boxes otherwise make the
    // first interpreter fallback allocate hundreds of thousands of objects.
    let mut headers = Vec::with_capacity(n);
    let mut entries = Vec::with_capacity(b.len() / 14);
    for _ in 0..n {
        let pc = r.u32()?;
        let type_name = r.u16()?;
        let kind = r.u16()?;
        let length = r.u8()? as i8;
        let nf = r.u8()? as usize;
        let start = entries.len();
        for _ in 0..nf {
            let key = r.u16()?;
            let stem = r.u16()?;
            let hi = r.u8()? as i8;
            let lo = r.u8()? as i8;
            let v = r.i64()? as Int;
            entries.push(FieldEntry(key, stem, hi, lo, v));
        }
        headers.push((pc, type_name, kind, length, start, entries.len()));
    }
    // These three arrays share the table's process lifetime, as the previous
    // individual leaked boxes did. No references escape a failed decode.
    let entries: &'static [FieldEntry] = Box::leak(entries.into_boxed_slice());
    let fields: &'static [Fields] = Box::leak(
        headers
            .iter()
            .map(|&(_, _, _, _, start, end)| Fields {
                kv: &entries[start..end],
            })
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    );
    let mut insns = Vec::with_capacity(n);
    for ((pc, type_name, kind, length, _, _), fields) in headers.into_iter().zip(fields) {
        if pc < (1 << 24) {
            let page = pages[(pc >> 12) as usize].get_or_insert_with(|| Box::new([0; 4096]));
            page[(pc & 0xFFF) as usize] = insns.len() as u32 + 1;
        }
        insns.push(Insn {
            type_name,
            fields,
            length_bytes: (length >= 0).then_some(length as Int),
            kind,
            offset: 0,
        });
    }
    let insns = Box::leak(insns.into_boxed_slice());
    Ok(InsnTable { pages, insns })
}

/// A field entry for KEY (e.g. "data[31:16]"): its stem and bit range.
pub fn field_entry(key: &str, value: Int) -> Option<FieldEntry> {
    let k = crate::sym_of(key)?;
    let (stem, range) = match key.find('[') {
        Some(i) => (&key[..i], &key[i..]),
        None => (key, ""),
    };
    let st = crate::sym_of(stem)?;
    let (mut hi, mut lo) = (-1i8, -1i8);
    if let Some(inner) = range.strip_prefix('[').and_then(|x| x.strip_suffix(']'))
        && let Some((h, l)) = inner.split_once(':')
    {
        hi = h.parse().unwrap_or(-1);
        lo = l.parse().unwrap_or(-1);
    }
    Some(FieldEntry(k, st, hi, lo, value))
}

#[cfg(test)]
mod instruction_table_tests {
    use super::*;

    fn blob() -> Vec<u8> {
        let mut out = b"SHIX".to_vec();
        out.extend_from_slice(&4_u32.to_le_bytes());
        for (pc, kind, length, values) in [
            (0xfff_u32, 2_u16, 6_i8, &[17_i64, -23][..]),
            (0x1000, 3, -1, &[][..]),
            (0xfff, 4, 4, &[99][..]),
            (1 << 24, 5, 2, &[123][..]),
        ] {
            out.extend_from_slice(&pc.to_le_bytes());
            out.extend_from_slice(&1_u16.to_le_bytes());
            out.extend_from_slice(&kind.to_le_bytes());
            out.extend_from_slice(&[length as u8, values.len() as u8]);
            for (index, &value) in values.iter().enumerate() {
                out.extend_from_slice(&(index as u16 + 10).to_le_bytes());
                out.extend_from_slice(&7_u16.to_le_bytes());
                out.extend_from_slice(&[31, 16]);
                out.extend_from_slice(&value.to_le_bytes());
            }
        }
        out
    }

    #[test]
    fn packed_fields_preserve_order_and_pc_lookup() {
        let table = parse_insn_table(&blob()).unwrap();
        assert_eq!(table.len(), 4);
        let original = &table.insns[0];
        assert_eq!(original.type_name, 1);
        assert_eq!(original.length_bytes, Some(6));
        assert_eq!(original.fields.kv.len(), 2);
        assert_eq!(original.fields.get(10), Some(17));
        assert_eq!(original.fields.get(11), Some(-23));
        let field = original.fields.kv[1];
        assert_eq!((field.0, field.1, field.2, field.3), (11, 7, 31, 16));
        // Duplicate PCs select the last instruction, as before. Adjacent
        // pages and zero-field instructions keep distinct lookup entries.
        let duplicate = table.get(0xfff).unwrap();
        assert_eq!(duplicate.kind, 4);
        assert_eq!(duplicate.fields.get(10), Some(99));
        let next = table.get(0x1000).unwrap();
        assert_eq!(next.kind, 3);
        assert_eq!(next.length_bytes, None);
        assert!(next.fields.kv.is_empty());
        for pc in [-1, 0, 0xffe, 0x1001, 1 << 24] {
            assert!(table.get(pc).is_none());
        }
    }

    #[test]
    fn truncated_field_arena_is_rejected() {
        let bytes = blob();
        for end in 0..bytes.len() {
            assert!(parse_insn_table(&bytes[..end]).is_err(), "length {end}");
        }
    }
}
