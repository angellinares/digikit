//! `.dkm`: a module file the digikit runtime loads from the card store at run time (docs/design/digikit-dkm.md).
//!
//! `pack` turns a ColdFire `.dko` object into the file; `Dkm::parse` reads and checks one; `Dkm::image` is the reference
//! loader (copy, relocate) that the runtime's own Rust loader (modules/core/runtime) must agree with. All integers are big
//! endian. The file holds no firmware address: absolute references are `base + offset` (kind 0) or a named import (kind 1).

use crate::dko::Dko;
use crate::error::*;
use crate::link;
use std::collections::{BTreeMap, BTreeSet};

// The layout numbers are `[format]` of sdk/sdk.toml, through `dkneeds` (the runtime reads the same constants).
pub const MAGIC: u32 = dkneeds::MAGIC;
pub const FORMAT: u32 = dkneeds::FORMAT;
/// The header size. The entry points are the `dk_module` record of the module (docs/plans/sdk.md section 5): `init`, `fini`, `menu` and `tick` are image
/// offsets in the header, [`NONE`] for a missing one. The runtime calls them with a `dk_module_ctx`.
pub const HEADER: usize = dkneeds::HEADER as usize;
pub const NONE: u32 = dkneeds::NONE;
pub const NAME_LEN: usize = dkneeds::NAME_LEN as usize;
pub const IMPORT_LEN: usize = dkneeds::CF_IMPORT_LEN as usize;
const CF_RELOC_LEN: usize = dkneeds::CF_RELOC_LEN as usize;
const CF_RELOC_OFFSET: usize = dkneeds::CF_RELOC_OFFSET as usize;
const CF_RELOC_KIND: usize = dkneeds::CF_RELOC_KIND as usize;
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
pub const H_MAGIC: usize = dkneeds::H_MAGIC as usize;
pub const H_FORMAT: usize = dkneeds::H_FORMAT as usize;
pub const H_SIZE: usize = dkneeds::H_SIZE as usize;
pub const H_HDRSIZE: usize = dkneeds::H_HDRSIZE as usize;
pub const H_SHA: usize = dkneeds::H_SHA as usize;
pub const H_RUNTIME: usize = dkneeds::H_RUNTIME as usize;
pub const H_VERSION: usize = dkneeds::H_VERSION as usize;
/// 0x38: the role of the file, [`ROLE_MODULE`] or [`ROLE_SERVICE`].
pub const H_ROLE: usize = dkneeds::H_ROLE as usize;
/// 0x3c..0x40: the ABI fingerprint of the object's `dk_svc_abi` data symbol (a service file built with `dkrt-svc-rt`); 0 when the object defines none, so a
/// module is byte-identical to the format before the word was used.
pub const H_SVC_ABI: usize = dkneeds::H_SVC_ABI as usize;
/// A content module (a `.dkm`).
pub const ROLE_MODULE: u32 = 0;
/// A service provider (a `.dks`): its table has `PROVIDES` and `EXPORTS`.
pub const ROLE_SERVICE: u32 = 1;
pub const H_NAME: usize = dkneeds::H_NAME as usize;
pub const H_IMAGE_OFF: usize = dkneeds::H_IMAGE_OFF as usize;
pub const H_IMAGE_SIZE: usize = dkneeds::H_IMAGE_SIZE as usize;
pub const H_BSS: usize = dkneeds::H_BSS as usize;
pub const H_ALIGN: usize = dkneeds::H_ALIGN as usize;
pub const H_RELOC_OFF: usize = dkneeds::H_RELOC_OFF as usize;
pub const H_RELOC_N: usize = dkneeds::H_RELOC_N as usize;
pub const H_IMPORT_OFF: usize = dkneeds::H_IMPORT_OFF as usize;
pub const H_IMPORT_N: usize = dkneeds::H_IMPORT_N as usize;
pub const H_INIT: usize = dkneeds::H_INIT as usize;
pub const H_FINI: usize = dkneeds::H_FINI as usize;
pub const H_MENU: usize = dkneeds::H_MENU as usize;
pub const H_TICK: usize = dkneeds::H_TICK as usize;
pub const H_REG_OFF: usize = dkneeds::H_REG_OFF as usize;
pub const H_REG_SIZE: usize = dkneeds::H_REG_SIZE as usize;
pub const H_DSP_OFF: usize = dkneeds::H_DSP_OFF as usize;
pub const H_DSP_SIZE: usize = dkneeds::H_DSP_SIZE as usize;
/// Bit 31 of the word at [`H_RUNTIME`]: the module was built with `DK_TRACE`. The word at [`H_RUNTIME`] is the runtime version the module needs
/// (the loader's rule: same major and minor, a patch at most the runtime's); a version never reaches this bit.
pub const FLAG_TRACE: u32 = 1 << 31;
/// The names of the fields of the `dk_module` record, in order (`sdk/include/dk/core/module.h`): the function offsets the packer reads from it.
const MODULE_FIELDS: [&str; 4] = ["init", "fini", "tick", "menu"];

/// The function offsets of the `dk_module` record at image offset `at`, by field (init, fini, tick, menu). A field is the offset of a function of the
/// image (a base-relative relocation puts it there) or NULL (no relocation, value 0). `relocs` are the module's relocations (image offset, kind).
fn module_record(image: &[u8], relocs: &[(u32, u32)], at: u32) -> Result<[Option<u32>; 4]> {
    let mut v = [None; 4];
    for (i, f) in v.iter_mut().enumerate() {
        let name = MODULE_FIELDS[i];
        let off = at as usize + 4 * i;
        let word = image
            .get(off..off + 4)
            .map(|b| u32::from_be_bytes(b.try_into().unwrap()))
            .ok_or_else(|| {
                err!(
                    "dk_module is shorter than its {} fields",
                    MODULE_FIELDS.len()
                )
            })?;
        match relocs.iter().find(|(o, _)| *o as usize == off) {
            Some((_, 0)) if (word as usize) < image.len() => *f = Some(word),
            Some((_, 0)) => {
                return Err(err!(
                    "dk_module.{name} = {word:#x} is outside the loaded image"
                ));
            }
            Some(_) => {
                return Err(err!(
                    "dk_module.{name} is an import: it must be a function of the module"
                ));
            }
            None if word == 0 => {}
            None => {
                return Err(err!(
                    "dk_module.{name} = {word:#x} is no function of the module (a function pointer carries a relocation)"
                ));
            }
        }
    }
    Ok(v)
}

