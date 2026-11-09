//! A SHARC+ object from an expanded source text, in this process: the public Selache assembler as a library (`selas`), the checks
//! around it, and a re-decode of every instruction with the decoder of the runtime (`sharc-rt`, the public ISA table).
//!
//! The assembler writes a relocatable ELF whose 16-bit parcels are big endian. Steps, in order:
//!
//! 1. [`bss_prepare`] takes `.PKBSS` / `.PKDATA name, size;` out (Selache drops `.VAR name[N];`); the object gets a section for each.
//! 2. `selas::assemble_text` writes the ELF; [`read_object`] reads the section, symbols, data sections and relocations.
//! 3. [`native_rows`] finds the instruction extents with our decoder and cross-checks Selache's own.
//! 4. [`link`] places the section at a placeholder base with placeholder externs; [`verify`] re-decodes every instruction and reads the
//!    relocated fields back.
//!
//! Relocations Selache emits (offsets and symbol values are in 16-bit parcels, the PC unit): type 11 is the absolute 24-bit target of a
//! 48-bit jump or call (low 24 bits of the instruction), type 12 the absolute 32-bit immediate or address (low 32 bits). Both carry the
//! symbol's address; the addend is in the symbol name (`sym + 4`).

use super::sharc_legal::{Placed, legality_problems, loop_end_problems};
use crate::dko::{Dko, DkoReloc, DkoSection, DkoSymbol, FORMAT};
use crate::error::*;
use regex_lite::{Regex, RegexBuilder};
use sharc_rt::decode::{self, Decoded};
use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

/// The Selache commit this build links (`selas`, `selelf` and `selinstr` in Cargo.toml). `tools/patchkit/pk_toolchains.py` pins the same
/// one for the command-line tools (`SELACHE_REV`); a test keeps the two equal.
pub const SELACHE_REV: &str = "02d6d785b8e3a125ab237a00bb89503aff835f5d";
const SECTION: &str = "seg_pmco";
const PROCESSOR: &str = "ADSP-21569";
const R_ABS24: u32 = 11;
const R_ABS32: u32 = 12;
/// Where the object is placed to read its relocated fields back (any value; the object itself keeps parcel offsets).
const PLACEHOLDER_BASE: i64 = 0xB90000;

fn rx(cell: &'static OnceLock<Regex>, pat: &str, flags: &str) -> &'static Regex {
    cell.get_or_init(|| {
        let mut b = RegexBuilder::new(pat);
        b.case_insensitive(flags.contains('i'))
            .multi_line(flags.contains('m'))
            .dot_matches_new_line(flags.contains('s'));
        b.build().expect("SHARC+ source pattern")
    })
}

/// Swap the two bytes of every complete 16-bit parcel (Selache order <-> boot-stream order); a trailing byte stays.
pub fn swap_parcels(data: &[u8]) -> Vec<u8> {
    let mut v = data.to_vec();
    for c in v.chunks_exact_mut(2) {
        c.swap(0, 1);
    }
    v
}

/// The 16-bit parcels of boot-order bytes (a trailing byte is dropped), at most three.
fn parcels(boot: &[u8]) -> Vec<u16> {
    boot.chunks_exact(2)
        .take(3)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect()
}

/// The native decode of one row (Selache parcel order), among the forms of at most `max_bytes`.
fn decode_row(raw: &[u8], max_bytes: u8) -> Decoded {
    decode::decode_parcels(&parcels(&swap_parcels(raw)), max_bytes)
}

// ------------------------------------------------------------------------------------------------------------------------ checks

/// `.PKDATA name, bytes;` (loaded zeros, part of the boot-stream block) or `.PKBSS name, bytes;` (reserved memory the stream does not
/// load: the program must write it before it reads it): the line is replaced by `.EXTERN name;`, and the object gets a data or bss
/// section of that size with a global data symbol `name` (the linker allocates it). The size may be a `#define` of a plain number in
/// the same source (the generated device includes of `patchkit gen`).
fn bss_prepare(source: &str) -> Result<(String, Vec<(String, u64, bool)>)> {
    static P: OnceLock<Regex> = OnceLock::new();
    static D: OnceLock<Regex> = OnceLock::new();
    let pk = rx(
        &P,
        r"^[ \t]*\.PK(BSS|DATA)[ \t]+([A-Za-z_.$][\w.$]*)[ \t]*,[ \t]*(0x[0-9a-fA-F]+|\d+|[A-Za-z_]\w*)[ \t]*;[^\n]*$",
        "m",
    );
    let def = rx(
        &D,
        r"^[ \t]*#define[ \t]+([A-Za-z_]\w*)[ \t]+(0x[0-9a-fA-F]+|\d+)[ \t]*(?://[^\n]*)?$",
        "m",
    );
    let mut defines: HashMap<String, u64> = HashMap::new();
    for m in def.captures_iter(source) {
        defines.insert(m[1].to_string(), parse_uint(&m[2])?);
    }
    let mut found = vec![];
    let mut failed: Option<Error> = None;
    let out = pk.replace_all(source, |c: &regex_lite::Captures| {
        let size = &c[3];
        let n = match defines.get(size) {
            Some(n) => *n,
            None if size.as_bytes()[0].is_ascii_digit() => match parse_uint(size) {
                Ok(n) => n,
                Err(e) => {
                    failed.get_or_insert(e);
                    0
                }
            },
            None => {
                failed.get_or_insert(err!(
                    ".PK{} {}: size {size} is not a number or a #define of one",
                    &c[1],
                    &c[2]
                ));
                0
            }
        };
        found.push((c[2].to_string(), n, &c[1] == "BSS"));
        format!(".EXTERN {};", &c[2])
    });
    if let Some(e) = failed {
        return Err(e);
    }
    Ok((out.into_owned(), found))
}

/// `int(text, 0)` for an unsigned literal: `0x` hex or decimal.
fn parse_uint(t: &str) -> Result<u64> {
    let r = match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        Some(h) => u64::from_str_radix(h, 16),
        None => t.parse(),
    };
    r.map_err(|_| err!("bad number {t:?}"))
}

