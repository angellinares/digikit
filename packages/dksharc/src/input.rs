//! What the analysis reads: the text of a module's DSP image as 16-bit parcels, its relocations, imports, entry points and text symbols.
//!
//! [`Input::from_dkds`] reads the DKDS section of a `.dkm` (layout: `packages/patchkit/src/dkmdsp.rs`) and the `.dko.json` of the DSP
//! object (symbols and the size of the text section). Other callers fill the struct themselves.

use crate::dkds::{Dkds, FX_LEN};
use crate::dko::Dko;
use serde::{Deserialize, Serialize};

/// First byte of L2 as the linker places a module (`L2_BYTE_BASE` of patchkit): the image is linked here.
pub const L2_BYTE_BASE: u32 = 0x2000_0000;
/// Short-word pc of the first byte of L2 (`L2_SW_BASE` of patchkit): text at image byte `b` has pc `L2_SW_BASE + b / 2`.
pub const L2_SW_BASE: u32 = 0x00B8_0000;

/// A relocation inside the text: it patches the instruction that starts at `parcel`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextReloc {
    /// Parcel index of the instruction from the start of the text.
    pub parcel: u32,
    /// 0: the 24-bit address field, 1: the 32-bit immediate.
    pub kind: u8,
    /// 0: a pc of this object, 1: a byte address of this object, 2: the addend of an import.
    pub class: u8,
    /// Index into [`Input::imports`] when `class` is 2.
    pub import: u32,
}

/// A symbol of the text section.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextSymbol {
    pub name: String,
    /// Parcel index from the start of the text.
    pub parcel: u32,
    pub global: bool,
}

/// An entry point the packer recorded (a voice's render or note-on routine, an effect's process routine).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// `render`, `note_on` or `fx_process`.
    pub role: String,
    pub parcel: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Input {
    /// The text as 16-bit parcels in boot-stream order (instruction word 0 first).
    pub text: Vec<u16>,
    /// Short-word pc of parcel 0; `L2_SW_BASE` for a module linked at the nominal base.
    pub base_sw: u32,
    pub relocs: Vec<TextReloc>,
    pub imports: Vec<String>,
    pub symbols: Vec<TextSymbol>,
    pub entries: Vec<Entry>,
}

impl Input {
    /// Read the DKDS section `dkds` (header at byte 0) and the DSP object `dko_json`. The text is the first `text` section of the object:
    /// the image starts with it, so its size says where the data begins.
    pub fn from_dkds(dkds: &[u8], dko_json: &[u8]) -> Result<Input, String> {
        let d = Dkds::parse(dkds)?;
        let image = d.image()?;
        let dko = Dko::parse(std::str::from_utf8(dko_json).map_err(|e| format!("dko: {e}"))?)?;
        let mut texts = dko.sections.iter().filter(|s| s.kind == "text");
        let text_sec = texts.next().ok_or("dko: no text section")?;
        if texts.next().is_some() {
            return Err("dko: more than one text section".into());
        }
        if text_sec.size % 2 != 0 || text_sec.size as usize > image.len() {
            return Err(format!(
                "dko: text size {} against image {}",
                text_sec.size,
                image.len()
            ));
        }
        let text: Vec<u16> = image[..text_sec.size as usize]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        let n_text = text.len() as u32;

        let imports = d.imports()?;
        let mut relocs = vec![];
        for r in d.relocs()? {
            if r.offset % 2 != 0 {
                return Err(format!("DKDS: reloc at odd byte {:#x}", r.offset));
            }
            if r.offset / 2 >= n_text {
                continue; // a field of the data section
            }
            if r.class() == 2 && r.import() as usize >= imports.len() {
                return Err(format!(
                    "DKDS: reloc at {:#x}: import {} of {}",
                    r.offset,
                    r.import(),
                    imports.len()
                ));
            }
            relocs.push(TextReloc {
                parcel: r.offset / 2,
                kind: r.kind() as u8,
                class: r.class() as u8,
                import: r.import(),
            });
        }

        let mut entries = vec![];
        for v in d.voices()?.chunks_exact(d.voice_len()) {
            let word = |at: usize| u32::from_be_bytes(v[at..at + 4].try_into().unwrap());
            entries.push(("render", word(4)));
            if d.version >= 3 && word(8) != 0 {
                entries.push(("note_on", word(8)));
            }
        }
        for f in d.fxs()?.as_chunks::<FX_LEN>().0 {
            entries.push(("fx_process", u32::from_be_bytes(f[..4].try_into().unwrap())));
        }
        let entries = entries
            .into_iter()
            .map(|(role, off)| {
                if off % 2 != 0 || off / 2 >= n_text {
                    Err(format!("DKDS: {role} entry {off:#x} is outside the text"))
                } else {
                    Ok(Entry {
                        role: role.into(),
                        parcel: off / 2,
                    })
                }
            })
            .collect::<Result<Vec<_>, _>>()?;

        let symbols = dko
            .symbols
            .iter()
            .filter(|s| s.section.as_deref() == Some(text_sec.name.as_str()))
            .map(|s| {
                if s.value >= n_text as u64 {
                    Err(format!(
                        "dko: symbol {} at parcel {} is outside the text",
                        s.name, s.value
                    ))
                } else {
                    Ok(TextSymbol {
                        name: s.name.clone(),
                        parcel: s.value as u32,
                        global: s.global,
                    })
                }
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Input {
            text,
            base_sw: L2_SW_BASE,
            relocs,
            imports,
            symbols,
            entries,
        })
    }
}
