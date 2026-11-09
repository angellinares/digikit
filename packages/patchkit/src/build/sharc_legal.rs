//! Instructions the SHARC+ ISA does not have, which the assembler and the emulator accept and the silicon does not run (finding 49
//! section 10). `patchkit build` refuses an object with one ([`legality_problems`], called for every instruction in
//! `sharc_obj::verify`), and the DSP image scan (`dspscan`) runs the same rules on the stock images, which have no hit. The loop-end
//! rule ([`loop_end_problems`]) looks at runs of instructions, not at one.
//!
//! A rule is added only when the programming reference states it, and only after the stock DT2 and DN2 images show zero hits.
//! Page numbers are printed pages of the SHARC+ Core Programming Reference (docs/sharc/SOURCES.md).
//!
//! Checked and not a rule: the DM/PM index pairing. Types 3a, 3b, 3d, 4, 15a and 15b have a 3-bit I field and a 3-bit M field, and
//! the g bit (DM or PM) picks the DAG, so a DM access with I8-I15 or a PM access with I0-I7 has no encoding.

use super::sharc_obj::field_joined;
use sharc_rt::decode::Decoded;

/// The forms that address memory through an index register (Ia/Ic, with an immediate or register modifier).
const INDEXED_MEMORY_FORMS: [&str; 5] = ["3a", "3b", "3d", "15a", "15b"];
/// The forms with a register modifier and a pre-/post-modify select (the `u` bit).
const MODIFIER_FORMS: [&str; 3] = ["3a", "3b", "3d"];

/// Every problem of one instruction, as `{where_}: ...` messages.
pub(crate) fn legality_problems(d: &Decoded, where_: &str) -> Vec<String> {
    let mut out = ureg_class_problems(d, where_);
    out.extend(post_modify_load_problems(d, where_));
    out
}

/// One instruction of a code run for [`loop_end_problems`]: its address in short words, its length in bytes and its decode.
pub(crate) type Placed = (u64, u8, Decoded);

/// The end of a hardware loop must be ISA (48-bit): of the last 11 instructions of a `DO` body, none may be a VISA (16- or 32-bit)
/// instruction, and a body shorter than 11 has none at all (SHARC+ PRM 4-41, "VISA-Related Restrictions on Hardware Loops": the sequencer
/// needs this to take an interrupt in the last instructions and to detect the fetch of the last one; finding 91). `code` is a run of
/// instructions sorted by address. A loop whose end is not the start of an instruction of the run is not judged. The loop forms are
/// Type 12a (`LCNTR = ..., DO ... UNTIL LCE`) and Type 13a (`DO ... UNTIL cond`); the end is `address + reladdr`. Messages start with
/// `where_`, positions are the addresses of `code`.
pub(crate) fn loop_end_problems(code: &[Placed], where_: &str) -> Vec<String> {
    let mut out = vec![];
    for (i, (at, _, d)) in code.iter().enumerate() {
        if !(d.type_name.starts_with("12a") || d.type_name == "13a") {
            continue;
        }
        let Some(rel) = field_joined(d, "reladdr") else {
            continue;
        };
        // 23-bit signed
        let rel = if rel >= 1 << 22 { rel - (1 << 23) } else { rel };
        let Some(end) = at.checked_add_signed(rel) else {
            continue;
        };
        let Ok(j) = code.binary_search_by_key(&end, |p| p.0) else {
            continue;
        };
        if j <= i {
            continue;
        }
        let body = &code[i + 1..=j];
        let tail = &body[body.len().saturating_sub(LOOP_END_ISA)..];
        let visa: Vec<&Placed> = tail.iter().filter(|p| p.1 != 6).collect();
        if let Some((first, bytes, _)) = visa.first() {
            out.push(format!(
                "{where_}DO loop at {at:#x} (end {end:#x}): {} of its last {} instructions are VISA, the first at {first:#x} ({bytes} bytes)",
                visa.len(),
                tail.len(),
            ));
        }
    }
    out
}

/// The number of instructions at the end of a hardware loop that must be 48-bit.
const LOOP_END_ISA: usize = 11;