/// `sym`, `sym + 4`, `sym - 0x10`: the symbol and the addend.
pub fn split_symbol(name: &str) -> Result<(String, i64)> {
    static C: OnceLock<Regex> = OnceLock::new();
    let re = rx(
        &C,
        r"^\s*([A-Za-z_.$][\w.$]*)\s*(?:([+-])\s*(\w+))?\s*$",
        "",
    );
    let bad = || err!("cannot parse relocation symbol {name:?}");
    let m = re.captures(name).ok_or_else(bad)?;
    let add = match m.get(2) {
        Some(sign) => {
            let n = parse_uint(&m[3]).map_err(|_| bad())? as i64;
            if sign.as_str() == "-" { -n } else { n }
        }
        None => 0,
    };
    Ok((m[1].to_string(), add))
}

// --------------------------------------------------------------------------------------------------------------------- the ELF

struct ElfSec {
    name: String,
    ty: u32,
    flags: u32,
    size: u32,
    align: u32,
    info: u32,
    data: Vec<u8>,
}

struct ElfSym {
    name: String,
    value: u32,
    global: bool,
    shndx: u16,
}

struct ElfRel {
    section: String,
    offset: u32,
    rtype: u32,
    symbol: usize,
    addend: i32,
}

struct Elf {
    sections: Vec<ElfSec>,
    symbols: Vec<ElfSym>,
    relocs: Vec<ElfRel>,
}

/// The sections, symbols and relocations of a relocatable ELF32 object of either byte order (reads with `selelf`).
fn parse_elf(blob: &[u8]) -> Result<Elf> {
    use selelf::elf as se;
    const SHT_SYMTAB: u32 = 2;
    let h = se::parse_header(blob).map_err(|e| err!("selas object: {e}"))?;
    let e = h.ei_data;
    let at = |off: usize, n: usize| -> Result<&[u8]> {
        blob.get(off..off.checked_add(n).unwrap_or(usize::MAX))
            .ok_or_else(|| err!("selas object: truncated"))
    };
    let mut raw = vec![];
    for i in 0..h.e_shnum as usize {
        let off = h.e_shoff as usize + i * h.e_shentsize as usize;
        raw.push(se::parse_section_header(at(off, 40)?, e));
    }
    let shstr = raw
        .get(h.e_shstrndx as usize)
        .ok_or_else(|| err!("selas object: no section name table"))?;
    let strtab = at(shstr.sh_offset as usize, shstr.sh_size as usize)?;
    let mut sections = vec![];
    for s in &raw {
        let data = if s.sh_type == se::SHT_NOBITS {
            vec![]
        } else {
            at(s.sh_offset as usize, s.sh_size as usize)?.to_vec()
        };
        sections.push(ElfSec {
            name: se::read_string_at(strtab, s.sh_name).to_string(),
            ty: s.sh_type,
            flags: s.sh_flags,
            size: s.sh_size,
            align: s.sh_addralign,
            info: s.sh_info,
            data,
        });
    }
    let mut symbols = vec![];
    if let Some(i) = raw.iter().position(|s| s.sh_type == SHT_SYMTAB) {
        let str_sec = raw
            .get(raw[i].sh_link as usize)
            .ok_or_else(|| err!("selas object: no symbol name table"))?;
        let names = at(str_sec.sh_offset as usize, str_sec.sh_size as usize)?;
        for k in 0..raw[i].sh_size as usize / 16 {
            let s = se::parse_symbol(at(raw[i].sh_offset as usize + k * 16, 16)?, e);
            symbols.push(ElfSym {
                name: if s.st_name == 0 {
                    String::new()
                } else {
                    se::read_string_at(names, s.st_name).to_string()
                },
                value: s.st_value,
                global: s.bind() != se::STB_LOCAL,
                shndx: s.st_shndx,
            });
        }
    }
    let mut relocs = vec![];
    for s in &sections {
        if s.ty != se::SHT_RELA && s.ty != se::SHT_REL {
            continue;
        }
        let target = sections
            .get(s.info as usize)
            .ok_or_else(|| err!("selas object: relocation section without a target"))?;
        let n = if s.ty == se::SHT_RELA { 12 } else { 8 };
        for k in 0..s.data.len() / n {
            let c = &s.data[k * n..(k + 1) * n];
            let (offset, info, addend) = if s.ty == se::SHT_RELA {
                let r = se::parse_rela(c, e);
                (r.r_offset, r.r_info, r.r_addend)
            } else {
                let r = se::parse_rel(c, e);
                (r.r_offset, r.r_info, 0)
            };
            relocs.push(ElfRel {
                section: target.name.clone(),
                offset,
                rtype: info & 0xFF,
                symbol: (info >> 8) as usize,
                addend,
            });
        }
    }
    Ok(Elf {
        sections,
        symbols,
        relocs,
    })
}

// ------------------------------------------------------------------------------------------------------------------ the object

#[derive(Clone, Debug)]
struct Reloc {
    /// Offset of the instruction, in parcels.
    parcel: u64,
    rtype: u32,
    symbol: String,
    addend: i64,
}

/// A row: an instruction at a parcel offset, with its bytes in Selache parcel order.
type Row = (u64, Vec<u8>);

/// An insertion-ordered map (a Python `dict`): a name set again keeps its place and takes the new value.
struct Ordered<V>(Vec<(String, V)>);

impl<V> Ordered<V> {
    fn put(&mut self, k: String, v: V) {
        match self.0.iter_mut().find(|(n, _)| *n == k) {
            Some(e) => e.1 = v,
            None => self.0.push((k, v)),
        }
    }
    fn has(&self, k: &str) -> bool {
        self.0.iter().any(|(n, _)| n == k)
    }
}

pub struct Obj {
    source_sha256: String,
    /// The text section, in Selache parcel order.
    section: Vec<u8>,
    /// Text symbols: name -> (parcel offset, global).
    symbols: BTreeMap<String, (u64, bool)>,
    relocs: Vec<Reloc>,
    rows: Vec<Row>,
    /// Data sections (DM): name -> (bytes in memory order, alignment, is bss).
    data: Ordered<(Vec<u8>, u32, bool)>,
    /// Data symbols: name -> (section, byte offset, global).
    data_symbols: Ordered<(String, u64, bool)>,
}