/// The runtime version a module needs: the newest `since` of the service imports it links, packed major << 16 | minor << 8 | patch (0 when it links none).
pub fn needed_runtime<'a>(imports: impl IntoIterator<Item = &'a str>) -> u32 {
    let table = crate::sdkgen::import_infos();
    imports
        .into_iter()
        .filter_map(|n| table.get(n))
        .map(|i| i.since)
        .max()
        .unwrap_or(0)
}

/// The `NEEDS` of a module that links `imports`: one per service its imports name, the newest `since` among them, hard unless each of them has a
/// stub for an absent provider. Sorted by service name hash.
pub fn needs_of_imports<'a>(
    imports: impl IntoIterator<Item = &'a str>,
) -> Vec<dkneeds::keys::Need> {
    let table = crate::sdkgen::import_infos();
    let mut by: BTreeMap<u32, dkneeds::keys::Need> = BTreeMap::new();
    for n in imports {
        if let Some(i) = table.get(n) {
            let h = dkneeds::keys::name_hash(&i.service);
            let e = by.entry(h).or_insert(dkneeds::keys::Need {
                service: h,
                min_version: 0,
                hard: false,
            });
            e.min_version = e.min_version.max(i.since);
            e.hard |= !i.optional;
        }
    }
    by.into_values().collect()
}

/// A hard NEED on the owning service of each record kind in `kinds` (the kinds of a module's `KEYS`, `[records.kinds] service`), only when that service has no
/// `rust` row, so a `.dks` provides it. A service in the runtime image is always there: modules whose kinds it owns stay byte-identical until it moves out.
/// The needs stay sorted by service name hash; a kind that is no record kind adds none.
fn record_kind_needs(needs: &mut Vec<dkneeds::keys::Need>, kinds: &[u32]) {
    let providers = crate::sdkgen::provider_services();
    for kind in kinds {
        let Some((_, _, service)) = dkneeds::KINDS.iter().find(|k| k.0 == *kind) else {
            continue;
        };
        if !providers.contains(*service) {
            continue;
        }
        let h = dkneeds::keys::name_hash(service);
        match needs.iter_mut().find(|n| n.service == h) {
            Some(n) => n.hard = true,
            None => needs.push(dkneeds::keys::Need {
                service: h,
                min_version: 0,
                hard: true,
            }),
        }
    }
    needs.sort_by_key(|n| n.service);
}

/// The `KINDS` of a service file: each record kind `service` owns (`[records.kinds]`) with `init` as the handler entry. The `service!` macro of `dkrt-svc-rt`
/// declares every kind with the address of `init`, and the loader compares the declarations with this table.
fn service_kinds(service: &str, init: u32) -> Vec<dkneeds::keys::KindDecl> {
    dkneeds::KINDS
        .iter()
        .filter(|k| k.2 == service)
        .map(|k| dkneeds::keys::KindDecl {
            kind: k.0,
            len: k.1,
            entry: init,
        })
        .collect()
}

/// The header table to write: `reg` is the table the caller built (empty: a module that takes and keeps nothing) with the `NEEDS` entry replaced by
/// the needs of `imports`.
///
/// `provides` and `exports` are added to the ones of `reg`; a table with any `PROVIDES` is written as version 2.
fn table_with_needs(
    reg: &[u8],
    imports: &[&str],
    provides: &[dkneeds::keys::Pair],
    exports_add: &[dkneeds::keys::Pair],
    hooks_add: &[dkneeds::keys::Pair],
    slots_add: &[dkneeds::keys::Slot],
    kinds_add: &[dkneeds::keys::KindDecl],
) -> Result<Vec<u8>> {
    use dkneeds::keys::{Bind, KeyGroup, Keys, Pair, Table, write_table};
    let keys = if reg.is_empty() {
        None
    } else {
        Some(Keys::parse(reg).map_err(|e| err!("header table: {e:?}"))?)
    };
    let mut needs = needs_of_imports(imports.iter().copied());
    if let Some(k) = &keys {
        let kinds: Vec<u32> = k.key_groups().map(|g| g.0).collect();
        record_kind_needs(&mut needs, &kinds);
    }
    let (mut uses, mut kinds, mut slots, mut hooks, mut exports, mut resources) =
        (vec![], vec![], vec![], vec![], vec![], vec![]);
    let mut groups: Vec<(u32, Vec<u32>)> = vec![];
    let mut provided: Vec<Pair> = vec![];
    let mut binds: Vec<Bind> = vec![];
    if let Some(k) = &keys {
        provided.extend(k.provides().map(|(a, b)| Pair { a, b }));
        binds.extend((0..k.bind_count()).filter_map(|i| k.bind(i)).map(|b| Bind {
            site: b.site,
            addr: b.addr,
            tramp: b.tramp,
            stock: b.stock,
        }));
        uses.extend(k.uses());
        kinds.extend(k.kinds());
        slots.extend(k.slots());
        hooks.extend(k.hooks());
        exports.extend(k.exports());
        resources.extend(k.resources());
        for (kind, ks) in k.key_groups() {
            groups.push((kind, ks.collect()));
        }
    }
    exports.extend_from_slice(exports_add);
    hooks.extend_from_slice(hooks_add);
    slots.extend_from_slice(slots_add);
    kinds.extend_from_slice(kinds_add);
    provided.extend_from_slice(provides);
    let key_groups: Vec<KeyGroup> = groups
        .iter()
        .map(|(kind, keys)| KeyGroup { kind: *kind, keys })
        .collect();
    let t = Table {
        provides: &provided,
        bind: &binds,
        needs: &needs,
        uses: &uses,
        keys: &key_groups,
        kinds: &kinds,
        slots: &slots,
        hooks: &hooks,
        exports: &exports,
        resources: &resources,
    };
    let mut out = vec![0u8; dkneeds::keys::table_size(&t)];
    write_table(&t, &mut out).map_err(|e| err!("header table: {e:?}"))?;
    Ok(out)
}

