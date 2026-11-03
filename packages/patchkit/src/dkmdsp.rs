//! The DSP section of a `.dkm`: the SHARC+ code and data of a run-time machine, linked at a nominal base and relocated by the
//! digikit runtime on the ColdFire before it sends the bytes to the DSP (docs/design/digikit-dkm.md, "DSP section").
//!
//! All integers are big endian. Layout (offsets from the start of the section):
//!
//! ```text
//! 0x00 u32 magic "DKDS"   0x04 u32 version = 3 (2: voices of 48 bytes; 1: of 16 bytes without ranges)   0x08 u32 device (1 dt2, 2 dn2)
//! 0x0c u32 image_size   bytes of text and data that are sent (a multiple of 4)
//! 0x10 u32 bss_size     bytes reserved behind the image, not sent
//! 0x14 u32 align        placement alignment in bytes (8 or more, a power of two)
//! 0x18 u32 image_off    0x1c u32 reloc_off   0x20 u32 reloc_count
//! 0x24 u32 import_off   0x28 u32 import_count   0x2c u32 voice_off   0x30 u32 voice_count
//! 0x34 u32 fx_off       0x38 u32 fx_count   (version 4+: effect records; 0 before)   0x3c u32 bank_record_off (version 5; 0 before)
//! imports: 32-byte NUL padded names of DSP symbols the runtime resolves (`dyndsp_info` of core/dyn-dsp)
//! relocs : 8 bytes each: u32 image byte offset of a 48-bit instruction, u32 kind << 28 | class << 24 | import index
//!            kind 0 abs24 (the instruction's 24-bit address field), kind 1 abs32 (its 32-bit immediate)
//!            class 0: the value is a short-word pc of this object (add the pc shift of the placement)
//!                  1: the value is a byte address of this object (add the byte shift)
//!                  2: the value is the addend of the import (add the import's value, a pc or a byte address as exported)
//! voices : 48 bytes each: u32 machine type, u32 image byte offset of the entry routine, 8 x u32 range of machine knob 0 .. 7 for the runtime
//!            guard of core/dsp-voice (min | max << 16; 0: undeclared), 2 x u32 reserved. Version 1 had 16 bytes (type, entry, 2 reserved)
//!          version 3: 64 bytes each: 0 u32 type, 4 u32 render entry (image byte offset), 8 u32 note_on entry (image byte offset; 0: none),
//!            12 u32 state_bytes (per voice), 16 u32 clear_bytes (zeroed at note-on), 20..51 8 x u32 ranges as above, 52 u32 voices (number of
//!            voice states to reserve behind the image and bss), 56..63 reserved 0. The packer writes version 3 only when a voice has a
//!            note_on entry or a state; otherwise version 2 (the runtime parses all three; v1 and v2 voices have note_on 0 and no state)
//! fx     : version 4: 64 bytes each: 0 u32 process (image byte offset of the process routine), 4 u32 desc (image byte offset of its `dk_fx_desc`),
//!            8 u32 state_bytes (from the desc: reserved behind the image, bss and voice states), 12 u32 n_params (from the desc), 16 u32 flags (the routes, below; 0 = master only),
//!            20..51 8 x u32 the words p0 .. p7 of the effect's row (the DSP reads them as they are), 52..63 reserved 0. A version 4 section has voices of 64
//!            bytes (as version 3); the packer writes it only when the module has an effect
//!            flags: bit 0 `FXF_SEND` the effect may run on a send bus, bit 1 `FXF_NOMASTER` it may not run on the master chain, bit 2 `FXF_DEFAULT_SEND` its default
//!            route is the send bus (the settings key `fx/route/<module id>` decides when it is there). 0, what every record before the routes has: master only.
//!            Allowed combinations: 0; 1; 1 | 4; 1 | 2 | 4 (send only)
//! image  : the linked bytes at the nominal base `L2_BYTE_BASE`; every relocated field holds `S + A` at that base
//! ```
//!
//! A text symbol is a short-word pc (`L2_SW_BASE + (byte - L2_BYTE_BASE) / 2`), a data or bss symbol a byte address. Placed at
//! the L2 byte address `B`, the pc shift is `(B - L2_BYTE_BASE) / 2` and the byte shift `B - L2_BYTE_BASE`.

use crate::dko::Dko;
use crate::dsp::{L2_BYTE_BASE, L2_SW_BASE};
use crate::error::*;
use crate::link;
use std::collections::{BTreeMap, BTreeSet};

pub const MAGIC: u32 = 0x444b_4453; // "DKDS"
pub const VERSION: u32 = 2;
/// The version with `note_on` entries and per-voice state (voices of `VOICE_LEN_V3` bytes).
pub const VERSION_3: u32 = 3;
/// The version with effect records (`FX_LEN` bytes each); its voices are those of version 3.
pub const VERSION_4: u32 = 4;
/// One module-owned sample bank; voices and effects retain version 4 layouts.
pub const VERSION_5: u32 = 5;
/// Little-endian words: version 1, local id, bank byte offset, capacity frames,
/// descriptor byte offset, then three reserved zero words. Offsets are relative
/// to the module DSP image base; unlike this record the enclosing header is BE.
pub const SAMPLE_BANK_LEN: usize = 32;
/// The same 32-byte record with kind word 2 declares the per-track sample table (API 1.5) instead of a bank: little-endian words
/// kind, local id, records byte offset, control byte offset, then four reserved zero words. A module has a bank or a table, never both.
pub const SAMPLE_TRACKS_KIND: u32 = 2;
/// 16 `dk_strack` records of 8 words, and 16 control sets of 4 words (`<dk/sample_track.h>`).
pub const STRACKS_RECORD_BYTES: u32 = 512;
pub const STRACKS_CONTROL_BYTES: u32 = 256;
/// The version without voice ranges (voices of `VOICE_LEN_V1` bytes); the runtime still loads it, with every range undeclared.
pub const VERSION_1: u32 = 1;
pub const HEADER: usize = 0x40;
pub const RELOC_LEN: usize = 8;
pub const VOICE_LEN: usize = 48;
pub const VOICE_LEN_V3: usize = 64;
/// `dk_machine_desc` (SDK contract): magic "DKMA" and the contract number the packer accepts, and its size in u32 words.
pub const DESC_MAGIC: u32 = 0x444b_4d41;
pub const DESC_CONTRACT: u32 = 1;
/// Bit 8 of the desc's contract word: a stereo machine (contract 1.2, `DK_MACHINE_STEREO`). The low byte is the major number.
pub const DESC_STEREO: u32 = 0x100;
pub const DESC_WORDS: usize = 10;
pub const VOICE_LEN_V1: usize = 16;
pub const FX_LEN: usize = 64;
/// The routes of an effect (the manifest's `route` and `routes`): the master chain and a send bus. `Effect::routes` is a mask of them.
pub const ROUTE_MASTER: u32 = 1;
pub const ROUTE_SEND: u32 = 2;
/// Bits of the effect record's flags word (offset 16); 0 = master only, the default route master (a record of the version before the routes).
pub const FXF_SEND: u32 = 1;
pub const FXF_NOMASTER: u32 = 2;
pub const FXF_DEFAULT_SEND: u32 = 4;
/// Effects a module may carry (`DK_FX_MAX` of <dk/fx.h>).
pub const MAX_FX: usize = dkneeds::MAX_FX as usize;
/// `dk_fx_desc` (<dk/fx.h>): magic "DKFX", the contract number the packer accepts, its size in u32 words and the most parameter words.
pub const FX_DESC_MAGIC: u32 = 0x444b_4658;
pub const FX_DESC_CONTRACT: u32 = 1;
pub const FX_DESC_WORDS: usize = 7;
pub const FX_MAX_PARAMS: u32 = 8;
pub const IMPORT_LEN: usize = 32;
pub const MAX_RELOCS: usize = dkneeds::MAX_RELOCS as usize;
pub const MAX_IMPORTS: usize = dkneeds::MAX_IMPORTS as usize;
pub const MAX_VOICES: usize = dkneeds::MAX_VOICES as usize;
/// Placement granule of the runtime's area allocator (bytes).
pub const GRANULE: u32 = dkneeds::GRANULE;

pub const KIND_ABS24: u32 = 0;
pub const KIND_ABS32: u32 = 1;
pub const CLASS_TEXT: u32 = 0;
pub const CLASS_DATA: u32 = 1;
pub const CLASS_IMPORT: u32 = 2;