/// Read what the assembler wrote: the text section, its symbols and relocations, the data sections and their symbols.
fn read_object(blob: &[u8], source_sha256: String) -> Result<Obj> {
    let elf = parse_elf(blob)?;
    let section = elf
        .sections
        .iter()
        .map(|s| s.name.as_str())
        .find(|n| *n == SECTION || *n == "seg_swco")
        .unwrap_or(SECTION)
        .to_string();
    let sec = elf
        .sections
        .iter()
        .find(|s| s.name == section)
        .ok_or_else(|| {
            err!("no section {section} (a .SECTION/PM {section}; directive is required)")
        })?;
    // Selache's own instruction extents (VISA: the width comes from the first parcels). A last instruction that runs past the end of
    // the section is listed as the bytes that are left (the same as the listing of `seldump`).
    let mut selache_rows: Vec<Row> = vec![];
    let mut at = 0usize;
    while at + 2 <= sec.data.len() {
        let n = selinstr::visa::instruction_len(&sec.data, at);
        let end = sec.data.len().min(at + n);
        selache_rows.push(((at / 2) as u64, sec.data[at..end].to_vec()));
        at = end;
    }
    let in_section = |s: &ElfSym, name: &dyn Fn(&str) -> bool| {
        !s.name.is_empty()
            && s.shndx > 0
            && s.shndx < 0xFF00
            && elf
                .sections
                .get(s.shndx as usize)
                .is_some_and(|x| name(&x.name))
    };
    let mut symbols = BTreeMap::new();
    for s in &elf.symbols {
        if in_section(s, &|n| n == section) {
            symbols.insert(s.name.clone(), (s.value as u64, s.global));
        }
    }
    let mut relocs = vec![];
    for r in &elf.relocs {
        if r.section != section {
            return Err(err!("relocation in unexpected section {}", r.section));
        }
        let sym = elf.symbols.get(r.symbol).ok_or_else(|| {
            err!(
                "relocation names symbol {} of {}",
                r.symbol,
                elf.symbols.len()
            )
        })?;
        if sym.name.is_empty() {
            return Err(err!("section-relative relocation not supported"));
        }
        let (name, add) = split_symbol(&sym.name)?;
        relocs.push(Reloc {
            parcel: r.offset as u64,
            rtype: r.rtype,
            symbol: name,
            addend: add + r.addend as i64,
        });
    }
    let mut data = Ordered(vec![]);
    for x in &elf.sections {
        if x.flags & 2 != 0 && x.flags & 4 == 0 && x.name != section && x.size != 0 {
            let nobits = x.ty == 8;
            data.put(
                x.name.clone(),
                (
                    if nobits { vec![] } else { x.data.clone() },
                    x.align.max(4),
                    nobits,
                ),
            );
        }
    }
    let mut data_symbols = Ordered(vec![]);
    for s in &elf.symbols {
        if in_section(s, &|n| data.has(n)) {
            data_symbols.put(
                s.name.clone(),
                (
                    elf.sections[s.shndx as usize].name.clone(),
                    s.value as u64,
                    s.global,
                ),
            );
        }
    }
    let rows = native_rows(&selache_rows, &relocs, &sec.data)?;
    Ok(Obj {
        source_sha256,
        section: sec.data.clone(),
        symbols,
        relocs,
        rows,
        data,
        data_symbols,
    })
}

/// The instruction rows of the section by our decoder's extents, cross-checked against Selache's listing.
///
/// Selache's listing is of the unrelocated object and splits some 48-bit forms early (Type 3a `DM(I,M) = Ureg` is listed as 4 bytes plus a
/// separate `0000` row), so its rows cannot be the extents. Ours are; the check against Selache is that every row it lists either starts
/// where ours does with the same length, or is a shorter listing of ours whose missing parcels are zero, and that each relocation sits on
/// the start of one of our instructions.
///
/// Width ties: a 16-bit short compute (Type 2c) and a 32-bit form share their leading bits, so a free-running decode reads `R0 = R0 + R1;`
/// (0xc001) plus the next word as one longer instruction. The emulator breaks the tie with a lookahead over the successors
/// (`sharc_rt::decode::decode_at`); here the assembler, which chose the form, decides: its shorter extent is taken when that extent itself
/// decodes to a form (finding 45).
fn native_rows(selache_rows: &[Row], relocs: &[Reloc], section: &[u8]) -> Result<Vec<Row>> {
    let selache_len: HashMap<u64, usize> =
        selache_rows.iter().map(|(p, r)| (*p, r.len())).collect();
    let boot = swap_parcels(section);
    let mut rows: Vec<Row> = vec![];
    let mut offset = 0usize;
    while offset < boot.len() {
        let win = &boot[offset..boot.len().min(offset + 6)];
        let d = decode::decode_parcels(&parcels(win), 6);
        let mut extent = d
            .length_bytes
            .map_or(2.min(boot.len() - offset), |n| n as usize);
        if let Some(&short) = selache_len.get(&((offset / 2) as u64))
            && short < extent
            && (short == 2 || short == 4)
        {
            let narrow =
                decode::decode_parcels(&parcels(&boot[offset..offset + short]), short as u8);
            if narrow.length_bytes == Some(short as u8) {
                extent = short;
            }
        }
        let end = boot.len().min(offset + extent);
        rows.push(((offset / 2) as u64, swap_parcels(&boot[offset..end])));
        offset += extent;
    }
    let nat: HashMap<u64, &Vec<u8>> = rows.iter().map(|(p, r)| (*p, r)).collect();
    // A Type 3a with a non-zero compute (a multifunction or multiplier with a register move, `F10 = F0 * F7, F0 = DM(I4, M6);`) is
    // also listed as 4 bytes, but the compute's low 16 bits are not a zero row: the listing is out of step with ours from there. The
    // 4-byte prefix is still compared; the rows that follow are ignored until one equals ours again.
    let mut in_step = true;
    for (parcel, raw) in selache_rows {
        let Some(mine) = nat.get(parcel) else {
            continue;
        };
        if *mine == raw || (mine.starts_with(raw) && mine[raw.len()..].iter().all(|b| *b == 0)) {
            in_step = true;
        } else if in_step && raw.len() == 4 && mine.len() == 6 && mine.starts_with(raw) {
            in_step = false;
        } else if in_step {
            return Err(err!(
                "parcel {parcel:#x}: Selache lists {}, our decoder {}",
                hex(raw),
                hex(mine)
            ));
        }
    }
    for r in relocs {
        if !nat.contains_key(&r.parcel) {
            return Err(err!(
                "relocation at parcel {:#x} is not on one of our instruction starts",
                r.parcel
            ));
        }
    }
    Ok(rows)
}