/// The services the imports name (`service` of `[[import]]`), each with the imports of it, sorted; a compiler helper names none.
pub fn services_of<'a>(
    imports: impl IntoIterator<Item = &'a str>,
) -> BTreeMap<String, Vec<String>> {
    let table = crate::sdkgen::import_infos();
    let mut v: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for n in imports {
        if let Some(i) = table.get(n) {
            v.entry(i.service.clone()).or_default().push(n.to_string());
        }
    }
    v
}

pub struct Options {
    pub name: String,
    /// major << 16 | minor << 8 | patch
    pub version: u32,
    /// A traced build (`patchkit pack --trace`, DK_TRACE defined): sets [`FLAG_TRACE`].
    pub trace: bool,
    /// A stereo machine (`DK_MACHINE_STEREO`); informational for tools.
    pub stereo: bool,
    /// A machine that reads `gate` (`DK_MACHINE_GATE` in its desc); informational for tools.
    pub gate: bool,
    /// A service provider (`role = "service"`): the file gets [`ROLE_SERVICE`] and a table with its `PROVIDES` and `EXPORTS`. `None`: a content module.
    pub service: Option<Provided>,
}

/// The low 24 bits of a `HOOKS` entry of a service file: the handler's image offset. The top byte is the device mask of the hook.
pub const HOOK_ENTRY_MASK: u32 = 0x00ff_ffff;

/// The device mask of a hook: bit `i` is the device `i` of `devdata::all()` (file-name order); no device is mask 0, which means every device. The packer writes it
/// in the top byte of the hook's `HOOKS` entry and `patchkit bind` reads it (and writes the bound file's entry without it), so a hook for the device of the firmware
/// that the port lacks is refused wherever bind runs.
pub fn hook_device_mask(devices: &[String]) -> Result<u32> {
    let all = crate::devdata::all();
    let mut mask = 0u32;
    for d in devices {
        let i = all.iter().position(|x| x.short == *d).ok_or_else(|| {
            err!(
                "hook device {d:?} is not one of {}",
                crate::devdata::short_list()
            )
        })?;
        if i >= 8 {
            return Err(err!("hook device {d:?}: the device mask has 8 bits"));
        }
        mask |= 1 << i;
    }
    Ok(mask)
}

/// What a service file provides: the service name (`[module] service`) and the functions it exports (`[[export]]`), each a global function of the object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provided {
    pub service: String,
    pub exports: Vec<String>,
    /// The hooks the service takes (`[[hook]]`): (hook name of the port, global function of the object that handles it, devices it is for; empty: every
    /// device). A `patchkit bind` of the file gives each a trampoline; the devices are the device mask in the top byte of the `HOOKS` entry
    /// ([`hook_device_mask`]).
    pub hooks: Vec<(String, String, Vec<String>)>,
    /// The pointer slots the service fills (`[[fill]]`): (slot name of the image adapter, global function of the object whose address goes in the slot).
    /// A `SLOTS` item of the table is the name hash and the entry offset. The third item lists the devices the fill is for (empty: every device); [`Provided::for_device`]
    /// drops the others, because a slot that the image of another device lacks is an unknown slot to the loader.
    pub fills: Vec<(String, String, Vec<String>)>,
}

