//! The instruction table of an image (tools/sharc_rsgen.py insn_blob) and the
//! little-endian reader the canonical formats share.

use crate::rt::*;

pub struct Rd<'a> {
    pub b: &'a [u8],
    pub off: usize,
}

impl<'a> Rd<'a> {
    pub fn take(&mut self, n: usize) -> Result<&'a [u8], i32> {
        if self.off + n > self.b.len() {
            return Err(-2);
        }
        let s = &self.b[self.off..self.off + n];
        self.off += n;
        Ok(s)
    }
    pub fn u8(&mut self) -> Result<u8, i32> {
        Ok(self.take(1)?[0])
    }
    pub fn u16(&mut self) -> Result<u16, i32> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub fn u32(&mut self) -> Result<u32, i32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub fn i32(&mut self) -> Result<i32, i32> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub fn i64(&mut self) -> Result<i64, i32> {
        Ok(i64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    pub fn u80(&mut self) -> Result<Int, i32> {
        let s = self.take(10)?;
        let mut v: Int = 0;
        for (i, &b) in s.iter().enumerate() {
            v |= (b as Int) << (8 * i);
        }
        Ok(v)
    }
    pub fn value(&mut self) -> Result<V, i32> {
        let kind = self.u8()?;
        let value = self.u32()?;
        let mask = self.u32()?;
        Ok(match kind {
            1 => V::c(value as Int),
            2 => V::partial(mask as Int, value as Int),
            _ => V::UNK,
        })
    }
    pub fn str(&mut self) -> Result<&'a str, i32> {
        let n = self.u16()? as usize;
        std::str::from_utf8(self.take(n)?).map_err(|_| -3)
    }
}

/// The decoded instructions of an image (tools/sharc_rsgen.py insn_blob),
/// by PC.
pub struct InsnTable {
    /// Two levels over the 24-bit short-word PC: 4096-PC pages of
    /// instruction indices plus one (0: none).
    pages: Vec<Option<Box<[u32; 4096]>>>,
    insns: Vec<Insn>,
}

impl InsnTable {
    #[inline(always)]
    pub fn get(&self, pc: Int) -> Option<Insn> {
        if !(0..(1 << 24)).contains(&pc) {
            return None;
        }
        let pc = pc as u32;
        let page = self.pages[(pc >> 12) as usize].as_deref()?;
        let i = page[(pc & 0xFFF) as usize];
        if i == 0 {
            None
        } else {
            Some(self.insns[i as usize - 1])
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
    // Decode into value instructions. The static table owns its Vec, while a
    // runtime engine owns and drops its separate decode cache.
    let mut headers = Vec::with_capacity(n);
    let mut entries = Vec::with_capacity(b.len() / 14);
    for _ in 0..n {
        let pc = r.u32()?;
        let type_name = r.u16()?;
        let kind = r.u16()?;
        let length = r.u8()? as i8;
        let nf = r.u8()? as usize;
        if nf > MAX_INSN_FIELDS {
            return Err(-8);
        }
        let start = entries.len();
        for _ in 0..nf {
            let key = r.u16()?;
            let stem = r.u16()?;
            let hi = r.u8()? as i8;
            let lo = r.u8()? as i8;
            let v = r.i64()?;
            entries.push(FieldEntry(key, stem, hi, lo, v));
        }
        headers.push((pc, type_name, kind, length, start, entries.len()));
    }
    let mut insns = Vec::with_capacity(n);
    for (pc, type_name, kind, length, start, end) in headers {
        if pc < (1 << 24) {
            let page = pages[(pc >> 12) as usize].get_or_insert_with(|| Box::new([0; 4096]));
            page[(pc & 0xFFF) as usize] = insns.len() as u32 + 1;
        }
        let mut kv = [FieldEntry(S_EMPTY, S_EMPTY, -1, -1, 0); MAX_INSN_FIELDS];
        kv[..end - start].copy_from_slice(&entries[start..end]);
        insns.push(Insn {
            type_name,
            fields: Fields::from_entries(kv, (end - start) as u8),
            length_bytes: (length >= 0).then_some(length as Int),
            kind,
            offset: 0,
        });
    }
    Ok(InsnTable { pages, insns })
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
        assert_eq!(original.fields.entries().len(), 2);
        assert_eq!(original.fields.get(10), Some(17));
        assert_eq!(original.fields.get(11), Some(-23));
        let field = original.fields.entries()[1];
        assert_eq!((field.0, field.1, field.2, field.3), (11, 7, 31, 16));
        // Duplicate PCs select the last instruction, as before. Adjacent
        // pages and zero-field instructions keep distinct lookup entries.
        let duplicate = table.get(0xfff).unwrap();
        assert_eq!(duplicate.kind, 4);
        assert_eq!(duplicate.fields.get(10), Some(99));
        let next = table.get(0x1000).unwrap();
        assert_eq!(next.kind, 3);
        assert_eq!(next.length_bytes, None);
        assert!(next.fields.entries().is_empty());
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