/// The value of the field `key` of a decoded instruction.
fn field(d: &Decoded, key: &str) -> Option<i64> {
    d.fields()
        .iter()
        .rev()
        .find(|f| f.key == key)
        .map(|f| f.value)
}

/// Join the pieces `stem[hi:lo]` of a decoded form into one number; a form with the whole field only gives that.
pub(super) fn field_joined(d: &Decoded, stem: &str) -> Option<i64> {
    let mut parts = vec![];
    for f in d.fields() {
        if let Some(r) = f.key.strip_prefix(stem)
            && let Some(r) = r.strip_prefix('[').and_then(|r| r.strip_suffix(']'))
            && let Some((hi, lo)) = r.split_once(':')
            && hi.parse::<u32>().is_ok()
            && let Ok(lo) = lo.parse::<u32>()
        {
            parts.push((lo, f.value));
        }
    }
    if parts.is_empty() {
        return field(d, stem);
    }
    Some(parts.iter().map(|(lo, v)| v << lo).sum())
}

// Shift-immediate (Type 6) forms. shiftimm[22:16] is the ShiftImm opcode, PRM Table 17-9 (bits 21-16); these are the defined values.
const SHIFT_IMM_FORMS: [&str; 3] = ["6a_mem", "6a_nomem", "6b_shiftimm"];
const SHIFT_IMM_OPCODES: [i64; 18] = [
    0x00, 0x01, 0x02, 0x08, 0x09, 0x10, 0x11, 0x12, 0x13, 0x14, 0x19, 0x1B, 0x1D, 0x1F, 0x30, 0x31,
    0x32, 0x33,
];
// LSHIFT, ASHIFT, OR LSHIFT, OR ASHIFT take a signed 8-bit count in shiftimm[15:8]; dataex[3:0] holds its sign extension (0xF for a
// negative count, else 0) in every such instruction of the stock images (finding 05, "Selache shift-immediate encodings"). The silicon
// raises ILOPI on a negative count without it. The assembler writes it; this checks the object, as the check against PRM Table 17-9 above.
const SIGNED_SHIFT_OPCODES: [i64; 4] = [0x00, 0x01, 0x08, 0x09];

fn shift_imm_problem(d: &Decoded) -> Option<String> {
    if !SHIFT_IMM_FORMS.contains(&d.type_name) {
        return None;
    }
    let op = field(d, "shiftimm[22:16]");
    let Some(op) = op.filter(|o| SHIFT_IMM_OPCODES.contains(o)) else {
        return Some(format!(
            "undefined ShiftImm opcode {} (PRM Table 17-9)",
            op.map_or("None".into(), |o| o.to_string())
        ));
    };
    if SIGNED_SHIFT_OPCODES.contains(&op) {
        let negative = (field(d, "shiftimm[15:0]").unwrap_or(0) >> 8) & 0x80;
        let want = if negative != 0 { 0xF } else { 0 };
        if field(d, "dataex[3:0]") != Some(want) {
            return Some("shift count without its sign extension (dataex)".into());
        }
    }
    None
}

// ---------------------------------------------------------------------------------------------------------- link and verify

#[derive(Debug)]
struct Linked {
    /// Boot-stream order (little endian per parcel), after relocation.
    code_boot: Vec<u8>,
    /// Short-word addresses.
    symbols: HashMap<String, i64>,
    checked: usize,
    ureg_class: Vec<String>,
    /// Hardware loops whose end is not 48-bit (`sharc_legal::loop_end_problems`).
    loop_end: Vec<String>,
}

/// Selache leaves a relocated field zero or holding the addend; anything else means we misread the relocation.
fn self_check(code: &[u8], at: usize, size: usize, r: &Reloc) -> Result<()> {
    let mask: u64 = (1u64 << (8 * size)) - 1;
    let have = code[at..at + size]
        .iter()
        .fold(0u64, |a, b| (a << 8) | *b as u64);
    if have != 0 && have != (r.addend as u64) & mask {
        return Err(err!(
            "parcel {:#x}: field {have:#x} is neither 0 nor the addend {} before relocation",
            r.parcel,
            r.addend
        ));
    }
    Ok(())
}

fn link(obj: &Obj, base_sw: i64, externs: &HashMap<String, i64>) -> Result<Linked> {
    let mut code = obj.section.clone();
    let syms: HashMap<String, i64> = obj
        .symbols
        .iter()
        .map(|(n, (off, _))| (n.clone(), base_sw + *off as i64))
        .collect();
    let extents: HashMap<u64, usize> = obj.rows.iter().map(|(p, r)| (*p, r.len())).collect();
    for r in &obj.relocs {
        let target = match (syms.get(&r.symbol), externs.get(&r.symbol)) {
            (Some(t), _) | (None, Some(t)) => *t,
            _ => return Err(err!("unresolved symbol {}", r.symbol)),
        } + r.addend;
        if extents.get(&r.parcel).copied().unwrap_or(0) != 6 {
            return Err(err!(
                "relocation at parcel {:#x}: not a 48-bit instruction",
                r.parcel
            ));
        }
        let at = r.parcel as usize * 2;
        match r.rtype {
            R_ABS24 => {
                if !(0..1 << 24).contains(&target) {
                    return Err(err!(
                        "{}: {target:#x} does not fit a 24-bit target",
                        r.symbol
                    ));
                }
                self_check(&code, at + 3, 3, r)?;
                code[at + 3..at + 6].copy_from_slice(&(target as u32).to_be_bytes()[1..]);
            }
            R_ABS32 => {
                self_check(&code, at + 2, 4, r)?;
                code[at + 2..at + 6].copy_from_slice(&(target as u32).to_be_bytes());
            }
            t => return Err(err!("relocation type {t} not supported")),
        }
    }
    let mut out = Linked {
        code_boot: swap_parcels(&code),
        symbols: syms,
        checked: 0,
        ureg_class: vec![],
        loop_end: vec![],
    };
    verify(obj, &code, &mut out, externs)?;
    Ok(out)
}