/// Name and DAG of a Ureg code in the I, M, L or B ranges (0x10-0x4F; DAG1 is register 0-7 of the range, DAG2 8-15).
fn dag_register(u: i64) -> Option<(String, i64)> {
    if !(0x10..=0x4F).contains(&u) {
        return None;
    }
    let name = ["I", "M", "L", "B"][(u >> 4) as usize - 1];
    Some((
        format!("{name}{}", u & 0xF),
        if (u & 0xF) < 8 { 1 } else { 2 },
    ))
}

/// The DAG of the index register: `g` selects DM (DAG1) or PM (DAG2).
fn index_dag(d: &Decoded) -> Option<i64> {
    field_joined(d, "g").map(|g| if g == 0 { 1 } else { 2 })
}

/// A store through DM with an index register cannot take a DAG1 register (I0-I7, M0-M7, L0-L7, B0-B7) and a store through PM cannot take
/// a DAG2 register: the ISA encodes the source of such a store as UREGXDAG1 / UREGXDAG2 (SHARC+ PRM 13-15 Type 3a, 13-18 Type 3b, 13-24
/// Type 3d, 15-7 Type 15a, 15-10 Type 15b encode tables; 13-13, 13-16, 15-8, 15-11: "the Ureg may not be from the same DAG as Ia/Mb or
/// Ic/Md"). Type 14a (`DM(<addr32>) = Ureg`) has no index register and takes the full UREG class. Loads through a pre-modify or an
/// immediate address take every register ([`post_modify_load_problems`] for the post-modify load). The stock DSP images have not one
/// excluded store (finding 49 section 10).
pub(crate) fn ureg_class_problems(d: &Decoded, where_: &str) -> Vec<String> {
    if !INDEXED_MEMORY_FORMS.contains(&d.type_name) || field_joined(d, "d") != Some(1) {
        return vec![];
    }
    let (Some(dag), Some((name, rdag))) =
        (index_dag(d), field_joined(d, "ureg").and_then(dag_register))
    else {
        return vec![];
    };
    if dag != rdag {
        return vec![];
    }
    vec![format!(
        "{where_}: {} store of {name} (DAG{rdag})",
        if dag == 2 { "PM" } else { "DM" },
    )]
}

