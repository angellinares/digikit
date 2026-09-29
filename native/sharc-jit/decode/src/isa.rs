//! SHARC+ VISA instruction forms, ported from `tools/sharc_isa.py`'s
//! `InstructionSet.from_json(..., mode="visa")` and `tools/sharc_visa_tables.py`.
//!
//! `tools/sharcspec/decode_table.json` gives each form's mask and value
//! MSB-aligned in a 48-bit frame (`frame = (w0 << 32) | (w1 << 16) | w2`,
//! `tools/sharc_isa.py::frame_of`), plus its field list `{label, hi, lo}` in
//! that same 48-bit frame. VISA mode keeps every row whose `"visa"` flag is
//! true (`Type10a_abs` is the only form excluded this way in the shipped
//! table); ISA mode (48-bit only) is not needed here and is not implemented.
//!
//! This module only extracts what `tools/sharc_disasm.py`'s decode path
//! actually reads: matching a form against a frame, its width/fixed-bit
//! ranking numbers, and its field list. It deliberately does not port
//! `sharc_isa.py`'s `OperandSpec`/`OperandKind` grouping -- nothing in the
//! decode path (`Instruction.field_dict()`) needs it.

use crate::json::{self, JsonError, Value};

pub const DECODE_TABLE_JSON: &str = include_str!("../../../../tools/sharcspec/decode_table.json");

/// One physical field in the 48-bit MSB-aligned frame: label exactly as the
/// table spells it (e.g. `"dmi[2:0]"` or the bare `"compute"`), and its
/// inclusive bit range within the 48-bit frame.
#[derive(Debug, Clone)]
pub struct Field {
    pub label: String,
    pub hi: u32,
    pub lo: u32,
}

impl Field {
    #[inline]
    fn extract(&self, frame: u64) -> i64 {
        let width = self.hi - self.lo + 1;
        let mask: u64 = if width >= 64 {
            u64::MAX
        } else {
            (1u64 << width) - 1
        };
        ((frame >> self.lo) & mask) as i64
    }
}

/// One immutable instruction encoding form (`tools/sharc_isa.py`'s
/// `InstructionForm`, VISA-relevant fields only).
#[derive(Debug, Clone)]
pub struct Form {
    /// The project id (`tools/sharc_isa.py::form_id`), e.g. `"5b_move"`.
    pub id: String,
    pub extent_bits: u32,
    pub frame_mask: u64,
    pub frame_value: u64,
    pub fixed_bits: u32,
    pub leading_fixed_bits: u32,
    pub fields: Vec<Field>,
    /// `unconfirmed_bits != 0`: the table marks some fixed bits unconfirmed,
    /// so a decode landing on this form is reported "uncertain".
    pub uncertain: bool,
    /// This id is in `sharc_disasm.NEVER_ALIGNED_FORMS`: never a real
    /// successor in the width-correction lookahead (see decode.rs).
    pub never_aligned: bool,
}

impl Form {
    #[inline]
    pub fn extent_words(&self) -> u32 {
        self.extent_bits / 16
    }

    #[inline]
    pub fn matches(&self, frame: u64) -> bool {
        frame & self.frame_mask == self.frame_value
    }

    pub fn extract_fields(&self, frame: u64) -> Vec<(String, i64)> {
        self.fields
            .iter()
            .map(|f| (f.label.clone(), f.extract(frame)))
            .collect()
    }
}

/// `tools/sharc_disasm.py::NEVER_ALIGNED_FORMS`: `"10a_abs"` never even
/// reaches here (its table row has `"visa": false`), but both names are
/// checked for clarity and in case a future table edit adds it back.
const NEVER_ALIGNED_FORMS: &[&str] = &["10a_rel", "10a_abs"];

/// `tools/sharc_isa.py::form_id`: `"Type5b (move)"` -> `"5b_move"`,
/// `"Type8a_abs"` -> `"8a_abs"`, `"Type1a"` -> `"1a"`.
pub fn form_id(table_name: &str) -> String {
    let rest = table_name.strip_prefix("Type").unwrap_or(table_name);
    if let Some(paren_start) = rest.find('(')
        && rest.ends_with(')')
    {
        let inner = &rest[paren_start + 1..rest.len() - 1];
        if !inner.is_empty() && inner.chars().all(|c| c.is_alphanumeric() || c == '_') {
            let prefix = rest[..paren_start].trim_end();
            return format!("{prefix}_{inner}");
        }
    }
    rest.to_string()
}

fn parse_hex_u64(s: &str, what: &str) -> Result<u64, JsonError> {
    let digits = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .ok_or_else(|| {
            JsonError(format!(
                "{what}: expected a 0x-prefixed hex string, got {s:?}"
            ))
        })?;
    u64::from_str_radix(digits, 16)
        .map_err(|e| JsonError(format!("{what}: bad hex literal {s:?}: {e}")))
}

fn parse_field(v: &Value) -> Result<Field, JsonError> {
    let label = v
        .get("label")
        .ok_or_else(|| JsonError("field missing 'label'".into()))?
        .as_str()?
        .to_string();
    let hi = v
        .get("hi")
        .ok_or_else(|| JsonError(format!("field {label:?} missing 'hi'")))?
        .as_int()? as u32;
    let lo = v
        .get("lo")
        .ok_or_else(|| JsonError(format!("field {label:?} missing 'lo'")))?
        .as_int()? as u32;
    Ok(Field { label, hi, lo })
}