/// Every instruction decodes natively with the extent Selache chose; the emulator reads the width the assembler wrote; relocated
/// fields read back.
fn verify(
    obj: &Obj,
    code_selache: &[u8],
    linked: &mut Linked,
    externs: &HashMap<String, i64>,
) -> Result<()> {
    let mut problems: Vec<String> = vec![];
    let mut by_parcel: HashMap<u64, Decoded> = HashMap::new();
    for (parcel, raw) in &obj.rows {
        let at = *parcel as usize * 2;
        let raw = &code_selache[at..at + raw.len()];
        let d = decode_row(raw, 6);
        linked
            .ureg_class
            .extend(legality_problems(&d, &format!("parcel {parcel:#x}")));
        if let Some(why) = shift_imm_problem(&d) {
            problems.push(format!("parcel {parcel:#x} {}: {why}", hex(raw)));
        }
        let agrees = d.length_bytes.is_some_and(|n| n as usize == raw.len());
        if d.length_bytes.is_none() || !agrees {
            // a width tie (Type 2c against a longer form): the assembler's extent decides when a form of that width matches
            let n = raw.len();
            if n == 2 || n == 4 {
                let tie = decode_row(raw, n as u8);
                if tie.length_bytes == Some(n as u8) {
                    by_parcel.insert(*parcel, d);
                    continue;
                }
            }
            problems.push(format!(
                "parcel {parcel:#x} {}: native {} extent_agrees={}",
                hex(raw),
                if d.length_bytes.is_some() {
                    d.type_name
                } else {
                    "None"
                },
                if agrees { "True" } else { "False" }
            ));
        }
        by_parcel.insert(*parcel, d);
    }
    let placed: Vec<Placed> = obj
        .rows
        .iter()
        .filter_map(|(parcel, raw)| Some((*parcel, raw.len() as u8, *by_parcel.get(parcel)?)))
        .collect();
    linked.loop_end = loop_end_problems(&placed, "parcel ");
    // The emulator breaks the width tie with a lookahead over the successors; the silicon has no such choice. An instruction the emulator
    // would read with another extent than the assembler's runs differently there (finding 45).
    let boot = &linked.code_boot;
    let word = |pc: u32| -> Option<u16> {
        let at = pc as usize * 2;
        boot.get(at..at + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
    };
    for (parcel, raw) in &obj.rows {
        let mine = raw.len();
        let seen = decode::decode_at(word, *parcel as u32);
        if let Some(n) = seen.length_bytes
            && n as usize != mine
        {
            problems.push(format!(
                "parcel {parcel:#x} {}: the emulator's width resolution reads {} of {n} bytes, the assembler wrote {mine} (a 16-bit \
                 `Rn = Rn op Rx`: the emulator would run another instruction than the silicon; use distinct registers, e.g. `R0 = R1 + R2`; \
                 finding 45)",
                hex(raw),
                seen.type_name
            ));
        }
    }
    for r in &obj.relocs {
        let d = &by_parcel[&r.parcel];
        let want = linked
            .symbols
            .get(&r.symbol)
            .or_else(|| externs.get(&r.symbol))
            .copied()
            .unwrap_or(0)
            + r.addend;
        let got = ["addr", "data", "reladdr"]
            .iter()
            .find_map(|stem| field_joined(d, stem));
        let mask: i64 = if r.rtype == R_ABS24 {
            0xFF_FFFF
        } else {
            0xFFFF_FFFF
        };
        if got != Some(want & mask) {
            problems.push(format!(
                "parcel {:#x} ({}): relocated field reads {} want {:#x}",
                r.parcel,
                d.type_name,
                got.map_or("None".into(), |g| g.to_string()),
                want & mask
            ));
        }
    }
    if !problems.is_empty() {
        return Err(err!(
            "Selache output disagrees with our decoder:\n  {}",
            problems.join("\n  ")
        ));
    }
    linked.checked = obj.rows.len();
    Ok(())
}

// ---------------------------------------------------------------------------------------------------------------------- the whole

/// Assemble `source` and read the object (no link).
pub fn assemble(source: &str) -> Result<Obj> {
    let (text, bss_decls) = bss_prepare(source)?;
    let elf = selas::assemble_text(&text, Some(PROCESSOR), &[], &[], true)
        .map_err(|e| err!("selas failed:\nselas: {e}"))?;
    let mut obj = read_object(&elf, sha256_hex(source.as_bytes()))?;
    for (name, size, is_bss) in bss_decls {
        if size == 0 || !size.is_multiple_of(4) {
            return Err(err!(
                ".PKBSS/.PKDATA {name}: size {size} is not a positive multiple of 4"
            ));
        }
        let sec = format!("{}_{name}", if is_bss { "pkbss" } else { "pkdata" });
        obj.data
            .put(sec.clone(), (vec![0; size as usize], 4, is_bss));
        obj.data_symbols.put(name, (sec, 0, true));
    }
    Ok(obj)
}

/// The boot-stream bytes of assembled `source`, whatever the ISA rules say (a test makes the stores the scan must find).
pub fn boot_bytes_unchecked_for_tests(source: &str) -> Result<Vec<u8>> {
    Ok(swap_parcels(&assemble(source)?.section))
}

/// The `.dko.json` object of an expanded source text: assembled, linked at a placeholder base with placeholder externs, every
/// instruction re-decoded and every relocated field read back.
pub fn object_from_text(source: &str) -> Result<Dko> {
    let obj = assemble(source)?;
    let mut undefined: Vec<String> = obj
        .relocs
        .iter()
        .map(|r| r.symbol.clone())
        .filter(|s| !obj.symbols.contains_key(s))
        .collect();
    undefined.sort();
    undefined.dedup();
    let mut externs: HashMap<String, i64> = undefined
        .iter()
        .enumerate()
        .map(|(i, n)| (n.clone(), PLACEHOLDER_BASE + 0x40 * i as i64))
        .collect();
    for (i, (name, _)) in obj.data_symbols.0.iter().enumerate() {
        externs.insert(name.clone(), 0x2009_0000 + 0x10 * i as i64);
    }
    let linked = link(&obj, PLACEHOLDER_BASE, &externs)?;
    if !linked.ureg_class.is_empty() {
        // an instruction the ISA excludes (sharc_legal): the silicon does not run it
        return Err(err!(
            "{} instruction(s) the ISA does not allow (finding 49 section 10): {}",
            linked.ureg_class.len(),
            linked.ureg_class.join("; ")
        ));
    }
    if !linked.loop_end.is_empty() {
        // a VISA instruction at the end of a hardware loop (sharc_legal, finding 91)
        return Err(err!(
            "{} hardware loop(s) end in instructions that are not 48-bit (finding 91; the last 11 instructions of a DO loop must be ISA, the assembler keeps a symbolic loop end 48-bit; any other end needs .NOCOMPRESS;): {}",
            linked.loop_end.len(),
            linked.loop_end.join("; ")
        ));
    }
    let mut sections = vec![];
    let code_boot = swap_parcels(&obj.section);
    sections.push(DkoSection {
        name: SECTION.into(),
        kind: "text".into(),
        align: 8,
        size: code_boot.len() as u32,
        hex: hex(&code_boot),
    });
    for (name, (blob, align, bss)) in &obj.data.0 {
        sections.push(DkoSection {
            name: name.clone(),
            kind: if *bss { "bss" } else { "data" }.into(),
            align: *align,
            size: blob.len() as u32,
            // bss is reserved by size (dko.rs does not read its hex)
            hex: if *bss { String::new() } else { hex(blob) },
        });
    }
    let mut symbols = vec![];
    for (name, (off, global)) in &obj.symbols {
        symbols.push(DkoSymbol {
            name: name.clone(),
            section: Some(SECTION.into()),
            value: *off,
            global: *global,
        });
    }
    let mut ds: Vec<_> = obj.data_symbols.0.iter().collect();
    ds.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, (sec, off, global)) in ds {
        symbols.push(DkoSymbol {
            name: name.clone(),
            section: Some(sec.clone()),
            value: *off,
            global: *global,
        });
    }
    for name in &undefined {
        if !obj.data_symbols.has(name) {
            symbols.push(DkoSymbol {
                name: name.clone(),
                section: None,
                value: 0,
                global: true,
            });
        }
    }
    let mut relocs = vec![];
    for r in &obj.relocs {
        relocs.push(DkoReloc {
            section: SECTION.into(),
            offset: r.parcel,
            rtype: if r.rtype == R_ABS24 {
                "sharc.abs24"
            } else {
                "sharc.abs32"
            }
            .into(),
            symbol: r.symbol.clone(),
            addend: r.addend,
        });
    }
    Ok(Dko {
        format: FORMAT,
        arch: "sharc".into(),
        source_sha256: obj.source_sha256,
        toolchain: BTreeMap::from([
            ("selache_rev".to_string(), SELACHE_REV.to_string()),
            (
                "instructions_redecoded".to_string(),
                linked.checked.to_string(),
            ),
        ]),
        sections,
        symbols,
        relocs,
    })
}