fn put(b: &mut [u8], off: usize, v: u32) {
    b[off..off + 4].copy_from_slice(&v.to_be_bytes());
}
fn get(b: &[u8], off: usize) -> u32 {
    u32::from_be_bytes(b[off..off + 4].try_into().unwrap())
}
fn align_up(x: usize, a: usize) -> usize {
    x.div_ceil(a) * a
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Voice {
    pub type_no: u32,
    /// byte offset of the entry routine in the image
    pub entry: u32,
    /// byte offset of the note_on routine in the image (0: none)
    pub note_on: u32,
    /// bytes of state per voice, bytes of it zeroed at note-on, number of voice states
    pub state_bytes: u32,
    pub clear_bytes: u32,
    pub voices: u32,
    /// range of machine knob 0 .. 7 (min | max << 16; 0 undeclared)
    pub ranges: [u32; 8],
}

impl Voice {
    /// The record needs version 3.
    fn contract(&self) -> bool {
        self.note_on != 0 || self.state_bytes != 0 || self.clear_bytes != 0 || self.voices != 0
    }
}

/// A voice as the manifest names it. `render` (and `note_on`) are global text symbols of the DSP object; `desc` is a data symbol
/// holding a `dk_machine_desc` (10 u32 words, little endian in the image) that gives voices, state and clear bytes.
#[derive(Clone, Debug)]
pub struct VoiceSpec {
    pub type_no: u32,
    pub render: String,
    pub note_on: Option<String>,
    pub desc: Option<String>,
    /// Declared stereo in `DK_MACHINE`; the desc must carry `DESC_STEREO` too.
    pub stereo: bool,
    pub ranges: [u32; 8],
}

impl VoiceSpec {
    /// A voice with only a render entry (no note_on, no state).
    pub fn old(type_no: u32, render: String, ranges: [u32; 8]) -> VoiceSpec {
        VoiceSpec {
            type_no,
            render,
            note_on: None,
            desc: None,
            stereo: false,
            ranges,
        }
    }
}

/// An effect record of version 4.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Effect {
    /// byte offset of the process routine in the image
    pub process: u32,
    /// byte offset of the `dk_fx_desc` in the image
    pub desc: u32,
    /// bytes of state and parameters the effect reads (both from the desc)
    pub state_bytes: u32,
    pub n_params: u32,
    /// the words p0 .. p7 of the row
    pub defaults: [u32; 8],
    /// The routes the effect may run on (`ROUTE_MASTER`, `ROUTE_SEND`; a mask) and the default one (`ROUTE_MASTER` or `ROUTE_SEND`).
    pub routes: u32,
    pub route: u32,
}

impl Effect {
    /// The flags word of the record for `routes` and `route`; `Err` for a combination the record cannot say.
    pub fn flags_of(routes: u32, route: u32) -> Result<u32> {
        if routes == 0 || routes & !(ROUTE_MASTER | ROUTE_SEND) != 0 {
            return Err(err!(
                "effect routes {routes:#x}: a mask of master (1) and send (2)"
            ));
        }
        if route != ROUTE_MASTER && route != ROUTE_SEND || routes & route == 0 {
            return Err(err!(
                "effect route {route}: not one of the routes {routes:#x}"
            ));
        }
        let mut f = 0;
        if routes & ROUTE_SEND != 0 {
            f |= FXF_SEND;
        }
        if routes & ROUTE_MASTER == 0 {
            f |= FXF_NOMASTER;
        }
        if route == ROUTE_SEND {
            f |= FXF_DEFAULT_SEND;
        }
        Ok(f)
    }

    /// The routes and the default route a flags word says (the runtime's checks: a bit that is not defined, or a combination that leaves the default outside the allowed routes, is refused).
    pub fn routes_of(flags: u32) -> Result<(u32, u32)> {
        if flags & !(FXF_SEND | FXF_NOMASTER | FXF_DEFAULT_SEND) != 0 {
            return Err(err!("effect flags {flags:#x}: unknown bits"));
        }
        let mut routes = 0;
        if flags & FXF_NOMASTER == 0 {
            routes |= ROUTE_MASTER;
        }
        if flags & FXF_SEND != 0 {
            routes |= ROUTE_SEND;
        }
        let route = if flags & FXF_DEFAULT_SEND != 0 {
            ROUTE_SEND
        } else {
            ROUTE_MASTER
        };
        if routes & route == 0 {
            return Err(err!(
                "effect flags {flags:#x}: the default route is not an allowed one"
            ));
        }
        Ok((routes, route))
    }
}

/// An effect as the manifest names it: `process` is a global text symbol of the DSP object, `desc` a data symbol holding a `dk_fx_desc`.
#[derive(Clone, Debug)]
pub struct FxSpec {
    pub process: String,
    pub desc: String,
    pub defaults: Vec<u32>,
    /// The allowed routes (mask of `ROUTE_*`) and the default one (`register::fx_routes` resolves the manifest's `route` and `routes`).
    pub routes: u32,
    pub route: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reloc {
    pub offset: u32,
    pub kind: u32,
    pub class: u32,
    pub import: u32,
}

/// Version 1 sample-bank wire record (little-endian words).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SampleBank {
    pub local_id: u32,
    pub bank_byte_offset: u32,
    pub capacity_frames: u32,
    pub descriptor_byte_offset: u32,
}

impl SampleBank {
    fn validate(&self, image: &[u8], bss: u32, relocs: &[Reloc]) -> Result<()> {
        let image_end = u32::try_from(image.len()).map_err(|_| err!("DSP image too large"))?;
        let end = image_end
            .checked_add(bss)
            .ok_or_else(|| err!("sample bank extent overflow"))?;
        let bank_end = self
            .capacity_frames
            .checked_mul(4)
            .and_then(|n| self.bank_byte_offset.checked_add(n))
            .ok_or_else(|| err!("sample bank capacity overflow"))?;
        let desc_end = self
            .descriptor_byte_offset
            .checked_add(16)
            .ok_or_else(|| err!("sample descriptor overflow"))?;
        if !(1..=8192).contains(&self.capacity_frames)
            || self.bank_byte_offset % 4 != 0
            || self.descriptor_byte_offset % 4 != 0
            || self.bank_byte_offset < image_end
            || bank_end > end
            || desc_end > image_end
        {
            return Err(err!(
                "sample bank or descriptor outside owned BSS/data, unaligned or invalid capacity"
            ));
        }
        if image[self.descriptor_byte_offset as usize..desc_end as usize]
            .iter()
            .any(|&b| b != 0)
            || relocs.iter().any(|r| {
                u64::from(r.offset) < u64::from(desc_end)
                    && u64::from(r.offset) + 6 > u64::from(self.descriptor_byte_offset)
            })
        {
            return Err(err!(
                "sample descriptor must be zero and contain no relocations"
            ));
        }
        Ok(())
    }
}

/// Version 5 sample-table wire record, kind 2 (little-endian words).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SampleTracks {
    pub local_id: u32,
    pub records_byte_offset: u32,
    pub control_byte_offset: u32,
}

