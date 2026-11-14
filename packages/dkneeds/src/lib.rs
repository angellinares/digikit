//! What a `.dkm` takes of the device, inferred from the file's own contents (docs/design/digikit-dkm.md, "Needs are inferred").
//!
//! A module does not say what it needs: this crate reads the same fields the loader reads (the header's image and bss sizes, the `register`
//! records, the DSP section's header and voice table) and works out the arena blocks, the DSP range, the voice rows and the registry rows a load
//! takes. A hand-built or contributed file cannot misreport, because there is no declaration to misreport. The runtime enforces the same limits at
//! load; this is the answer before the load (the DIGIKIT menu, the host app, `dk run`, `module:check`).
//!
//! The file is read through [`Bytes`], so the device can feed it from store sector reads (it never stages the file) and a host from a slice.
//! Every field is checked the way the loader checks it (bounds, counts); a file the loader would refuse gives an [`Error`], never a panic.
//! The code avoids `match` on dense integer values (a jump table is not ColdFire code), multiplications that can overflow and anything that
//! allocates: it runs in the DIGIKIT menu.
//!
//! What is not checked: the file's sha256 (the loader does), the device and the runtime version, names, imports, relocations and the contents of
//! the records. The only input the answer needs from outside is the device's limits, which [`Needs`] is compared with elsewhere (`dkrt_fit`).
#![no_std]

#[rustfmt::skip]
mod gen_limits;
/// The loader's format limits, from `[limits.format]` of sdk/sdk.toml (`HEADER`, `DSP_HEADER`, `MAX_RECORDS`, `MAX_RELOCS`, `MAX_IMPORTS`,
/// `MAX_CF_IMPORTS`, `MAX_VOICES`, `MAX_FX`, `MAX_STATE_VOICES`, `MAX_STATE_BYTES`, `GRANULE`, `DSP_SLACK`, `ARENA_HDR`).
pub use gen_limits::*;

/// Magic of a `.dkm`, "DKMF".
pub const MAGIC: u32 = 0x444b_4d46;
/// The only `.dkm` format.
pub const FORMAT: u32 = 1;
/// Header offsets read.
pub const H_SIZE: u32 = 0x08;
pub const H_HDRSIZE: u32 = 0x0c;
pub const H_IMAGE_OFF: u32 = 0x60;
pub const H_IMAGE_SIZE: u32 = 0x64;
pub const H_BSS: u32 = 0x68;
pub const H_ALIGN: u32 = 0x6c;
pub const H_RELOC_OFF: u32 = 0x70;
pub const H_RELOC_N: u32 = 0x74;
pub const H_IMPORT_OFF: u32 = 0x78;
pub const H_IMPORT_N: u32 = 0x7c;
pub const H_MENU: u32 = 0x88;
pub const H_TICK: u32 = 0x8c;
pub const H_REG_OFF: u32 = 0x90;
pub const H_REG_SIZE: u32 = 0x94;
pub const H_DSP_OFF: u32 = 0x98;
pub const H_DSP_SIZE: u32 = 0x9c;

/// The DSP section: magic "DKDS", versions 1 to 5, header 0x40 bytes, voices of 16, 48 or 64 bytes (version 4: voices of 64 bytes and effect records).
pub const DSP_MAGIC: u32 = 0x444b_4453;
/// The size of an effect record of a version 4 section (the most the loader takes is `MAX_FX`).
pub const FX_LEN: u32 = 64;

/// A source of the file's bytes: a slice on a host, the store's sector reads on the device.
pub trait Bytes {
    /// The file's size in bytes.
    fn size(&self) -> u32;
    /// Fill `out` with the bytes at `at`; false when they cannot be read (past the end, a read error).
    fn read(&mut self, at: u32, out: &mut [u8]) -> bool;
}