// ------------------------------------------------------------------------------------------------ the test rigs (`patchkit sharc`)

/// What the Python test rigs read of an assembled object (`patchkit sharc asm`): the section in Selache parcel order and what the
/// rigs place themselves.
pub fn object_json(o: &Obj) -> serde_json::Value {
    use serde_json::json;
    let mut data = serde_json::Map::new();
    for (n, (b, a, bss)) in &o.data.0 {
        data.insert(n.clone(), json!([hex(b), a, bss]));
    }
    let mut ds = serde_json::Map::new();
    for (n, (sec, off, g)) in &o.data_symbols.0 {
        ds.insert(n.clone(), json!([sec, off, g]));
    }
    json!({
        "source_sha256": o.source_sha256,
        "section": hex(&o.section),
        "symbols": o.symbols.iter().map(|(n, (p, g))| (n.clone(), json!([p, g]))).collect::<serde_json::Map<_, _>>(),
        "relocs": o.relocs.iter().map(|r| json!({"parcel": r.parcel, "rtype": r.rtype, "symbol": r.symbol, "addend": r.addend})).collect::<Vec<_>>(),
        "rows": o.rows.iter().map(|(p, r)| json!([p, hex(r)])).collect::<Vec<_>>(),
        "data": data,
        "data_symbols": ds,
        "toolchain": {"selache_rev": SELACHE_REV},
    })
}

/// An ELF object another Selache tool wrote (the C compiler), read the same way.
pub fn object_from_elf(blob: &[u8], source: &str) -> Result<Obj> {
    read_object(blob, sha256_hex(source.as_bytes()))
}

/// Place `obj` at `base_sw` (short-word address) with `externs` and verify it: the linked code and symbols as JSON.
pub fn link_json(
    obj: &Obj,
    base_sw: i64,
    externs: &HashMap<String, i64>,
) -> Result<serde_json::Value> {
    let l = link(obj, base_sw, externs)?;
    let mut syms: Vec<_> = l.symbols.iter().collect();
    syms.sort();
    Ok(serde_json::json!({
        "base_sw": base_sw,
        "code_selache": hex(&swap_parcels(&l.code_boot)),
        "code_boot": hex(&l.code_boot),
        "symbols": syms.into_iter().map(|(n, a)| (n.clone(), serde_json::json!(a))).collect::<serde_json::Map<_, _>>(),
        "checked": l.checked,
        "ureg_class": l.ureg_class,
        "loop_end": l.loop_end,
    }))
}