impl SampleTracks {
    fn validate(&self, image: &[u8], relocs: &[Reloc]) -> Result<()> {
        let blocks = [
            (self.records_byte_offset, STRACKS_RECORD_BYTES),
            (self.control_byte_offset, STRACKS_CONTROL_BYTES),
        ];
        for (off, len) in blocks {
            let end = off
                .checked_add(len)
                .ok_or_else(|| err!("sample track table overflow"))?;
            if off % 4 != 0 || end as usize > image.len() {
                return Err(err!(
                    "sample track table outside the initialized DSP data or unaligned"
                ));
            }
            if image[off as usize..end as usize].iter().any(|&b| b != 0)
                || relocs
                    .iter()
                    .any(|r| u64::from(r.offset) < u64::from(end) && u64::from(r.offset) + 6 > u64::from(off))
            {
                return Err(err!(
                    "sample track table must be zero and contain no relocations"
                ));
            }
        }
        let (r, c) = (self.records_byte_offset, self.control_byte_offset);
        if r < c + STRACKS_CONTROL_BYTES && c < r + STRACKS_RECORD_BYTES {
            return Err(err!("sample track records and control sets overlap"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DspSection {
    pub sample_bank: Option<SampleBank>,
    pub sample_tracks: Option<SampleTracks>,
    pub device: u32,
    pub image: Vec<u8>,
    pub bss: u32,
    pub align: u32,
    pub imports: Vec<String>,
    pub relocs: Vec<Reloc>,
    pub voices: Vec<Voice>,
    pub fxs: Vec<Effect>,
}

/// Link `obj` (a SHARC+ object) at the nominal base and encode the section. `voices` names the entry routine of each machine type
/// (a global text symbol of the object), `fxs` the process routine and descriptor of each effect.
pub fn pack(obj: &Dko, device: u32, voices: &[VoiceSpec], fxs: &[FxSpec]) -> Result<Vec<u8>> {
    pack_with_bank(obj, device, voices, fxs, None)
}

/// Pack an optional module-owned sample bank.
pub fn pack_with_bank(
    obj: &Dko,
    device: u32,
    voices: &[VoiceSpec],
    fxs: &[FxSpec],
    bank: Option<&crate::typed::SampleBankDef>,
) -> Result<Vec<u8>> {
    pack_with_samples(obj, device, voices, fxs, bank, None)
}

/// Pack an optional module-owned sample bank or per-track sample table (not both).
pub fn pack_with_samples(
    obj: &Dko,
    device: u32,
    voices: &[VoiceSpec],
    fxs: &[FxSpec],
    bank: Option<&crate::typed::SampleBankDef>,
    tracks: Option<&crate::typed::SampleTracksDef>,
) -> Result<Vec<u8>> {
    if bank.is_some() && tracks.is_some() {
        return Err(err!("a module declares a sample bank or a track table, not both"));
    }
    if obj.arch != "sharc" {
        return Err(err!(
            "a DSP section holds a SHARC+ object; the object is {}",
            obj.arch
        ));
    }
    let kind_of: BTreeMap<&str, &str> = obj
        .sections
        .iter()
        .map(|s| (s.name.as_str(), s.kind.as_str()))
        .collect();
    let defined: BTreeMap<&str, (&str, bool)> = obj
        .symbols
        .iter()
        .filter_map(|s| {
            s.section
                .as_ref()
                .map(|sec| (s.name.as_str(), (kind_of[sec.as_str()], s.global)))
        })
        .collect();
    let mut imports: BTreeSet<&str> = BTreeSet::new();
    for r in &obj.relocs {
        if !defined.contains_key(r.symbol.as_str()) {
            imports.insert(r.symbol.as_str());
        }
    }
    let imports: Vec<&str> = imports.into_iter().collect();
    if imports.len() > MAX_IMPORTS {
        return Err(err!("{} DSP imports; at most {MAX_IMPORTS}", imports.len()));
    }
    for n in &imports {
        if n.len() >= IMPORT_LEN {
            return Err(err!(
                "DSP import name {n} is longer than {}",
                IMPORT_LEN - 1
            ));
        }
    }
    let externs: BTreeMap<String, u64> = imports.iter().map(|n| (n.to_string(), 0)).collect();
    let linked = link::link(obj, L2_BYTE_BASE as u64, &externs)?;
    let image = linked.image.clone();
    if image.is_empty() || image.len() % 4 != 0 {
        return Err(err!("the DSP image is {} bytes", image.len()));
    }
    let align = obj
        .sections
        .iter()
        .map(|s| s.align)
        .max()
        .unwrap_or(8)
        .max(8);
    if !align.is_power_of_two() || align > GRANULE {
        return Err(err!(
            "DSP section alignment {align}: a power of two up to {GRANULE}"
        ));
    }
    let mut relocs = vec![];
    for r in &obj.relocs {
        let sec = linked.places[&r.section];
        let off = sec - L2_BYTE_BASE as u64 + r.offset * 2;
        if kind_of.get(r.section.as_str()) != Some(&"text") || off + 6 > image.len() as u64 {
            return Err(err!(
                "DSP relocation in {} at parcel {}",
                r.section,
                r.offset
            ));
        }
        let kind = match r.rtype.as_str() {
            "sharc.abs24" => KIND_ABS24,
            "sharc.abs32" => KIND_ABS32,
            t => return Err(err!("DSP relocation type {t}")),
        };
        let (class, import) = match defined.get(r.symbol.as_str()) {
            Some(("text", _)) => (CLASS_TEXT, 0),
            Some(_) => (CLASS_DATA, 0),
            None => (
                CLASS_IMPORT,
                imports.iter().position(|n| *n == r.symbol).unwrap() as u32,
            ),
        };
        if kind == KIND_ABS24 && class == CLASS_DATA {
            return Err(err!(
                "{}: a 24-bit field cannot hold a data address of the placement",
                r.symbol
            ));
        }
        relocs.push(Reloc {
            offset: off as u32,
            kind,
            class,
            import,
        });
    }
    relocs.sort_by_key(|r| (r.offset, r.kind));
    if relocs.len() > MAX_RELOCS {
        return Err(err!(
            "{} DSP relocations; at most {MAX_RELOCS}",
            relocs.len()
        ));
    }
    if voices.len() > MAX_VOICES {
        return Err(err!("{} voices; at most {MAX_VOICES}", voices.len()));
    }
    let mut vs = vec![];
    let entry_of = |name: &String| -> Result<u32> {
        match defined.get(name.as_str()) {
            Some(("text", true)) => {}
            Some(_) => {
                return Err(err!(
                    "voice entry {name}: not a global text symbol of the DSP object"
                ));
            }
            None => {
                return Err(err!(
                    "voice entry {name}: the DSP object does not define it"
                ));
            }
        }
        let pc = linked.symbols[name.as_str()];
        Ok(((pc - L2_SW_BASE as u64) * 2) as u32)
    };
    for v in voices {
        let entry = entry_of(&v.render)?;
        let note_on = match &v.note_on {
            Some(n) => {
                let e = entry_of(n)?;
                if e == 0 {
                    return Err(err!(
                        "voice entry {n}: at image offset 0, which the section reads as no note_on"
                    ));
                }
                e
            }
            None => 0,
        };
        let (mut voices_n, mut state_bytes, mut clear_bytes) = (0, 0, 0);
        if let Some(d) = &v.desc {
            match defined.get(d.as_str()) {
                Some(("data", _)) => {}
                Some(_) => {
                    return Err(err!("voice desc {d}: not a data symbol of the DSP object"));
                }
                None => {
                    return Err(err!("voice desc {d}: the DSP object does not define it"));
                }
            }
            let at = (linked.symbols[d.as_str()] - L2_BYTE_BASE as u64) as usize;
            let bytes = image
                .get(at..at + DESC_WORDS * 4)
                .ok_or_else(|| err!("voice desc {d}: shorter than {} bytes", DESC_WORDS * 4))?;
            let w = |i: usize| u32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap());
            if w(0) != DESC_MAGIC {
                return Err(err!(
                    "voice desc {d}: magic {:#x}, not DKMA ({DESC_MAGIC:#x})",
                    w(0)
                ));
            }
            if w(1) & !DESC_STEREO != DESC_CONTRACT {
                return Err(err!(
                    "voice desc {d}: contract {:#x}, this packer takes {DESC_CONTRACT} (optionally with {DESC_STEREO:#x}, stereo)",
                    w(1)
                ));
            }
            if (w(1) & DESC_STEREO != 0) != v.stereo {
                return Err(err!(
                    "voice desc {d}: DK_MACHINE says stereo = {}, the desc's contract word says {}",
                    v.stereo as u32,
                    (w(1) & DESC_STEREO != 0) as u32
                ));
            }
            voices_n = w(3);
            state_bytes = w(4);
            clear_bytes = w(5);
            if clear_bytes > state_bytes {
                return Err(err!(
                    "voice desc {d}: state_clear_bytes {clear_bytes} is more than state_bytes {state_bytes}"
                ));
            }
        }
        vs.push(Voice {
            type_no: v.type_no,
            entry,
            note_on,
            state_bytes,
            clear_bytes,
            voices: voices_n,
            ranges: v.ranges,
        });
    }
    if fxs.len() > MAX_FX {
        return Err(err!("{} effects; at most {MAX_FX}", fxs.len()));
    }
    let mut es = vec![];
    for f in fxs {
        let process = entry_of(&f.process).map_err(|e| err!("effect process: {e}"))?;
        match defined.get(f.desc.as_str()) {
            Some(("data", _)) => {}
            Some(_) => {
                return Err(err!(
                    "effect desc {}: not a data symbol of the DSP object",
                    f.desc
                ));
            }
            None => {
                return Err(err!(
                    "effect desc {}: the DSP object does not define it",
                    f.desc
                ));
            }
        }
        let at = (linked.symbols[f.desc.as_str()] - L2_BYTE_BASE as u64) as usize;
        let bytes = image.get(at..at + FX_DESC_WORDS * 4).ok_or_else(|| {
            err!(
                "effect desc {}: shorter than {} bytes",
                f.desc,
                FX_DESC_WORDS * 4
            )
        })?;
        let w = |i: usize| u32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap());
        if w(0) != FX_DESC_MAGIC {
            return Err(err!(
                "effect desc {}: magic {:#x}, not DKFX ({FX_DESC_MAGIC:#x})",
                f.desc,
                w(0)
            ));
        }
        if w(1) != FX_DESC_CONTRACT {
            return Err(err!(
                "effect desc {}: contract {}, this packer takes {FX_DESC_CONTRACT}",
                f.desc,
                w(1)
            ));
        }
        let (n_params, state_bytes) = (w(2), w(3));
        if n_params > FX_MAX_PARAMS || state_bytes % 4 != 0 {
            return Err(err!(
                "effect desc {}: n_params {n_params} (at most {FX_MAX_PARAMS}), state_bytes {state_bytes} (a multiple of 4)",
                f.desc
            ));
        }
        if f.defaults.len() > n_params as usize {
            return Err(err!(
                "effect {}: {} default words for {n_params} parameters",
                f.process,
                f.defaults.len()
            ));
        }
        let mut defaults = [0u32; 8];
        defaults[..f.defaults.len()].copy_from_slice(&f.defaults);
        es.push(Effect {
            process,
            desc: at as u32,
            state_bytes,
            n_params,
            defaults,
            routes: f.routes,
            route: f.route,
        });
        Effect::flags_of(f.routes, f.route).map_err(|e| err!("effect {}: {}", f.process, e.0))?;
    }
    let symbol = |name: &str, kind: &str, len: u32| -> Result<u32> {
        let sym = obj
            .symbols
            .iter()
            .find(|s| s.name == name && s.section.is_some())
            .ok_or_else(|| {
                err!("sample symbol {name}: not defined by this DSP object")
            })?;
        let sec = obj
            .sections
            .iter()
            .find(|s| Some(&s.name) == sym.section.as_ref())
            .unwrap();
        if sec.kind != kind
            || sym
                .value
                .checked_add(u64::from(len))
                .is_none_or(|end| end > u64::from(sec.size))
        {
            return Err(err!(
                "sample bank symbol {name}: outside its {kind} section"
            ));
        }
        let at = linked.symbols[name]
            .checked_sub(u64::from(L2_BYTE_BASE))
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| err!("sample symbol {name}: offset overflow"))?;
        Ok(at)
    };
    let sample_bank = bank
        .map(|b| -> Result<SampleBank> {
            let len = b
                .capacity_frames
                .checked_mul(4)
                .ok_or_else(|| err!("sample bank capacity overflow"))?;
            let bank = SampleBank {
                local_id: b.local_id,
                bank_byte_offset: symbol(&b.bank, "bss", len)?,
                descriptor_byte_offset: symbol(&b.descriptor, "data", 16)?,
                capacity_frames: b.capacity_frames,
            };
            bank.validate(
                &image,
                u32::try_from(linked.bss_size).map_err(|_| err!("DSP BSS too large"))?,
                &relocs,
            )?;
            Ok(bank)
        })
        .transpose()?;
    let sample_tracks = tracks
        .map(|t| -> Result<SampleTracks> {
            let tracks = SampleTracks {
                local_id: t.local_id,
                records_byte_offset: symbol(&t.records, "data", STRACKS_RECORD_BYTES)?,
                control_byte_offset: symbol(&t.control, "data", STRACKS_CONTROL_BYTES)?,
            };
            tracks.validate(&image, &relocs)?;
            Ok(tracks)
        })
        .transpose()?;
    let s = DspSection {
        sample_bank,
        sample_tracks,
        device,
        image,
        bss: u32::try_from(
            linked
                .bss_size
                .div_ceil(4)
                .checked_mul(4)
                .ok_or_else(|| err!("DSP BSS overflow"))?,
        )
        .map_err(|_| err!("DSP BSS too large"))?,
        align,
        imports: imports.iter().map(|s| s.to_string()).collect(),
        relocs,
        voices: vs,
        fxs: es,
    };
    Ok(s.encode())
}

impl DspSection {
    pub fn encode(&self) -> Vec<u8> {
        let import_off = HEADER;
        let reloc_off = import_off + self.imports.len() * IMPORT_LEN;
        let voice_off = reloc_off + self.relocs.len() * RELOC_LEN;
        let v5 = self.sample_bank.is_some() || self.sample_tracks.is_some();
        let v4 = v5 || !self.fxs.is_empty();
        let v3 = v4 || self.voices.iter().any(|v| v.contract());
        let vlen = if v3 { VOICE_LEN_V3 } else { VOICE_LEN };
        let fx_off = voice_off + self.voices.len() * vlen;
        let bank_off = fx_off + self.fxs.len() * FX_LEN;
        let image_off = align_up(bank_off + if v5 { SAMPLE_BANK_LEN } else { 0 }, 16);
        let mut f = vec![0u8; image_off + self.image.len()];
        put(&mut f, 0, MAGIC);
        put(
            &mut f,
            4,
            if v5 {
                VERSION_5
            } else if v4 {
                VERSION_4
            } else if v3 {
                VERSION_3
            } else {
                VERSION
            },
        );
        put(&mut f, 8, self.device);
        put(&mut f, 0x0c, self.image.len() as u32);
        put(&mut f, 0x10, self.bss);
        put(&mut f, 0x14, self.align);
        put(&mut f, 0x18, image_off as u32);
        put(&mut f, 0x1c, reloc_off as u32);
        put(&mut f, 0x20, self.relocs.len() as u32);
        put(&mut f, 0x24, import_off as u32);
        put(&mut f, 0x28, self.imports.len() as u32);
        put(&mut f, 0x2c, voice_off as u32);
        put(&mut f, 0x30, self.voices.len() as u32);
        if v4 {
            put(&mut f, 0x34, fx_off as u32);
            put(&mut f, 0x38, self.fxs.len() as u32);
        }
        let record = match (&self.sample_bank, &self.sample_tracks) {
            (Some(b), _) => Some([
                1,
                b.local_id,
                b.bank_byte_offset,
                b.capacity_frames,
                b.descriptor_byte_offset,
                0,
                0,
                0,
            ]),
            (None, Some(t)) => Some([
                SAMPLE_TRACKS_KIND,
                t.local_id,
                t.records_byte_offset,
                t.control_byte_offset,
                0,
                0,
                0,
                0,
            ]),
            (None, None) => None,
        };
        if let Some(words) = record {
            put(&mut f, 0x3c, bank_off as u32);
            for (i, w) in words.iter().enumerate() {
                f[bank_off + i * 4..bank_off + i * 4 + 4].copy_from_slice(&w.to_le_bytes());
            }
        }
        for (i, n) in self.imports.iter().enumerate() {
            let at = import_off + i * IMPORT_LEN;
            f[at..at + n.len()].copy_from_slice(n.as_bytes());
        }
        for (i, r) in self.relocs.iter().enumerate() {
            let at = reloc_off + i * RELOC_LEN;
            put(&mut f, at, r.offset);
            put(&mut f, at + 4, r.kind << 28 | r.class << 24 | r.import);
        }
        for (i, v) in self.voices.iter().enumerate() {
            let at = voice_off + i * vlen;
            put(&mut f, at, v.type_no);
            put(&mut f, at + 4, v.entry);
            if v3 {
                put(&mut f, at + 8, v.note_on);
                put(&mut f, at + 12, v.state_bytes);
                put(&mut f, at + 16, v.clear_bytes);
                for (k, w) in v.ranges.iter().enumerate() {
                    put(&mut f, at + 20 + 4 * k, *w);
                }
                put(&mut f, at + 52, v.voices);
            } else {
                for (k, w) in v.ranges.iter().enumerate() {
                    put(&mut f, at + 8 + 4 * k, *w);
                }
            }
        }
        for (i, e) in self.fxs.iter().enumerate() {
            let at = fx_off + i * FX_LEN;
            put(&mut f, at, e.process);
            put(&mut f, at + 4, e.desc);
            put(&mut f, at + 8, e.state_bytes);
            put(&mut f, at + 12, e.n_params);
            put(
                &mut f,
                at + 16,
                Effect::flags_of(e.routes, e.route).expect("routes checked by pack"),
            );
            for (k, w) in e.defaults.iter().enumerate() {
                put(&mut f, at + 20 + 4 * k, *w);
            }
        }
        f[image_off..].copy_from_slice(&self.image);
        f
    }

    /// Bytes of the range the runtime reserves for the section (`dsp.rs`, `extent`, then the granule): image and bss rounded to 8, then
    /// `voices * state_bytes` (each rounded to 8) of every version 3 voice, then `state_bytes` (rounded to 8) of every effect, rounded up to a granule. `dkneeds` computes the same from the file.
    pub fn range_len(&self) -> u32 {
        let mut n = (self.image.len() as u32 + self.bss).div_ceil(8) * 8;
        for v in &self.voices {
            n += v.voices * v.state_bytes.div_ceil(8) * 8;
        }
        for e in &self.fxs {
            n += e.state_bytes.div_ceil(8) * 8;
        }
        n.div_ceil(GRANULE) * GRANULE
    }

    /// Parse and check a section the way the runtime does.
    pub fn parse(f: &[u8]) -> Result<DspSection> {
        if f.len() < HEADER || get(f, 0) != MAGIC {
            return Err(err!("not a DSP section (magic)"));
        }
        let version = get(f, 4);
        if version != VERSION
            && version != VERSION_1
            && version != VERSION_3
            && version != VERSION_4
            && version != VERSION_5
        {
            return Err(err!("DSP section version {version}"));
        }
        let v3 = version >= VERSION_3;
        let vlen = match version {
            VERSION_1 => VOICE_LEN_V1,
            VERSION_3 | VERSION_4 | VERSION_5 => VOICE_LEN_V3,
            _ => VOICE_LEN,
        };
        let rng = |off: u32, n: usize| -> Result<&[u8]> {
            let off = off as usize;
            f.get(off..off.checked_add(n).ok_or_else(|| err!("overflow"))?)
                .ok_or_else(|| err!("DSP section table at {off:#x}+{n:#x} is outside the section"))
        };
        let image_size = get(f, 0x0c) as usize;
        let align = get(f, 0x14);
        if image_size == 0
            || image_size % 4 != 0
            || !align.is_power_of_two()
            || !(8..=GRANULE).contains(&align)
        {
            return Err(err!(
                "DSP section: image size {image_size}, alignment {align}"
            ));
        }
        let image = rng(get(f, 0x18), image_size)?.to_vec();
        let nrel = get(f, 0x20) as usize;
        let nimp = get(f, 0x28) as usize;
        let nvoice = get(f, 0x30) as usize;
        let (fx_off, nfx) = if version >= VERSION_4 {
            (get(f, 0x34), get(f, 0x38) as usize)
        } else {
            (0, 0)
        };
        if nrel > MAX_RELOCS || nimp > MAX_IMPORTS || nvoice > MAX_VOICES || nfx > MAX_FX {
            return Err(err!(
                "DSP section: too many relocations, imports, voices or effects"
            ));
        }
        let imp = rng(get(f, 0x24), nimp * IMPORT_LEN)?;
        let imports = (0..nimp)
            .map(|i| {
                let e = &imp[i * IMPORT_LEN..(i + 1) * IMPORT_LEN];
                let l = e.iter().position(|b| *b == 0).unwrap_or(IMPORT_LEN);
                String::from_utf8_lossy(&e[..l]).into_owned()
            })
            .collect::<Vec<_>>();
        let rel = rng(get(f, 0x1c), nrel * RELOC_LEN)?;
        let mut relocs = vec![];
        for i in 0..nrel {
            let off = get(rel, i * RELOC_LEN);
            let w = get(rel, i * RELOC_LEN + 4);
            let r = Reloc {
                offset: off,
                kind: w >> 28,
                class: w >> 24 & 15,
                import: w & 0xffff,
            };
            if off as usize + 6 > image_size
                || r.kind > KIND_ABS32
                || r.class > CLASS_IMPORT
                || (r.class == CLASS_IMPORT && r.import as usize >= nimp)
            {
                return Err(err!("DSP relocation {i}: {off:#x} {w:#x}"));
            }
            relocs.push(r);
        }
        let vo = rng(get(f, 0x2c), nvoice * vlen)?;
        let voices = (0..nvoice)
            .map(|i| {
                let at = i * vlen;
                Voice {
                    type_no: get(vo, at),
                    entry: get(vo, at + 4),
                    note_on: if v3 { get(vo, at + 8) } else { 0 },
                    state_bytes: if v3 { get(vo, at + 12) } else { 0 },
                    clear_bytes: if v3 { get(vo, at + 16) } else { 0 },
                    voices: if v3 { get(vo, at + 52) } else { 0 },
                    ranges: match version {
                        VERSION_1 => [0; 8],
                        VERSION_3 | VERSION_4 | VERSION_5 => {
                            std::array::from_fn(|k| get(vo, at + 20 + 4 * k))
                        }
                        _ => std::array::from_fn(|k| get(vo, at + 8 + 4 * k)),
                    },
                }
            })
            .collect::<Vec<_>>();
        if voices.iter().any(|v| {
            v.entry as usize >= image_size
                || v.entry % 2 != 0
                || v.note_on as usize >= image_size
                || v.note_on % 2 != 0
                || v.clear_bytes > v.state_bytes
        }) {
            return Err(err!("DSP voice entry outside the image"));
        }
        let fo = rng(fx_off, nfx * FX_LEN)?;
        let fxs = (0..nfx)
            .map(|i| {
                let at = i * FX_LEN;
                let (routes, route) = Effect::routes_of(get(fo, at + 16))?;
                Ok(Effect {
                    process: get(fo, at),
                    desc: get(fo, at + 4),
                    state_bytes: get(fo, at + 8),
                    n_params: get(fo, at + 12),
                    defaults: std::array::from_fn(|k| get(fo, at + 20 + 4 * k)),
                    routes,
                    route,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        if fxs.iter().any(|e| {
            e.process as usize >= image_size
                || e.process % 2 != 0
                || e.desc as usize + FX_DESC_WORDS * 4 > image_size
                || e.desc % 4 != 0
                || e.n_params > FX_MAX_PARAMS
                || e.state_bytes % 4 != 0
        }) {
            return Err(err!("DSP effect record outside the image or malformed"));
        }
        let (sample_bank, sample_tracks) = if version == VERSION_5 {
            let off = get(f, 0x3c);
            let mut ranges = vec![
                (0u64, HEADER as u64),
                (
                    u64::from(get(f, 0x18)),
                    u64::from(get(f, 0x18)) + image_size as u64,
                ),
                (u64::from(off), u64::from(off) + SAMPLE_BANK_LEN as u64),
            ];
            for (start, len) in [
                (get(f, 0x24), nimp * IMPORT_LEN),
                (get(f, 0x1c), nrel * RELOC_LEN),
                (get(f, 0x2c), nvoice * vlen),
                (fx_off, nfx * FX_LEN),
            ] {
                if len != 0 {
                    if start % 4 != 0 {
                        return Err(err!("unaligned DSP table"));
                    }
                    ranges.push((u64::from(start), u64::from(start) + len as u64));
                }
            }
            ranges.sort_unstable();
            if get(f, 0x10) % 4 != 0 || ranges.windows(2).any(|w| w[0].1 > w[1].0) {
                return Err(err!(
                    "sample bank section has overlapping ranges or unaligned BSS"
                ));
            }
            let table_end = [
                HEADER as u64,
                u64::from(get(f, 0x24)) + (nimp * IMPORT_LEN) as u64,
                u64::from(get(f, 0x1c)) + (nrel * RELOC_LEN) as u64,
                u64::from(get(f, 0x2c)) + (nvoice * vlen) as u64,
                u64::from(fx_off) + (nfx * FX_LEN) as u64,
            ]
            .into_iter()
            .max()
            .unwrap();
            if off % 4 != 0
                || u64::from(off) < table_end
                || u64::from(off) + SAMPLE_BANK_LEN as u64 > u64::from(get(f, 0x18))
            {
                return Err(err!("sample bank record overlaps header, tables or image"));
            }
            let raw = rng(off, SAMPLE_BANK_LEN)?;
            let w = |i: usize| u32::from_le_bytes(raw[i * 4..i * 4 + 4].try_into().unwrap());
            if w(0) == SAMPLE_TRACKS_KIND {
                if (4..8).any(|i| w(i) != 0) {
                    return Err(err!("sample track record reserved words"));
                }
                let t = SampleTracks {
                    local_id: w(1),
                    records_byte_offset: w(2),
                    control_byte_offset: w(3),
                };
                t.validate(&image, &relocs)?;
                (None, Some(t))
            } else {
                if w(0) != 1 || (5..8).any(|i| w(i) != 0) {
                    return Err(err!("sample bank record version or reserved words"));
                }
                let b = SampleBank {
                    local_id: w(1),
                    bank_byte_offset: w(2),
                    capacity_frames: w(3),
                    descriptor_byte_offset: w(4),
                };
                b.validate(&image, get(f, 0x10), &relocs)?;
                (Some(b), None)
            }
        } else {
            (None, None)
        };
        Ok(DspSection {
            sample_bank,
            sample_tracks,
            device: get(f, 8),
            image,
            bss: get(f, 0x10),
            align,
            imports,
            relocs,
            voices,
            fxs,
        })
    }

    /// The reference loader: the image placed at the L2 byte address `base`, imports resolved by `resolve` (pc or byte address as
    /// the DSP symbol is exported). The runtime's Rust loader must produce the same bytes.
    pub fn relocated(&self, base: u32, resolve: &dyn Fn(&str) -> Option<u32>) -> Result<Vec<u8>> {
        if base % self.align != 0 || base < L2_BYTE_BASE {
            return Err(err!("DSP placement {base:#x}"));
        }
        let pc_shift = (base - L2_BYTE_BASE) / 2;
        let byte_shift = base - L2_BYTE_BASE;
        let mut m = self.image.clone();
        for r in &self.relocs {
            let at = r.offset as usize;
            let add = match r.class {
                CLASS_TEXT => pc_shift,
                CLASS_DATA => byte_shift,
                _ => resolve(&self.imports[r.import as usize]).ok_or_else(|| {
                    err!(
                        "DSP import {} is not provided",
                        self.imports[r.import as usize]
                    )
                })?,
            };
            let le16 = |m: &[u8], o: usize| u16::from_le_bytes([m[o], m[o + 1]]) as u32;
            if r.kind == KIND_ABS24 {
                let v = ((m[at + 2] as u32) << 16 | le16(&m, at + 4)).wrapping_add(add);
                if v >= 1 << 24 {
                    return Err(err!(
                        "DSP relocation at {at:#x}: {v:#x} does not fit 24 bits"
                    ));
                }
                m[at + 2] = (v >> 16) as u8;
                m[at + 4..at + 6].copy_from_slice(&(v as u16).to_le_bytes());
            } else {
                let v = (le16(&m, at + 2) << 16 | le16(&m, at + 4)).wrapping_add(add);
                m[at + 2..at + 4].copy_from_slice(&((v >> 16) as u16).to_le_bytes());
                m[at + 4..at + 6].copy_from_slice(&(v as u16).to_le_bytes());
            }
        }
        Ok(m)
    }

    /// The pc of the process routine of effect `i` placed at `base`.
    pub fn fx_pc(&self, i: usize, base: u32) -> u32 {
        L2_SW_BASE + (base - L2_BYTE_BASE + self.fxs[i].process) / 2
    }

    /// The pc of voice `i` placed at `base`.
    pub fn voice_pc(&self, i: usize, base: u32) -> u32 {
        L2_SW_BASE + (base - L2_BYTE_BASE + self.voices[i].entry) / 2
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dko::*;

    fn sec(name: &str, kind: &str, align: u32, bytes: &[u8]) -> DkoSection {
        DkoSection {
            name: name.into(),
            kind: kind.into(),
            align,
            size: bytes.len() as u32,
            hex: hex(bytes),
        }
    }
    fn sym(name: &str, section: Option<&str>, value: u64, global: bool) -> DkoSymbol {
        DkoSymbol {
            name: name.into(),
            section: section.map(String::from),
            value,
            global,
        }
    }
    fn rel(offset: u64, t: &str, symbol: &str, addend: i64) -> DkoReloc {
        DkoReloc {
            section: "t".into(),
            offset,
            rtype: t.into(),
            symbol: symbol.into(),
            addend,
        }
    }
    /// Three instructions (CALL local, load a data address, CALL an import) and a bss.
    fn object() -> Dko {
        let text = [0u8; 24];
        Dko {
            format: crate::dko::FORMAT,
            arch: "sharc".into(),
            source_sha256: "0".repeat(64),
            toolchain: Default::default(),
            sections: vec![
                sec("t", "text", 8, &text),
                sec("d", "data", 4, &[1, 2, 3, 4]),
                DkoSection {
                    name: "b".into(),
                    kind: "bss".into(),
                    align: 4,
                    size: 64,
                    hex: String::new(),
                },
            ],
            symbols: vec![
                sym("entry", Some("t"), 0, true),
                sym("again", Some("t"), 3, false),
                sym("var", Some("d"), 0, false),
                sym("buf", Some("b"), 8, true),
                sym("helper", None, 0, true),
            ],
            relocs: vec![
                rel(0, "sharc.abs24", "again", 0),
                rel(3, "sharc.abs32", "buf", 4),
                rel(6, "sharc.abs24", "helper", 0),
                rel(9, "sharc.abs32", "var", 0),
            ],
        }
    }

    fn bank_object() -> (Dko, crate::typed::SampleBankDef) {
        let mut o = object();
        o.sections[1] = sec("d", "data", 4, &[0; 16]);
        o.relocs.clear();
        (
            o,
            crate::typed::SampleBankDef {
                local_id: 0x12345678,
                bank: "buf".into(),
                descriptor: "var".into(),
                capacity_frames: 14,
            },
        )
    }

    fn tracks_object() -> (Dko, crate::typed::SampleTracksDef) {
        let mut o = object();
        o.sections[1] = sec("d", "data", 4, &[0; 768]);
        o.symbols.push(sym("ctl", Some("d"), 512, true));
        o.relocs.clear();
        (
            o,
            crate::typed::SampleTracksDef {
                local_id: 7,
                records: "var".into(),
                control: "ctl".into(),
            },
        )
    }

    #[test]
    fn sample_tracks_record_roundtrips_and_a_bank_record_stays_a_bank() {
        let (o, t) = tracks_object();
        let bytes = pack_with_samples(&o, 1, &[], &[], None, Some(&t)).unwrap();
        assert_eq!(get(&bytes, 4), VERSION_5);
        let at = get(&bytes, 0x3c) as usize;
        assert_eq!(&bytes[at..at + 8], &[2, 0, 0, 0, 7, 0, 0, 0]);
        let d = DspSection::parse(&bytes).unwrap();
        assert!(d.sample_bank.is_none());
        let tr = d.sample_tracks.as_ref().unwrap();
        assert_eq!(tr.control_byte_offset - tr.records_byte_offset, 512);
        assert_eq!(d.encode(), bytes);
        let (ob, b) = bank_object();
        let bank = pack_with_bank(&ob, 1, &[], &[], Some(&b)).unwrap();
        assert!(DspSection::parse(&bank).unwrap().sample_tracks.is_none());
        assert!(pack_with_samples(&ob, 1, &[], &[], Some(&b), Some(&t)).is_err());
    }

    #[test]
    fn sample_tracks_need_zero_data_inside_the_image_and_no_overlap() {
        let (o, t) = tracks_object();
        // a block that is not in an initialized data section, is too short, overlaps the other, or is not zero
        for (records, control) in [("buf", "ctl"), ("var", "buf"), ("var", "var"), ("helper", "ctl")] {
            let t = crate::typed::SampleTracksDef { records: records.into(), control: control.into(), ..t.clone() };
            assert!(pack_with_samples(&o, 1, &[], &[], None, Some(&t)).is_err(), "{records} {control}");
        }
        let mut bad = o.clone();
        bad.sections[1] = sec("d", "data", 4, &[0; 767]);
        assert!(pack_with_samples(&bad, 1, &[], &[], None, Some(&t)).is_err());
        let mut bad = o.clone();
        let mut bytes = [0u8; 768];
        bytes[700] = 1;
        bad.sections[1] = sec("d", "data", 4, &bytes);
        assert!(pack_with_samples(&bad, 1, &[], &[], None, Some(&t)).is_err());
        // the parser refuses reserved words, a relocation inside a block and a wrong kind
        let good = pack_with_samples(&o, 1, &[], &[], None, Some(&t)).unwrap();
        let at = get(&good, 0x3c) as usize;
        for (word, value) in [(0, 3u32), (2, 2), (3, u32::MAX - 3), (4, 1), (7, 1)] {
            let mut bad = good.clone();
            bad[at + word * 4..at + word * 4 + 4].copy_from_slice(&value.to_le_bytes());
            assert!(DspSection::parse(&bad).is_err(), "word {word}: {value}");
        }
        let mut d = DspSection::parse(&good).unwrap();
        d.relocs.push(Reloc {
            offset: d.sample_tracks.as_ref().unwrap().control_byte_offset + 250,
            kind: KIND_ABS32,
            class: CLASS_DATA,
            import: 0,
        });
        assert!(DspSection::parse(&d.encode()).is_err());
    }

    #[test]
    fn sample_bank_v5_roundtrips_little_endian_record_and_old_sections_stay_old() {
        let (o, b) = bank_object();
        let bytes = pack_with_bank(&o, 1, &[], &[], Some(&b)).unwrap();
        assert_eq!(get(&bytes, 4), VERSION_5);
        let at = get(&bytes, 0x3c) as usize;
        assert_eq!(&bytes[at..at + 8], &[1, 0, 0, 0, 0x78, 0x56, 0x34, 0x12]);
        let d = DspSection::parse(&bytes).unwrap();
        assert_eq!(d.sample_bank.as_ref().unwrap().capacity_frames, 14);
        assert_eq!(d.encode(), bytes);
        let old = pack(&o, 1, &[], &[]).unwrap();
        assert_eq!(get(&old, 4), VERSION);
        assert_eq!(get(&old, 0x3c), 0);
        assert!(DspSection::parse(&old).unwrap().sample_bank.is_none());
    }

    #[test]
    fn sample_bank_requires_owned_bss_and_initialized_zero_descriptor() {
        let (o, b) = bank_object();
        for (bank, desc, cap) in [
            ("helper", "var", 1),
            ("var", "var", 1),
            ("buf", "buf", 1),
            ("buf", "var", 15),
            ("buf", "var", 0),
            ("buf", "var", 8193),
        ] {
            let b = crate::typed::SampleBankDef {
                bank: bank.into(),
                descriptor: desc.into(),
                capacity_frames: cap,
                ..b.clone()
            };
            assert!(pack_with_bank(&o, 1, &[], &[], Some(&b)).is_err());
        }
        let mut bad = o.clone();
        bad.sections[1].hex = hex(&[1; 16]);
        assert!(pack_with_bank(&bad, 1, &[], &[], Some(&b)).is_err());
        let mut bad = o.clone();
        bad.symbols
            .iter_mut()
            .find(|s| s.name == "var")
            .unwrap()
            .value = 4;
        assert!(pack_with_bank(&bad, 1, &[], &[], Some(&b)).is_err());
        let mut bad = o;
        bad.symbols
            .iter_mut()
            .find(|s| s.name == "buf")
            .unwrap()
            .value = 9;
        assert!(pack_with_bank(&bad, 1, &[], &[], Some(&b)).is_err());
    }

    #[test]
    fn sample_bank_parser_rejects_malformed_records_bounds_and_relocation_overlap() {
        let (o, b) = bank_object();
        let good = pack_with_bank(&o, 1, &[], &[], Some(&b)).unwrap();
        let at = get(&good, 0x3c) as usize;
        for (word, value) in [
            (0, 2u32),
            (2, 0),
            (2, u32::MAX - 3),
            (3, 0),
            (3, 8193),
            (4, 1),
            (4, u32::MAX - 3),
            (5, 1),
            (6, 1),
            (7, 1),
        ] {
            let mut bad = good.clone();
            bad[at + word * 4..at + word * 4 + 4].copy_from_slice(&value.to_le_bytes());
            assert!(DspSection::parse(&bad).is_err(), "word {word}: {value}");
        }
        for off in [0, HEADER as u32 + 1, get(&good, 0x18), u32::MAX - 3] {
            let mut bad = good.clone();
            put(&mut bad, 0x3c, off);
            assert!(DspSection::parse(&bad).is_err());
        }
        let mut d = DspSection::parse(&good).unwrap();
        d.relocs.push(Reloc {
            offset: d.sample_bank.as_ref().unwrap().descriptor_byte_offset - 2,
            kind: KIND_ABS32,
            class: CLASS_DATA,
            import: 0,
        });
        assert!(DspSection::parse(&d.encode()).is_err());
        let mut d = DspSection::parse(&good).unwrap();
        d.bss = u32::MAX;
        assert!(DspSection::parse(&d.encode()).is_err());
    }

    #[test]
    fn relocation_at_any_base_equals_the_linker() {
        let o = object();
        let bytes = pack(
            &o,
            1,
            &[VoiceSpec::old(
                16,
                "entry".into(),
                [0, 0x7f00_0000, 0, 0, 0, 0, 0, 0],
            )],
            &[],
        )
        .unwrap();
        let d = DspSection::parse(&bytes).unwrap();
        assert_eq!(d.imports, vec!["helper"]);
        assert_eq!(d.bss, 64);
        assert_eq!(
            d.voices,
            vec![Voice {
                type_no: 16,
                entry: 0,
                note_on: 0,
                state_bytes: 0,
                clear_bytes: 0,
                voices: 0,
                ranges: [0, 0x7f00_0000, 0, 0, 0, 0, 0, 0]
            }]
        );
        for base in [0x2000_0000u32, 0x2001_ab40, 0x2003_f000] {
            let pc_of = |b: u32| L2_SW_BASE + (b - L2_BYTE_BASE) / 2;
            let ext = BTreeMap::from([("helper".to_string(), 0xb9_1234u64)]);
            let l = link::link(&o, base as u64, &ext).unwrap();
            let mem = d
                .relocated(base, &|n| (n == "helper").then_some(0xb9_1234))
                .unwrap();
            assert_eq!(mem, l.image, "base {base:#x}");
            assert_eq!(d.voice_pc(0, base), pc_of(base));
            assert_eq!(l.symbols["entry"] as u32, d.voice_pc(0, base));
        }
    }

    #[test]
    fn bad_objects_and_sections_are_refused() {
        let o = object();
        assert!(
            pack(&o, 1, &[VoiceSpec::old(16, "nope".into(), [0; 8])], &[])
                .unwrap_err()
                .0
                .contains("does not define")
        );
        assert!(
            pack(&o, 1, &[VoiceSpec::old(16, "again".into(), [0; 8])], &[])
                .unwrap_err()
                .0
                .contains("global text")
        );
        let mut c = object();
        c.relocs.push(rel(0, "sharc.abs24", "var", 0));
        assert!(pack(&c, 1, &[], &[]).unwrap_err().0.contains("24-bit"));
        let mut c = object();
        c.arch = "cf".into();
        assert!(pack(&c, 1, &[], &[]).is_err());
        let good = pack(&o, 1, &[], &[]).unwrap();
        let mut bad = good.clone();
        bad[0] ^= 1;
        assert!(DspSection::parse(&bad).is_err());
        // a placement that leaves a 24-bit field out of range
        let d = DspSection::parse(&good).unwrap();
        assert!(d.relocated(0x2000_0004, &|_| Some(0)).is_err());
        assert!(d.relocated(0x2000_0000, &|_| None).is_err());
        assert!(d.relocated(0x2000_0000, &|_| Some(0x100_0000)).is_err());
    }

    /// An effect: a process routine in the text, a `dk_fx_desc` (magic, contract 1, n_params, state_bytes, ...) in the data and a state-less bss.
    fn fx_object(n_params: u32, state_bytes: u32) -> Dko {
        let mut o = object();
        let words = [FX_DESC_MAGIC, 1, n_params, state_bytes, 200, 0, 0];
        let desc: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        o.sections[1] = sec("d", "data", 4, &desc);
        o.symbols.push(sym("dkdesc", Some("d"), 0, true));
        o.symbols.push(sym("proc", Some("t"), 3, true));
        o
    }

    fn fx_spec(defaults: Vec<u32>) -> FxSpec {
        FxSpec {
            process: "proc".into(),
            desc: "dkdesc".into(),
            defaults,
            routes: ROUTE_MASTER,
            route: ROUTE_MASTER,
        }
    }

    /// A contract machine: `render` and `note_on` in the text, a `dk_machine_desc` with the given contract word in the data.
    fn machine_object(contract: u32) -> Dko {
        let mut o = object();
        let words = [DESC_MAGIC, contract, 1, 1, 8, 8, 100, 100, 0, 0];
        let desc: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        o.sections[1] = sec("d", "data", 4, &desc);
        o.symbols.push(sym("mdesc", Some("d"), 0, true));
        o.symbols.push(sym("note", Some("t"), 3, true));
        o
    }

    fn machine_spec(stereo: bool) -> VoiceSpec {
        VoiceSpec {
            type_no: 1,
            render: "entry".into(),
            note_on: Some("note".into()),
            desc: Some("mdesc".into()),
            stereo,
            ranges: [0; 8],
        }
    }

    #[test]
    fn a_stereo_declaration_must_agree_with_the_contract_word_of_the_desc() {
        let mono = machine_object(DESC_CONTRACT);
        let stereo = machine_object(DESC_CONTRACT | DESC_STEREO);
        assert!(pack(&mono, 1, &[machine_spec(false)], &[]).is_ok());
        assert!(pack(&stereo, 1, &[machine_spec(true)], &[]).is_ok());
        let e = |o: &Dko, v: bool| pack(o, 1, &[machine_spec(v)], &[]).unwrap_err().0;
        assert!(e(&stereo, false).contains("stereo = 0"));
        assert!(e(&mono, true).contains("stereo = 1"));
        // another major number, or an unknown flag, is refused
        assert!(e(&machine_object(2), false).contains("contract 0x2"));
        assert!(e(&machine_object(DESC_CONTRACT | 0x200), false).contains("contract 0x201"));
    }

    #[test]
    fn the_routes_are_the_flags_of_the_record_and_zero_is_master_only() {
        let o = fx_object(1, 4);
        let flags = |routes, route| {
            let mut f = fx_spec(vec![]);
            f.routes = routes;
            f.route = route;
            let b = pack(&o, 1, &[], &[f]).unwrap();
            let d = DspSection::parse(&b).unwrap();
            let at = get(&b, 0x34) as usize + 16;
            assert_eq!((d.fxs[0].routes, d.fxs[0].route), (routes, route));
            get(&b, at)
        };
        // master only is the word 0 that every record before the routes has, so a module without `route` packs to the bytes it always did
        assert_eq!(flags(ROUTE_MASTER, ROUTE_MASTER), 0);
        assert_eq!(flags(ROUTE_MASTER | ROUTE_SEND, ROUTE_MASTER), FXF_SEND);
        assert_eq!(
            flags(ROUTE_MASTER | ROUTE_SEND, ROUTE_SEND),
            FXF_SEND | FXF_DEFAULT_SEND
        );
        assert_eq!(
            flags(ROUTE_SEND, ROUTE_SEND),
            FXF_SEND | FXF_NOMASTER | FXF_DEFAULT_SEND
        );
        // the packer refuses a default that is not allowed, and no route at all
        let e = |routes, route| {
            let mut f = fx_spec(vec![]);
            f.routes = routes;
            f.route = route;
            pack(&o, 1, &[], &[f]).unwrap_err().0
        };
        assert!(e(ROUTE_MASTER, ROUTE_SEND).contains("not one of the routes"));
        assert!(e(ROUTE_SEND, ROUTE_MASTER).contains("not one of the routes"));
        assert!(e(0, ROUTE_MASTER).contains("a mask"));
        assert!(e(4, ROUTE_MASTER).contains("a mask"));
        // the parser (the runtime's check) refuses a bit it does not know and a default outside the allowed routes
        let good = pack(&o, 1, &[], &[fx_spec(vec![])]).unwrap();
        let at = get(&good, 0x34) as usize + 16;
        for (word, ok) in [
            (0, true),
            (FXF_SEND, true),
            (FXF_SEND | FXF_DEFAULT_SEND, true),
            (FXF_SEND | FXF_NOMASTER | FXF_DEFAULT_SEND, true),
            (FXF_DEFAULT_SEND, false),
            (FXF_NOMASTER, false),
            (FXF_SEND | FXF_NOMASTER, false),
            (8, false),
        ] {
            let mut b = good.clone();
            put(&mut b, at, word);
            assert_eq!(DspSection::parse(&b).is_ok(), ok, "flags {word:#x}");
        }
    }

    #[test]
    fn an_effect_is_a_version_4_record_the_loader_can_place() {
        let o = fx_object(1, 4);
        let bytes = pack(&o, 1, &[], &[fx_spec(vec![0x3f00_0000])]).unwrap();
        assert_eq!(get(&bytes, 4), VERSION_4);
        let d = DspSection::parse(&bytes).unwrap();
        assert_eq!(d.voices, vec![]);
        assert_eq!(d.fxs.len(), 1);
        let e = &d.fxs[0];
        assert_eq!((e.state_bytes, e.n_params), (4, 1));
        assert_eq!(e.defaults, [0x3f00_0000, 0, 0, 0, 0, 0, 0, 0]);
        // the process is the pc of `proc` (text offset 6 = 3 short words), the desc the byte offset of its data in the image
        assert_eq!(e.process, 6);
        assert_eq!(d.fx_pc(0, 0x2000_0000), L2_SW_BASE + 3);
        assert_eq!(
            &d.image[e.desc as usize..e.desc as usize + 4],
            FX_DESC_MAGIC.to_le_bytes()
        );
        // the range holds the image (24 text + 28 data = 52 -> 56), the bss (64 -> 120) and the state (4 -> 8): 128, one granule
        assert_eq!(d.range_len(), 128);
        // the record is the only thing the section adds: without effects the section is version 2
        assert_eq!(get(&pack(&o, 1, &[], &[]).unwrap(), 4), VERSION);
        // and a section with an effect round-trips through encode
        assert_eq!(DspSection::parse(&d.encode()).unwrap(), d);
    }

    #[test]
    fn a_bad_effect_is_refused() {
        let e = |o: &Dko, f: FxSpec| pack(o, 1, &[], &[f]).unwrap_err().0;
        let o = fx_object(1, 4);
        let mut f = fx_spec(vec![]);
        f.process = "nope".into();
        assert!(e(&o, f).contains("does not define"));
        let mut f = fx_spec(vec![]);
        f.desc = "again".into();
        assert!(e(&o, f).contains("not a data symbol"));
        // more defaults than parameters, too many parameters, a state that is not a multiple of 4, another contract
        assert!(e(&o, fx_spec(vec![1, 2])).contains("default words"));
        assert!(e(&fx_object(9, 4), fx_spec(vec![])).contains("n_params 9"));
        assert!(e(&fx_object(1, 6), fx_spec(vec![])).contains("multiple of 4"));
        let mut c = fx_object(1, 4);
        let words = [FX_DESC_MAGIC, 2, 1, 4, 0, 0, 0];
        let desc: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        c.sections[1] = sec("d", "data", 4, &desc);
        assert!(e(&c, fx_spec(vec![])).contains("contract 2"));
        // a data symbol that is too short to be a descriptor
        let mut f = fx_spec(vec![]);
        f.desc = "var".into();
        let mut short = fx_object(1, 4);
        short.symbols.push(sym("var2", Some("d"), 24, true));
        f.desc = "var2".into();
        assert!(e(&short, f).contains("shorter"));
        // five effects are more than the chain
        let five = vec![fx_spec(vec![]); 5];
        assert!(pack(&o, 1, &[], &five).unwrap_err().0.contains("at most 4"));
        // a malformed section is refused by the parser
        let good = pack(&o, 1, &[], &[fx_spec(vec![])]).unwrap();
        let mut bad = good.clone();
        put(&mut bad, 0x38, 9);
        assert!(DspSection::parse(&bad).is_err());
        let mut bad = good.clone();
        put(&mut bad, 0x34, 0x7fff_fff0);
        assert!(DspSection::parse(&bad).is_err());
    }

    #[test]
    fn packing_is_deterministic() {
        let o = object();
        assert_eq!(
            pack(&o, 1, &[], &[]).unwrap(),
            pack(&o, 1, &[], &[]).unwrap()
        );
    }
}