impl Bytes for &[u8] {
    fn size(&self) -> u32 {
        <[u8]>::len(self) as u32
    }
    fn read(&mut self, at: u32, out: &mut [u8]) -> bool {
        let at = at as usize;
        match at.checked_add(out.len()) {
            Some(end) if end <= <[u8]>::len(self) => {
                out.copy_from_slice(&self[at..end]);
                true
            }
            _ => false,
        }
    }
}

/// Why a file has no needs: the loader would refuse it (or the source could not be read).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// the source could not be read
    Unreadable,
    /// not a `.dkm` of this format, or its size fields do not match the file
    Header,
    /// the sizes of the image, bss or a table do not fit the file
    Sizes,
    /// the `register` table is malformed
    Register,
    /// the DSP section is malformed
    Dsp,
}

impl Error {
    /// A short text for a log or a tool.
    pub fn text(self) -> &'static str {
        if self == Error::Unreadable {
            "the file cannot be read"
        } else if self == Error::Header {
            "not a .dkm of this format"
        } else if self == Error::Sizes {
            "the sizes of the image or a table do not fit the file"
        } else if self == Error::Register {
            "the register table is malformed"
        } else {
            "the DSP section is malformed"
        }
    }
}

/// What a load takes of each resource.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Needs {
    /// The file's size: it must fit the staging buffer.
    pub file_size: u32,
    /// The arena blocks the module keeps: image and bss (the payload rounded to 16, plus the block header), and the record block its `init` committed (the keys table's `record_bytes`).
    pub arena: u32,
    /// The arena block of the relocated DSP image, held while it is sent (0: no DSP section).
    pub arena_xfer: u32,
    /// DSP area bytes: image, bss and voice states, rounded to the granule (0: no DSP section).
    pub dsp_area: u32,
    pub dsp_align: u32,
    pub machine_rows: u32,
    pub voice_rows: u32,
    pub param_rows: u32,
    pub page_rows: u32,
    /// Effect slots: the effect records of the DSP section (version 4).
    pub fx_slots: u32,
    /// DSP instructions per block of one voice of the module's machines (the header table's `DSP_COST_VOICE`; 0: none). The broker takes the largest of the selection.
    pub dsp_cost_voice: u32,
    /// DSP instructions per block of the module's effects (the header table's `DSP_COST_FX`; 0: none). The broker adds them up.
    pub dsp_cost_fx: u32,
}

