//! `.dkm`: a module file the digikit runtime loads from the card store at run time (docs/design/digikit-dkm.md).
//!
//! `pack` turns a ColdFire `.dko` object into the file; `Dkm::parse` reads and checks one; `Dkm::image` is the reference
//! loader (copy, relocate) that the runtime's own Rust loader (modules/core/runtime) must agree with. All integers are big
//! endian. The file holds no firmware address: absolute references are `base + offset` (kind 0) or a named import (kind 1).

use crate::dko::Dko;
use crate::error::*;
use crate::link;
use std::collections::{BTreeMap, BTreeSet};

pub const MAGIC: u32 = 0x444b_4d46; // "DKMF"
pub const FORMAT: u32 = 1;
pub const HEADER: usize = 0xa0;
/// The header of a module that exports `dkm_menu`: the menu address (image offset) follows the 0xa0 bytes.
pub const HEADER_MENU: usize = 0xa4;
pub const NONE: u32 = 0xffff_ffff;
pub const SLOTS: u32 = 8;
pub const NAME_LEN: usize = 32;
pub const IMPORT_LEN: usize = 32;
/// Largest module file the v0 runtime stages (its staging buffer): `[limits.max] stage_bytes` of sdk/sdk.toml.
pub fn max_file() -> usize {
    crate::sdkgen::limits_max().stage_bytes as usize
}
/// The runtime version this packer targets (`dkrt_sdk::RUNTIME_VERSION`), packed major << 16 | minor << 8 | patch, from sdk/sdk.toml.
pub fn runtime_version() -> u32 {
    crate::sdkgen::embedded_runtime()
}

/// A packed runtime version as "major.minor.patch" (a plain integer below 256 from an old runtime reads as 0.0.N).
pub fn fmt_runtime(v: u32) -> String {
    format!("{}.{}.{}", v >> 16, v >> 8 & 255, v & 255)
}

pub use dkneeds::Needs;

/// What a load of the file takes of each resource, inferred from the file's own contents (the register records, the DSP section and the
/// sizes the loader reads: `dkneeds`, the code the runtime's menu runs on the device). Nothing in the file declares it.
pub fn needs(f: &[u8]) -> Result<Needs> {
    dkneeds::needs_of(&mut &f[..]).map_err(|e| err!("{}", e.text()))
}

/// The needs of a file as JSON, for the command line and the wasm export: `file_size` and the CAPACITY names of [`Needs::fields`].
pub fn needs_json(f: &[u8]) -> Result<serde_json::Value> {
    let n = needs(f)?;
    let mut o = serde_json::Map::new();
    o.insert("file_size".into(), n.file_size.into());
    for (name, v) in n.fields() {
        o.insert(name.into(), v.into());
    }
    Ok(serde_json::Value::Object(o))
}

// header field offsets
pub const H_SIZE: usize = 0x08;
pub const H_HDRSIZE: usize = 0x0c;
pub const H_SHA: usize = 0x10;
pub const H_RUNTIME: usize = 0x30;
pub const H_VERSION: usize = 0x34;
/// 0x38..0x40: contract versions built against, (major, minor) of api, machine, fx, service; major 0 = not implemented.
pub const H_CONTRACT: usize = 0x38;
pub const H_NAME: usize = 0x40;
pub const H_IMAGE_OFF: usize = 0x60;
pub const H_IMAGE_SIZE: usize = 0x64;
pub const H_BSS: usize = 0x68;
pub const H_ALIGN: usize = 0x6c;
pub const H_RELOC_OFF: usize = 0x70;
pub const H_RELOC_N: usize = 0x74;
pub const H_IMPORT_OFF: usize = 0x78;
pub const H_IMPORT_N: usize = 0x7c;
pub const H_INIT: usize = 0x80;
pub const H_FINI: usize = 0x84;
pub const H_SLOT_OFF: usize = 0x88;
pub const H_SLOT_N: usize = 0x8c;
pub const H_REG_OFF: usize = 0x90;
pub const H_REG_SIZE: usize = 0x94;
pub const H_DSP_OFF: usize = 0x98;
pub const H_DSP_SIZE: usize = 0x9c;
pub const H_MENU: usize = 0xa0; // only when the header size is HEADER_MENU
/// Bit 31 of the word at [`H_RUNTIME`]: the module was built with `DK_TRACE`. The word is informational for the loader (it does not refuse
/// on it), so a runtime that does not know the bit loads the file as before; a version never reaches this bit.
pub const FLAG_TRACE: u32 = 1 << 31;
/// The runtime version that loads a DSP section (`dkrt_sdk::RUNTIME_VERSION` is at least this).
pub const RUNTIME_DSP: u32 = 5; // DSP sections of version 2 (voices with knob ranges, finding 51); a runtime 2 to 4 loads version 1 only

/// The newest minor of the machine contract that has no stereo machines (1.1: `track` appended to `voice_in`).
pub const MACHINE_MONO_MINOR: u8 = 1;
pub const MACHINE_STEREO_MINOR: u8 = 2;
pub const MACHINE_LATCHED_MINOR: u8 = 3;
pub const MACHINE_NATIVE_PAGE_MINOR: u8 = 4;
pub const MACHINE_NATIVE_PAGES_MINOR: u8 = 5;
/// A machine whose desc carries `DK_MACHINE_GATE` reads `gate` in `dk_voice_in` (1.6).
pub const MACHINE_GATE_MINOR: u8 = 6;
/// The newest minor of the Api contract before the per-track sample table (1.5). A module declares 1.5 only when its DSP section holds a
/// track table (`DK_SAMPLE_TRACKS`); every other module declares 1.4, so a runtime of Api 1.4 still accepts it.
pub const API_PRE_TRACKS_MINOR: u8 = 4;
/// The Api minor of the per-track sample table (1.5).
pub const API_TRACKS_MINOR: u8 = 5;

/// The Api minor a module declares: the lowest that has everything it needs. 1.4 for a module that needs nothing newer; 1.5 when its DSP
/// section holds a track table; the newest `since` of the entries its manifest lists in `uses_api` (`Options::api_minor`) when that is
/// higher. Never above the Api of the SDK this packer was built with.
pub fn api_minor(sdk_minor: u8, uses_tracks: bool, listed: u8) -> u8 {
    let base = if uses_tracks {
        API_TRACKS_MINOR
    } else {
        API_PRE_TRACKS_MINOR
    };
    base.max(listed).min(sdk_minor)
}

