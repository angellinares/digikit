//! The DSP image scan (container section 7): instructions the SHARC+ ISA excludes (`build::sharc_legal`).
//!
//! A store through an index register (Types 3a, 3b, 3d, 15a, 15b) cannot take a source register of the same DAG: `DM(x, Ia) = Ureg`
//! not a DAG1 register, `PM(x, Ic) = Ureg` not a DAG2 register (finding 49 section 10). The rule is
//! `build::sharc_legal::legality_problems`, the same rules a module build fails on, with the post-modify load of a same-DAG register. The
//! stock images have none. The scan also applies the loop-end rule (`build::sharc_legal::loop_end_problems`: no VISA instruction in the
//! last 11 of a hardware loop).
//!
//! The scan decodes the code blocks of the boot stream with the aligned decode that built the sharcdb databases: every even offset
//! decoded with the confident width, then the instructions a sweep from the start reaches, kept when at least [`MIN_DEPTH`] decoded
//! instructions in a row end there. Code blocks are the stock ones (by target address; the DT2 and DN2 lists) and the run of blocks
//! below 0x80000000 a patch appended before the final block. Data blocks are not scanned: decoded as code they give false stores.
//! With the code ranges of the patch's plan (`Plan::sharc_code`), an appended block is scanned only inside them, as a module's data
//! placed behind its code in the same block gives false stores too.

use crate::build::sharc_legal::{Placed, legality_problems, loop_end_problems};
use crate::dsp::{L2_BYTE_BASE, L2_BYTE_LIMIT, L2_SW_BASE, SW_ALIAS_BASE, Stream};
use crate::error::*;
use sharc_rt::decode::{self, DecodeKind};

/// Decoded instructions in a row that make an offset count as code.
pub const MIN_DEPTH: u32 = 8;

/// The stock code blocks, by the target address they load to. Left out: the DT2 block at 0x282d7158 (388 bytes; its one hit is
/// inside a data table) and the DN2 block at 0x2001e888 (data decoded as code; finding 49 section 10.2).
const CODE_TARGETS: [u32; 9] = [
    0x2000_0000,
    0x2838_004C,
    0x2838_01A4,
    0x2838_0328,
    0x2838_0548,
    0x2838_26C8,
    0x2838_27CC,
    // Digitone II only
    0x2838_261C,
    0x2838_2720,
];

/// Indices of the blocks to scan: stock code by target address, then the appended run before the final block.
pub fn code_blocks(s: &Stream) -> Vec<usize> {
    let blocks = &s.blocks;
    let mut out: Vec<usize> = blocks
        .iter()
        .enumerate()
        .filter(|(_, b)| !b.fill() && b.count != 0 && CODE_TARGETS.contains(&b.target))
        .map(|(i, _)| i)
        .collect();
    let mut k = blocks.len() as isize - 1;
    if blocks.last().is_some_and(|b| b.is_final()) {
        k -= 1;
    }
    while k >= 0 {
        let b = &blocks[k as usize];
        if b.fill() || b.count == 0 || b.target >= 0x8000_0000 {
            break;
        }
        if !out.contains(&(k as usize)) {
            out.push(k as usize);
        }
        k -= 1;
    }
    out.sort_unstable();
    out
}

/// Short-word address of a code block's first word.
fn sw_base(target: u32) -> Option<u32> {
    if target >= SW_ALIAS_BASE {
        Some((target - SW_ALIAS_BASE) / 2)
    } else if (L2_BYTE_BASE..L2_BYTE_LIMIT).contains(&target) {
        Some(L2_SW_BASE + (target - L2_BYTE_BASE) / 2)
    } else {
        None
    }
}

