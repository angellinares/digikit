//! Module manifests (`module.toml`) and the library of modules found under `modules/`.

use crate::dko::Dko;
use crate::error::*;
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Deserialize)]
pub struct Manifest {
    #[serde(skip)]
    pub sample_tracks: Option<crate::typed::SampleTracksDef>,
    pub module: ModuleInfo,
    #[serde(default)]
    pub targets: BTreeMap<String, String>,
    #[serde(default)]
    pub object: Vec<ObjectRef>,
    /// Resolution rules for `tools/patchkit/port_resolve.py`; patchkit does not read them.
    #[serde(default)]
    pub anchor_rule: Vec<toml::Value>,
    #[serde(default)]
    pub op: Vec<Op>,
    /// `[[slot]]`: pointer slots (a zero data word each) that `op = "stub"` sites load. Each is an entry of the image's one table `dkrt_info_table` (name hash and
    /// the address of its word; `prim::build_info_table`).
    #[serde(default)]
    pub slot: Vec<SlotDef>,
    /// `[[info]]`: data tables of this module that a service reads through `dkc_info`. Each is an entry of `dkrt_info_table`, with the address of `symbol`.
    #[serde(default)]
    pub info: Vec<InfoDef>,
    #[serde(default)]
    pub claim: Vec<Claim>,
    /// P1: sections copied from the user's image (and extended) at apply time.
    #[serde(default)]
    pub section: Vec<SectionDef>,
    /// P2: registries this module declares (a framework) ...
    #[serde(default)]
    pub registry: Vec<RegistryDef>,
    /// ... and entries it adds to registries (a content module).
    #[serde(default)]
    pub entry: Vec<EntryDef>,
    /// Typed sections (`crate::typed`), filled at load from the descriptor records of the objects (`crate::desc`);
    /// `typed::lower` turns them into `entry` rows per device.
    #[serde(skip)]
    pub machine: Vec<crate::typed::MachineDef>,
    #[serde(skip)]
    pub param: Vec<crate::typed::ParamDef>,
    #[serde(skip)]
    pub page: Vec<crate::typed::PageDef>,
    #[serde(skip)]
    pub native_page: Vec<crate::typed::NativePageDef>,
    #[serde(skip)]
    pub voice: Vec<crate::typed::VoiceDef>,
    /// `[[machine]] id = "..."`: the machines the module declares in init (`dk_module_ctx_add_machine`); the id is all the manifest says of one. Their keys go in the
    /// `.dkm` keys table (`dkneeds::keys`).
    #[serde(default, rename = "machine")]
    pub machines: Vec<IdRef>,
    /// `[[fx]] id = "..."`: the same for effects (`dk_module_ctx_add_fx`).
    #[serde(default, rename = "fx")]
    pub fxs: Vec<IdRef>,
    /// `[[record]] kind = "lfo_shape", key = 7`: a record of a generic kind (`[records.kinds]` `generic = true`) that `init` emits with `dk_record_add`, with its key (the first
    /// word of the record's body). The keys go in the KEYS entry of the header table, where the boot planner checks that the kind has an owner and that no other module has the key.
    #[serde(default, rename = "record")]
    pub records: Vec<RecordKey>,
    /// `[[export]] name = "dk_svctest_ping"`: the functions a service module (`role = "service"`) exports; each is an `[[import]]` of its service in sdk/sdk.toml.
    /// Their name hashes and entry offsets go in the `EXPORTS` entry of the header table.
    #[serde(default, rename = "export")]
    pub exports: Vec<ExportDef>,
    /// `[[hook]] name = "cf.menu.hook.settings_list.tail"`, `entry = "my_handler"`: the hook sites a service module takes and the global function of the object
    /// that handles each. Their name hashes and entry offsets go in the `HOOKS` entry of the header table; `patchkit bind` gives each a trampoline.
    #[serde(default, rename = "hook")]
    pub hooks: Vec<ServiceHook>,
    /// `[[fill]] slot = "frametype"`, `entry = "mf_frametype"`: a pointer slot of an image adapter (`[[slot]]` of its module.toml) that a service module fills with the address
    /// of a global function of the object when it is loaded (the loader finds the slot word by the hash of the name in the image's info table).
    #[serde(default, rename = "fill")]
    pub fills: Vec<ServiceFill>,
    /// `[uses.dt2]`, `[uses.dn2]`: registry rows the module takes on each device (the keys table's three counts). Per device because the runtime
    /// builds different rows per device (DT2 has no page rows).
    #[serde(default)]
    pub uses: UsesByDevice,
    /// Removed forms (`[[param]]`, `[[page]]`, `[[voice]]`): kept only to refuse them with a clear error.
    #[serde(default, rename = "param")]
    param_old: Option<toml::Value>,
    #[serde(default, rename = "page")]
    page_old: Option<toml::Value>,
    #[serde(default, rename = "voice")]
    voice_old: Option<toml::Value>,
    /// `[[gen]]`: files `patchkit gen` writes from device data (`crate::devgen`).
    #[serde(default, rename = "gen")]
    pub gens: Vec<GenDef>,
    /// Set by `typed::lower`.
    #[serde(skip)]
    pub lowered: bool,
}

impl Manifest {
    /// Checks independent of objects, allocation tables, device models and firmware.
    fn validate_header(&self) -> Result<()> {
        if self.module.category == "core" && self.module.role.is_none() {
            return Err(err!(
                "{}: a core module needs `role = \"runtime\" | \"installer\" | \"adapter\" | \"app\"` in [module]",
                self.module.id
            ));
        }
        self.provided_service()?;
        for (table, ids) in [("machine", &self.machines), ("fx", &self.fxs)] {
            let mut seen = std::collections::BTreeSet::new();
            for r in ids {
                let ok = !r.id.is_empty()
                    && r.id.bytes().all(|c| {
                        c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'_'
                    });
                if !ok {
                    return Err(err!(
                        "[[{table}]] id {:?}: use lowercase ascii letters, digits, `-` and `_`",
                        r.id
                    ));
                }
                if !seen.insert(r.id.as_str()) {
                    return Err(err!("[[{table}]] id {:?} is declared twice", r.id));
                }
                if let Some(d) = &r.dsp {
                    let ok = !d.is_empty()
                        && d.bytes()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_');
                    if !ok {
                        return Err(err!(
                            "[[{table}]] id {:?}: dsp {d:?}: use lowercase ascii letters, digits and `_`",
                            r.id
                        ));
                    }
                }
            }
        }
        for (name, old) in [
            ("param", &self.param_old),
            ("page", &self.page_old),
            ("voice", &self.voice_old),
        ] {
            if old.is_some() {
                return Err(err!(
                    "[[{name}]] is no longer read: declare machines and effects with `dk_module_ctx_add_machine` / `dk_module_ctx_add_fx` in init and list their ids as [[machine]] / [[fx]] in module.toml"
                ));
            }
        }
        Ok(())
    }

