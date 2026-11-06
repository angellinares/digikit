//! A relocatable ColdFire ELF as an object prototype: the allocatable sections, the symbols and the relocations, in the order
//! the ELF lists them (the same reading as the former `pk_link.object_from_elf`).

use crate::error::*;
use object::BigEndian;
use object::elf;
use object::read::elf::{FileHeader, SectionHeader, Sym};

const EM_68K: u16 = 4;
const SHN_LORESERVE: usize = 0xFF00;
const SHN_COMMON: usize = 0xFFF2;

/// (ELF relocation type, name, byte size, pc-relative)
pub const R_68K: [(u32, &str, usize, bool); 9] = [
    (1, "32", 4, false),
    (2, "16", 2, false),
    (3, "8", 1, false),
    (4, "PC32", 4, true),
    (5, "PC16", 2, true),
    (6, "PC8", 1, true),
    (18, "PLT32", 4, true),
    (19, "PLT16", 2, true),
    (20, "PLT8", 1, true),
];

#[derive(Clone, Debug)]
pub struct PSection {
    pub name: String,
    /// `text`, `rodata`, `data` or `bss`
    pub kind: &'static str,
    pub align: u32,
    pub size: u32,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct PSymbol {
    pub name: String,
    pub section: Option<String>,
    pub value: u64,
    pub global: bool,
}

#[derive(Clone, Debug)]
pub enum RelocTo {
    Symbol(String),
    Section(String),
}

#[derive(Clone, Debug)]
pub struct PReloc {
    pub section: String,
    pub offset: u64,
    pub rtype: &'static str,
    pub to: RelocTo,
    pub addend: i64,
}

#[derive(Clone, Debug, Default)]
pub struct Proto {
    pub sections: Vec<PSection>,
    pub symbols: Vec<PSymbol>,
    pub relocs: Vec<PReloc>,
}

type Hdr = elf::FileHeader32<BigEndian>;

fn bad<E: std::fmt::Display>(e: E) -> Error {
    err!("ELF: {e}")
}

pub fn check_machine(blob: &[u8]) -> Result<()> {
    let h = Hdr::parse(blob).map_err(bad)?;
    let e = h
        .endian()
        .map_err(|_| err!("not a big-endian m68k object"))?;
    if h.e_machine(e) != EM_68K {
        return Err(err!("not a big-endian m68k object"));
    }
    Ok(())
}

/// The `e_flags` of an ELF32 header (big endian), 0 for a blob that is too short.
pub fn e_flags(blob: &[u8]) -> u32 {
    blob.get(36..40)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
        .unwrap_or(0)
}

/// True when the object has no allocatable section with content (`size` prints 0 0 0).
pub fn is_empty(blob: &[u8]) -> Result<bool> {
    let h = Hdr::parse(blob).map_err(bad)?;
    let e = h.endian().map_err(bad)?;
    let secs = h.sections(e, blob).map_err(bad)?;
    Ok(!secs
        .iter()
        .any(|s| s.sh_flags(e) & elf::SHF_ALLOC != 0 && s.sh_size(e) > 0))
}

/// The executable sections `(name, bytes)` of a relocatable m68k ELF.
pub fn text_sections(blob: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    check_machine(blob)?;
    let h = Hdr::parse(blob).map_err(bad)?;
    let e = h.endian().map_err(bad)?;
    let secs = h.sections(e, blob).map_err(bad)?;
    let mut out = vec![];
    for (i, s) in secs.iter().enumerate() {
        if s.sh_type(e) == elf::SHT_PROGBITS
            && s.sh_flags(e) & elf::SHF_EXECINSTR != 0
            && s.sh_size(e) > 0
        {
            let sec = secs.section(object::SectionIndex(i)).map_err(bad)?;
            let name =
                String::from_utf8_lossy(secs.section_name(e, sec).map_err(bad)?).into_owned();
            out.push((name, s.data(e, blob).map_err(bad)?.to_vec()));
        }
    }
    Ok(out)
}

/// The members of a GNU or BSD `ar` archive that are ELF objects: `(name, bytes)`. The symbol table and the long-name table are skipped.
pub fn ar_members(blob: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    if !blob.starts_with(b"!<arch>\n") {
        return Err(err!("not an ar archive"));
    }
    let (mut out, mut pos, mut longnames) = (vec![], 8usize, &blob[..0]);
    while pos + 60 <= blob.len() {
        let hdr = &blob[pos..pos + 60];
        let field = |a: usize, b: usize| String::from_utf8_lossy(&hdr[a..b]).trim().to_string();
        let mut name = field(0, 16);
        let size: usize = field(48, 58)
            .parse()
            .map_err(|_| err!("ar: bad member size"))?;
        let body = blob
            .get(pos + 60..pos + 60 + size)
            .ok_or_else(|| err!("ar: member runs past the end"))?;
        pos += 60 + size + (size & 1);
        if name == "//" {
            longnames = body;
            continue;
        }
        if name == "/" || name == "/SYM64/" {
            continue;
        }
        if let Some(n) = name.strip_prefix('/').and_then(|n| n.parse::<usize>().ok()) {
            let tail = longnames.get(n..).unwrap_or(&[]);
            let end = tail
                .windows(2)
                .position(|w| w == b"/\n")
                .unwrap_or(tail.len());
            name = String::from_utf8_lossy(&tail[..end]).into_owned();
        }
        out.push((name.trim_end_matches('/').to_string(), body.to_vec()));
    }
    Ok(out)
}

pub fn proto_from_elf(blob: &[u8]) -> Result<Proto> {
    check_machine(blob)?;
    let h = Hdr::parse(blob).map_err(bad)?;
    let e = h.endian().map_err(bad)?;
    let secs = h.sections(e, blob).map_err(bad)?;
    let name_of = |i: usize| -> Result<String> {
        let s = secs.section(object::SectionIndex(i)).map_err(bad)?;
        let n = secs.section_name(e, s).map_err(bad)?;
        Ok(String::from_utf8_lossy(n).into_owned())
    };
    // `keep`: allocatable PROGBITS and NOBITS sections with content, by name (a later one of the same name replaces an earlier one)
    let mut out = Proto::default();
    let mut names: Vec<String> = vec![];
    for (i, s) in secs.iter().enumerate() {
        let ty = s.sh_type(e);
        let flags = s.sh_flags(e);
        let size = s.sh_size(e);
        if flags & elf::SHF_ALLOC == 0
            || !(ty == elf::SHT_PROGBITS || ty == elf::SHT_NOBITS)
            || size == 0
        {
            continue;
        }
        let kind = if ty == elf::SHT_NOBITS {
            "bss"
        } else if flags & elf::SHF_EXECINSTR != 0 {
            "text"
        } else if flags & elf::SHF_WRITE != 0 {
            "data"
        } else {
            "rodata"
        };
        let name = name_of(i)?;
        let data = if ty == elf::SHT_NOBITS {
            vec![]
        } else {
            s.data(e, blob).map_err(bad)?.to_vec()
        };
        let sec = PSection {
            name: name.clone(),
            kind,
            align: s.sh_addralign(e).max(1),
            size,
            data,
        };
        match names.iter().position(|n| *n == name) {
            Some(k) => out.sections[k] = sec,
            None => {
                names.push(name);
                out.sections.push(sec);
            }
        }
    }
    let kept = |n: &str| names.iter().any(|k| k == n);

    let symtab = secs.symbols(e, blob, elf::SHT_SYMTAB).map_err(bad)?;
    for sym in symtab.iter() {
        let name =
            String::from_utf8_lossy(sym.name(e, symtab.strings()).map_err(bad)?).into_owned();
        if name.is_empty() || sym.st_type() == elf::STT_FILE {
            continue;
        }
        let shndx = sym.st_shndx(e) as usize;
        if shndx > 0 && shndx < SHN_LORESERVE && kept(&name_of(shndx)?) {
            out.symbols.push(PSymbol {
                name,
                section: Some(name_of(shndx)?),
                value: sym.st_value(e) as u64,
                global: sym.st_bind() != elf::STB_LOCAL,
            });
        } else if shndx == 0 {
            out.symbols.push(PSymbol {
                name,
                section: None,
                value: 0,
                global: true,
            });
        } else if shndx == SHN_COMMON {
            return Err(err!("common symbol {name}: compile with -fno-common"));
        }
    }

    for (i, s) in secs.iter().enumerate() {
        let ty = s.sh_type(e);
        if ty != elf::SHT_RELA && ty != elf::SHT_REL {
            continue;
        }
        let target = name_of(s.sh_info(e) as usize)?;
        if !kept(&target) {
            continue; // relocations of debug and unwind sections
        }
        let mut entries: Vec<(u64, u32, u32, i64)> = vec![];
        if ty == elf::SHT_RELA {
            if let Some((rels, _)) = s.rela(e, blob).map_err(bad)? {
                for r in rels {
                    entries.push((
                        r.r_offset.get(e) as u64,
                        r.r_sym(e),
                        r.r_type(e),
                        r.r_addend.get(e) as i64,
                    ));
                }
            }
        } else if let Some((rels, _)) = s.rel(e, blob).map_err(bad)? {
            for r in rels {
                entries.push((r.r_offset.get(e) as u64, r.r_sym(e), r.r_type(e), 0));
            }
        }
        let _ = i;
        for (offset, symi, rtype, addend) in entries {
            let sym = symtab
                .symbol(object::SymbolIndex(symi as usize))
                .map_err(bad)?;
            let sname =
                String::from_utf8_lossy(sym.name(e, symtab.strings()).map_err(bad)?).into_owned();
            let Some(&(_, tname, _, _)) = R_68K.iter().find(|r| r.0 == (rtype & 0xFF)) else {
                return Err(err!(
                    "unsupported relocation type {} in {target}+{offset:#x} -> {sname}",
                    rtype & 0xFF
                ));
            };
            let to = if sym.st_type() == elf::STT_SECTION {
                RelocTo::Section(name_of(sym.st_shndx(e) as usize)?)
            } else {
                RelocTo::Symbol(sname)
            };
            out.relocs.push(PReloc {
                section: target.clone(),
                offset,
                rtype: tname,
                to,
                addend,
            });
        }
    }
    Ok(out)
}