impl Provided {
    /// The service as packed for `device`: the fills limited to other devices are left out.
    pub fn for_device(mut self, device: &str) -> Provided {
        self.fills
            .retain(|(_, _, devs)| devs.is_empty() || devs.iter().any(|d| d == device));
        self
    }
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

/// Pack a ColdFire object. The entry points are the fields of its `dk_module` constant. Every undefined symbol is an import resolved by the
/// runtime's table at load time; only 32-bit absolute relocations may name one.
pub fn pack(obj: &Dko, o: &Options) -> Result<Vec<u8>> {
    pack_inner(Some(obj), false, &[], &[], o, max_file())
}

/// The image of a data-only module: `init` is `moveq #0,d0; rts`. The `register` records do the work.
const STUB_IMAGE: [u8; 4] = [0x70, 0x00, 0x4e, 0x75];

/// The global symbols of a ColdFire object with their offsets in the loaded image. The
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

/// [`pack_keys`] with another file size limit: `dkm_synth` makes files over the staging buffer on purpose (the capacity tests of the
/// runtime's `E_TOO_BIG`).
pub fn pack_full_max(
    obj: Option<&Dko>,
    keys: &[u8],
    dsp: &[u8],
    o: &Options,
    max_file: usize,
) -> Result<Vec<u8>> {
    pack_inner(obj, obj.is_none(), keys, dsp, o, max_file)
}

/// Pack an object (or none), the keys table and a DSP section (see [`crate::dkmdsp`]; empty: none). The table at the header's 0x90 pair is the
/// keys table (`dkneeds::keys`, from `dkneeds::keys::write_keys`; size 0 for a module with no machine and no effect). The module declares its
/// machines, pages and parameters with builder calls in `init`.
pub fn pack_keys(obj: Option<&Dko>, keys: &[u8], dsp: &[u8], o: &Options) -> Result<Vec<u8>> {
    pack_inner(obj, obj.is_none(), keys, dsp, o, max_file())
}

/// [`pack_full_max`] for an object that is only data: its `.text` starts with the stub (`moveq #0,d0; rts`) that is `init`, there is no `dk_module`, and
/// the other sections are padding and bss. `dkm_synth` makes the capacity-test modules this way, so that a file is exactly header, stub and padding.
pub fn pack_data_only_max(
    obj: &Dko,
    keys: &[u8],
    dsp: &[u8],
    o: &Options,
    max_file: usize,
) -> Result<Vec<u8>> {
    pack_inner(Some(obj), true, keys, dsp, o, max_file)
}

fn pack_inner(
    obj: Option<&Dko>,
    data_only: bool,
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
            name: "dk_init_stub".into(),
            section: Some(".text".into()),
            value: 0,
            global: true,
        }],
        relocs: vec![],
    };
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
    // the old entry is gone: refuse its symbols with a pointer to the new one (a received source module may still have them)
    for s in &obj.symbols {
        if s.section.is_some()
            && (matches!(s.name.as_str(), "dkm_init" | "dkm_fini" | "dkm_menu")
                || s.name.starts_with("dkm_slot_"))
        {
            return Err(err!(
                "{}: the entry points dkm_init, dkm_fini, dkm_menu and dkm_slot_<n> are gone: define the constant dk_module (docs/plans/sdk.md section 5)",
                s.name
            ));
        }
    }
    // the entry point: the constant `dk_module` (init, fini, tick, menu); a data-only module has the stub's init
    let [init, fini, tick, menu] = if !data_only {
        let at = sym("dk_module")?
            .ok_or_else(|| err!("the object defines no dk_module (docs/plans/sdk.md section 5)"))?;
        module_record(&image, &relocs, at)?
    } else {
        [Some(0), None, None, None]
    };
    // the ABI fingerprint of a service built with `dkrt-svc-rt`: the value of the data symbol `dk_svc_abi` (a big-endian word in the image)
    let svc_abi = match sym("dk_svc_abi")? {
        None => 0,
        Some(at) => {
            let at = at as usize;
            match image.get(at..at + 4) {
                Some(w) => u32::from_be_bytes([w[0], w[1], w[2], w[3]]),
                None => return Err(err!("dk_svc_abi is not a whole word of the image")),
            }
        }
    };
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

    // the header table: the packer's uses and keys (none: a module that keeps nothing) and the needs of the imports the module links
    let (provides, svc_exports, svc_hooks, svc_slots, svc_kinds) = match &o.service {
        None => (vec![], vec![], vec![], vec![], vec![]),
        Some(s) => {
            let mut hk: Vec<dkneeds::keys::Pair> = vec![];
            for (hook, entry, devices) in &s.hooks {
                if !linked.globals.iter().any(|g| g == entry) {
                    return Err(err!(
                        "the service hooks {hook} with {entry}, which is no global symbol of the object"
                    ));
                }
                let at = sym(entry)?.ok_or_else(|| {
                    err!("the service hooks {hook} with {entry}, which the object does not define")
                })?;
                let a = dkneeds::keys::name_hash(hook);
                if hk.iter().any(|p| p.a == a) {
                    return Err(err!("the service hooks {hook} twice"));
                }
                if at > HOOK_ENTRY_MASK {
                    return Err(err!(
                        "the service hooks {hook} with {entry} at image offset {at:#x}, over the 24 bits of a HOOKS entry"
                    ));
                }
                hk.push(dkneeds::keys::Pair {
                    a,
                    b: at | hook_device_mask(devices)? << 24,
                });
            }
            hk.sort_by_key(|p| p.a);
            let mut sl: Vec<dkneeds::keys::Slot> = vec![];
            for (slot, entry, _) in &s.fills {
                let at = sym(entry)?.ok_or_else(|| {
                    err!("the service fills {slot} with {entry}, which the object does not define")
                })?;
                let id = dkneeds::keys::name_hash(slot);
                if sl.iter().any(|p| p.id == id) {
                    return Err(err!("the service fills {slot} twice"));
                }
                sl.push(dkneeds::keys::Slot {
                    id,
                    flags: 0,
                    entry: at,
                });
            }
            sl.sort_by_key(|p| p.id);
            let mut ex: Vec<dkneeds::keys::Pair> = vec![];
            for n in &s.exports {
                let at = sym(n)?.ok_or_else(|| {
                    err!("the service exports {n}, which the object does not define")
                })?;
                let a = dkneeds::keys::name_hash(n);
                if ex.iter().any(|p| p.a == a) {
                    return Err(err!("the service exports {n} twice"));
                }
                ex.push(dkneeds::keys::Pair { a, b: at });
            }
            ex.sort_by_key(|p| p.a);
            (
                vec![dkneeds::keys::Pair {
                    a: dkneeds::keys::name_hash(&s.service),
                    b: o.version,
                }],
                ex,
                hk,
                sl,
                service_kinds(&s.service, init.unwrap_or(0)),
            )
        }
    };
    let table = table_with_needs(
        reg,
        &imports,
        &provides,
        &svc_exports,
        &svc_hooks,
        &svc_slots,
        &svc_kinds,
    )?;
    emit(&EmitParts {
        name: &o.name,
        version: o.version,
        role: if o.service.is_some() {
            ROLE_SERVICE
        } else {
            ROLE_MODULE
        },
        // the runtime version the module needs: the newest `since` of its service imports, at least the version that introduced the header table,
        // and the trace flag
        runtime_word: needed_runtime(imports.iter().copied()).max(crate::sdkgen::table_runtime())
            | if o.trace { FLAG_TRACE } else { 0 },
        svc_abi,
        image: &image,
        bss: linked.bss_size as u32,
        align,
        imports: &imports,
        relocs: &relocs,
        entries: [init, fini, tick, menu],
        table: &table,
        dsp,
        max_file,
    })
}