impl Needs {
    /// (name, value) of every field except `file_size`, the names of CAPACITY's kinds with their rows: `ARENA`, `ARENA_XFER`, `DSP_AREA`,
    /// `DSP_ALIGN`, `MACHINE_ROWS`, `VOICE_ROWS`, `PARAM_ROWS`, `PAGE_ROWS`, `FX_SLOTS`, `DSP_COST_VOICE`, `DSP_COST_FX` (the last two are the module's own
    /// cost, [records.resources]). The hosts (the wasm export, the command line) say them so.
    pub fn fields(&self) -> [(&'static str, u32); 11] {
        [
            ("ARENA", self.arena),
            ("ARENA_XFER", self.arena_xfer),
            ("DSP_AREA", self.dsp_area),
            ("DSP_ALIGN", self.dsp_align),
            ("MACHINE_ROWS", self.machine_rows),
            ("VOICE_ROWS", self.voice_rows),
            ("PARAM_ROWS", self.param_rows),
            ("PAGE_ROWS", self.page_rows),
            ("FX_SLOTS", self.fx_slots),
            ("DSP_COST_VOICE", self.dsp_cost_voice),
            ("DSP_COST_FX", self.dsp_cost_fx),
        ]
    }

    /// DSP area bytes with the room a placement can lose to alignment (up to `dsp_align - 8` bytes).
    pub fn dsp_need(&self) -> u32 {
        if self.dsp_area == 0 {
            0
        } else {
            self.dsp_area
                .saturating_add(self.dsp_align.max(DSP_SLACK) - DSP_SLACK)
        }
    }
}

/// The arena bytes a block of `n` payload bytes takes: `n` rounded to 16 and the header.
pub fn arena_block(n: u32) -> Option<u32> {
    (n.checked_add(15)? & !15).checked_add(ARENA_HDR)
}

fn word<B: Bytes>(src: &mut B, at: u32) -> Result<u32, Error> {
    let mut b = [0u8; 4];
    if !src.read(at, &mut b) {
        return Err(Error::Unreadable);
    }
    Ok(u32::from_be_bytes(b))
}

/// `off + len` lies in `size` (the loader's `in_file`), without overflow.
fn inside(off: u32, len: u32, size: u32) -> bool {
    off <= size && len <= size - off
}

/// The needs of the file `src` reads: see the module's text. `Err` for a file the loader would refuse for its format, or one that cannot be read.
pub fn needs_of<B: Bytes>(src: &mut B) -> Result<Needs, Error> {
    let size = src.size();
    if size < HEADER || word(src, 0)? != MAGIC || word(src, 4)? != FORMAT {
        return Err(Error::Header);
    }
    let hdr = word(src, H_HDRSIZE)?;
    if word(src, H_SIZE)? != size || hdr != HEADER {
        return Err(Error::Header);
    }
    let image = word(src, H_IMAGE_SIZE)?;
    let bss = word(src, H_BSS)?;
    let align = word(src, H_ALIGN)?;
    let (image_off, reloc_off, reloc_n) = (
        word(src, H_IMAGE_OFF)?,
        word(src, H_RELOC_OFF)?,
        word(src, H_RELOC_N)?,
    );
    let (import_off, import_n) = (word(src, H_IMPORT_OFF)?, word(src, H_IMPORT_N)?);
    // the loader's own checks of what it copies (bss against the arena is the one it makes with the device's size: a fit says it)
    if align == 0
        || align > 16
        || align & (align - 1) != 0
        || image == 0
        || image > size
        || image_off & 15 != 0
        || reloc_n > MAX_RELOCS
        || import_n > MAX_CF_IMPORTS
        || !inside(image_off, image, size)
        || !inside(reloc_off, reloc_n * 8, size)
        || !inside(import_off, import_n * 32, size)
    {
        return Err(Error::Sizes);
    }
    let total = image.checked_add(bss).ok_or(Error::Sizes)?;
    let mut n = Needs {
        file_size: size,
        arena: arena_block(total).ok_or(Error::Sizes)?,
        ..Needs::default()
    };
    let (reg_off, reg_size) = (word(src, H_REG_OFF)?, word(src, H_REG_SIZE)?);
    if !inside(reg_off, reg_size, size) {
        return Err(Error::Sizes);
    }
    // every file has a header table (a file of the 0.7 format has none, or another one: the loader refuses it)
    keys_table(src, reg_off, reg_size, &mut n)?;
    let (dsp_off, dsp_size) = (word(src, H_DSP_OFF)?, word(src, H_DSP_SIZE)?);
    if !inside(dsp_off, dsp_size, size) {
        return Err(Error::Sizes);
    }
    if dsp_size != 0 {
        dsp(src, dsp_off, dsp_size, &mut n)?;
    }
    Ok(n)
}

/// The header table of the file (`keys.rs`): the resources the packer wrote in its `USES` entry. The table is read entry by entry (the head, then
/// each entry head; only `USES` is read in full), so the menu never holds the whole table.
fn keys_table<B: Bytes>(src: &mut B, off: u32, size: u32, n: &mut Needs) -> Result<(), Error> {
    let mut head = [0u8; keys::HEAD];
    if (size as usize) < head.len() || size % 4 != 0 || !src.read(off, &mut head) {
        return Err(Error::Register);
    }
    if u32::from_be_bytes([head[0], head[1], head[2], head[3]]) != keys::MAGIC
        || u16::from_be_bytes([head[4], head[5]]) != keys::VERSION
    {
        return Err(Error::Register);
    }
    let entries = u16::from_be_bytes([head[6], head[7]]) as usize;
    let mut at = keys::HEAD as u32;
    let mut last = 0u16;
    let mut i = 0;
    while i < entries {
        let mut e = [0u8; keys::ENTRY_HEAD];
        if size - at < e.len() as u32 || !src.read(off + at, &mut e) {
            return Err(Error::Register);
        }
        let tg = u16::from_be_bytes([e[0], e[1]]);
        let count = u16::from_be_bytes([e[2], e[3]]) as u32;
        let len = u32::from_be_bytes([e[4], e[5], e[6], e[7]]);
        if !(keys::tag::NEEDS..=keys::tag::RESOURCES).contains(&tg)
            || tg <= last
            || len % 4 != 0
            || len > size - at - e.len() as u32
        {
            return Err(Error::Register);
        }
        last = tg;
        if tg == keys::tag::USES {
            // 8 bytes an item; at most the resources of `[records.resources]`, each read as it comes
            if count.checked_mul(8) != Some(len) {
                return Err(Error::Register);
            }
            let mut k = 0;
            while k < count {
                let mut u = [0u8; 8];
                if !src.read(off + at + e.len() as u32 + 8 * k, &mut u) {
                    return Err(Error::Register);
                }
                let (id, c) = (
                    u32::from_be_bytes([u[0], u[1], u[2], u[3]]),
                    u32::from_be_bytes([u[4], u[5], u[6], u[7]]),
                );
                if id == RES_MACHINE_ROWS {
                    n.machine_rows = n.machine_rows.saturating_add(c);
                } else if id == RES_PARAM_ROWS {
                    n.param_rows = n.param_rows.saturating_add(c);
                } else if id == RES_PAGE_ROWS {
                    n.page_rows = n.page_rows.saturating_add(c);
                } else if id == RES_DSP_COST_VOICE {
                    n.dsp_cost_voice = n.dsp_cost_voice.saturating_add(c);
                } else if id == RES_DSP_COST_FX {
                    n.dsp_cost_fx = n.dsp_cost_fx.saturating_add(c);
                } else if id == RES_RECORD_BYTES && c != 0 {
                    // the record block the module keeps after `init` is one more arena block (the loader's `arena::alloc` of the records)
                    n.arena = n
                        .arena
                        .checked_add(arena_block(c).ok_or(Error::Sizes)?)
                        .ok_or(Error::Sizes)?;
                }
                k += 1;
            }
        }
        at += e.len() as u32 + len;
        i += 1;
    }
    if at != size {
        return Err(Error::Register);
    }
    Ok(())
}

/// The DSP section at `off` (`size` bytes): the arena block of its relocated image, the range it takes of the DSP area and its voice rows. The
/// checks are those of `dsp::check` that decide what the section takes; the range is `extent` of `dsp.rs`, then the granule.
fn dsp<B: Bytes>(src: &mut B, off: u32, size: u32, n: &mut Needs) -> Result<(), Error> {
    if size < DSP_HEADER || word(src, off)? != DSP_MAGIC {
        return Err(Error::Dsp);
    }
    let version = word(src, off + 4)?;
    let stride = if version == 1 {
        16
    } else if version == 2 {
        48
    } else if (3..=5).contains(&version) {
        64
    } else {
        return Err(Error::Dsp);
    };
    let image = word(src, off + 0x0c)?;
    let bss = word(src, off + 0x10)?;
    let align = word(src, off + 0x14)?;
    let (image_off, reloc_off, nreloc) = (
        word(src, off + 0x18)?,
        word(src, off + 0x1c)?,
        word(src, off + 0x20)?,
    );
    let (import_off, nimport) = (word(src, off + 0x24)?, word(src, off + 0x28)?);
    let (voice_off, nvoice) = (word(src, off + 0x2c)?, word(src, off + 0x30)?);
    let (fx_off, nfx) = if version >= 4 {
        (word(src, off + 0x34)?, word(src, off + 0x38)?)
    } else {
        (0, 0)
    };
    if image == 0
        || image & 3 != 0
        || bss & 3 != 0
        || !(8..=GRANULE).contains(&align)
        || align & (align - 1) != 0
        || nreloc > MAX_RELOCS
        || nimport > MAX_IMPORTS
        || nvoice > MAX_VOICES
        || nfx > MAX_FX
        || !inside(image_off, image, size)
        || !inside(reloc_off, nreloc * 8, size)
        || !inside(import_off, nimport * 32, size)
        || !inside(voice_off, nvoice * stride, size)
        || !inside(fx_off, nfx * FX_LEN, size)
    {
        return Err(Error::Dsp);
    }
    if version == 5 {
        let record_off = word(src, off + 0x3c)?;
        if record_off < DSP_HEADER || record_off & 3 != 0 || !inside(record_off, 32, image_off) {
            return Err(Error::Dsp);
        }
        for (table, len) in [
            (reloc_off, nreloc * 8),
            (import_off, nimport * 32),
            (voice_off, nvoice * stride),
            (fx_off, nfx * FX_LEN),
        ] {
            if len != 0 && table < record_off + 32 && record_off < table + len {
                return Err(Error::Dsp);
            }
        }
        let at = off + record_off;
        // The record has little-endian words. Kind 2 is the per-track sample table (two zero blocks of the initialized image, no
        // relocations in them), the only kind.
        if word(src, at)?.swap_bytes() != 2 {
            return Err(Error::Dsp);
        }
        let records = word(src, at + 8)?.swap_bytes();
        let control = word(src, at + 12)?.swap_bytes();
        for i in 4..8 {
            if word(src, at + i * 4)? != 0 {
                return Err(Error::Dsp);
            }
        }
        let blocks = [(records, 512u32), (control, 256u32)];
        for (b, len) in blocks {
            if b & 3 != 0 || !inside(b, len, image) {
                return Err(Error::Dsp);
            }
            for i in 0..len / 4 {
                if word(src, off + image_off + b + i * 4)? != 0 {
                    return Err(Error::Dsp);
                }
            }
            for i in 0..nreloc {
                let r = word(src, off + reloc_off + i * 8)?;
                if u64::from(r) < u64::from(b) + u64::from(len) && u64::from(r) + 6 > u64::from(b) {
                    return Err(Error::Dsp);
                }
            }
        }
        if records < control + 256 && control < records + 512 {
            return Err(Error::Dsp);
        }
    }
    // the range: image and bss to 8, then (version 3) every voice record's states, each rounded to 8
    let mut total = image
        .checked_add(bss)
        .and_then(|t| t.checked_add(7))
        .ok_or(Error::Dsp)?
        & !7;
    if version >= 3 {
        let mut i = 0;
        while i < nvoice {
            let at = off + voice_off + stride * i;
            let (state, count) = (word(src, at + 12)?, word(src, at + 52)?);
            if state > MAX_STATE_BYTES || count > MAX_STATE_VOICES {
                return Err(Error::Dsp);
            }
            // both are bounded: the product is under 2^30
            total = total
                .checked_add(count * ((state + 7) & !7))
                .ok_or(Error::Dsp)?;
            i += 1;
        }
    }
    // version 4: the state of every effect (state_bytes at +8 of its record), rounded to 8
    let mut i = 0;
    while i < nfx {
        let state = word(src, off + fx_off + FX_LEN * i + 8)?;
        if state > MAX_STATE_BYTES {
            return Err(Error::Dsp);
        }
        total = total.checked_add((state + 7) & !7).ok_or(Error::Dsp)?;
        i += 1;
    }
    n.fx_slots = nfx;
    n.arena_xfer = arena_block(image).ok_or(Error::Dsp)?;
    n.dsp_area = total.checked_add(GRANULE - 1).ok_or(Error::Dsp)? & !(GRANULE - 1);
    n.dsp_align = align;
    n.voice_rows = nvoice;
    Ok(())
}

pub mod keys;

#[cfg(test)]
mod tests;
