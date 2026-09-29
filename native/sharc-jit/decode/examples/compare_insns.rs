//! Verification tool #1 (not part of the library): decode every record of
//! the native code generator's own instruction table
//! (`out/native/opt/gen-final/insns.bin`, `tools/sharc_rsgen.py::insn_blob`
//! format, `native/sharc/src/canon.rs::parse_insn_table`) with this crate,
//! over the `dt2-1.16` image loaded from a `pack_image` blob, and compare
//! against the table's own recorded decode (which is exactly Python's
//! `sharc_core.sequencer.decode_at(mem, None, pc)` at generation time, with
//! `tools/sharc_rsgen.py::split_compute()` already applied).
//!
//! Firmware-derived inputs only: a `pack_image` blob for `dt2-1.16` (from
//! `tools/sharc_transpile_run.py::pack_image`) and `insns.bin`/`syms.rs`'s
//! `SYM_NAMES` (from a native build of the generated core). Neither is read
//! by the library; this binary reads them itself and is not needed to build
//! or use `sharc_decode`.
//!
//! Usage:
//!     cargo run --release --example compare_insns -- \
//!         IMAGE.pack INSNS.bin SYM_NAMES.txt
//!
//! `SYM_NAMES.txt` is one symbol string per line, in `Sym` id order (id 0
//! first) -- see the sibling Python dump script that writes it.

use std::env;
use std::fs;
use std::process::ExitCode;

use sharc_decode::{Decoder, parse_pack_image};

struct Rd<'a> {
    b: &'a [u8],
    off: usize,
}

impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self
            .off
            .checked_add(n)
            .filter(|&e| e <= self.b.len())
            .ok_or_else(|| "insns.bin truncated".to_string())?;
        let s = &self.b[self.off..end];
        self.off = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn i8(&mut self) -> Result<i8, String> {
        Ok(self.take(1)?[0] as i8)
    }
    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn i64(&mut self) -> Result<i64, String> {
        Ok(i64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
}

struct RefInsn {
    pc: u32,
    type_name: String,
    kind: String,
    length_bytes: Option<u32>,
    fields: Vec<(String, i64)>,
}

fn parse_insns_bin(bytes: &[u8], sym_names: &[String]) -> Result<Vec<RefInsn>, String> {
    let mut r = Rd { b: bytes, off: 0 };
    if r.take(4)? != b"SHIX" {
        return Err("not an insns.bin (bad magic)".to_string());
    }
    let count = r.u32()?;
    let mut out = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let pc = r.u32()?;
        let type_sym = r.u16()?;
        let kind_sym = r.u16()?;
        let length = r.i8()?;
        let nfields = r.u8()?;
        let mut fields = Vec::with_capacity(nfields as usize);
        for _ in 0..nfields {
            let key_sym = r.u16()?;
            let _stem_sym = r.u16()?;
            let _hi = r.i8()?;
            let _lo = r.i8()?;
            let value = r.i64()?;
            fields.push((sym_names[key_sym as usize].clone(), value));
        }
        out.push(RefInsn {
            pc,
            type_name: sym_names[type_sym as usize].clone(),
            kind: sym_names[kind_sym as usize].clone(),
            length_bytes: if length < 0 {
                None
            } else {
                Some(length as u32)
            },
            fields,
        });
    }
    if r.off != r.b.len() {
        return Err(format!("trailing bytes: {} of {}", r.off, r.b.len()));
    }
    Ok(out)
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() != 4 {
        eprintln!("usage: compare_insns IMAGE.pack INSNS.bin SYM_NAMES.txt");
        return ExitCode::FAILURE;
    }
    let pack_bytes = fs::read(&args[1]).expect("read pack image");
    let insns_bytes = fs::read(&args[2]).expect("read insns.bin");
    let sym_names: Vec<String> = fs::read_to_string(&args[3])
        .expect("read SYM_NAMES.txt")
        .lines()
        .map(|s| s.to_string())
        .collect();

    let image = parse_pack_image(&pack_bytes).expect("parse pack image");
    let refs = parse_insns_bin(&insns_bytes, &sym_names).expect("parse insns.bin");
    let decoder = Decoder::new();

    let mut mismatches = Vec::new();
    for r in &refs {
        let got = decoder.decode_at(&image, r.pc);
        let ok = got.type_name == r.type_name
            && got.kind == r.kind
            && got.length_bytes == r.length_bytes
            && got.fields == r.fields;
        if !ok {
            mismatches.push((r, got));
        }
    }

    println!("compare_insns: {} records", refs.len());
    println!("  exact matches: {}", refs.len() - mismatches.len());
    println!("  mismatches:    {}", mismatches.len());
    for (r, got) in mismatches.iter().take(20) {
        println!(
            "  pc={:#x} ref=({:?},{:?},{:?},{:?}) got=({:?},{:?},{:?},{:?})",
            r.pc,
            r.type_name,
            r.kind,
            r.length_bytes,
            r.fields,
            got.type_name,
            got.kind,
            got.length_bytes,
            got.fields
        );
    }
    if mismatches.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