/// What [`emit`] lays out as a file.
pub(crate) struct EmitParts<'a> {
    pub name: &'a str,
    pub version: u32,
    /// [`ROLE_MODULE`] or [`ROLE_SERVICE`], the word at [`H_ROLE`].
    pub role: u32,
    /// The word at [`H_RUNTIME`]: the runtime version the module needs and the trace flag.
    pub runtime_word: u32,
    /// The word at [`H_SVC_ABI`]: the service ABI fingerprint of a service (0 for a module).
    pub svc_abi: u32,
    pub image: &'a [u8],
    pub bss: u32,
    pub align: u32,
    pub imports: &'a [&'a str],
    /// (image offset, 0 = base-relative or 1 << 24 | import index), sorted.
    pub relocs: &'a [(u32, u32)],
    /// The image offsets of `init`, `fini`, `tick` and `menu`.
    pub entries: [Option<u32>; 4],
    /// The header table, already written.
    pub table: &'a [u8],
    /// The DSP section (empty: none).
    pub dsp: &'a [u8],
    pub max_file: usize,
}

/// The file layout: header, imports, relocations, header table, DSP section, image, with the file hash. 0x3c..0x40 stay zero.
pub(crate) fn emit(p: &EmitParts) -> Result<Vec<u8>> {
    let [init, fini, tick, menu] = p.entries;
    let (imports, relocs, reg, dsp, image) = (p.imports, p.relocs, p.table, p.dsp, p.image);
    let import_off = HEADER;
    let reloc_off = import_off + imports.len() * IMPORT_LEN;
    let reg_off = reloc_off + relocs.len() * 8;
    let reg_size = reg.len();
    let dsp_off = align_up(reg_off + reg_size, 16);
    let image_off = align_up(dsp_off + dsp.len(), 16);
    let total = image_off + image.len();
    if total > p.max_file {
        return Err(err!(
            "the module file is {total} bytes; the runtime stages {}",
            p.max_file
        ));
    }
    let mut f = vec![0u8; total];
    put(&mut f, H_MAGIC, MAGIC);
    put(&mut f, H_FORMAT, FORMAT);
    put(&mut f, H_SIZE, total as u32);
    put(&mut f, H_HDRSIZE, HEADER as u32);
    put(&mut f, H_RUNTIME, p.runtime_word);
    put(&mut f, H_VERSION, p.version);
    put(&mut f, H_SVC_ABI, p.svc_abi);
    put(&mut f, H_ROLE, p.role);
    f[H_NAME..H_NAME + p.name.len()].copy_from_slice(p.name.as_bytes());
    put(&mut f, H_IMAGE_OFF, image_off as u32);
    put(&mut f, H_IMAGE_SIZE, image.len() as u32);
    put(&mut f, H_BSS, p.bss);
    put(&mut f, H_ALIGN, p.align);
    put(&mut f, H_RELOC_OFF, reloc_off as u32);
    put(&mut f, H_RELOC_N, relocs.len() as u32);
    put(&mut f, H_IMPORT_OFF, import_off as u32);
    put(&mut f, H_IMPORT_N, imports.len() as u32);
    put(&mut f, H_INIT, init.unwrap_or(NONE));
    put(&mut f, H_FINI, fini.unwrap_or(NONE));
    put(&mut f, H_MENU, menu.unwrap_or(NONE));
    put(&mut f, H_TICK, tick.unwrap_or(NONE));
    put(&mut f, H_REG_OFF, reg_off as u32);
    put(&mut f, H_REG_SIZE, reg_size as u32);
    for (i, n) in imports.iter().enumerate() {
        let at = import_off + i * IMPORT_LEN;
        f[at..at + n.len()].copy_from_slice(n.as_bytes());
    }
    for (i, (off, k)) in relocs.iter().enumerate() {
        put(&mut f, reloc_off + i * CF_RELOC_LEN + CF_RELOC_OFFSET, *off);
        put(&mut f, reloc_off + i * CF_RELOC_LEN + CF_RELOC_KIND, *k);
    }
    f[reg_off..reg_off + reg_size].copy_from_slice(reg);
    if !dsp.is_empty() {
        put(&mut f, H_DSP_OFF, dsp_off as u32);
        put(&mut f, H_DSP_SIZE, dsp.len() as u32);
        f[dsp_off..dsp_off + dsp.len()].copy_from_slice(dsp);
    }
    f[image_off..].copy_from_slice(image);
    let sha = file_sha(&f);
    f[H_SHA..H_SHA + 32].copy_from_slice(&sha);
    Ok(f)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dkm {
    pub name: String,
    pub version: u32,
    /// [`ROLE_MODULE`] or [`ROLE_SERVICE`].
    pub role: u32,
    /// The word at [`H_SVC_ABI`].
    pub svc_abi: u32,
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
    /// The image offset of `dk_module.menu`.
    pub menu: Option<u32>,
    /// The image offset of `dk_module.tick`.
    pub tick: Option<u32>,
    pub register: Vec<u8>,
    /// The DSP section, as bytes (empty: none); parse it with [`crate::dkmdsp::DspSection::parse`].
    pub dsp: Vec<u8>,
}

impl Dkm {
    /// The size breakdown of a file of `total` bytes: `<total> bytes (image <n>, bss <n>, reloc <n>, import <n>, registry <n>, dsp <n>)`.
    /// The file is the 0xa0-byte header, the import and reloc tables, the registry, the DSP section and the image, in
    /// that order, with padding to 16 before the DSP section and the image. So `total` = header + padding (under 32) + image + reloc
    /// (8 bytes each) + import (32 bytes each) + registry + dsp; `bss` is memory the loader adds, not file bytes.
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
        if get(f, H_SIZE) as usize != f.len() || get(f, H_HDRSIZE) as usize != HEADER {
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
        let role = get(f, H_ROLE);
        if role != ROLE_MODULE && role != ROLE_SERVICE {
            return Err(err!("role {role} is not supported (0 module, 1 service)"));
        }
        let opt = |v: u32| (v != NONE).then_some(v);
        Ok(Dkm {
            name,
            version: get(f, H_VERSION),
            role,
            svc_abi: get(f, H_SVC_ABI),
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
            menu: opt(get(f, H_MENU)),
            tick: opt(get(f, H_TICK)),
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
    /// text: `jsr imp.l; jsr f.l; rts` plus a data pointer to bss and to an import with an addend, and the `dk_module` record (init, fini, tick, menu) at data offset 8
    /// with `init` and `tick` set. Carries the build's ISA check record.
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
                sec(".data", "data", 4, &[0; 24]),
                DkoSection {
                    name: ".bss".into(),
                    kind: "bss".into(),
                    align: 4,
                    size: 12,
                    hex: String::new(),
                },
            ],
            symbols: vec![
                sym("init_fn", Some(".text"), 12),
                sym("tick_fn", Some(".text"), 6),
                sym("dk_module", Some(".data"), 8),
                sym("counter", Some(".bss"), 4),
                sym("fw_thing", None, 0),
            ],
            relocs: vec![
                rel(".text", 2, "cf.32", "fw_thing", 0),
                rel(".text", 8, "cf.32", "tick_fn", 0),
                rel(".data", 0, "cf.32", "counter", 0),
                rel(".data", 4, "cf.32", "fw_thing", 0x10),
                rel(".data", 8, "cf.32", "init_fn", 0),
                rel(".data", 16, "cf.32", "tick_fn", 0),
            ],
        }
    }
    const FORMAT_DKO: u32 = crate::dko::FORMAT;

    fn opts() -> Options {
        Options {
            name: "test/x".into(),
            version: 0x010203,
            trace: false,
            stereo: false,
            gate: false,
            service: None,
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
        assert_eq!(syms["init_fn"], d.init.unwrap());
        assert_eq!(syms["tick_fn"], d.tick.unwrap());
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
        assert_eq!(d.tick, Some(6));
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
        for old in ["dkm_init", "dkm_fini", "dkm_menu", "dkm_slot_0"] {
            let mut o = object();
            o.symbols.push(sym(old, Some(".text"), 6));
            o.stamp_isa();
            assert!(
                pack(&o, &opts()).unwrap_err().0.contains("are gone"),
                "{old}"
            );
        }
        let mut o = object();
        o.symbols.retain(|s| s.name != "dk_module");
        o.stamp_isa();
        assert!(pack(&o, &opts()).unwrap_err().0.contains("no dk_module"));
        let mut o = Options {
            name: "bad name".into(),
            ..opts()
        };
        assert!(pack(&object(), &o).is_err());
        o.name = "x".repeat(32);
        assert!(pack(&object(), &o).is_err());
    }

    #[test]
    fn the_abi_fingerprint_of_a_service_object_is_header_word_0x3c() {
        let mut o = unstamped();
        // the dk_module record fills data bytes 8 to 24; the fingerprint follows it
        let mut data = [0u8; 28];
        data[24..28].copy_from_slice(&0xabcd_0123u32.to_be_bytes());
        o.sections[1] = sec(".data", "data", 4, &data);
        o.symbols.push(sym("dk_svc_abi", Some(".data"), 24));
        o.stamp_isa();
        assert_eq!(get(&pack(&o, &opts()).unwrap(), H_SVC_ABI), 0xabcd_0123);
        // an object without the symbol leaves the word zero
        assert_eq!(get(&pack(&object(), &opts()).unwrap(), H_SVC_ABI), 0);
    }

    /// The header table of `o` packed with `service` as the provided service of the file.
    fn service_table(service: &str, o: &Dko) -> (Dkm, Vec<u8>) {
        let opts = Options {
            service: Some(Provided {
                service: service.into(),
                exports: vec![],
                hooks: vec![],
                fills: vec![],
            }),
            ..opts()
        };
        let f = pack(o, &opts).unwrap();
        (Dkm::parse(&f).unwrap(), f)
    }

    #[test]
    fn a_service_file_records_init_as_the_handler_of_each_kind_its_service_owns() {
        use dkneeds::keys::{Keys, KindDecl};
        // `lfo` has no `rust` row: a `.dks` owns lfo_shape, and the `service!` glue declares it with the address of `init`
        let (d, _) = service_table("lfo", &object());
        let k = Keys::parse(&d.register).unwrap();
        assert!(k.kinds().eq([KindDecl {
            kind: dkneeds::KIND_LFO_SHAPE,
            len: dkneeds::LEN_LFO_SHAPE,
            entry: d.init.unwrap(),
        }]));
        assert_ne!(d.init, Some(0), "the object's init is no stub at offset 0");
        // a service that owns no kind (svc-test) has an empty KINDS: its file is as before
        let (d, _) = service_table("svc-test", &object());
        assert_eq!(Keys::parse(&d.register).unwrap().kinds().count(), 0);
        // a module that provides nothing has none either
        let d = Dkm::parse(&pack(&object(), &opts()).unwrap()).unwrap();
        assert_eq!(Keys::parse(&d.register).unwrap().kinds().count(), 0);
    }

    #[test]
    fn a_service_file_that_fills_a_slot_has_a_slots_item_with_the_name_hash_and_the_entry_offset() {
        use dkneeds::keys::Keys;
        let o = object();
        let opts = Options {
            service: Some(Provided {
                service: "svc-test".into(),
                exports: vec![],
                hooks: vec![],
                fills: vec![("frametype".into(), "tick_fn".into(), vec![])],
            }),
            ..opts()
        };
        let d = Dkm::parse(&pack(&o, &opts).unwrap()).unwrap();
        let k = Keys::parse(&d.register).unwrap();
        let slots: Vec<_> = k.slots().collect();
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].id, dkneeds::keys::name_hash("frametype"));
        assert_ne!(slots[0].entry, 0);
        // a fill with a function the object does not define is refused, and one slot twice
        let bad = Options {
            service: Some(Provided {
                service: "svc-test".into(),
                exports: vec![],
                hooks: vec![],
                fills: vec![("frametype".into(), "no_such_function".into(), vec![])],
            }),
            ..self::opts()
        };
        assert!(pack(&o, &bad).is_err());
    }

    #[test]
    fn a_fill_limited_to_a_device_is_in_the_pack_for_that_device_only() {
        use dkneeds::keys::Keys;
        let o = object();
        let slots_for = |device: &str| {
            let opts = Options {
                service: Some(
                    Provided {
                        service: "svc-test".into(),
                        exports: vec![],
                        hooks: vec![],
                        fills: vec![("frametype".into(), "tick_fn".into(), vec!["dn2".into()])],
                    }
                    .for_device(device),
                ),
                ..opts()
            };
            let d = Dkm::parse(&pack(&o, &opts).unwrap()).unwrap();
            Keys::parse(&d.register).unwrap().slots().count()
        };
        assert_eq!(slots_for("dt2"), 0);
        assert_eq!(slots_for("dn2"), 1);
    }

    /// The table of a module whose KEYS name the record kinds `kinds` (one key each), packed as a `.dkm`.
    fn needs_of_keys(kinds: &[u32]) -> Vec<dkneeds::keys::Need> {
        use dkneeds::keys::{KeyGroup, Keys, Table, table_size, write_table};
        let ks = [7u32];
        let groups: Vec<KeyGroup> = kinds
            .iter()
            .map(|k| KeyGroup {
                kind: *k,
                keys: &ks,
            })
            .collect();
        let t = Table {
            keys: &groups,
            ..Table::default()
        };
        let mut reg = vec![0u8; table_size(&t)];
        write_table(&t, &mut reg).unwrap();
        let f = pack_keys(Some(&object()), &reg, &[], &opts()).unwrap();
        Keys::parse(&Dkm::parse(&f).unwrap().register)
            .unwrap()
            .needs()
            .collect()
    }

    #[test]
    fn a_record_kind_of_a_service_file_is_a_hard_need_and_a_kind_of_an_in_crate_service_is_none() {
        // lfo_shape: its service (lfo) is provided by a .dks, so the module needs it
        let needs = needs_of_keys(&[dkneeds::KIND_LFO_SHAPE]);
        let lfo = dkneeds::keys::name_hash("lfo");
        let own: Vec<_> = needs.iter().filter(|n| n.service == lfo).collect();
        assert_eq!(own.len(), 1);
        assert!(own[0].hard);
        // effects are owned by a service in the image (no need for them); machines and pages by the machines service file (a hard need on it)
        let mach = dkneeds::keys::name_hash("machines");
        assert!(
            needs_of_keys(&[dkneeds::KIND_EFFECT])
                .iter()
                .all(|n| n.service != lfo && n.service != mach),
        );
        let m = needs_of_keys(&[dkneeds::KIND_MACHINE, dkneeds::KIND_PAGE]);
        assert!(m.iter().all(|n| n.service != lfo));
        assert_eq!(m.iter().filter(|n| n.service == mach && n.hard).count(), 1);
        // the same kind twice is one need
        let twice = needs_of_keys(&[dkneeds::KIND_LFO_SHAPE, dkneeds::KIND_LFO_SHAPE]);
        assert_eq!(twice.iter().filter(|n| n.service == lfo).count(), 1);
        // no KEYS, no record kind
        let f = pack(&object(), &opts()).unwrap();
        assert!(
            dkneeds::keys::Keys::parse(&Dkm::parse(&f).unwrap().register)
                .unwrap()
                .needs()
                .all(|n| n.service != lfo)
        );
    }

    #[test]
    fn the_header_has_the_needed_runtime_and_no_contract_pairs() {
        // 0x38..0x40 are reserved zero; 0x30 is the newest `since` of the imports (0: none) with the trace flag
        let f = pack(&object(), &opts()).unwrap();
        assert_eq!(&f[H_ROLE..H_ROLE + 4], &[0u8; 4]);
        assert_eq!(&f[H_SVC_ABI..H_SVC_ABI + 4], &[0u8; 4]);
        // no import is linked: the file needs the version that introduced the header table
        assert_eq!(get(&f, H_RUNTIME), crate::sdkgen::table_runtime());
        let f = pack_keys(Some(&object()), &[], &[0u8; 16], &opts()).unwrap();
        assert_eq!(&f[H_ROLE..H_ROLE + 8], &[0u8; 8]);
        let traced = Options {
            trace: true,
            ..opts()
        };
        let f = pack(&object(), &traced).unwrap();
        assert_eq!(
            get(&f, H_RUNTIME),
            FLAG_TRACE | crate::sdkgen::table_runtime()
        );
        assert_eq!(
            Dkm::parse(&f).unwrap().runtime_min,
            crate::sdkgen::table_runtime()
        );
        assert!(Dkm::parse(&f).unwrap().trace);
        // the header table is validated; any other bytes are refused
        assert!(pack_keys(None, &[1, 2, 3], &[], &opts()).is_err());
        // the needs of the linked imports are written: the service and the newest version among its imports
        let mut o = unstamped();
        for s in o.symbols.iter_mut().filter(|s| s.name == "fw_thing") {
            s.name = "dk_settings_open".into();
        }
        for r in o.relocs.iter_mut().filter(|r| r.symbol == "fw_thing") {
            r.symbol = "dk_settings_open".into();
        }
        o.stamp_isa();
        let f = pack(&o, &opts()).unwrap();
        let d = Dkm::parse(&f).unwrap();
        let k = dkneeds::keys::Keys::parse(&d.register).unwrap();
        let n: Vec<_> = k.needs().collect();
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].service, dkneeds::keys::name_hash("runtime"));
        assert_eq!(n[0].min_version, 0x000700);
        assert!(n[0].hard);
    }

    /// A DSP section of an 8-byte image with BSS bytes of bss and one version 3 voice with `voices` states of 20 bytes (0: version 2).
    fn dsp_section(bss: u32, voices: u32) -> Vec<u8> {
        crate::dkmdsp::DspSection {
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
        let f = pack_keys(Some(&object()), &[], &dsp_section(100, 3), &opts()).unwrap();
        let n = needs(&f).unwrap();
        assert_eq!(
            (n.dsp_area, n.dsp_align, n.voice_rows, n.arena_xfer),
            (192, 16, 1, 32)
        );
        // `DspSection::range_len`, the packer's own account of the range, agrees
        let s = crate::dkmdsp::DspSection::parse(&Dkm::parse(&f).unwrap().dsp).unwrap();
        assert_eq!(s.range_len(), n.dsp_area);
        // without state (version 2) the voice adds nothing: 8 + 100 = 108 -> 112 -> granule 128
        let f = pack_keys(Some(&object()), &[], &dsp_section(100, 0), &opts()).unwrap();
        assert_eq!(needs(&f).unwrap().dsp_area, 128);
        // a keys table: the row counts the packer wrote
        let mut keys = vec![0u8; dkneeds::keys::keys_size((1, 2, 1), 0, 1, 0)];
        dkneeds::keys::write_keys((1, 2, 1), 0, &[0x123400], &[], &mut keys).unwrap();
        let f = pack_keys(None, &keys, &[], &opts()).unwrap();
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

    /// An object with the entry point: four `rts` functions and a 16-byte `dk_module` in the data. `fields` are the data offsets (0, 4, 8, 12 =
    /// init, fini, tick, menu) that point to a function (`init_fn`, `fini_fn`, `tick_fn`, `menu_fn` at 0, 2, 4, 6).
    fn entry_object(fields: &[u64]) -> Dko {
        let funs = ["init_fn", "fini_fn", "tick_fn", "menu_fn"];
        let mut o = unstamped();
        o.sections = vec![
            sec(
                ".text",
                "text",
                2,
                &[0x4e, 0x75, 0x4e, 0x75, 0x4e, 0x75, 0x4e, 0x75],
            ),
            sec(".data", "data", 4, &[0; 16]),
        ];
        o.symbols = vec![sym("dk_module", Some(".data"), 0)];
        for (i, f) in funs.iter().enumerate() {
            o.symbols.push(sym(f, Some(".text"), 2 * i as u64));
        }
        o.relocs = fields
            .iter()
            .map(|at| rel(".data", *at, "cf.32", funs[(*at / 4) as usize], 0))
            .collect();
        o.stamp_isa();
        o
    }

    #[test]
    fn dk_module_gives_the_entry_points_and_a_missing_field_is_none() {
        // init, tick and menu are set; fini is NULL (no relocation, value 0)
        let o = entry_object(&[0, 8, 12]);
        let syms = image_symbols(&o).unwrap();
        let f = pack(&o, &opts()).unwrap();
        assert_eq!(get(&f, H_HDRSIZE) as usize, HEADER);
        let d = Dkm::parse(&f).unwrap();
        assert_eq!(d.init, Some(syms["init_fn"]));
        assert_eq!(d.fini, None);
        assert_eq!(d.tick, Some(syms["tick_fn"]));
        assert_eq!(d.menu, Some(syms["menu_fn"]));
        // no menu: the field is NONE
        let f = pack(&entry_object(&[0, 4, 8]), &opts()).unwrap();
        assert_eq!(get(&f, H_MENU), NONE);
        assert_eq!(Dkm::parse(&f).unwrap().menu, None);
    }

    #[test]
    fn dk_module_is_refused_when_it_is_not_made_of_functions_of_the_module() {
        let mut o = entry_object(&[0, 8]);
        o.symbols.push(sym("dkm_init", Some(".text"), 0));
        o.stamp_isa();
        assert!(pack(&o, &opts()).unwrap_err().0.contains("are gone"));
        // a field that is an import
        let mut o = entry_object(&[0, 8]);
        o.relocs[0].symbol = "fw_thing".into();
        o.stamp_isa();
        assert!(
            pack(&o, &opts())
                .unwrap_err()
                .0
                .contains("dk_module.init is an import")
        );
        // a field that holds a number and no relocation
        let mut o = entry_object(&[0, 8]);
        o.sections[1] = sec(
            ".data",
            "data",
            4,
            &[0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 0],
        );
        o.stamp_isa();
        assert!(
            pack(&o, &opts())
                .unwrap_err()
                .0
                .contains("dk_module.fini = 0x7 is no function")
        );
    }

    #[test]
    fn a_module_needs_the_newest_since_of_its_imports() {
        let runtime = runtime_version();
        // the compiler helpers and no import need no version
        assert_eq!(needed_runtime(["memcpy", "__muldi3"]), 0);
        assert_eq!(needed_runtime([]), 0);
        // a service import: its own `since`, never above the runtime's
        let table = crate::sdkgen::import_infos();
        assert_eq!(table["dk_settings_open"].since, 0x000700);
        assert_eq!(needed_runtime(["dk_settings_open", "memcpy"]), 0x000700);
        assert!(table.values().all(|i| i.since <= runtime));
        // a packed module declares it
        let mut o = entry_object(&[0, 8]);
        o.symbols.push(sym("dk_settings_open", None, 0));
        o.relocs
            .push(rel(".text", 0, "cf.32", "dk_settings_open", 0));
        o.stamp_isa();
        let f = pack(&o, &opts()).unwrap();
        // the newest `since` of its imports, and at least the version that introduced the header table
        assert_eq!(get(&f, H_RUNTIME), crate::sdkgen::table_runtime());
        assert_eq!(Dkm::parse(&f).unwrap().imports, vec!["dk_settings_open"]);
        // a module that links nothing of the SDK needs the header table's version
        assert_eq!(
            get(&pack(&object(), &opts()).unwrap(), H_RUNTIME),
            crate::sdkgen::table_runtime()
        );
    }

    #[test]
    fn services_are_the_service_tags_of_the_imports() {
        let s = services_of([
            "dk_settings_open",
            "dk_usb_bulk_open",
            "dk_usb_bulk_state",
            "memcpy",
            "dk_fw_alloc",
            "__muldi3",
        ]);
        assert_eq!(s["runtime"], vec!["dk_settings_open", "dk_fw_alloc"]);
        assert_eq!(s["usb_bulk"], vec!["dk_usb_bulk_open", "dk_usb_bulk_state"]);
        assert_eq!(s.len(), 2, "a compiler helper names no service");
        assert!(services_of(["memcpy"]).is_empty());
    }
}