/// A post-modify load (`Ureg = DM(Ia, Mb)`, `u` = 1, `d` = 0) cannot load a register of the index register's DAG: its encode tables list
/// the destination as UREGXDAG1 / UREGXDAG2, while the pre-modify load (`Ureg = DM(Mb, Ia)`, `u` = 0) lists the full UREG class (PRM
/// 13-15 Type 3a, 13-18/13-19 Type 3b, 13-24 Type 3d; the same sentence as for stores, 13-13, 13-16). Types 15a and 15b (an immediate
/// address, no post-modify) load every register.
pub(crate) fn post_modify_load_problems(d: &Decoded, where_: &str) -> Vec<String> {
    if !MODIFIER_FORMS.contains(&d.type_name)
        || field_joined(d, "d") != Some(0)
        || field_joined(d, "u") != Some(1)
    {
        return vec![];
    }
    let (Some(dag), Some((name, rdag))) =
        (index_dag(d), field_joined(d, "ureg").and_then(dag_register))
    else {
        return vec![];
    };
    if dag != rdag {
        return vec![];
    }
    vec![format!(
        "{where_}: {} post-modify load of {name} (DAG{rdag})",
        if dag == 2 { "PM" } else { "DM" },
    )]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build::sharc_obj::object_from_text;
    use sharc_rt::decode::decode_isa;

    /// A Type 3a word (ISA frame) with no condition and no compute.
    fn t3a(u: u64, g: u64, d: u64, i: u64, m: u64, ureg: u64) -> Decoded {
        decode_isa(
            0b010 << 45 | u << 44 | i << 41 | m << 38 | 0x1F << 33 | g << 32 | d << 31 | ureg << 23,
        )
    }

    #[test]
    fn a_post_modify_load_of_a_register_of_the_index_dag_is_refused_by_word() {
        // u = 1 (post-modify), d = 0 (load); g = 0 is DM with I0-I7, g = 1 is PM with I8-I15 (PRM 13-15)
        assert_eq!(
            post_modify_load_problems(&t3a(1, 0, 0, 4, 4, 0x10), "x"),
            ["x: DM post-modify load of I0 (DAG1)"]
        );
        assert_eq!(
            post_modify_load_problems(&t3a(1, 0, 0, 4, 4, 0x45), "x"),
            ["x: DM post-modify load of B5 (DAG1)"]
        );
        assert_eq!(
            post_modify_load_problems(&t3a(1, 1, 0, 4, 4, 0x2F), "x"),
            ["x: PM post-modify load of M15 (DAG2)"]
        );
        // the nearest legal ones: the other DAG, a data register, the pre-modify load (u = 0), a store (the store rule's)
        for ok in [
            t3a(1, 0, 0, 4, 4, 0x18),
            t3a(1, 1, 0, 4, 4, 0x10),
            t3a(1, 0, 0, 4, 4, 0x03),
            t3a(0, 0, 0, 4, 4, 0x10),
            t3a(0, 1, 0, 4, 4, 0x18),
            t3a(1, 0, 1, 4, 4, 0x10),
        ] {
            assert_eq!(post_modify_load_problems(&ok, "x"), Vec::<String>::new());
        }
    }

    /// `n` instructions after a Type 12a `LCNTR = 4, DO end UNTIL LCE` at address 0, 3 short words apart; the one at body index `visa`
    /// (if any) is 2 bytes long.
    fn counted_loop(n: u64, visa: Option<usize>) -> Vec<Placed> {
        let mut code: Vec<Placed> = vec![(0, 6, decode_isa(0x0c00_0400_0000 | 3 * n))];
        for k in 0..n as usize {
            let bytes = if visa == Some(k) { 2 } else { 6 };
            code.push((3 * (k as u64 + 1), bytes, decode_isa(0)));
        }
        code
    }

    #[test]
    fn a_visa_instruction_among_the_last_11_of_a_loop_is_refused() {
        assert_eq!(counted_loop(12, Some(0))[0].2.type_name, "12a_imm");
        // body of 12: the last 11 are body[1..]
        let e = loop_end_problems(&counted_loop(12, Some(1)), "x ");
        assert_eq!(e.len(), 1, "{e:?}");
        assert!(
            e[0].starts_with("x DO loop at 0x0 (end 0x24): 1 of its last 11 instructions are VISA, the first at 0x6 (2 bytes)"),
            "{e:?}"
        );
        assert_eq!(loop_end_problems(&counted_loop(12, Some(11)), "").len(), 1);
        // a body shorter than 11 is 48-bit as a whole
        assert_eq!(loop_end_problems(&counted_loop(5, Some(0)), "").len(), 1);
        // the nearest legal ones: the VISA instruction before the last 11, a loop of 48-bit instructions
        assert!(loop_end_problems(&counted_loop(12, Some(0)), "").is_empty());
        assert!(loop_end_problems(&counted_loop(12, None), "").is_empty());
        assert!(loop_end_problems(&counted_loop(5, None), "").is_empty());
    }

    /// A loop whose end is a numeric offset (`DO (PC, n)`), which the assembler does not track, so it compresses the body. The
    /// body is two NOPs of 2 bytes each (end at 4 short words) or, with `.NOCOMPRESS;`, of 6 bytes each (end at 6).
    fn numeric_loop(body: &str, end: u32) -> String {
        format!(
            ".SECTION/PM seg_pmco;\n_x:\n    {body}LCNTR = 4, DO (PC, {end}) UNTIL LCE;\n        NOP;\n        NOP;\n    RTS;\n"
        )
    }

    #[test]
    fn a_loop_whose_end_is_compressed_is_refused_and_the_48_bit_form_passes() {
        let e = object_from_text(&numeric_loop("", 4))
            .err()
            .expect("accepted")
            .0;
        assert!(e.contains("DO loop at") && e.contains("finding 91"), "{e}");
        object_from_text(&numeric_loop(".NOCOMPRESS;\n    ", 6)).unwrap();
    }
}
