//! The container of a DSP section (`DKDS`): header, table offsets and the raw tables. The layout is documented in
//! `packages/patchkit/src/dkmdsp.rs`. This is the one reader of it: [`Input::from_dkds`](crate::Input::from_dkds) (lenient, for analysis) and
//! patchkit's `DspSection::parse` (strict, the checks of the runtime) both start here and add their own checks on the fields.
//!
//! Integers are big endian. Table accessors check that the table lies inside the section; nothing else is checked here.

// The layout numbers are `[format]` of sdk/sdk.toml (through `dkneeds`, which the runtime reads too).
use dkneeds::*;

pub const MAGIC: u32 = DSP_MAGIC;
pub const HEADER: usize = DSP_HEADER as usize;
pub const IMPORT_LEN: usize = DSP_IMPORT_LEN as usize;
pub const RELOC_LEN: usize = dkneeds::RELOC_LEN as usize;
pub const VOICE_LEN_V1: usize = VOICE_V1 as usize;
pub const VOICE_LEN: usize = VOICE_V2 as usize;
pub const VOICE_LEN_V3: usize = VOICE_V3 as usize;
pub const FX_LEN: usize = dkneeds::FX_LEN as usize;
pub const VERSION_1: u32 = DSP_V1;
pub const VERSION_3: u32 = DSP_V3;
pub const VERSION_4: u32 = DSP_V4;
pub const VERSION_5: u32 = DSP_V5;

/// A DSP section: its bytes and its version (magic and version are checked).
#[derive(Clone, Copy, Debug)]
pub struct Dkds<'a> {
    pub bytes: &'a [u8],
    pub version: u32,
}

/// A relocation record as stored: the image byte offset of the instruction and the packed word `kind << 28 | class << 24 | import`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawReloc {
    pub offset: u32,
    pub word: u32,
}

impl RawReloc {
    pub fn kind(self) -> u32 {
        self.word >> 28
    }
    pub fn class(self) -> u32 {
        self.word >> 24 & 15
    }
    /// The import index field, 24 bits.
    pub fn import(self) -> u32 {
        self.word & 0x00ff_ffff
    }
}

fn be32(b: &[u8], off: usize) -> u32 {
    u32::from_be_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

impl<'a> Dkds<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Dkds<'a>, String> {
        if bytes.len() < HEADER || be32(bytes, DSP_H_MAGIC as usize) != MAGIC {
            return Err("not a DSP section (magic)".into());
        }
        let version = be32(bytes, DSP_H_VERSION as usize);
        if !(VERSION_1..=VERSION_5).contains(&version) {
            return Err(format!("DSP section version {version}"));
        }
        Ok(Dkds { bytes, version })
    }

    /// A header word at byte `off` (below `HEADER`).
    pub fn word(&self, off: usize) -> u32 {
        be32(self.bytes, off)
    }
    pub fn device(&self) -> u32 {
        self.word(DSP_H_DEVICE as usize)
    }
    pub fn image_size(&self) -> u32 {
        self.word(DSP_H_IMAGE_SIZE as usize)
    }
    pub fn bss(&self) -> u32 {
        self.word(DSP_H_BSS as usize)
    }
    pub fn align(&self) -> u32 {
        self.word(DSP_H_ALIGN as usize)
    }
    pub fn image_off(&self) -> u32 {
        self.word(DSP_H_IMAGE_OFF as usize)
    }
    pub fn reloc_count(&self) -> usize {
        self.word(DSP_H_RELOC_N as usize) as usize
    }
    pub fn import_count(&self) -> usize {
        self.word(DSP_H_IMPORT_N as usize) as usize
    }
    pub fn voice_count(&self) -> usize {
        self.word(DSP_H_VOICE_N as usize) as usize
    }
    /// Offset and count of the effect records (version 4 and later; none before).
    pub fn fx(&self) -> (u32, usize) {
        if self.version >= VERSION_4 {
            (
                self.word(DSP_H_FX_OFF as usize),
                self.word(DSP_H_FX_N as usize) as usize,
            )
        } else {
            (0, 0)
        }
    }
    /// Bytes of one voice record.
    pub fn voice_len(&self) -> usize {
        match self.version {
            VERSION_1 => VOICE_LEN_V1,
            VERSION_3..=VERSION_5 => VOICE_LEN_V3,
            _ => VOICE_LEN,
        }
    }

    /// `n` bytes at `off`, when they lie inside the section.
    pub fn range(&self, off: u32, n: usize) -> Result<&'a [u8], String> {
        let off = off as usize;
        self.bytes
            .get(off..off.checked_add(n).ok_or("overflow")?)
            .ok_or_else(|| format!("DSP section table at {off:#x}+{n:#x} is outside the section"))
    }

    /// The image (the text and data that are sent to the DSP).
    pub fn image(&self) -> Result<&'a [u8], String> {
        self.range(self.image_off(), self.image_size() as usize)
    }

    /// The names of the imports (32-byte NUL padded entries).
    pub fn imports(&self) -> Result<Vec<String>, String> {
        let n = self.import_count();
        let raw = self.range(
            self.word(DSP_H_IMPORT_OFF as usize),
            n.checked_mul(IMPORT_LEN).ok_or("overflow")?,
        )?;
        Ok(raw
            .as_chunks::<IMPORT_LEN>()
            .0
            .iter()
            .map(|e| {
                let l = e.iter().position(|b| *b == 0).unwrap_or(IMPORT_LEN);
                String::from_utf8_lossy(&e[..l]).into_owned()
            })
            .collect())
    }

    pub fn relocs(&self) -> Result<Vec<RawReloc>, String> {
        let n = self.reloc_count();
        let raw = self.range(
            self.word(DSP_H_RELOC_OFF as usize),
            n.checked_mul(RELOC_LEN).ok_or("overflow")?,
        )?;
        Ok(raw
            .as_chunks::<RELOC_LEN>()
            .0
            .iter()
            .map(|r| RawReloc {
                offset: be32(r, RELOC_OFFSET as usize),
                word: be32(r, RELOC_WORD as usize),
            })
            .collect())
    }

    /// The voice records, `voice_len()` bytes each.
    pub fn voices(&self) -> Result<&'a [u8], String> {
        let n = self.voice_count();
        self.range(
            self.word(DSP_H_VOICE_OFF as usize),
            n.checked_mul(self.voice_len()).ok_or("overflow")?,
        )
    }

    /// The effect records, `FX_LEN` bytes each.
    pub fn fxs(&self) -> Result<&'a [u8], String> {
        let (off, n) = self.fx();
        self.range(off, n.checked_mul(FX_LEN).ok_or("overflow")?)
    }
}