pub struct Options {
    pub name: String,
    /// major << 16 | minor << 8 | patch
    pub version: u32,
    pub runtime_min: u32,
    /// A traced build (`patchkit pack --trace`, DK_TRACE defined): sets [`FLAG_TRACE`].
    pub trace: bool,
    /// A stereo machine (`DK_MACHINE_STEREO`): the file declares machine contract 1.2. A mono machine declares 1.1, so a runtime of
    /// machine contract 1.1 still accepts it.
    pub stereo: bool,
    /// A machine that reads `gate` (`DK_MACHINE_GATE` in its desc): the file declares machine contract 1.6.
    pub gate: bool,
    /// The newest Api minor of the entries the manifest lists in `uses_api` (0: none); see [`api_minor`].
    pub api_minor: u8,
}

/// "1.2.3" -> 0x010203
pub fn parse_version(s: &str) -> Result<u32> {
    let p: Vec<&str> = s.split('.').collect();
    if p.len() != 3 {
        return Err(err!("version {s:?}: expected major.minor.patch"));
    }
    let mut v = 0u32;
    for x in p {
        let n: u32 = x.parse().map_err(|_| err!("version {s:?}: bad number"))?;
        if n > 255 {
            return Err(err!("version {s:?}: a part is over 255"));
        }
        v = v << 8 | n;
    }
    Ok(v)
}

fn put(b: &mut [u8], off: usize, v: u32) {
    b[off..off + 4].copy_from_slice(&v.to_be_bytes());
}
fn get(b: &[u8], off: usize) -> u32 {
    u32::from_be_bytes(b[off..off + 4].try_into().unwrap())
}
fn align_up(x: usize, a: usize) -> usize {
    x.div_ceil(a) * a
}

fn file_sha(file: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(&file[..H_SHA]);
    h.update([0u8; 32]);
    h.update(&file[H_SHA + 32..]);
    h.finalize().into()
}

fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() < NAME_LEN
        && n.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b'/'))
}

/// Pack a ColdFire object. Exports by name: `dkm_init`, `dkm_fini`, `dkm_slot_<n>` (n < 8). Every other undefined symbol is
/// an import resolved by the runtime's table at load time; only 32-bit absolute relocations may name one.
pub fn pack(obj: &Dko, o: &Options) -> Result<Vec<u8>> {
    pack_with(Some(obj), &[], o)
}

/// The image of a data-only module: `dkm_init` is `moveq #0,d0; rts`. The `register` records do the work.
const STUB_IMAGE: [u8; 4] = [0x70, 0x00, 0x4e, 0x75];

/// Do the `register` records hold a machine record? (A data-only machine module has no DSP section but is a machine.) Walks the records
/// by their kind and length words; a malformed table is not one (the loader refuses it with E_FORMAT).
fn has_machine_record(reg: &[u8]) -> bool {
    let w = |at: usize| -> Option<usize> {
        let b = reg.get(at..at + 4)?;
        Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
    };
    let mut at = crate::register::HEADER;
    while let (Some(kind), Some(len)) = (w(at), w(at + 4)) {
        if kind == crate::register::KIND_MACHINE as usize {
            return true;
        }
        if len < 8 {
            return false;
        }
        at += len;
    }
    false
}

/// The global symbols of a ColdFire object with their offsets in the loaded image (the offsets `pack` writes for `dkm_init` and the slots). The
/// register records that name a function (the click callbacks) need them before the file is packed.
pub fn image_symbols(obj: &Dko) -> Result<BTreeMap<String, u32>> {
    let defined: BTreeSet<&str> = obj
        .symbols
        .iter()
        .filter(|s| s.section.is_some())
        .map(|s| s.name.as_str())
        .collect();
    let externs: BTreeMap<String, u64> = obj
        .relocs
        .iter()
        .filter(|r| !defined.contains(r.symbol.as_str()))
        .map(|r| (r.symbol.clone(), 0))
        .collect();
    let linked = link::link(obj, 0, &externs)?;
    let len = linked.image.len() as u64;
    Ok(linked
        .globals
        .iter()
        .filter_map(|n| {
            let v = *linked.symbols.get(n)?;
            (v < len).then_some((n.clone(), v as u32))
        })
        .collect())
}

/// Pack an object (or none: a data-only module) with `register` records (see [`register`]).
pub fn pack_with(obj: Option<&Dko>, reg: &[u8], o: &Options) -> Result<Vec<u8>> {
    pack_full(obj, reg, &[], o)
}

/// Pack an object (or none), `register` records and a DSP section (see [`crate::dkmdsp`]; empty: none). A file with a DSP section
/// needs a runtime of version [`RUNTIME_DSP`] or later.
pub fn pack_full(obj: Option<&Dko>, reg: &[u8], dsp: &[u8], o: &Options) -> Result<Vec<u8>> {
    pack_full_max(obj, reg, dsp, o, max_file())
}

