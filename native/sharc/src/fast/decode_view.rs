//! A decoded instruction as the lowering reads it: the generic decoder's
//! output (decode.rs) with `tools/sharc_core`'s field lookup rules
//! (`encoding._field`: an exact key, else the first `stem[...]` key).

use crate::decode::{DecodeKind, Decoded};

#[derive(Clone, Debug)]
pub struct Dec {
    pub form: &'static str,
    pub len_sw: u32,
    fields: Vec<(&'static str, i64)>,
}

impl Dec {
    /// A placeholder for an address that does not decode (CFG stop nodes).
    pub fn nop() -> Dec {
        Dec {
            form: "nop",
            len_sw: 1,
            fields: Vec::new(),
        }
    }

    pub fn from_decoded(d: &Decoded) -> Option<Dec> {
        if d.kind != DecodeKind::Confident {
            return None;
        }
        Some(Dec {
            form: d.type_name,
            len_sw: d.length_bytes? as u32 / 2,
            fields: d.fields().iter().map(|f| (f.key, f.value)).collect(),
        })
    }

    /// The same instruction with its execution condition (`cond`, `cond[..]`)
    /// replaced by "always" (0x1f). The lowering handles a condition once, for
    /// every form, and gives the form code the unconditional view.
    pub fn with_unconditional(&self) -> Dec {
        let mut d = self.clone();
        for f in d.fields.iter_mut() {
            if f.0 == "cond" || f.0.starts_with("cond[") {
                f.1 = 0x1f;
            }
        }
        d
    }

    pub fn get(&self, key: &str) -> Option<i64> {
        self.fields.iter().find(|f| f.0 == key).map(|f| f.1)
    }

    /// `encoding._field`: exact key, else the first `stem[` key.
    pub fn field(&self, stem: &str) -> Option<i64> {
        if let Some(v) = self.get(stem) {
            return Some(v);
        }
        self.fields
            .iter()
            .find(|f| {
                f.0.strip_prefix(stem)
                    .is_some_and(|rest| rest.starts_with('['))
            })
            .map(|f| f.1)
    }

    /// The 23-bit compute field of a full compute (0: none).
    pub fn compute(&self) -> Option<u32> {
        if let Some(c) = self.get("compute") {
            return Some(c as u32);
        }
        let hi = self.get("compute[22:16]")?;
        let lo = self.get("compute[15:0]")?;
        Some(((hi as u32) << 16) | lo as u32)
    }

    pub fn wide(&self, stem: &str) -> Option<u32> {
        let hi = self.get(&format!("{stem}[31:16]"))?;
        let lo = self.get(&format!("{stem}[15:0]"))?;
        Some(((hi as u32) << 16) | lo as u32)
    }
}