    /// What a `role = "service"` module provides (`None` for any other role). The service is a `[[service]]` of sdk/sdk.toml without `rust` (provided by a
    /// `.dks`), and each `[[export]]` is an `[[import]]` of that service.
    pub fn provided_service(&self) -> Result<Option<crate::dkm::Provided>> {
        let id = &self.module.id;
        if self.module.role != Some(ModuleRole::Service) {
            if self.module.service.is_some()
                || !self.exports.is_empty()
                || !self.hooks.is_empty()
                || !self.fills.is_empty()
            {
                return Err(err!(
                    "{id}: `service` in [module], [[export]], [[hook]] and [[fill]] belong to a module with `role = \"service\"`"
                ));
            }
            return Ok(None);
        }
        let Some(service) = &self.module.service else {
            return Err(err!(
                "{id}: a module with `role = \"service\"` needs `service = \"NAME\"` in [module]"
            ));
        };
        if !crate::sdkgen::provider_services().contains(service) {
            return Err(err!(
                "{id}: service {service:?} is not a [[service]] of sdk/sdk.toml without `rust` (a service provided by a .dks)"
            ));
        }
        // The runtime's find and Transfer name a provider by its file stem: `services/<service>.dks`, and the stem is the id's leaf.
        let leaf = id.rsplit('/').next().unwrap_or(id);
        if leaf.len() > 50 {
            return Err(err!(
                "{id}: the id's last part is {} bytes: a service file name is at most 50 (`services/<name>.dks` and its NUL must fit the 64 bytes of a selection path)",
                leaf.len()
            ));
        }
        if leaf != service {
            return Err(err!(
                "{id}: the id's last part {leaf:?} must equal the provided service {service:?} (the file is `{service}.dks`: the runtime finds a provider by its file name)"
            ));
        }
        let imports = crate::sdkgen::import_infos();
        let mut exports = vec![];
        for e in &self.exports {
            match imports.get(&e.name) {
                Some(i) if i.service == *service => exports.push(e.name.clone()),
                _ => {
                    return Err(err!(
                        "{id}: [[export]] {:?} is not an [[import]] of the service {service:?} in sdk/sdk.toml",
                        e.name
                    ));
                }
            }
        }
        let mut hooks: Vec<(String, String, Vec<String>)> = vec![];
        for h in &self.hooks {
            if h.name.is_empty() || h.entry.is_empty() {
                return Err(err!(
                    "{id}: [[hook]] needs a `name` and an `entry`, both not empty"
                ));
            }
            if hooks.iter().any(|(n, _, _)| *n == h.name) {
                return Err(err!(
                    "{id}: [[hook]] {:?} is declared twice (one name has one entry, so a hook for two devices is one entry without `devices`)",
                    h.name
                ));
            }
            for d in &h.devices {
                if crate::devdata::by_short(d).is_none() {
                    return Err(err!(
                        "{id}: [[hook]] {:?}: device {d:?} is not one of {}",
                        h.name,
                        crate::devdata::short_list()
                    ));
                }
            }
            hooks.push((h.name.clone(), h.entry.clone(), h.devices.clone()));
        }
        let mut fills: Vec<(String, String, Vec<String>)> = vec![];
        for f in &self.fills {
            if f.slot.is_empty() || f.entry.is_empty() {
                return Err(err!(
                    "{id}: [[fill]] needs a `slot` and an `entry`, both not empty"
                ));
            }
            if fills.iter().any(|(n, _, _)| *n == f.slot) {
                return Err(err!("{id}: [[fill]] {:?} is declared twice", f.slot));
            }
            for d in &f.devices {
                if crate::devdata::by_short(d).is_none() {
                    return Err(err!(
                        "{id}: [[fill]] {:?}: device {d:?} is not one of {}",
                        f.slot,
                        crate::devdata::short_list()
                    ));
                }
            }
            fills.push((f.slot.clone(), f.entry.clone(), f.devices.clone()));
        }
        if fills.len() > dkneeds::SLOTS_MAX as usize {
            return Err(err!(
                "{id}: {} [[fill]] entries, over the {} a service file fills",
                fills.len(),
                dkneeds::SLOTS_MAX
            ));
        }
        Ok(Some(crate::dkm::Provided {
            service: service.clone(),
            exports,
            hooks,
            fills,
        }))
    }
}

/// The module's provided service for `pack` (see [`Manifest::provided_service`]).
pub fn service_of(m: &Module) -> Result<Option<crate::dkm::Provided>> {
    m.manifest.provided_service()
}

/// An entry of `[[export]]`: the name of an exported function.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExportDef {
    pub name: String,
}

/// An entry of `[[fill]]` of a service: the pointer slot of an image adapter and the global function of the object whose address the installer writes in it.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ServiceFill {
    pub slot: String,
    pub entry: String,
    /// The devices the fill is for (`dt2`, `dn2`); empty: every device. Needed when the slot is limited to some devices (`devices` of its `[[slot]]`).
    #[serde(default)]
    pub devices: Vec<String>,
}

/// An entry of `[[hook]]` of a service: the hook of the port files (`devices/ports/`) and the global function of the object that handles it.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ServiceHook {
    pub name: String,
    pub entry: String,
    /// The devices the hook is for (`dt2`, `dn2`); empty: every device that declares it. `service!` lists the hooks of each device the same way
    /// (`hooks_dt2`, `hooks_dn2`, `hooks`), and `patchkit service-check` compares the two per device.
    #[serde(default)]
    pub devices: Vec<String>,
}

/// An entry of `[[record]]`: the kind by its lower-case name (`dkneeds::KIND_NAMES`) and the key of one record of it.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RecordKey {
    pub kind: String,
    pub key: u32,
}

/// An entry of `[[machine]]` or `[[fx]]`: the id and an optional DSP symbol prefix (any other key is refused).
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct IdRef {
    pub id: String,
    /// The prefix of the DSP entry symbols (`_<prefix>_render`, ...) when it is not the id: for DSP code that exists already. It never changes the id,
    /// which the keys hash and `module_ids.h` names its constant from.
    #[serde(default)]
    pub dsp: Option<String>,
}

impl IdRef {
    /// The prefix of this entry's DSP symbols and where it came from: `dsp` of module.toml, else the id with `-` as `_`.
    pub fn dsp_prefix(&self) -> (String, &'static str) {
        match &self.dsp {
            Some(p) => (p.clone(), "`dsp` of its [[machine]]/[[fx]] in module.toml"),
            None => (self.id.replace('-', "_"), "its id"),
        }
    }
}

/// `[uses.<device>]`: one table per device (short names as in `devices/*.toml`); a module has only the tables of the devices it builds for.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UsesByDevice {
    #[serde(default)]
    pub dt2: Option<Uses>,
    #[serde(default)]
    pub dn2: Option<Uses>,
}