/// The problem a 48-bit instruction word (`ISA` frame, the value of the 6 bytes read big-endian) has as a shift-immediate instruction, or
/// None: the rule of the build, for a check against the stock images.
pub fn shift_problem_of_word(word: u64) -> Option<String> {
    shift_imm_problem(&decode::decode_isa(word))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD: &str = ".SECTION/PM seg_pmco;\n";
    const BASE: i64 = 0xB90000;

    fn asm(body: &str) -> Result<Obj> {
        assemble(&format!("{HEAD}{body}"))
    }

    fn rows_hex(o: &Obj) -> Vec<String> {
        o.rows.iter().map(|(_, r)| hex(r)).collect()
    }

    #[test]
    fn the_pinned_selache_commit_is_the_one_of_the_command_line_tools() {
        let py = include_str!("../../../../tools/patchkit/pk_toolchains.py");
        assert!(py.contains(&format!("SELACHE_REV = \"{SELACHE_REV}\"")));
        let toml = include_str!("../../Cargo.toml");
        assert_eq!(toml.matches(&format!("rev = \"{SELACHE_REV}\"")).count(), 3);
    }

    #[test]
    fn shift_immediates_are_assembled_to_the_table_17_9_opcodes() {
        // The words are the stock forms (finding 05, "Selache shift-immediate encodings"): `R2 = lshift(R15, -1)` is 023e7800ff2f. The
        // fork's selas writes the opcode of PRM Table 17-9 and dataex 0xF for a negative LSHIFT/ASHIFT count itself.
        let o = asm("R0 = LSHIFT R11 BY -16;\nR0 = LSHIFT R11 BY 16;\nR2 = LSHIFT R15 BY -1;\nR0 = ASHIFT R11 BY -3;\nR0 = R0 OR LSHIFT R11 BY 3;\nR0 = R0 OR ASHIFT R11 BY -3;\nR0 = FDEP R11 BY 4:8;\nR0 = R0 OR FDEP R11 BY 4:8;\nR0 = FEXT R11 BY 4:8 (SE);\n")
            .unwrap();
        assert_eq!(
            rows_hex(&o),
            [
                "023e7800f00b",
                "023e0000100b",
                "023e7800ff2f",
                "023e7801fd0b",
                "023e0008030b",
                "023e7809fd0b",
                "023e1011040b",
                "023e101b040b",
                "023e1012040b"
            ]
        );
        assert_eq!(hex(&o.section), rows_hex(&o).concat());
        link(&o, BASE, &HashMap::new()).unwrap();
    }

    #[test]
    fn register_shift_counts_and_comments_pass() {
        let o = asm("R0 = ASHIFT R11 BY R1;  // not ASHIFT R0 BY -3\n/* R0 = FDEP R1 BY 4:8; */\nR0 = FEXT R11 BY 4:8;\n").unwrap();
        assert_eq!(o.rows.len(), 2);
    }

    #[test]
    fn the_shift_rule_names_the_stock_encodings_it_accepts() {
        // Selache's ASHIFT (opcode 0x20), LSHIFT BY -16 without its sign extension, with it, a positive count, a FEXT
        let word = |op: u64, low: u64, ex: u64| (op << 16 | low) | ex << 27 | 0x023e_0000_0000;
        let w = |op: u64, low: u64, ex: u64| shift_problem_of_word(word(op, low, ex));
        assert!(
            w(0x20, 0x030B, 0)
                .unwrap_or_default()
                .starts_with("undefined")
        );
        assert!(w(0x00, 0xF00B, 0).is_some());
        assert_eq!(w(0x00, 0xF00B, 0xF), None);
        assert_eq!(w(0x01, 0x100B, 0), None);
        assert_eq!(w(0x10, 0x040B, 2), None); // FEXT: dataex is part of the field
    }

    #[test]
    fn a_store_the_isa_excludes_is_refused_by_form_and_the_direct_store_is_not() {
        // through an index register: a DM store of a DAG1 register, a PM store of a DAG2 register (finding 49 section 10)
        for bad in [
            "DM(15, I4) = I0;",
            "DM(0x1000, I4) = I0;",
            "DM(I4, M5) = M1;",
            "PM(15, I12) = I8;",
        ] {
            let e = object_from_text(&format!("{HEAD}    {bad}\n    RTS;\n")).unwrap_err();
            assert!(e.0.contains("the ISA does not allow"), "{bad}: {e}");
        }
        let e =
            object_from_text(&format!("{HEAD}_x:\n    DM(-16, I6) = I0;\n    RTS;\n")).unwrap_err();
        assert!(e.0.contains("parcel 0x0: DM store of I0 (DAG1)"), "{e}");
        // the direct-address store (Type 14a), a store of a data register, loads of every register
        for ok in [
            "DM(0x20000000) = I0;",
            "PM(0x20000000) = I8;",
            "DM(15, I4) = R0;",
            "I0 = DM(15, I4);",
        ] {
            object_from_text(&format!("{HEAD}    {ok}\n    RTS;\n"))
                .unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
    }

    #[test]
    fn a_post_modify_load_of_a_register_of_the_index_dag_is_refused_and_its_neighbours_are_not() {
        // Ureg = DM(Ia, Mb) loads UREGXDAG1 only (PRM 13-15). With a compute the assembler emits Type 3a (u = 1 for the (I, M) order);
        // the word-level rule, with PM, is tested in sharc_legal.
        let c = ", R0 = R1 + R2;";
        for bad in [
            "I0 = DM(I4, M4)",
            "M1 = DM(I4, M4)",
            "L7 = DM(I0, M0)",
            "B3 = DM(I4, M4)",
        ] {
            let e = object_from_text(&format!("{HEAD}    {bad}{c}\n    RTS;\n")).unwrap_err();
            assert!(e.0.contains("the ISA does not allow"), "{bad}: {e}");
        }
        let e = object_from_text(&format!("{HEAD}_x:\n    I0 = DM(I4, M4){c}\n    RTS;\n"))
            .unwrap_err();
        assert!(
            e.0.contains("parcel 0x0: DM post-modify load of I0 (DAG1)"),
            "{e}"
        );
        // the nearest legal ones: a DAG2 register, a data register, the pre-modify load, the immediate-address load
        for ok in [
            "I8 = DM(I4, M4)",
            "R3 = DM(I4, M4)",
            "I0 = DM(M4, I4)",
            "I0 = DM(15, I4)",
        ] {
            let src = if ok.contains("15") {
                format!("{ok};")
            } else {
                format!("{ok}{c}")
            };
            object_from_text(&format!("{HEAD}    {src}\n    RTS;\n"))
                .unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
    }

    const TIE: &str = "\n.GLOBAL _t;\n_t:\n    R0 = R0 + R1;\n    NOP;\n    NOP;\n    RTS;\n";

    #[test]
    fn a_16_bit_compute_is_accepted_at_the_assemblers_width() {
        // `R0 = R0 + R1` is the 16-bit Type 2c word 0xc001, which shares its leading bits with longer forms: the assembler's extent decides
        let o = asm(TIE).unwrap();
        assert_eq!(o.rows[0], (0, vec![0xc0, 0x01]));
        assert_eq!(
            link(&o, BASE, &HashMap::new()).unwrap().checked,
            o.rows.len()
        );
        let ok = asm(&TIE.replace("R0 = R0 + R1", "R0 = R1 + R2")).unwrap();
        assert_eq!(ok.rows[0].1.len(), 4);
    }

    #[test]
    fn relocations_are_absolute_parcel_addresses_and_an_unresolved_extern_is_an_error() {
        let o = asm(".GLOBAL _a;\n.EXTERN _hook;\n_a:\n    CALL _b;\n    JUMP _hook;\n_b:\n    R0 = 7;\n    RTS;\n").unwrap();
        let mut r: Vec<_> = o
            .relocs
            .iter()
            .map(|r| (r.rtype, r.symbol.as_str()))
            .collect();
        r.sort();
        assert_eq!(r, [(11, "_b"), (11, "_hook")]);
        assert!(
            link(&o, BASE, &HashMap::new())
                .unwrap_err()
                .0
                .contains("unresolved symbol _hook")
        );
        let l = link(&o, BASE, &HashMap::from([("_hook".into(), 0xB9F000)])).unwrap();
        let b = l.symbols["_b"];
        assert_eq!(b, BASE + o.symbols["_b"].0 as i64);
        // CALL _b: the low 24 bits of the 48-bit word hold the absolute short-word address
        let call = o.relocs.iter().find(|r| r.symbol == "_b").unwrap().parcel as usize * 2;
        let sel = swap_parcels(&l.code_boot);
        assert_eq!(&sel[call + 3..call + 6], &(b as u32).to_be_bytes()[1..]);
        // a symbol with an addend
        let a = asm(".EXTERN _x;\n    R0 = DM(_x + 4);\n").unwrap();
        assert_eq!(
            (
                a.relocs[0].symbol.as_str(),
                a.relocs[0].addend,
                a.relocs[0].rtype
            ),
            ("_x", 4, 12)
        );
        assert_eq!(split_symbol("sym - 0x10").unwrap(), ("sym".into(), -16));
        assert!(split_symbol("1 + x").is_err());
    }

    #[test]
    fn a_word_that_is_not_the_instruction_selache_chose_fails_the_decoder_check() {
        let o = asm(".GLOBAL _f;\n_f:\n    R0 = 7;\n    RTS;\n").unwrap();
        let mut broken = o;
        broken.section[0..2].copy_from_slice(&[0xff, 0xff]); // not an instruction
        let e = link(&broken, BASE, &HashMap::new()).unwrap_err();
        assert!(e.0.contains("disagrees with our decoder"), "{e}");
    }

    #[test]
    fn a_relocated_field_that_is_not_the_target_fails_the_read_back() {
        let o = asm(".EXTERN _x;\n    R0 = DM(_x);\n").unwrap();
        let mut moved = o;
        moved.section[2..6].copy_from_slice(&[0, 0, 0, 5]); // neither 0 nor the addend
        let e = link(&moved, BASE, &HashMap::from([("_x".into(), 0x100)])).unwrap_err();
        assert!(e.0.contains("neither 0 nor the addend"), "{e}");
        let far = asm(".EXTERN _t;\n    JUMP _t;\n").unwrap();
        let e = link(&far, BASE, &HashMap::from([("_t".into(), 1 << 24)])).unwrap_err();
        assert!(e.0.contains("does not fit a 24-bit target"), "{e}");
    }

    #[test]
    fn short_word_loads_are_assembled_to_the_short_word_form() {
        let o =
            asm(".EXTERN _t;\n    R1 = DM(_t) (SW);\n    R2 = DM(_t) (SWSE);\n    R3 = DM(_t);\n")
                .unwrap();
        let sw = &o.section;
        assert_eq!(sw[0], 0x1A);
        assert_eq!(sw[1] & 0xF0, 0x40);
        assert_eq!(sw[6], 0x1A);
        assert_eq!(sw[7] & 0xF0, 0x50);
        assert_eq!(sw[12], 0x10); // the plain load is left alone
        assert!(o.symbols.is_empty(), "the marking labels do not stay");
        let l = link(&o, BASE, &HashMap::from([("_t".into(), 0x2009_0000)])).unwrap();
        assert_eq!(l.checked, 3);
    }

    #[test]
    fn modify_with_an_m_register_is_the_32_bit_type_7b() {
        // PRM Figure 14-20: frame bits 22-16 are 0111111; without them our decoder reads a 48-bit Type 7a
        let o = asm("MODIFY(I5, M2);\nIF EQ MODIFY(I12, M13);\n").unwrap();
        assert_eq!(rows_hex(&o), ["043f503f", "0441283f"]);
        let l = link(&o, BASE, &HashMap::new()).unwrap();
        assert_eq!(l.checked, 2);
    }

    #[test]
    fn pkbss_and_pkdata_declare_data_sections_with_a_symbol() {
        let o = asm(
            "#define N 16\n.PKBSS buf, N;\n.PKDATA tab, 8;  // zeros\n.GLOBAL _f;\n_f:\n    R0 = DM(buf);\n    R1 = DM(tab);\n    RTS;\n",
        )
        .unwrap();
        let sec: Vec<_> = o
            .data
            .0
            .iter()
            .map(|(n, (b, a, bss))| (n.as_str(), b.len(), *a, *bss))
            .collect();
        assert!(sec.contains(&("pkbss_buf", 16, 4, true)), "{sec:?}");
        assert!(sec.contains(&("pkdata_tab", 8, 4, false)), "{sec:?}");
        assert_eq!(o.data_symbols.0.iter().filter(|(_, v)| v.2).count(), 2);
        for bad in [".PKBSS _b, 6;", ".PKBSS _b, 0;", ".PKBSS _b, SOME;"] {
            assert!(asm(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_object_is_the_canonical_json_of_the_text() {
        let src = format!("{HEAD}.GLOBAL _f;\n.EXTERN _g;\n_f:\n    CALL _g;\n    RTS;\n");
        let d = object_from_text(&src).unwrap();
        assert_eq!(
            (d.arch.as_str(), d.sections[0].name.as_str()),
            ("sharc", "seg_pmco")
        );
        assert_eq!(d.source_sha256, sha256_hex(src.as_bytes()));
        assert_eq!(d.toolchain["instructions_redecoded"], "2");
        assert_eq!(d.toolchain["selache_rev"], SELACHE_REV);
        let undefined: Vec<_> = d
            .symbols
            .iter()
            .filter(|s| s.section.is_none())
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(undefined, ["_g"]);
        assert_eq!(d.relocs[0].rtype, "sharc.abs24");
    }
}