/// [`pack_full`] with another file size limit: `dkm_synth` makes files over the staging buffer on purpose (the capacity tests of the
/// runtime's `E_TOO_BIG`).
pub fn pack_full_max(
    obj: Option<&Dko>,
    reg: &[u8],
    dsp: &[u8],
    o: &Options,
    max_file: usize,
) -> Result<Vec<u8>> {
    if let Some(obj) = obj {
        obj.require_isa_checked()?;
    }
    let stub = Dko {
        format: crate::dko::FORMAT,
        arch: "cf".into(),
        source_sha256: "0".repeat(64),
        toolchain: Default::default(),
        sections: vec![crate::dko::DkoSection {
            name: ".text".into(),
            kind: "text".into(),
            align: 4,
            size: STUB_IMAGE.len() as u32,
            hex: hex(&STUB_IMAGE),
        }],
        symbols: vec![crate::dko::DkoSymbol {
            name: "dkm_init".into(),
            section: Some(".text".into()),
            value: 0,
            global: true,
        }],
        relocs: vec![],
    };
    let has_obj = obj.is_some();
    let obj = obj.unwrap_or(&stub);
    if obj.arch != "cf" {
        return Err(err!(
            "a .dkm holds ColdFire code; the object is {}",
            obj.arch
        ));
    }
    if !valid_name(&o.name) {
        return Err(err!(
            "module name {:?}: 1 to 31 characters of A-Z a-z 0-9 - _ . /",
            o.name
        ));
    }
    let defined: BTreeSet<&str> = obj
        .symbols
        .iter()
        .filter(|s| s.section.is_some())
        .map(|s| s.name.as_str())
        .collect();
    let mut imports: BTreeSet<&str> = BTreeSet::new();
    for r in &obj.relocs {
        if !defined.contains(r.symbol.as_str()) {
            if r.rtype != "cf.32" {
                return Err(err!(
                    "relocation {} against the import {} in {}: only cf.32 can name an import (a PC-relative reference \
                     to the firmware would depend on where the module is loaded)",
                    r.rtype,
                    r.symbol,
                    r.section
                ));
            }
            imports.insert(r.symbol.as_str());
        }
    }
    let imports: Vec<&str> = imports.into_iter().collect();
    for n in &imports {
        if n.len() >= IMPORT_LEN {
            return Err(err!("import name {n} is longer than {}", IMPORT_LEN - 1));
        }
    }
    // Link at base 0 with every import at 0: the linker leaves S + A in each word, which is exactly what the loader adds
    // the base (or the import's address) to. PC-relative references inside the module are final.
    let externs: BTreeMap<String, u64> = imports.iter().map(|n| (n.to_string(), 0)).collect();
    let linked = link::link(obj, 0, &externs)?;
    let image = linked.image.clone();
    let kind_of: BTreeMap<&str, &str> = obj
        .sections
        .iter()
        .map(|s| (s.name.as_str(), s.kind.as_str()))
        .collect();
    let mut relocs: Vec<(u32, u32)> = vec![];
    for r in &obj.relocs {
        let sec = kind_of
            .get(r.section.as_str())
            .ok_or_else(|| err!("reloc in unknown section {}", r.section))?;
        if *sec == "bss" {
            return Err(err!("relocation in bss section {}", r.section));
        }
        let off = linked.places[&r.section] + r.offset;
        if off + 4 > image.len() as u64 && r.rtype == "cf.32" {
            return Err(err!("relocation at {off:#x} is past the image"));
        }
        if let Some(i) = imports.iter().position(|n| *n == r.symbol) {
            relocs.push((off as u32, 1 << 24 | i as u32));
        } else if r.rtype == "cf.32" {
            relocs.push((off as u32, 0));
        }
    }
    relocs.sort();
    let sym = |n: &str| -> Result<Option<u32>> {
        match linked.symbols.get(n) {
            None => Ok(None),
            Some(v) if *v < image.len() as u64 => Ok(Some(*v as u32)),
            Some(v) => Err(err!("{n} = {v:#x} is outside the loaded image")),
        }
    };
    let init = sym("dkm_init")?;
    let fini = sym("dkm_fini")?;
    let menu = sym("dkm_menu")?;
    let mut slots: Vec<(u32, u32)> = vec![];
    for n in 0..SLOTS {
        if let Some(v) = sym(&format!("dkm_slot_{n}"))? {
            slots.push((n, v));
        }
    }
    for s in &obj.symbols {
        if let Some(n) = s.name.strip_prefix("dkm_slot_")
            && s.section.is_some()
            && n.parse::<u32>().map(|n| n >= SLOTS).unwrap_or(true)
        {
            return Err(err!("{}: slot numbers are 0 to {}", s.name, SLOTS - 1));
        }
    }
    if init.is_none() && fini.is_none() && slots.is_empty() && menu.is_none() {
        return Err(err!(
            "the object exports none of dkm_init, dkm_fini, dkm_slot_<n>, dkm_menu"
        ));
    }
    let align = obj
        .sections
        .iter()
        .filter(|s| s.kind != "bss" || s.size > 0)
        .map(|s| s.align)
        .max()
        .unwrap_or(4)
        .clamp(4, 16);
    if obj.sections.iter().any(|s| s.align > 16) {
        return Err(err!("a section needs alignment over 16 (the arena's)"));
    }

    let header = if menu.is_some() { HEADER_MENU } else { HEADER };
    let import_off = header;
    let reloc_off = import_off + imports.len() * IMPORT_LEN;
    let slot_off = reloc_off + relocs.len() * 8;
    let reg_off = slot_off + slots.len() * 8;
    let reg_size = reg.len();
    let dsp_off = align_up(reg_off + reg_size, 16);
    let image_off = align_up(dsp_off + dsp.len(), 16);
    let total = image_off + image.len();
    if total > max_file {
        return Err(err!(
            "the module file is {total} bytes; the runtime stages {max_file}"
        ));
    }
    let mut f = vec![0u8; total];
    put(&mut f, 0, MAGIC);
    put(&mut f, 4, FORMAT);
    put(&mut f, H_SIZE, total as u32);
    put(&mut f, H_HDRSIZE, header as u32);
    if let Some(m) = menu {
        put(&mut f, H_MENU, m);
    }
    put(
        &mut f,
        H_RUNTIME,
        (if dsp.is_empty() {
            o.runtime_min
        } else {
            o.runtime_min.max(RUNTIME_DSP)
        }) | if o.trace { FLAG_TRACE } else { 0 },
    );
    put(&mut f, H_VERSION, o.version);
    // contracts: api always; machine with a DSP section (that is not only effects) or a machine record; fx with an effect record; service with slot functions or a real `dkm_init`/`dkm_fini` (not the stub)
    let sdk = crate::sdkgen::embedded();
    let has_service =
        !slots.is_empty() || has_obj && (init.is_some() || fini.is_some() || menu.is_some());
    let none = (0, 0);
    // a DSP section of version 4 with effects and no voice implements the fx contract and not the machine contract (a machine needs a voice or a record)
    let word = |off: usize| {
        dsp.get(off..off + 4)
            .map_or(0, |b| u32::from_be_bytes(b.try_into().unwrap()))
    };
    let has_fx =
        matches!(word(4), crate::dkmdsp::VERSION_4 | crate::dkmdsp::VERSION_5) && word(0x38) > 0;
    let fx_only = has_fx && word(0x30) == 0;
    // a version 5 DSP section whose record has kind 2 holds the per-track sample table: the module uses Api 1.5
    let uses_tracks = word(4) == crate::dkmdsp::VERSION_5
        && dsp
            .get(word(0x3c) as usize..)
            .and_then(|b| b.get(..4))
            .is_some_and(|b| {
                u32::from_le_bytes(b.try_into().unwrap()) == crate::dkmdsp::SAMPLE_TRACKS_KIND
            });
    let uses_latched = matches!(
        word(4),
        crate::dkmdsp::VERSION_3 | crate::dkmdsp::VERSION_4 | crate::dkmdsp::VERSION_5
    ) && dsp.get(word(0x2c) as usize..).is_some_and(|voices| {
        voices
            .chunks_exact(crate::dkmdsp::VOICE_LEN_V3)
            .take(word(0x30) as usize)
            .any(|v| u32::from_be_bytes(v[56..60].try_into().unwrap()) != 0)
    });
    let pairs = [
        (sdk.api.0, api_minor(sdk.api.1, uses_tracks, o.api_minor)),
        if (dsp.is_empty() || fx_only) && !has_machine_record(reg) {
            none
        } else if o.gate {
            (sdk.machine.0, sdk.machine.1.min(MACHINE_GATE_MINOR))
        } else if !reg.is_empty()
            && crate::register::Register::parse(reg)?
                .native_pages
                .iter()
                .any(|p| p.version == 2)
        {
            (sdk.machine.0, sdk.machine.1.min(MACHINE_NATIVE_PAGES_MINOR))
        } else if !reg.is_empty()
            && !crate::register::Register::parse(reg)?
                .native_pages
                .is_empty()
        {
            (sdk.machine.0, sdk.machine.1.min(MACHINE_NATIVE_PAGE_MINOR))
        } else if uses_latched {
            (sdk.machine.0, sdk.machine.1.min(MACHINE_LATCHED_MINOR))
        } else if o.stereo {
            (sdk.machine.0, sdk.machine.1.min(MACHINE_STEREO_MINOR))
        } else {
            (sdk.machine.0, sdk.machine.1.min(MACHINE_MONO_MINOR))
        },
        if has_fx { sdk.fx } else { none },
        if has_service { sdk.service } else { none },
    ];
    for (i, (maj, min)) in pairs.iter().enumerate() {
        f[H_CONTRACT + 2 * i] = *maj;
        f[H_CONTRACT + 2 * i + 1] = *min;
    }
    f[H_NAME..H_NAME + o.name.len()].copy_from_slice(o.name.as_bytes());
    put(&mut f, H_IMAGE_OFF, image_off as u32);
    put(&mut f, H_IMAGE_SIZE, image.len() as u32);
    put(&mut f, H_BSS, linked.bss_size as u32);
    put(&mut f, H_ALIGN, align);
    put(&mut f, H_RELOC_OFF, reloc_off as u32);
    put(&mut f, H_RELOC_N, relocs.len() as u32);
    put(&mut f, H_IMPORT_OFF, import_off as u32);
    put(&mut f, H_IMPORT_N, imports.len() as u32);
    put(&mut f, H_INIT, init.unwrap_or(NONE));
    put(&mut f, H_FINI, fini.unwrap_or(NONE));
    put(&mut f, H_SLOT_OFF, slot_off as u32);
    put(&mut f, H_SLOT_N, slots.len() as u32);
    put(&mut f, H_REG_OFF, reg_off as u32);
    put(&mut f, H_REG_SIZE, reg_size as u32);
    for (i, n) in imports.iter().enumerate() {
        let at = import_off + i * IMPORT_LEN;
        f[at..at + n.len()].copy_from_slice(n.as_bytes());
    }
    for (i, (off, k)) in relocs.iter().enumerate() {
        put(&mut f, reloc_off + i * 8, *off);
        put(&mut f, reloc_off + i * 8 + 4, *k);
    }
    for (i, (n, v)) in slots.iter().enumerate() {
        put(&mut f, slot_off + i * 8, *n);
        put(&mut f, slot_off + i * 8 + 4, *v);
    }
    f[reg_off..reg_off + reg_size].copy_from_slice(reg);
    if !dsp.is_empty() {
        put(&mut f, H_DSP_OFF, dsp_off as u32);
        put(&mut f, H_DSP_SIZE, dsp.len() as u32);
        f[dsp_off..dsp_off + dsp.len()].copy_from_slice(dsp);
    }
    f[image_off..].copy_from_slice(&image);
    let sha = file_sha(&f);
    f[H_SHA..H_SHA + 32].copy_from_slice(&sha);
    Ok(f)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dkm {
    pub name: String,
    pub version: u32,
    pub runtime_min: u32,
    /// Built with `DK_TRACE` ([`FLAG_TRACE`]); not part of `runtime_min`.
    pub trace: bool,
    pub sha256: String,
    pub image: Vec<u8>,
    pub bss: u32,
    pub align: u32,
    /// (offset in the loaded memory, 0 = base-relative or 1 + import index)
    pub relocs: Vec<(u32, Option<usize>)>,
    pub imports: Vec<String>,
    pub init: Option<u32>,
    pub fini: Option<u32>,
    /// The image offset of `dkm_menu`.
    pub menu: Option<u32>,
    pub slots: Vec<(u32, u32)>,
    pub register: Vec<u8>,
    /// The DSP section, as bytes (empty: none); parse it with [`crate::dkmdsp::DspSection::parse`].
    pub dsp: Vec<u8>,
}

impl Dkm {
    /// The size breakdown of a file of `total` bytes: `<total> bytes (image <n>, bss <n>, reloc <n>, import <n>, registry <n>, dsp <n>)`.
    /// The file is the 0xa0-byte header, the import, reloc and slot tables, the registry, the DSP section and the image, in
    /// that order, with padding to 16 before the DSP section and the image. So `total` = header + slots (8 bytes each,
    /// not listed) + padding (under 32) + image + reloc (8 bytes each) + import (32 bytes each) + registry + dsp; `bss` is
    /// memory the loader adds, not file bytes.
    pub fn size_summary(&self, total: usize) -> String {
        format!(
            "{total} bytes (image {}, bss {}, reloc {}, import {}, registry {}, dsp {})",
            self.image.len(),
            self.bss,
            self.relocs.len() * 8,
            self.imports.len() * IMPORT_LEN,
            self.register.len(),
            self.dsp.len()
        )
    }

    /// Parse and check a file the way the runtime does: magic, sizes, bounds, sha256.
    pub fn parse(f: &[u8]) -> Result<Dkm> {
        if f.len() < HEADER || get(f, 0) != MAGIC {
            return Err(err!("not a .dkm file (magic)"));
        }
        if get(f, 4) != FORMAT {
            return Err(err!("format {} is not supported", get(f, 4)));
        }
        let header = get(f, H_HDRSIZE) as usize;
        if get(f, H_SIZE) as usize != f.len()
            || (header != HEADER && header != HEADER_MENU)
            || f.len() < header
        {
            return Err(err!("size fields do not match the file"));
        }
        if file_sha(f) != f[H_SHA..H_SHA + 32] {
            return Err(err!("sha256 mismatch"));
        }
        let rng = |off: usize, n: usize| -> Result<&[u8]> {
            f.get(off..off.checked_add(n).ok_or_else(|| err!("overflow"))?)
                .ok_or_else(|| err!("table at {off:#x}+{n:#x} is outside the file"))
        };
        let name_raw = &f[H_NAME..H_NAME + NAME_LEN];
        let nl = name_raw.iter().position(|b| *b == 0).unwrap_or(NAME_LEN);
        let name =
            String::from_utf8(name_raw[..nl].to_vec()).map_err(|_| err!("name is not UTF-8"))?;
        if !valid_name(&name) {
            return Err(err!("bad module name {name:?}"));
        }
        let image_size = get(f, H_IMAGE_SIZE) as usize;
        let image = rng(get(f, H_IMAGE_OFF) as usize, image_size)?.to_vec();
        let imp_n = get(f, H_IMPORT_N) as usize;
        let imp = rng(get(f, H_IMPORT_OFF) as usize, imp_n * IMPORT_LEN)?;
        let imports = (0..imp_n)
            .map(|i| {
                let e = &imp[i * IMPORT_LEN..(i + 1) * IMPORT_LEN];
                let l = e.iter().position(|b| *b == 0).unwrap_or(IMPORT_LEN);
                String::from_utf8_lossy(&e[..l]).into_owned()
            })
            .collect::<Vec<_>>();
        let rel_n = get(f, H_RELOC_N) as usize;
        let rel = rng(get(f, H_RELOC_OFF) as usize, rel_n * 8)?;
        let mut relocs = vec![];
        for i in 0..rel_n {
            let off = get(rel, i * 8);
            let k = get(rel, i * 8 + 4);
            if off as usize + 4 > image_size {
                return Err(err!("relocation at {off:#x} is outside the image"));
            }
            relocs.push((
                off,
                match k >> 24 {
                    0 => None,
                    1 if ((k & 0xff_ffff) as usize) < imports.len() => {
                        Some((k & 0xff_ffff) as usize)
                    }
                    _ => return Err(err!("bad relocation kind {k:#x}")),
                },
            ));
        }
        let slot_n = get(f, H_SLOT_N) as usize;
        let sl = rng(get(f, H_SLOT_OFF) as usize, slot_n * 8)?;
        let slots = (0..slot_n)
            .map(|i| (get(sl, i * 8), get(sl, i * 8 + 4)))
            .collect::<Vec<_>>();
        let opt = |v: u32| (v != NONE).then_some(v);
        Ok(Dkm {
            name,
            version: get(f, H_VERSION),
            runtime_min: get(f, H_RUNTIME) & !FLAG_TRACE,
            trace: get(f, H_RUNTIME) & FLAG_TRACE != 0,
            sha256: hex(&f[H_SHA..H_SHA + 32]),
            image,
            bss: get(f, H_BSS),
            align: get(f, H_ALIGN),
            relocs,
            imports,
            init: opt(get(f, H_INIT)),
            fini: opt(get(f, H_FINI)),
            menu: if header == HEADER_MENU {
                opt(get(f, H_MENU))
            } else {
                None
            },
            slots,
            register: rng(get(f, H_REG_OFF) as usize, get(f, H_REG_SIZE) as usize)?.to_vec(),
            dsp: rng(get(f, H_DSP_OFF) as usize, get(f, H_DSP_SIZE) as usize)?.to_vec(),
        })
    }

    /// The reference loader: the module's memory at `base` (image, then zeroed bss), imports resolved by `resolve`.
    pub fn memory(&self, base: u32, resolve: &dyn Fn(&str) -> Option<u32>) -> Result<Vec<u8>> {
        let mut m = self.image.clone();
        m.resize(self.image.len() + self.bss as usize, 0);
        for (off, k) in &self.relocs {
            let add = match k {
                None => base,
                Some(i) => resolve(&self.imports[*i])
                    .ok_or_else(|| err!("import {} is not provided", self.imports[*i]))?,
            };
            let at = *off as usize;
            let v = u32::from_be_bytes(m[at..at + 4].try_into().unwrap()).wrapping_add(add);
            m[at..at + 4].copy_from_slice(&v.to_be_bytes());
        }
        Ok(m)
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
    fn sym(name: &str, section: Option<&str>, value: u64) -> DkoSymbol {
        DkoSymbol {
            name: name.into(),
            section: section.map(String::from),
            value,
            global: true,
        }
    }
    fn rel(section: &str, offset: u64, t: &str, symbol: &str, addend: i64) -> DkoReloc {
        DkoReloc {
            section: section.into(),
            offset,
            rtype: t.into(),
            symbol: symbol.into(),
            addend,
        }
    }
    /// text: `jsr imp.l; jsr f.l; rts` plus a data pointer to bss and to an import with an addend. Carries the build's ISA check record.
    fn object() -> Dko {
        let mut o = unstamped();
        o.stamp_isa();
        o
    }
    fn unstamped() -> Dko {
        let text = [0x4e, 0xb9, 0, 0, 0, 0, 0x4e, 0xb9, 0, 0, 0, 0, 0x4e, 0x75];
        Dko {
            format: FORMAT_DKO,
            arch: "cf".into(),
            source_sha256: "0".repeat(64),
            toolchain: Default::default(),
            sections: vec![
                sec(".text", "text", 2, &text),
                sec(".data", "data", 4, &[0; 8]),
                DkoSection {
                    name: ".bss".into(),
                    kind: "bss".into(),
                    align: 4,
                    size: 12,
                    hex: String::new(),
                },
            ],
            symbols: vec![
                sym("dkm_init", Some(".text"), 12),
                sym("dkm_slot_0", Some(".text"), 6),
                sym("counter", Some(".bss"), 4),
                sym("fw_thing", None, 0),
            ],
            relocs: vec![
                rel(".text", 2, "cf.32", "fw_thing", 0),
                rel(".text", 8, "cf.32", "dkm_slot_0", 0),
                rel(".data", 0, "cf.32", "counter", 0),
                rel(".data", 4, "cf.32", "fw_thing", 0x10),
            ],
        }
    }
    const FORMAT_DKO: u32 = crate::dko::FORMAT;

    fn opts() -> Options {
        Options {
            name: "test/x".into(),
            version: 0x010203,
            runtime_min: 1,
            trace: false,
            stereo: false,
            gate: false,
            api_minor: 0,
        }
    }

    #[test]
    fn image_symbols_are_the_offsets_the_packer_writes_for_global_symbols() {
        let mut o = unstamped();
        let mut local = sym("local_fn", Some(".text"), 0);
        local.global = false;
        o.symbols.push(local);
        o.stamp_isa();
        let syms = image_symbols(&o).unwrap();
        let d = Dkm::parse(&pack(&o, &opts()).unwrap()).unwrap();
        assert_eq!(syms["dkm_init"], d.init.unwrap());
        assert_eq!(syms["dkm_slot_0"], d.slots[0].1);
        assert!(
            !syms.contains_key("local_fn"),
            "a static function has no symbol to call by"
        );
        assert!(
            !syms.contains_key("fw_thing"),
            "an import is not in the image"
        );
    }

    #[test]
    fn pack_parse_and_relocate_equal_the_linker() {
        let o = object();
        let f = pack(&o, &opts()).unwrap();
        let d = Dkm::parse(&f).unwrap();
        assert_eq!(d.name, "test/x");
        assert_eq!(d.imports, vec!["fw_thing"]);
        assert_eq!(d.init, Some(12));
        assert_eq!(d.slots, vec![(0, 6)]);
        assert_eq!(d.bss, 12);
        // the loader at any base equals the patch-time linker at that base with the import at its address
        for base in [0x47e0_0000u32, 0x47e1_2340] {
            let mem = d
                .memory(base, &|n| (n == "fw_thing").then_some(0x4012_deda))
                .unwrap();
            let ext = BTreeMap::from([("fw_thing".to_string(), 0x4012_dedau64)]);
            let l = link::link(&o, base as u64, &ext).unwrap();
            assert_eq!(&mem[..l.image.len()], &l.image[..]);
            assert!(mem[l.image.len()..].iter().all(|b| *b == 0));
            assert_eq!(mem.len() as u64, l.image.len() as u64 + l.bss_size);
        }
    }

    #[test]
    fn corruption_and_bad_inputs_are_refused() {
        let f = pack(&object(), &opts()).unwrap();
        let mut bad = f.clone();
        let n = bad.len();
        bad[n - 1] ^= 1;
        assert!(Dkm::parse(&bad).unwrap_err().0.contains("sha256"));
        let mut bad = f.clone();
        bad[0] ^= 1;
        assert!(Dkm::parse(&bad).unwrap_err().0.contains("magic"));
        let mut o = object();
        o.relocs.push(rel(".text", 2, "cf.pc32", "fw_thing", 0));
        o.stamp_isa();
        assert!(pack(&o, &opts()).unwrap_err().0.contains("PC-relative"));
        let mut o = object();
        o.symbols.push(sym("dkm_slot_9", Some(".text"), 6));
        o.stamp_isa();
        assert!(pack(&o, &opts()).unwrap_err().0.contains("slot numbers"));
        let mut o = Options {
            name: "bad name".into(),
            ..opts()
        };
        assert!(pack(&object(), &o).is_err());
        o.name = "x".repeat(32);
        assert!(pack(&object(), &o).is_err());
    }

    #[test]
    fn the_api_minor_is_the_lowest_that_has_what_the_module_needs() {
        // (sdk minor, track table, newest listed entry) -> declared minor
        assert_eq!(api_minor(6, false, 0), 4);
        assert_eq!(api_minor(6, true, 0), 5);
        assert_eq!(api_minor(6, false, 6), 6);
        assert_eq!(api_minor(6, true, 6), 6);
        assert_eq!(
            api_minor(6, false, 3),
            4,
            "an old entry never lowers the floor"
        );
        assert_eq!(api_minor(6, false, 5), 5);
        assert_eq!(
            api_minor(5, false, 6),
            5,
            "never above the SDK this packer was built with"
        );
        // the entries of the Api: the table of sdk.toml says when each came
        assert_eq!(crate::sdkgen::api_since("sample_pool_path").unwrap(), 6);
        assert_eq!(crate::sdkgen::api_since("sample_pool_add").unwrap(), 5);
        assert_eq!(crate::sdkgen::api_since("store_read").unwrap(), 0);
        assert!(crate::sdkgen::api_since("no_such_entry").is_err());
    }

    #[test]
    fn native_page_requires_machine_contract_1_4() {
        use crate::register::{Machine, NativePage, Register};
        let mut reg = Register {
            device: 1,
            ..Register::default()
        };
        reg.machines.push(Machine {
            type_no: 16,
            clone_of: 0,
            position: 0,
            own_dsp: 1,
            level: 10,
            knobs: [0; 8],
            name: "Native".into(),
            short: "NAT".into(),
        });
        let mut bindings = [[0; 3]; 8];
        bindings[0] = [2, 0, 2];
        reg.native_pages.push(NativePage {
            version: 1,
            page: 1,
            flags: 0,
            type_no: 16,
            draw: 2,
            event: 4,
            title: "Native".into(),
            bindings,
        });
        let file = pack_full(Some(&object()), &reg.encode(), &dsp_section(0, 1), &opts()).unwrap();
        assert_eq!(&file[H_CONTRACT + 2..H_CONTRACT + 4], &[1, 4]);
        needs(&file).expect("dkneeds accepts the native-page record");
    }

    #[test]
    fn replacement_pages_require_machine_contract_1_5() {
        use crate::register::{Machine, NativePage, Register};
        let mut reg = Register {
            device: 1,
            ..Register::default()
        };
        reg.machines.push(Machine {
            type_no: 16,
            clone_of: 0,
            position: 0,
            own_dsp: 1,
            level: 10,
            knobs: [0; 8],
            name: "Custom".into(),
            short: "CST".into(),
        });
        let mut bindings = [[0; 3]; 8];
        bindings[0] = [2, 0, 6];
        for page in 0..2 {
            reg.native_pages.push(NativePage {
                version: 2,
                page,
                flags: 0,
                type_no: 16,
                draw: 2,
                event: 4,
                title: "Custom".into(),
                bindings,
            });
        }
        let file = pack_full(Some(&object()), &reg.encode(), &dsp_section(0, 1), &opts()).unwrap();
        assert_eq!(&file[H_CONTRACT + 2..H_CONTRACT + 4], &[1, 5]);
        needs(&file).expect("replacement-page requirements are readable");
    }

    #[test]
    fn latched_voice_requires_machine_contract_1_3() {
        let mut section = crate::dkmdsp::DspSection::parse(&dsp_section(0, 1)).unwrap();
        section.voices[0].latched = 1;
        section.voices[0].note_on = 2;
        let dsp = section.encode();
        let decoded = crate::dkmdsp::DspSection::parse(&dsp).unwrap();
        assert_eq!(decoded.voices[0].latched, 1);
        for stereo in [false, true] {
            let options = Options { stereo, ..opts() };
            let file = pack_full(Some(&object()), &[], &dsp, &options).unwrap();
            assert_eq!(&file[H_CONTRACT + 2..H_CONTRACT + 4], &[1, 3]);
        }
    }

    #[test]
    fn the_header_carries_the_contract_versions() {
        // 0x38..0x40: api, machine, fx, service as (major, minor)
        let c = |f: &[u8]| f[H_CONTRACT..H_CONTRACT + 8].to_vec();
        // ColdFire code with slots and init, no DSP section: api and service
        let f = pack(&object(), &opts()).unwrap();
        assert_eq!(c(&f), vec![1, 4, 0, 0, 0, 0, 1, 0]);
        // with a DSP section: machine too (the section's bytes are not parsed by pack)
        let f = pack_full(Some(&object()), &[], &[0u8; 16], &opts()).unwrap();
        assert_eq!(c(&f), vec![1, 4, 1, 1, 0, 0, 1, 0]);
        // a stereo machine: contract 1.2
        let stereo = Options {
            stereo: true,
            ..opts()
        };
        let f = pack_full(Some(&object()), &[], &[0u8; 16], &stereo).unwrap();
        assert_eq!(c(&f), vec![1, 4, 1, 2, 0, 0, 1, 0]);
        // a machine that reads the gate: contract 1.6, above every other machine minor
        let gate = Options {
            gate: true,
            ..opts()
        };
        let f = pack_full(Some(&object()), &[], &[0u8; 16], &gate).unwrap();
        assert_eq!(c(&f), vec![1, 4, 1, 6, 0, 0, 1, 0]);
        let both = Options {
            gate: true,
            stereo: true,
            ..opts()
        };
        let f = pack_full(Some(&object()), &[], &[0u8; 16], &both).unwrap();
        assert_eq!(c(&f), vec![1, 4, 1, 6, 0, 0, 1, 0]);
        // a DSP section with a track table: Api 1.5, and nothing else changes
        let tracks = crate::dkmdsp::DspSection {
            sample_bank: None,
            sample_tracks: Some(crate::dkmdsp::SampleTracks {
                local_id: 0,
                records_byte_offset: 0,
                control_byte_offset: 512,
            }),
            device: 1,
            image: vec![0; 768],
            bss: 0,
            align: 16,
            imports: vec![],
            relocs: vec![],
            voices: vec![],
            fxs: vec![],
        };
        let f = pack_full(Some(&object()), &[], &tracks.encode(), &stereo).unwrap();
        assert_eq!(c(&f), vec![1, 5, 1, 2, 0, 0, 1, 0]);
        needs(&f).expect("dkneeds accepts the kind 2 record the packer wrote");
        // a manifest that lists an Api entry of 1.6 (`uses_api`): 1.6, with or without a table; a module of 1.4 stays 1.4 without it
        let uses6 = Options {
            api_minor: 6,
            ..opts()
        };
        let f = pack(&object(), &uses6).unwrap();
        assert_eq!(c(&f), vec![1, 6, 0, 0, 0, 0, 1, 0]);
        let f = pack_full(
            Some(&object()),
            &[],
            &tracks.encode(),
            &Options {
                api_minor: 6,
                ..stereo
            },
        )
        .unwrap();
        assert_eq!(c(&f), vec![1, 6, 1, 2, 0, 0, 1, 0]);
        // an entry of 1.3 does not lower what the table needs
        let f = pack_full(
            Some(&object()),
            &[],
            &tracks.encode(),
            &Options {
                api_minor: 3,
                stereo: true,
                ..opts()
            },
        )
        .unwrap();
        assert_eq!(c(&f), vec![1, 5, 1, 2, 0, 0, 1, 0]);
        // a data-only module with a DSP section: machine, no service
        let f = pack_full(None, &[], &[0u8; 16], &opts()).unwrap();
        assert_eq!(c(&f), vec![1, 4, 1, 1, 0, 0, 0, 0]);
        // a data-only module without DSP code: api only
        let f = pack_with(None, &[], &opts()).unwrap();
        assert_eq!(c(&f), vec![1, 4, 0, 0, 0, 0, 0, 0]);
        // register records that hold a machine: machine too (a data-only machine module); param and page records alone: not
        let rec = record;
        let table = |recs: &[Vec<u8>]| {
            let mut t = vec![0u8; crate::register::HEADER];
            t[0..4].copy_from_slice(&crate::register::MAGIC.to_be_bytes());
            t[4..8].copy_from_slice(&crate::register::VERSION.to_be_bytes());
            t[12..16].copy_from_slice(&(recs.len() as u32).to_be_bytes());
            for r in recs {
                t.extend(r);
            }
            t
        };
        let param = rec(crate::register::KIND_PARAM, crate::register::LEN_PARAM);
        let machine = rec(crate::register::KIND_MACHINE, crate::register::LEN_MACHINE);
        let f = pack_with(None, &table(&[param.clone(), machine]), &opts()).unwrap();
        assert_eq!(c(&f), vec![1, 4, 1, 1, 0, 0, 0, 0]);
        let f = pack_with(None, &table(&[param]), &opts()).unwrap();
        assert_eq!(c(&f), vec![1, 4, 0, 0, 0, 0, 0, 0]);
    }

    /// One register record of a kind and length. One-letter names: the packer validates the register section.
    fn record(kind: u32, len: usize) -> Vec<u8> {
        use crate::register::{KIND_MACHINE, KIND_PAGE, KIND_PARAM};
        let mut r = kind.to_be_bytes().to_vec();
        r.extend((len as u32).to_be_bytes());
        r.resize(len, 0);
        // Offsets of the text fields in the whole record, the 8-byte prefix included.
        let text: &[usize] = match kind {
            KIND_PARAM => &[32, 64],
            KIND_MACHINE => &[60, 84],
            KIND_PAGE => &[48, 72],
            _ => &[],
        };
        for &o in text {
            r[o] = b'x';
        }
        r
    }

    /// A DSP section of an 8-byte image with BSS bytes of bss and one version 3 voice with `voices` states of 20 bytes (0: version 2).
    fn dsp_section(bss: u32, voices: u32) -> Vec<u8> {
        crate::dkmdsp::DspSection {
            sample_bank: None,
            sample_tracks: None,
            device: 1,
            image: vec![0; 8],
            bss,
            align: 16,
            imports: vec![],
            relocs: vec![],
            voices: vec![crate::dkmdsp::Voice {
                type_no: 17,
                entry: 0,
                note_on: 0,
                state_bytes: if voices == 0 { 0 } else { 20 },
                clear_bytes: 0,
                voices,
                ranges: [0; 8],
                latched: 0,
            }],
            fxs: vec![],
        }
        .encode()
    }

    #[test]
    fn the_needs_are_inferred_from_the_packed_file() {
        // the file declares nothing: the header is the format 1 header
        let f = pack(&object(), &opts()).unwrap();
        assert_eq!(get(&f, 4), super::FORMAT);
        assert_eq!(get(&f, H_HDRSIZE) as usize, HEADER);
        // ColdFire only: the arena block of image + bss (12); no DSP, no records
        let d = Dkm::parse(&f).unwrap();
        let n = needs(&f).unwrap();
        assert_eq!(n.file_size as usize, f.len());
        assert_eq!(n.arena, (d.image.len() as u32 + 12).div_ceil(16) * 16 + 16);
        assert_eq!(n.arena % 16, 0);
        assert_eq!(
            Needs {
                file_size: n.file_size,
                arena: n.arena,
                ..Needs::default()
            },
            n
        );
        // a DSP section: image 8, bss 100, three states of 20 (rounded to 24): 8 + 100 -> 112, + 72 = 184, granule 192; the relocated image is a block of 8
        let f = pack_full(Some(&object()), &[], &dsp_section(100, 3), &opts()).unwrap();
        let n = needs(&f).unwrap();
        assert_eq!(
            (n.dsp_area, n.dsp_align, n.voice_rows, n.arena_xfer),
            (192, 16, 1, 32)
        );
        // `DspSection::range_len`, the packer's own account of the range, agrees
        let s = crate::dkmdsp::DspSection::parse(&Dkm::parse(&f).unwrap().dsp).unwrap();
        assert_eq!(s.range_len(), n.dsp_area);
        // without state (version 2) the voice adds nothing: 8 + 100 = 108 -> 112 -> granule 128
        let f = pack_full(Some(&object()), &[], &dsp_section(100, 0), &opts()).unwrap();
        assert_eq!(needs(&f).unwrap().dsp_area, 128);
        // register records: the counts of each kind
        let rec = record;
        let mut t = vec![0u8; crate::register::HEADER];
        t[0..4].copy_from_slice(&0x444b_5247u32.to_be_bytes());
        t[4..8].copy_from_slice(&1u32.to_be_bytes());
        t[12..16].copy_from_slice(&4u32.to_be_bytes());
        for r in [
            rec(crate::register::KIND_PARAM, crate::register::LEN_PARAM),
            rec(crate::register::KIND_PARAM, crate::register::LEN_PARAM),
            rec(crate::register::KIND_MACHINE, crate::register::LEN_MACHINE),
            rec(crate::register::KIND_PAGE, crate::register::LEN_PAGE),
        ] {
            t.extend(r);
        }
        let f = pack_with(None, &t, &opts()).unwrap();
        let n = needs(&f).unwrap();
        assert_eq!(
            (n.param_rows, n.machine_rows, n.page_rows, n.fx_slots),
            (2, 1, 1, 0)
        );
        // a file of another format has none, and the JSON names the CAPACITY kinds
        let mut g = f.clone();
        put(&mut g, 4, 2);
        assert!(needs(&g).is_err());
        let j = needs_json(&f).unwrap();
        assert_eq!(j["PARAM_ROWS"], 2);
        assert_eq!(j["file_size"], f.len());
    }

    #[test]
    fn dkneeds_reads_the_formats_the_packer_writes() {
        // dkneeds has its own copies of the constants of the files it reads (it has no dependency): they are the packer's
        assert_eq!(dkneeds::MAGIC, MAGIC);
        assert_eq!(dkneeds::FORMAT, super::FORMAT);
        assert_eq!(dkneeds::HEADER as usize, HEADER);
        assert_eq!(dkneeds::SLOTS, crate::dkm::SLOTS);
        for (a, b) in [
            (dkneeds::H_SIZE, H_SIZE),
            (dkneeds::H_HDRSIZE, H_HDRSIZE),
            (dkneeds::H_IMAGE_OFF, H_IMAGE_OFF),
            (dkneeds::H_IMAGE_SIZE, H_IMAGE_SIZE),
            (dkneeds::H_BSS, H_BSS),
            (dkneeds::H_ALIGN, H_ALIGN),
            (dkneeds::H_RELOC_OFF, H_RELOC_OFF),
            (dkneeds::H_RELOC_N, H_RELOC_N),
            (dkneeds::H_IMPORT_OFF, H_IMPORT_OFF),
            (dkneeds::H_IMPORT_N, H_IMPORT_N),
            (dkneeds::H_SLOT_OFF, H_SLOT_OFF),
            (dkneeds::H_SLOT_N, H_SLOT_N),
            (dkneeds::H_REG_OFF, H_REG_OFF),
            (dkneeds::H_REG_SIZE, H_REG_SIZE),
            (dkneeds::H_DSP_OFF, H_DSP_OFF),
            (dkneeds::H_DSP_SIZE, H_DSP_SIZE),
        ] {
            assert_eq!(a as usize, b);
        }
        use crate::register as r;
        assert_eq!(dkneeds::REG_HEADER as usize, r::HEADER);
        assert_eq!(dkneeds::KIND_PARAM, r::KIND_PARAM);
        assert_eq!(dkneeds::KIND_MACHINE, r::KIND_MACHINE);
        assert_eq!(dkneeds::KIND_PAGE, r::KIND_PAGE);
        assert_eq!(dkneeds::LEN_PARAM as usize, r::LEN_PARAM);
        assert_eq!(dkneeds::LEN_MACHINE as usize, r::LEN_MACHINE);
        assert_eq!(dkneeds::LEN_PAGE as usize, r::LEN_PAGE);
        assert_eq!(dkneeds::KIND_EFFECT, r::KIND_EFFECT);
        assert_eq!(dkneeds::LEN_EFFECT as usize, r::LEN_EFFECT);
        use crate::dkmdsp as d;
        assert_eq!(dkneeds::DSP_MAGIC, d::MAGIC);
        assert_eq!(dkneeds::DSP_HEADER as usize, d::HEADER);
    }

    #[test]
    fn packing_is_deterministic() {
        assert_eq!(
            pack(&object(), &opts()).unwrap(),
            pack(&object(), &opts()).unwrap()
        );
    }

    #[test]
    fn an_object_that_has_not_passed_the_isa_check_is_refused() {
        // no record: made by hand, or built before the record existed
        let e = pack(&unstamped(), &opts()).unwrap_err().0;
        assert!(e.contains("no ISA check record"), "{e}");
        // tampered: a byte of the code, a relocation, a symbol, a section's alignment changed after the build
        let mut o = object();
        let mut b = unhex(&o.sections[0].hex).unwrap();
        b[13] ^= 0x04; // `rts` becomes another word
        o.sections[0].hex = hex(&b);
        assert!(pack(&o, &opts()).unwrap_err().0.contains("does not match"));
        let mut o = object();
        o.relocs[0].addend = 4;
        assert!(pack(&o, &opts()).unwrap_err().0.contains("does not match"));
        let mut o = object();
        o.symbols[1].value = 12;
        assert!(pack(&o, &opts()).unwrap_err().0.contains("does not match"));
        let mut o = object();
        o.sections[1].align = 8;
        assert!(pack(&o, &opts()).unwrap_err().0.contains("does not match"));
        // a record copied from another object does not help
        let mut o = object();
        o.toolchain.insert(ISA_CHECK_KEY.into(), "0".repeat(64));
        assert!(pack(&o, &opts()).is_err());
        // the untouched object packs
        assert!(pack(&object(), &opts()).is_ok());
    }
}