fn leading_fixed_bits(mask: u64) -> u32 {
    let mut count = 0;
    for bit in (0..48).rev() {
        if (mask >> bit) & 1 == 0 {
            break;
        }
        count += 1;
    }
    count
}

fn parse_form(v: &Value) -> Result<Option<Form>, JsonError> {
    let visa = v
        .get("visa")
        .ok_or_else(|| JsonError("form missing 'visa'".into()))?
        .as_bool()?;
    if !visa {
        return Ok(None);
    }
    let name = v
        .get("name")
        .ok_or_else(|| JsonError("form missing 'name'".into()))?
        .as_str()?;
    let id = form_id(name);
    let extent_bits = v
        .get("width")
        .ok_or_else(|| JsonError(format!("{id}: missing 'width'")))?
        .as_int()? as u32;
    let mask = parse_hex_u64(
        v.get("mask")
            .ok_or_else(|| JsonError(format!("{id}: missing 'mask'")))?
            .as_str()?,
        &format!("{id}.mask"),
    )?;
    let value = parse_hex_u64(
        v.get("value")
            .ok_or_else(|| JsonError(format!("{id}: missing 'value'")))?
            .as_str()?,
        &format!("{id}.value"),
    )?;
    let fixed_bits = v
        .get("fixed_bits")
        .ok_or_else(|| JsonError(format!("{id}: missing 'fixed_bits'")))?
        .as_int()? as u32;
    let unconfirmed_bits = match v.get("unconfirmed_bits") {
        Some(Value::Int(n)) => *n,
        _ => 0,
    };
    let fields = v
        .get("fields")
        .ok_or_else(|| JsonError(format!("{id}: missing 'fields'")))?
        .as_arr()?
        .iter()
        .map(parse_field)
        .collect::<Result<Vec<_>, _>>()?;
    let never_aligned = NEVER_ALIGNED_FORMS.contains(&id.as_str());
    Ok(Some(Form {
        leading_fixed_bits: leading_fixed_bits(mask),
        id,
        extent_bits,
        frame_mask: mask,
        frame_value: value,
        fixed_bits,
        fields,
        uncertain: unconfirmed_bits != 0,
        never_aligned,
    }))
}

/// Load every VISA form from `text` (`tools/sharcspec/decode_table.json`'s
/// own text; `Decoder::new()` passes the embedded copy).
pub fn load_visa_forms(text: &str) -> Result<Vec<Form>, JsonError> {
    let root = json::parse(text)?;
    let rows = root
        .get("forms")
        .ok_or_else(|| JsonError("decode table missing 'forms'".into()))?
        .as_arr()?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        if let Some(form) = parse_form(row)? {
            out.push(form);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_id_matches_python_form_id() {
        // tools/sharc_isa.py::form_id, spot-checked against every name that
        // appears in decode_table.json (see the crate's verification notes).
        let cases: &[(&str, &str)] = &[
            ("Type1a", "1a"),
            ("Type5a_move", "5a_move"),
            ("Type5a (swap)", "5a_swap"),
            ("Type5b (move)", "5b_move"),
            ("Type5b (swap)", "5b_swap"),
            ("Type6a (mem)", "6a_mem"),
            ("Type6a (nomem)", "6a_nomem"),
            ("Type8a_abs", "8a_abs"),
            ("Type6b_shiftimm", "6b_shiftimm"),
            ("Type21p_undoc16", "21p_undoc16"),
            ("Type10a_abs", "10a_abs"),
            ("Type10a_rel", "10a_rel"),
        ];
        for (input, expected) in cases {
            assert_eq!(&form_id(input), expected, "form_id({input:?})");
        }
    }

    #[test]
    fn loads_the_embedded_table() {
        let forms = load_visa_forms(DECODE_TABLE_JSON).unwrap();
        // 61 of the table's 62 rows have visa=true (only Type10a_abs is isa-only).
        assert_eq!(forms.len(), 61);
        assert!(forms.iter().any(|f| f.id == "1a" && f.extent_bits == 48));
        assert!(!forms.iter().any(|f| f.id == "10a_abs"));
        let type3a = forms.iter().find(|f| f.id == "3a").unwrap();
        assert!(type3a.fields.iter().any(|f| f.label == "compute"));
    }

    #[test]
    fn matches_and_extract_fields_are_consistent_with_the_frame_layout() {
        let forms = load_visa_forms(DECODE_TABLE_JSON).unwrap();
        let f = forms.iter().find(|f| f.id == "1a").unwrap();
        assert!(f.matches(f.frame_value));
        let fields = f.extract_fields(f.frame_value);
        // Every field of a form whose frame is exactly its own frame_value
        // decodes to the bits frame_value carries at that field's position.
        for (label, value) in fields {
            let field = f.fields.iter().find(|x| x.label == label).unwrap();
            let width = field.hi - field.lo + 1;
            let mask = if width >= 64 {
                u64::MAX
            } else {
                (1u64 << width) - 1
            };
            let expected = ((f.frame_value >> field.lo) & mask) as i64;
            assert_eq!(value, expected);
        }
    }
}