impl UsesByDevice {
    /// The counts for DEVICE (`dt2`, `dn2`); all 0 when the module has no table for it.
    pub fn for_device(&self, device: &str) -> Uses {
        match device {
            "dt2" => self.dt2,
            "dn2" => self.dn2,
            _ => None,
        }
        .unwrap_or_default()
    }
}

/// One device's registry rows a module takes, all default 0.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Uses {
    #[serde(default)]
    pub machine_rows: u16,
    #[serde(default)]
    pub param_rows: u16,
    #[serde(default)]
    pub page_rows: u16,
    /// Payload bytes of the record block `init` commits (the `module_commit` event's `records_size`): the keys table's `record_bytes`.
    #[serde(default)]
    pub record_bytes: u16,
    /// DSP instructions per block of one voice of the module's machines, the worst steady block `module:check` measured (the keys table's `DSP_COST_VOICE`).
    #[serde(default)]
    pub dsp_cost_voice: u32,
    /// DSP instructions per block of the module's effects, summed (the keys table's `DSP_COST_FX`).
    #[serde(default)]
    pub dsp_cost_fx: u32,
}

/// One generated file of device data: `kind` is `rust` (constants every device agrees on, one file), `gas-shared` (the same as `.equ` lines), `gas` (a ColdFire assembler
/// include, `.equ` lines, one file per device), `sharc` (a SHARC+ include, `#define` lines, one file per device) `sharc-adapter` (the `ADM_*` defines of `[adapter.machine]`, one file per device), `gas-sample-pool` or `sharc-sample-pool` (the `SP_*` numbers of `[adapter.sample_pool]` for ColdFire and SHARC+ assembly, one file per device). `path` is relative
/// to the module; `{dev}` stands for the device's short name in a per-device path. `uses` lists the constant names the module reads; a trailing `*` matches a numbered
/// family (`FX_VALUE_*`). It is required for `rust`, `gas-shared`, `gas` and `sharc`, and refused for the other kinds.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenDef {
    pub kind: String,
    pub path: String,
    #[serde(default)]
    pub uses: Vec<String>,
}

/// A number or an expression (`crate::expr`) written either as a TOML integer or as a string.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum ExprSrc {
    Int(i64),
    Text(String),
}

impl ExprSrc {
    pub fn text(&self) -> String {
        match self {
            ExprSrc::Int(i) => i.to_string(),
            ExprSrc::Text(t) => t.clone(),
        }
    }
}

/// P1. `[[section]] name, region, from = { anchor, offset, len, expect_sha256 }, append = { registry, row }`.
/// The section's bytes are copied from the user's image when the patch is applied (firmware is never stored in the
/// repository), one row per entry of the registry is appended, and the section is placed in `region`. The symbol
/// `<name>` is its address and `<name>.size` its length in bytes.
#[derive(Clone, Debug, Deserialize)]
pub struct SectionDef {
    pub name: String,
    pub region: String,
    #[serde(default)]
    pub devices: Vec<String>,
    /// Alignment of the placed section in bytes (default 4).
    #[serde(default)]
    pub align: Option<u64>,
    pub from: SectionFrom,
    #[serde(default)]
    pub append: Option<SectionAppend>,
    #[serde(default)]
    pub edit: Option<SectionEdit>,
}