/// `(byte offset, length in bytes, decode)` of the aligned instructions of `data` that [`MIN_DEPTH`] decoded instructions in a row end at.
pub fn aligned(data: &[u8]) -> Vec<(usize, decode::Decoded)> {
    let read = |sw: u32| -> Option<u16> {
        let at = sw as usize * 2;
        data.get(at..at + 2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
    };
    // every even offset that decodes (the direct-landing forms 10a never count as aligned)
    let slots = data.len() / 2 + 4;
    let mut table: Vec<Option<decode::Decoded>> = vec![None; slots];
    for off in (0..data.len().saturating_sub(1)).step_by(2) {
        let d = decode::decode_at(read, (off / 2) as u32);
        if d.kind != DecodeKind::Unknown && !matches!(d.type_name, "10a_rel" | "10a_abs") {
            table[off / 2] = Some(d);
        }
    }
    // depth: the longest decoded run ending with the instruction there
    let mut depth = vec![0u32; slots];
    for i in 0..data.len() / 2 {
        let Some(d) = &table[i] else { continue };
        let here = depth[i] + 1;
        depth[i] = here;
        let succ = i + d.length_bytes.unwrap_or(2) as usize / 2;
        if succ < slots && depth[succ] < here {
            depth[succ] = here;
        }
    }
    // the sweep from the start
    let mut out = vec![];
    let mut i = 0;
    while i < data.len() / 2 {
        match &table[i] {
            None => i += 1,
            Some(d) => {
                if depth[i] >= MIN_DEPTH {
                    out.push((i * 2, *d));
                }
                i += d.length_bytes.unwrap_or(2) as usize / 2;
            }
        }
    }
    out
}

/// Every scanned instruction as `block INDEX OFFSET TYPE` (a comparison aid: the same list from the Python decoder must be equal).
pub fn list(blob: &[u8]) -> Result<Vec<String>> {
    let stream = Stream::parse(blob.to_vec())?;
    let mut out = vec![];
    for idx in code_blocks(&stream) {
        let b = &stream.blocks[idx];
        if sw_base(b.target).is_none() {
            continue;
        }
        let Some(payload) = stream.read(b.target, b.count as usize) else {
            continue;
        };
        for (off, d) in aligned(&payload) {
            out.push(format!("block {idx} {off} {}", d.type_name));
        }
    }
    Ok(out)
}

/// One message per excluded store in the code blocks of the boot stream `blob` (container section 7, depacked). `code`: the byte
/// ranges a patch placed as code; an appended block is scanned only inside them (none: the whole block).
pub fn scan(blob: &[u8], code: Option<&[(u64, u64)]>) -> Result<Vec<String>> {
    let stream = Stream::parse(blob.to_vec())?;
    let mut found = vec![];
    for idx in code_blocks(&stream) {
        let b = &stream.blocks[idx];
        let Some(base) = sw_base(b.target) else {
            continue;
        };
        let Some(payload) = stream.read(b.target, b.count as usize) else {
            continue;
        };
        let stock = CODE_TARGETS.contains(&b.target);
        let placed_code = |off: usize| {
            let at = b.target as u64 + off as u64;
            stock || code.is_none_or(|c| c.iter().any(|&(lo, hi)| (lo..hi).contains(&at)))
        };
        let insns: Vec<_> = aligned(&payload)
            .into_iter()
            .filter(|(off, _)| placed_code(*off))
            .collect();
        for (off, d) in &insns {
            for msg in legality_problems(d, "") {
                found.push(format!(
                    "block {idx} pc {:#x}{msg}",
                    base + (off / 2) as u32
                ));
            }
        }
        // the loop ends (finding 91): positions are short-word addresses
        let placed: Vec<Placed> = insns
            .iter()
            .map(|(off, d)| {
                (
                    (base + (off / 2) as u32) as u64,
                    d.length_bytes.unwrap_or(2),
                    *d,
                )
            })
            .collect();
        found.extend(loop_end_problems(&placed, &format!("block {idx} ")));
    }
    Ok(found)
}

/// The scan of a firmware image: section 7 must exist and be clean (`code` as in [`scan`]). `Ok` carries the summary line.
pub fn check_section7(dsp: &[u8], code: Option<&[(u64, u64)]>) -> Result<String> {
    let found = scan(dsp, code)?;
    if found.is_empty() {
        return Ok("section 7: no instruction the ISA excludes".into());
    }
    let n = found.len();
    let shown: Vec<&str> = found.iter().take(8).map(String::as_str).collect();
    Err(err!(
        "{n} instruction(s) the SHARC+ ISA does not allow in section 7 (finding 49 section 10): {}{}",
        shown.join("; "),
        if n > shown.len() { "; ..." } else { "" }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build::sharc_obj::boot_bytes_unchecked_for_tests;
    use crate::dsp::block_header;

    const FINAL: u32 = 1 << 15;
    const APPENDED: u32 = 0x2001_AB80;

    fn with_code(h: [u8; 16], code: u32) -> [u8; 16] {
        let mut h = h;
        h[0..4].copy_from_slice(&code.to_le_bytes());
        h[2] = 0;
        h[2] = h.iter().fold(0, |a, &b| a ^ b);
        h
    }

    /// A boot stream: one appended code block, then the final block.
    fn stream_of(code: &[u8]) -> Vec<u8> {
        let mut s = block_header(APPENDED, code.len() as u32).to_vec();
        s.extend_from_slice(code);
        s.extend_from_slice(&with_code(block_header(0x1C_1338, 0), 0xAD00_0001 | FINAL));
        s
    }

    fn assembled(stores: &str) -> Vec<u8> {
        let nops = "    NOP;\n".repeat(10);
        boot_bytes_unchecked_for_tests(&format!(
            ".SECTION/PM seg_pmco;\n_x:\n{nops}{stores}    RTS;\n"
        ))
        .unwrap()
    }

    #[test]
    fn a_code_block_with_a_same_dag_store_is_refused_and_an_allowed_one_passes() {
        let bad = scan(
            &stream_of(&assembled("    DM(-16, I6) = I0;\n    PM(15, I12) = I8;\n")),
            None,
        )
        .unwrap();
        assert_eq!(bad.len(), 2, "{bad:?}");
        assert!(bad[0].starts_with("block 0 pc 0x"), "{bad:?}");
        assert!(bad[0].ends_with(": DM store of I0 (DAG1)"), "{bad:?}");
        assert!(bad[1].ends_with(": PM store of I8 (DAG2)"), "{bad:?}");
        let e = check_section7(&stream_of(&assembled("    DM(-16, I6) = I0;\n")), None)
            .unwrap_err()
            .0;
        assert!(
            e.contains("1 instruction(s)") && e.contains("DM store of I0"),
            "{e}"
        );
        // the direct-address store and a store of a data register are allowed
        let good = assembled("    DM(0x20000000) = I0;\n    DM(15, I4) = R0;\n");
        assert_eq!(scan(&stream_of(&good), None).unwrap(), Vec::<String>::new());
        check_section7(&stream_of(&good), None).unwrap();
    }

    #[test]
    fn with_the_plans_code_ranges_only_placed_code_of_an_appended_block_is_scanned() {
        // code, then bytes that decode as an excluded store (a module's data behind its code in the same block)
        let code = assembled("");
        let data = assembled("    DM(-16, I6) = I0;\n");
        let blob = stream_of(&[code.clone(), data].concat());
        assert_eq!(scan(&blob, None).unwrap().len(), 1);
        let placed = [(APPENDED as u64, APPENDED as u64 + code.len() as u64)];
        assert!(scan(&blob, Some(&placed)).unwrap().is_empty());
        // a store inside the code ranges is still found
        let all = [(APPENDED as u64, APPENDED as u64 + 0x1000)];
        assert_eq!(scan(&blob, Some(&all)).unwrap().len(), 1);
    }

    #[test]
    fn a_short_run_of_code_is_not_scanned() {
        // fewer than MIN_DEPTH instructions before the store: the decode is not trusted as code
        let short = boot_bytes_unchecked_for_tests(
            ".SECTION/PM seg_pmco;\n_x:\n    DM(-16, I6) = I0;\n    RTS;\n",
        )
        .unwrap();
        assert!(scan(&stream_of(&short), None).unwrap().is_empty());
    }

    #[test]
    fn the_stock_code_blocks_are_chosen_by_target_and_the_appended_run_before_the_final_block() {
        let mut s = vec![];
        for (target, count, code) in [
            (0x2000_0000u32, 8u32, 0xAD00_0001u32),
            (0x282D_7158, 8, 0xAD00_0001), // a data table: not scanned
            (0x2838_27CC, 8, 0xAD00_0001),
            (0x8046_7CD0, 8, 0xAD00_0001), // SDRAM data
            (0x2001_AB80, 8, 0xAD00_0001), // appended
            (0x2009_AFE8, 8, 0xAD00_0001), // appended
            (0x1C_1338, 0, 0xAD00_0001 | FINAL),
        ] {
            s.extend_from_slice(&with_code(block_header(target, count), code));
            s.extend_from_slice(&vec![0; count as usize]);
        }
        assert_eq!(code_blocks(&Stream::parse(s).unwrap()), [0, 2, 4, 5]);
    }
}