/// Rewrites of the copied bytes, one per entry of a registry: an entry names the copied row (`at`) and new values for
/// some of its words (a module changes a stock row of a framework's table).
#[derive(Clone, Debug, Deserialize)]
pub struct SectionEdit {
    pub registry: String,
    /// Byte offset of the edited row in the copied bytes (expression; the entry's fields are `$name`).
    pub at: ExprSrc,
    pub fields: Vec<SectionEditField>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SectionEditField {
    /// Byte offset in the row.
    pub offset: u64,
    /// The new value; `u8:EXPR` and `u16:EXPR` as in `append.row`.
    pub value: ExprSrc,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SectionFrom {
    pub anchor: String,
    #[serde(default)]
    pub offset: i64,
    /// Bytes to copy (expression, static: anchors and image reads only). Default: the rest of the anchor.
    #[serde(default)]
    pub len: Option<ExprSrc>,
    /// sha256 of the copied bytes, needed when the range reaches outside the anchor.
    #[serde(default)]
    pub expect_sha256: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SectionAppend {
    pub registry: String,
    /// One expression per word of the row; `u8:EXPR` and `u16:EXPR` emit narrower values (default `u32`). The
    /// fields of the entry are `$name`, its index is `$index`.
    pub row: Vec<ExprSrc>,
}

/// P2. A framework declares a registry; patchkit builds its row table.
#[derive(Clone, Debug, Deserialize)]
pub struct RegistryDef {
    pub name: String,
    /// Index of the first entry (expression, static).
    pub first: ExprSrc,
    /// Most entries the framework can take.
    pub max: u32,
    /// A device limit (`[limits]` of the device file, see `devdata::LIMIT_NAMES`) that caps the entries on that device below `max`.
    #[serde(default)]
    pub limit: Option<String>,
    pub region: String,
    #[serde(default)]
    pub devices: Vec<String>,
    /// One all-zero row behind the entries (not counted in `<reg>.count`): a table a routine scans until a zero row.
    #[serde(default)]
    pub terminator: bool,
    #[serde(default)]
    pub field: Vec<FieldDef>,
    /// Permanent entry numbers: a TOML file of the framework (path relative to the module) with, per device, the
    /// highest number the framework supports (`last`) and one number per entry, keyed `"<module id>:<entry id>"`
    /// (`[dt2.alloc]`). An entry's index is its allocated number, not `first + k`. The registry then spans
    /// `first ..= highest selected number`; a number nobody selected is a gap row built from `[registry.gap]`.
    #[serde(default)]
    pub index_table: Option<String>,
    /// Field values of a gap row (the registry's field defaults fill the rest).
    #[serde(default)]
    pub gap: Option<BTreeMap<String, toml::Value>>,
    /// The registry takes no entry from a patched-in module: its rows are made at run time. The text says why and is part of the error that a module with an entry gets.
    #[serde(default)]
    pub refuse_entries: Option<String>,
    /// Modules that may still add entries to a registry with `refuse_entries` (`core/dynreg` reserves the rows that run-time machines fill).
    #[serde(default)]
    pub refuse_entries_except: Vec<String>,
}

/// The parsed `index_table` of a registry: device -> numbers.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct AllocTable {
    #[serde(flatten)]
    pub devices: BTreeMap<String, DevAlloc>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DevAlloc {
    /// The highest index the framework supports on this device.
    pub last: i64,
    #[serde(default)]
    pub alloc: BTreeMap<String, i64>,
    /// First number of the range reserved for run-time modules (`core/dynreg` reserves `dynamic_first ..= last`).
    #[serde(default)]
    pub dynamic_first: Option<i64>,
}

impl AllocTable {
    pub fn parse(text: &str) -> Result<AllocTable> {
        let t: AllocTable = toml::from_str(text)?;
        for (dev, d) in &t.devices {
            if crate::devdata::by_short(dev).is_none() {
                return Err(err!("allocation table: unknown device [{dev}]"));
            }
            let mut seen: BTreeMap<i64, &str> = BTreeMap::new();
            for (k, v) in &d.alloc {
                if *v < 0 || *v > d.last {
                    return Err(err!(
                        "allocation table [{dev}]: {k} = {v} is outside 0..={}",
                        d.last
                    ));
                }
                if let Some(prev) = seen.insert(*v, k) {
                    return Err(err!(
                        "allocation table [{dev}]: {prev} and {k} both have number {v}"
                    ));
                }
            }
            d.check_dynamic(dev)?;
        }
        Ok(t)
    }
}

impl DevAlloc {
    /// Check the run-time range: a static `alloc` number never lies in it (run-time machines take their number at load,
    /// docs/design/module-sdk.md 11.5 D7).
    fn check_dynamic(&self, dev: &str) -> Result<()> {
        let Some(first) = self.dynamic_first else {
            return Ok(());
        };
        // the range belongs to the run-time modules; `core/dynreg` reserves its rows there
        if let Some((k, v)) = self
            .alloc
            .iter()
            .find(|(k, v)| **v >= first && !k.starts_with("core/dynreg:"))
        {
            return Err(err!(
                "allocation table [{dev}]: {k} = {v} lies in the run-time range {first}.."
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct FieldDef {
    pub name: String,
    /// `u8`, `u16`, `u32`, `cstr` (a pointer to a string placed behind the rows) or `sym` (an expression; a symbol
    /// of any module, or `<registry>.<id>.index`)
    pub kind: String,
    #[serde(default)]
    pub max_len: Option<usize>,
    #[serde(default)]
    pub default: Option<toml::Value>,
}

/// P2. A content module adds an entry: `[[entry]] registry = "machines", id = "pluck", <field> = value ...`.
#[derive(Clone, Debug, Deserialize)]
pub struct EntryDef {
    pub registry: String,
    pub id: String,
    #[serde(default)]
    pub devices: Vec<String>,
    #[serde(flatten)]
    pub fields: BTreeMap<String, toml::Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModuleKind {
    Machine,
    Fx,
    Service,
    Raw,
    Framework,
}

/// What a `core/*` module is in the layering (docs/ARCHITECTURE.md): the loader and policy owner, the reset-time installer, an adapter to
/// one firmware plug-in point, or an app that uses the adapters' APIs. Checked by `tools/devtools/layercheck.py`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModuleRole {
    Runtime,
    Installer,
    Adapter,
    App,
    /// A service provider (a `.dks`; any category): needs `service` in [module].
    Service,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ModuleInfo {
    /// `category/name`; unique in a library (it need not equal the directory)
    pub id: String,
    pub version: String,
    pub name: String,
    pub category: String,
    /// The layer of a `core` module (required for `category = "core"`, see `ModuleRole`)
    #[serde(default)]
    pub role: Option<ModuleRole>,
    /// With `role = "service"`: the service the module provides (a `[[service]]` of sdk/sdk.toml without `rust`)
    #[serde(default)]
    pub service: Option<String>,
    /// What the module is; `framework` is internal (core modules)
    #[serde(rename = "type")]
    pub kind: ModuleKind,
    pub author: String,
    pub status: String,
    pub summary: String,
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub conflicts: Vec<String>,
    /// Restrict to these ports (`dt2-1.17`); empty = any firmware whose anchors resolve.
    #[serde(default)]
    pub ports: Vec<String>,
    /// A module that is loaded at run time (`patchkit pack`, a `.dkm`), never patched into an image. Its typed sections
    /// are not lowered into registry rows; the packer lowers them into the `.dkm` `register` records.
    #[serde(default)]
    pub dynamic: bool,
}

impl ModuleInfo {
    /// A module the run-time loader takes as a `.dkm`: not `core`, not a framework, and dynamic, raw or a service
    /// (the rule of the workbench library and of `apps/web/src/data/shipped.mjs`).
    pub fn is_runtime(&self) -> bool {
        self.category != "core"
            && !matches!(self.kind, ModuleKind::Framework)
            && (self.dynamic || matches!(self.kind, ModuleKind::Raw | ModuleKind::Service))
    }

    /// Whether the module builds for the device `short` (`dt2`): no `ports`, or a port of that device.
    pub fn builds_for(&self, short: &str) -> bool {
        self.ports.is_empty()
            || self
                .ports
                .iter()
                .any(|p| p.split('-').next() == Some(short))
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct ObjectRef {
    pub path: String,
    /// Port region the object is placed in: `dsp.l2`, `cf.expansion`, `cf.flash_cave`.
    pub place: String,
    /// Restrict to these devices (`dt2`, `dn2`); empty = all.
    #[serde(default)]
    pub devices: Vec<String>,
    /// Undefined symbol -> anchor name (an undefined symbol named like an anchor needs no entry).
    #[serde(default)]
    pub externs: BTreeMap<String, String>,
    /// Undefined symbols that resolve to 0 when no selected module provides them (an optional link to another module).
    #[serde(default)]
    pub optional: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Op {
    /// `write`, `hook` or `stub`
    pub op: String,
    #[serde(default)]
    pub anchor: String,
    /// write: `PREFIX.*` repeats the op for every anchor named `PREFIX.<number>` (a multi-site rule of
    /// `tools/patchkit/port_resolve.py`), in numeric order; `this` names the current anchor in expressions.
    #[serde(default)]
    pub anchors: Option<String>,
    /// write: the new value (expression) and its width in bytes (1, 2 or 4), range checked
    #[serde(default)]
    pub value: Option<ExprSrc>,
    #[serde(default)]
    pub width: Option<u32>,
    /// hook: the declared hook this op attaches to; its site, offset and mode come from the port file
    #[serde(default)]
    pub hook: Option<String>,
    /// hook: `run` (default; the displaced instructions run in `hook.<anchor>.tail`) or `drop`
    #[serde(default)]
    pub displaced: Option<String>,
    /// hook: `cf` or `sharc`; optional, must agree with the anchor
    #[serde(default)]
    pub cpu: Option<String>,
    /// hook: region of the tail (default `cf.expansion` / `dsp.l2`)
    #[serde(default)]
    pub place: Option<String>,
    #[serde(default)]
    pub devices: Vec<String>,
    /// byte offset from the anchor address
    #[serde(default)]
    pub offset: i64,
    /// retarget: the module symbol the call or jump now goes to
    pub to: Option<String>,
    /// write: the new bytes
    pub hex: Option<String>,
    /// write: sha256 of the bytes being replaced (never the bytes themselves)
    pub expect_sha256: Option<String>,
    #[serde(default)]
    pub reloc: Vec<OpReloc>,
    /// stub: the declared slot the stub loads (a `[[slot]]` of the module)
    #[serde(default)]
    pub slot: Option<String>,
    /// stub: the register the stub overwrites (`d0`..`d7`, `a0`..`a6`); it must be one the hook's contract lets the hook clobber
    #[serde(default)]
    pub scratch: Option<String>,
    /// stub: expression of a hand-written stock path; the stub jumps there instead of running the re-encoded displaced instructions
    #[serde(default)]
    pub tail: Option<String>,
}

/// `[[info]] name = "fxpages_info", symbol = "fxpages_info", devices = ["dt2"]`: an entry of the image's info table: the hash of `name` and the address that the
/// expression `symbol` evaluates to (a global symbol of the module's objects). `name` is what a service passes to `dkc_info` (as its name hash); it shares one
/// namespace with the `[[slot]]` names of all image adapters, and `patchkit apply` refuses two entries with one hash.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InfoDef {
    pub name: String,
    /// Expression of the address; default: the symbol named `name`.
    #[serde(default)]
    pub symbol: Option<String>,
    #[serde(default)]
    pub devices: Vec<String>,
}

/// `[[slot]] name = "frame_type", devices = ["dn2"]`: one 4-byte pointer slot in the module's data region, zero at build.
#[derive(Clone, Debug, Deserialize)]
pub struct SlotDef {
    pub name: String,
    #[serde(default)]
    pub devices: Vec<String>,
    /// region of the slot words (default `cf.expansion`)
    #[serde(default)]
    pub place: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct OpReloc {
    pub offset: u64,
    #[serde(rename = "type")]
    pub rtype: String,
    pub symbol: String,
    #[serde(default)]
    pub addend: i64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Claim {
    pub resource: String,
    #[serde(default)]
    pub devices: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Module {
    pub manifest: Manifest,
    pub objects: Vec<Dko>,
    /// parsed `index_table` files, by registry name
    pub alloc: BTreeMap<String, AllocTable>,
    /// sha256 of the manifest text and the object texts, in order (for the plan)
    pub digest: String,
    /// The directory of the manifest (empty for a module built from parts)
    pub dir: std::path::PathBuf,
}

impl Module {
    /// Build a module from its manifest text and a reader for the object paths it names.
    pub fn from_parts(
        manifest_text: &str,
        read: &dyn Fn(&str) -> Result<String>,
    ) -> Result<Module> {
        Self::from_parts_with(manifest_text, read, false)
    }

    /// Like `from_parts`, but an object that cannot be read is left out of `objects` (only `patchkit gen` loads this way: the generated
    /// headers are inputs of the build that makes the objects). Such a module must not be applied or packed.
    pub fn from_parts_lenient(
        manifest_text: &str,
        read: &dyn Fn(&str) -> Result<String>,
    ) -> Result<Module> {
        Self::from_parts_with(manifest_text, read, true)
    }

    fn from_parts_with(
        manifest_text: &str,
        read: &dyn Fn(&str) -> Result<String>,
        lenient: bool,
    ) -> Result<Module> {
        let manifest: Manifest = toml::from_str(manifest_text)?;
        manifest.validate_header()?;
        let mut objects = vec![];
        let mut all = manifest_text.to_string();
        for o in &manifest.object {
            let t = match read(&o.path) {
                Ok(t) => t,
                Err(_) if lenient => continue,
                Err(e) => return Err(e),
            };
            all.push_str(&t);
            objects.push(Dko::parse(&t).map_err(|e| err!("{}: {e}", o.path))?);
        }
        let mut alloc = BTreeMap::new();
        for r in &manifest.registry {
            if let Some(path) = &r.index_table {
                let t = read(path)?;
                all.push_str(&t);
                let a = AllocTable::parse(&t).map_err(|e| err!("{path}: {e}"))?;
                alloc.insert(r.name.clone(), a);
            }
        }
        crate::prim::validate_manifest(&manifest)?;
        Ok(Module {
            manifest,
            objects,
            alloc,
            digest: sha256_hex(all.as_bytes()),
            dir: Default::default(),
        })
    }

    pub fn id(&self) -> &str {
        &self.manifest.module.id
    }

    pub fn tag(&self) -> String {
        format!(
            "{}@{}",
            self.manifest.module.id, self.manifest.module.version
        )
    }
}

#[derive(Clone, Debug, Default)]
pub struct Library {
    pub modules: Vec<Module>,
}

impl Library {
    pub fn get(&self, id: &str) -> Option<&Module> {
        self.modules.iter().find(|m| m.id() == id)
    }

    /// Lower the typed sections of every module with the device models (`crate::typed`).
    pub fn lower(&mut self, models: &crate::typed::Models) -> Result<()> {
        for m in &mut self.modules {
            crate::typed::lower(&mut m.manifest, models)
                .map_err(|e| err!("{}: {e}", m.manifest.module.id))?;
        }
        Ok(())
    }

    /// The selection plus everything it requires, transitively. Returns the full list (the selection first, in the
    /// order given, then the added modules in the order found) and the added ones. Refuses a module that is not in
    /// the library, a requirement that is missing, and a `requires` cycle.
    pub fn with_requires(&self, ids: &[String]) -> Result<(Vec<String>, Vec<String>)> {
        fn visit(
            lib: &Library,
            id: &str,
            stack: &mut Vec<String>,
            done: &mut Vec<String>,
        ) -> Result<()> {
            if stack.iter().any(|s| s == id) {
                return Err(err!("requires cycle: {} -> {id}", stack.join(" -> ")));
            }
            if done.iter().any(|d| d == id) {
                return Ok(());
            }
            let m = lib.get(id).ok_or_else(|| match stack.last() {
                Some(by) => err!("{by} requires {id}, which is not in the module library"),
                None => err!("unknown module {id}"),
            })?;
            stack.push(id.to_string());
            for r in &m.manifest.module.requires {
                visit(lib, r, stack, done)?;
            }
            stack.pop();
            done.push(id.to_string());
            Ok(())
        }
        let mut done: Vec<String> = vec![];
        for id in ids {
            visit(self, id, &mut vec![], &mut done)?;
        }
        let mut all: Vec<String> = vec![];
        for id in ids {
            if !all.contains(id) {
                all.push(id.clone());
            }
        }
        let added: Vec<String> = done.into_iter().filter(|d| !all.contains(d)).collect();
        all.extend(added.iter().cloned());
        Ok((all, added))
    }
}

/// How a scan of the module roots gets the objects of content modules (`category` other than `core`): they are build output in the cache
/// `out/cache/dko` (`crate::build`), not files of the module. A stale or missing object is built on demand with the toolchain.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Copy)]
pub struct Build<'a> {
    /// The repository root: the toolchain and the cache are there.
    pub root: &'a std::path::Path,
    /// Load the objects of the content modules with these ids only (built when stale); the other content modules load without objects
    /// (as in `from_parts_lenient`), so nothing is hashed or built for them and no toolchain is needed. Lenient generation reads their
    /// existing cache without checking freshness or building. `None`: all content modules.
    pub only: Option<&'a [String]>,
    /// Build defines (`DK_TRACE`) for the objects built on demand; they are part of the cache key (`crate::build::toolchain::Env::defines`).
    pub defines: &'a [String],
}

/// Where a module library comes from, besides the repository's module roots and PATCHKIT_MODULES.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Default, Clone)]
pub struct LibrarySource {
    /// `--manifest PATH` values: extra module manifests.
    pub manifests: Vec<std::path::PathBuf>,
    /// `--modules-dir DIR`: extra module roots.
    pub modules_dirs: Vec<std::path::PathBuf>,
    /// `--trace`: builds with DK_TRACE defined.
    pub trace: bool,
}

/// `modules/` and `tests/fixtures/modules/` of the repository, then the extra roots of `src.modules_dirs` and the PATCHKIT_MODULES
/// environment variable (a path list), in that order. Generated modules (derived from a firmware) live there. Content modules get their
/// objects from the cache `out/cache/dko`; a stale object is built.
/// With `lenient`, a module whose object files are missing still loads (for `gen`); with `only`, the objects of these
/// content modules only are loaded or built (`pack` and `apply`: the others load without objects).
#[cfg(not(target_arch = "wasm32"))]
pub fn library_with(
    root: &std::path::Path,
    src: &LibrarySource,
    lenient: bool,
    only: Option<&[String]>,
) -> Result<Library> {
    // `--trace` builds the modules with DK_TRACE defined (C: -DDK_TRACE; SHARC+: `#ifdef DK_TRACE`)
    let defines: Vec<String> = if src.trace {
        vec!["DK_TRACE".into()]
    } else {
        vec![]
    };
    let mut lib = Library::scan_repo_defined(root, lenient, only, &defines)?;
    let mut extra: Vec<std::path::PathBuf> = src.modules_dirs.clone();
    if let Some(v) = std::env::var_os("PATCHKIT_MODULES") {
        extra.extend(std::env::split_paths(&v));
    }
    let build = Build {
        root,
        only,
        defines: &defines,
    };
    for d in extra {
        if d.is_dir() {
            lib.extend_unique(Library::scan_in(Some(build), &d, lenient)?.modules)?;
        }
    }
    lib.lower(&crate::typed::Models::from_dir(&root.join("devices"))?)?;
    Ok(lib)
}

/// Explicit manifests replace all discovery roots, including PATCHKIT_MODULES. They must include
/// the selected IDs, their requires closure and any allocation providers used by generation.
#[cfg(not(target_arch = "wasm32"))]
pub fn selected_library(
    root: &std::path::Path,
    src: &LibrarySource,
    lenient: bool,
    only: &[String],
    selected: &[String],
) -> Result<Library> {
    if src.manifests.is_empty() {
        return library_with(root, src, lenient, Some(only));
    }
    let defines = if src.trace {
        vec!["DK_TRACE".to_string()]
    } else {
        vec![]
    };
    let mut lib = Library::from_manifests(
        Build {
            root,
            only: Some(only),
            defines: &defines,
        },
        &src.manifests,
        lenient,
        selected,
    )?;
    lib.lower(&crate::typed::Models::from_dir(&root.join("devices"))?)?;
    if lenient && lib.get("core/machines").is_none() {
        for id in selected {
            let m = lib.get(id).unwrap();
            if !m.manifest.gens.is_empty() || !m.manifest.machine.is_empty() {
                return Err(Error(format!(
                    "{id}: generation requires --manifest for core/machines (machine allocations)"
                )));
            }
        }
    }
    Ok(lib)
}

#[cfg(not(target_arch = "wasm32"))]
impl Library {
    /// Every `module.toml` under `root` (a module root), without lowering.
    fn read_root(
        root: &std::path::Path,
        lenient: bool,
        build: Option<(&crate::build::toolchain::Env, Build)>,
    ) -> Result<Vec<Module>> {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            let Ok(rd) = std::fs::read_dir(dir) else {
                return;
            };
            let mut es: Vec<_> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
            es.sort();
            for p in es {
                if p.is_dir() {
                    walk(&p, out);
                } else if p.file_name().is_some_and(|n| n == "module.toml") {
                    out.push(p);
                }
            }
        }
        let mut found = vec![];
        walk(root, &mut found);
        Self::read_paths(&found, lenient, build)
    }

    /// Read only the named manifests, without walking module roots or lowering.
    fn read_paths(
        paths: &[std::path::PathBuf],
        lenient: bool,
        build: Option<(&crate::build::toolchain::Env, Build)>,
    ) -> Result<Vec<Module>> {
        let mut modules = vec![];
        for p in paths {
            let dir = p.parent().unwrap().to_path_buf();
            let text = std::fs::read_to_string(p).map_err(|e| err!("{}: {e}", p.display()))?;
            // a content module's objects come from the cache (built when stale); the others are files of the module
            let built = match build {
                Some((env, b)) => {
                    let m = crate::build::ModuleToml::parse(&text)
                        .map_err(|e| err!("{}: {e}", p.display()))?;
                    let allow = b.only.is_none_or(|ids| ids.iter().any(|i| i == m.id()));
                    Some((env, m, allow))
                }
                None => None,
            };
            let read = |rel: &str| -> Result<String> {
                if let Some((env, m, allow)) = &built
                    && m.builds(rel)
                {
                    if !allow {
                        if lenient {
                            return crate::build::read_cached_object(env, &dir, m, rel);
                        }
                        return Err(err!("{rel}: not loaded"));
                    }
                    return crate::build::load_object(env, &dir, m, rel);
                }
                std::fs::read_to_string(dir.join(rel))
                    .map_err(|e| err!("{}: {e}", dir.join(rel).display()))
            };
            let skip_missing = lenient || built.as_ref().is_some_and(|(_, _, allow)| !allow);
            let mut m = Module::from_parts_with(&text, &read, skip_missing)
                .map_err(|e| err!("{}: {e}", p.display()))?;
            m.dir = dir;
            modules.push(m);
        }
        Ok(modules)
    }

    /// Manifest metadata only: never read objects, source files or allocation tables.
    fn read_headers(paths: &[std::path::PathBuf]) -> Result<Library> {
        let mut headers = Library::default();
        for path in paths {
            let text =
                std::fs::read_to_string(path).map_err(|e| err!("{}: {e}", path.display()))?;
            crate::build::ModuleToml::parse(&text).map_err(|e| err!("{}: {e}", path.display()))?;
            let manifest = toml::from_str(&text).map_err(|e| err!("{}: {e}", path.display()))?;
            headers.extend_unique(vec![Module {
                manifest,
                objects: vec![],
                alloc: BTreeMap::new(),
                digest: String::new(),
                dir: path.parent().unwrap().to_path_buf(),
            }])?;
        }
        Ok(headers)
    }

    /// Validate every repository and test-fixture manifest and its requires graph without reading
    /// objects or lowering. Model, descriptor, allocation and firmware checks stay with pack/gen/apply.
    pub fn catalog_check(root: &std::path::Path) -> Result<usize> {
        fn walk(dir: &std::path::Path, paths: &mut Vec<std::path::PathBuf>) -> Result<()> {
            let rd = match std::fs::read_dir(dir) {
                Ok(rd) => rd,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(e) => return Err(err!("{}: {e}", dir.display())),
            };
            let mut entries = rd
                .map(|e| e.map(|e| e.path()))
                .collect::<std::io::Result<Vec<_>>>()
                .map_err(|e| err!("{}: {e}", dir.display()))?;
            entries.sort();
            for path in entries {
                if path.is_dir() {
                    walk(&path, paths)?;
                } else if path.file_name().is_some_and(|n| n == "module.toml") {
                    paths.push(path);
                }
            }
            Ok(())
        }
        let mut paths = vec![];
        walk(&root.join("modules"), &mut paths)?;
        walk(&root.join("tests/fixtures/modules"), &mut paths)?;
        let headers = Self::read_headers(&paths)?;
        for m in &headers.modules {
            m.manifest
                .validate_header()
                .and_then(|()| crate::prim::validate_manifest(&m.manifest))
                .map_err(|e| err!("{}: {e}", m.dir.join("module.toml").display()))?;
        }
        let ids: Vec<_> = headers.modules.iter().map(|m| m.id().to_string()).collect();
        headers.with_requires(&ids)?;
        Ok(headers.modules.len())
    }

    /// An explicit manifest library, without discovery or lowering. The caller supplies all required modules; the selection
    /// and dependency closure are validated before loading objects. Paths are used as given (relative to the working directory).
    pub fn from_manifests(
        build: Build,
        paths: &[std::path::PathBuf],
        lenient: bool,
        selected: &[String],
    ) -> Result<Library> {
        // Validate the complete explicit selection before any objects are read or built.
        let headers = Self::read_headers(paths)?;
        headers.with_requires(selected)?;
        // Support manifests (for example core/machines allocations) are explicit inputs too;
        // validate their requirements rather than silently accepting a partial library.
        let ids: Vec<_> = headers.modules.iter().map(|m| m.id().to_string()).collect();
        headers.with_requires(&ids)?;
        let closure = build
            .only
            .map(|ids| headers.with_requires(ids).map(|(all, _)| all))
            .transpose()?;
        let build = Build {
            only: closure.as_deref(),
            ..build
        };
        let env = crate::build::toolchain::Env::new(build.root).with_defines(build.defines);
        let mut lib = Library::default();
        lib.extend_unique(Self::read_paths(paths, lenient, Some((&env, build)))?)?;
        Ok(lib)
    }

    /// Add modules; refuses an id that is already there (the error names both directories).
    pub fn extend_unique(&mut self, more: Vec<Module>) -> Result<()> {
        for m in more {
            if let Some(o) = self.get(m.id()) {
                return Err(err!(
                    "duplicate module id {}: {} and {}",
                    m.id(),
                    o.dir.display(),
                    m.dir.display()
                ));
            }
            self.modules.push(m);
        }
        Ok(())
    }

    /// The default module roots of a repository: `modules/` and `tests/fixtures/modules/`, lowered with `devices/`. Content modules whose
    /// object is stale in the cache are built (it needs the toolchain: `uv run python -m tools.patchkit.pk_toolchains m68k`).
    pub fn scan_repo(root: &std::path::Path) -> Result<Library> {
        Self::scan_repo_with(root, false)
    }

    /// `scan_repo` for tests: building the content modules needs the m68k-elf toolchain (`uv run python -m tools.patchkit.pk_toolchains
    /// m68k`). Without it the test cannot run: this prints why and returns `None`. Any other error panics.
    pub fn scan_repo_or_skip(root: &std::path::Path) -> Option<Library> {
        Self::scan_repo_for_or_skip(root, None)
    }

    /// [`Library::scan_repo_or_skip`] that loads (and builds) the objects of the content modules `only` names (see [`Build::only`]).
    pub fn scan_repo_for_or_skip(
        root: &std::path::Path,
        only: Option<&[String]>,
    ) -> Option<Library> {
        if crate::build::toolchain::Env::new(root).m68k("gcc").is_err() {
            eprintln!(
                "SKIP: the m68k-elf toolchain is not installed, so the content modules cannot be built (uv run python -m tools.patchkit.pk_toolchains m68k)"
            );
            return None;
        }
        Some(Self::scan_repo_for(root, false, only).unwrap_or_else(|e| panic!("{e}")))
    }

    /// `scan_repo`; with `lenient`, a missing object file is left out (see `Module::from_parts_lenient`).
    pub fn scan_repo_with(root: &std::path::Path, lenient: bool) -> Result<Library> {
        Self::scan_repo_for(root, lenient, None)
    }

    /// `scan_repo_with`; with `only`, the objects of these content modules only are loaded (see [`Build::only`]).
    pub fn scan_repo_for(
        root: &std::path::Path,
        lenient: bool,
        only: Option<&[String]>,
    ) -> Result<Library> {
        Self::scan_repo_defined(root, lenient, only, &[])
    }

    /// [`Library::scan_repo_for`] with build `defines` for the objects it builds (a traced build: `DK_TRACE`).
    pub fn scan_repo_defined(
        root: &std::path::Path,
        lenient: bool,
        only: Option<&[String]>,
        defines: &[String],
    ) -> Result<Library> {
        let env = crate::build::toolchain::Env::new(root).with_defines(defines);
        let b = Build {
            root,
            only,
            defines,
        };
        let mut lib = Library::default();
        lib.extend_unique(Self::read_root(
            &root.join("modules"),
            lenient,
            Some((&env, b)),
        )?)?;
        lib.extend_unique(Self::read_root(
            &root.join("tests/fixtures/modules"),
            lenient,
            Some((&env, b)),
        )?)?;
        lib.lower(&crate::typed::Models::from_dir(&root.join("devices"))?)?;
        Ok(lib)
    }

    /// Every `module.toml` under `root` (a module root); lowered when `devices/` sits next to it. Objects are only read from files.
    pub fn scan(root: &std::path::Path) -> Result<Library> {
        Self::scan_with(root, false)
    }

    /// `scan`; with `lenient`, a missing object file is left out (see `Module::from_parts_lenient`).
    pub fn scan_with(root: &std::path::Path, lenient: bool) -> Result<Library> {
        Self::scan_in(None, root, lenient)
    }

    /// `scan_with`; with `build`, the content modules of the root get their objects from the cache like in `scan_repo_for`.
    pub fn scan_in(build: Option<Build>, root: &std::path::Path, lenient: bool) -> Result<Library> {
        let env = build.map(|b| crate::build::toolchain::Env::new(b.root).with_defines(b.defines));
        let mut lib = Library::default();
        lib.extend_unique(Self::read_root(root, lenient, env.as_ref().zip(build))?)?;
        // the device models sit next to `modules/` (`devices/*.toml`); an extra module root without them
        // is lowered by the caller
        let devices = root.parent().map(|p| p.join("devices"));
        if let Some(d) = devices.filter(|d| d.is_dir()) {
            lib.lower(&crate::typed::Models::from_dir(&d)?)?;
        }
        Ok(lib)
    }
}
#[cfg(all(test, not(target_arch = "wasm32")))]
mod cache_generation_tests {
    use super::*;

    #[test]
    fn core_generation_matches_explicit_core_manifest_catalog() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let started = std::time::Instant::now();
        // Match gen's first pass: read existing cache declarations, never check freshness or build.
        let legacy = Library::scan_repo_defined(&root, true, Some(&[]), &[]).unwrap();
        let core: Vec<_> = legacy
            .modules
            .iter()
            .filter(|m| m.dir.starts_with(root.join("modules/core")))
            .collect();
        assert!(!core.is_empty());
        let paths: Vec<_> = core.iter().map(|m| m.dir.join("module.toml")).collect();
        let ids: Vec<_> = core.iter().map(|m| m.id().to_string()).collect();
        // This also verifies every required module is included in the explicit core set.
        let mut explicit = Library::from_manifests(
            Build {
                root: &root,
                only: Some(&[]),
                defines: &[],
            },
            &paths,
            true,
            &ids,
        )
        .unwrap();
        let models = crate::typed::Models::from_dir(&root.join("devices")).unwrap();
        let facts = crate::devgen::Facts::from_dir(&root.join("devices")).unwrap();
        explicit.lower(&models).unwrap();
        let generate = |lib: &Library, id: &str| {
            let mut files = crate::modgen::generate(lib, &models, id).unwrap();
            files.extend(crate::devgen::generate(lib, &facts, id).unwrap());
            files.sort();
            files
        };
        let mut count = 0;
        for id in &ids {
            let baseline = generate(&legacy, id);
            assert_eq!(
                baseline,
                generate(&explicit, id),
                "core generation differs for {id}"
            );
            count += baseline.len();
            println!(
                "{id}: {}",
                baseline
                    .iter()
                    .map(|(path, _)| path.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        // The content pass cannot add a gallery for these core modules: all their objects were
        // already read in both libraries above, because Build::only filters content modules only.
        assert!(count > 0);
        println!(
            "compared {} core modules, {count} generated files in {:?}",
            ids.len(),
            started.elapsed()
        );
    }
}

#[cfg(test)]
mod manifest_tests {
    use super::*;

    const HEAD: &str = "[module]\nid = \"test/m\"\nversion = \"0.1.0\"\nname = \"M\"\ncategory = \"test\"\ntype = \"machine\"\nauthor = \"digikit\"\nstatus = \"experimental\"\nsummary = \"s\"\n";

    fn load(extra: &str) -> Result<Manifest> {
        let m: Manifest = toml::from_str(&format!("{HEAD}{extra}"))?;
        m.validate_header()?;
        Ok(m)
    }

    #[test]
    fn machine_fx_and_uses_are_read() {
        let m = load("[[machine]]\nid = \"analog\"\n[[machine]]\nid = \"b-2_x\"\n[[fx]]\nid = \"fshift\"\n[uses.dt2]\nmachine_rows = 2\npage_rows = 3\nrecord_bytes = 700\ndsp_cost_voice = 6239\ndsp_cost_fx = 4200\n[uses.dn2]\nmachine_rows = 2\npage_rows = 4\n").unwrap();
        let ids = |v: &[IdRef]| v.iter().map(|r| r.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&m.machines), ["analog", "b-2_x"]);
        assert_eq!(ids(&m.fxs), ["fshift"]);
        assert_eq!(
            m.uses.for_device("dt2"),
            Uses {
                machine_rows: 2,
                param_rows: 0,
                page_rows: 3,
                record_bytes: 700,
                dsp_cost_voice: 6239,
                dsp_cost_fx: 4200
            }
        );
        assert_eq!(m.uses.for_device("dn2").page_rows, 4);
        let m = load("[uses.dn2]\nparam_rows = 1\n").unwrap();
        assert_eq!(m.uses.dt2, None);
        assert_eq!(m.uses.for_device("dt2"), Uses::default());
    }

    #[test]
    fn a_dsp_prefix_is_optional_and_checked() {
        let m = load("[[machine]]\nid = \"sine\"\ndsp = \"sn\"\n[[fx]]\nid = \"a-b\"\n").unwrap();
        assert_eq!(m.machines[0].dsp_prefix().0, "sn");
        assert_eq!(m.fxs[0].dsp_prefix().0, "a_b");
        for bad in ["Sn", "s-n", "", "s n"] {
            let e = load(&format!("[[machine]]\nid = \"a\"\ndsp = \"{bad}\"\n")).unwrap_err();
            assert!(e.to_string().contains("dsp"), "{bad}: {e}");
        }
    }

    #[test]
    fn no_tables_parse_as_before() {
        let m = load("").unwrap();
        assert!(m.machines.is_empty() && m.fxs.is_empty());
        assert_eq!(m.uses, UsesByDevice::default());
    }

    #[test]
    fn duplicate_and_bad_ids_are_refused() {
        let e = load("[[fx]]\nid = \"a\"\n[[fx]]\nid = \"a\"\n").unwrap_err();
        assert!(e.to_string().contains("twice"), "{e}");
        let e = load("[[machine]]\nid = \"Analog\"\n").unwrap_err();
        assert!(e.to_string().contains("lowercase"), "{e}");
    }

    #[test]
    fn other_keys_are_refused() {
        let e = load("[[machine]]\nid = \"a\"\nname = \"A\"\n").unwrap_err();
        assert!(e.to_string().contains("unknown field"), "{e}");
        let e = load("[uses.dn2]\nvoices = 1\n").unwrap_err();
        assert!(e.to_string().contains("unknown field"), "{e}");
        // one table for all devices, and a device that does not exist
        let e = load("[uses]\nmachine_rows = 1\n").unwrap_err();
        assert!(e.to_string().contains("unknown field"), "{e}");
        let e = load("[uses.dt3]\nmachine_rows = 1\n").unwrap_err();
        assert!(e.to_string().contains("unknown field"), "{e}");
    }
}
