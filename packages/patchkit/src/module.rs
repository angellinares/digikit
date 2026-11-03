//! Module manifests (`module.toml`) and the library of modules found under `modules/`.

use crate::dko::Dko;
use crate::error::*;
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Deserialize)]
pub struct Manifest {
    #[serde(skip)]
    pub sample_bank: Option<crate::typed::SampleBankDef>,
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
    pub voice: Vec<crate::typed::VoiceDef>,
    /// Removed forms (`[[fx]]`, `[[machine]]`, `[[param]]`, `[[page]]`, `[[voice]]`): kept only to refuse them with a clear error.
    #[serde(default)]
    fx: Option<toml::Value>,
    #[serde(default, rename = "machine")]
    machine_old: Option<toml::Value>,
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
    /// Set by `Library::lower`: the state block of each patched-in contract voice, by machine id.
    #[serde(skip)]
    pub voice_state: BTreeMap<String, crate::typed::VoiceState>,
}

/// One generated file of device data: `kind` is `rust` (constants every device agrees on, one file), `gas-shared` (the same as `.equ` lines), `gas` (a ColdFire assembler
/// include, `.equ` lines, one file per device), `sharc` (a SHARC+ include, `#define` lines, one file per device) `sharc-adapter` (the `ADM_*` defines of `[adapter.machine]`, one file per device), `gas-sample-pool` or `sharc-sample-pool` (the `SP_*` numbers of `[adapter.sample_pool]` for ColdFire and SHARC+ assembly, one file per device). `path` is relative
/// to the module; `{dev}` stands for the device's short name in a per-device path.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenDef {
    pub kind: String,
    pub path: String,
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
    /// Names of Api entries (`[api]` of sdk/sdk.toml) that the module calls and the packer cannot see in the file. `pack` declares the
    /// newest `since` of them as the Api minor, so a runtime without the entry refuses the module at load, and a runtime that has it
    /// accepts it. Calls a module makes only after it checked `api->size` need no entry here.
    #[serde(default)]
    pub uses_api: Vec<String>,
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
    /// `write` or `hook`
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

/// A patched-in contract voice (`[[voice]]` with `desc`) needs memory for the state of its voices, which a `.dkm` gets from the runtime at load
/// (`dkmdsp.rs`). Here the module's DSP object gets a bss section `dk.state.<machine>` of `voices` x `stride` bytes and the global symbol
/// `_dkstate_<machine>`, which the voice row names as its state base; the patch linker places it with the rest of the object. `desc` is read
/// from the object's data (`dk_machine_desc`, little endian: voices at word 3, state_bytes at 4, state_clear_bytes at 5).
fn reserve_voice_state(m: &mut Module) -> Result<()> {
    let voices: Vec<(String, String)> = m
        .manifest
        .voice
        .iter()
        .map(|v| (v.machine.clone(), v.desc.clone()))
        .collect();
    for (machine, desc) in voices {
        if m.manifest.voice_state.contains_key(&machine) {
            continue;
        }
        let found = m.objects.iter().position(|o| {
            o.arch == "sharc"
                && o.symbols
                    .iter()
                    .any(|s| s.name == desc && s.section.is_some())
        });
        let Some(oi) = found else {
            return Err(err!(
                "voice {machine}: desc {desc} is not defined by a DSP object of the module"
            ));
        };
        let o = &mut m.objects[oi];
        let sym = o.symbols.iter().find(|s| s.name == desc).unwrap().clone();
        let sec = o
            .sections
            .iter()
            .find(|x| Some(&x.name) == sym.section.as_ref())
            .unwrap();
        if sec.kind != "data" {
            return Err(err!("voice {machine}: desc {desc} is not initialised data"));
        }
        let bytes = unhex(&sec.hex)?;
        let at = sym.value as usize;
        let words = bytes
            .get(at..at + crate::dkmdsp::DESC_WORDS * 4)
            .ok_or_else(|| {
                err!(
                    "voice {machine}: desc {desc} is shorter than {} bytes",
                    crate::dkmdsp::DESC_WORDS * 4
                )
            })?;
        let w = |i: usize| u32::from_le_bytes(words[i * 4..i * 4 + 4].try_into().unwrap());
        if w(0) != crate::dkmdsp::DESC_MAGIC
            || w(1) & !crate::dkmdsp::DESC_STEREO != crate::dkmdsp::DESC_CONTRACT
        {
            return Err(err!(
                "voice {machine}: desc {desc} is no dk_machine_desc of contract {}",
                crate::dkmdsp::DESC_CONTRACT
            ));
        }
        let (voices_n, state, clear) = (w(3), w(4), w(5));
        if clear > state || voices_n == 0 {
            return Err(err!(
                "voice {machine}: desc {desc}: {voices_n} voices, state {state} bytes, clear {clear}"
            ));
        }
        let stride = state.div_ceil(8) * 8;
        let name = format!("dk.state.{machine}");
        let symbol = format!("_dkstate_{machine}");
        o.sections.push(crate::dko::DkoSection {
            name: name.clone(),
            kind: "bss".into(),
            align: 8,
            size: voices_n * stride,
            hex: String::new(),
        });
        o.symbols.push(crate::dko::DkoSymbol {
            name: symbol.clone(),
            section: Some(name),
            value: 0,
            global: true,
        });
        m.manifest.voice_state.insert(
            machine,
            crate::typed::VoiceState {
                symbol,
                stride,
                clear,
            },
        );
    }
    Ok(())
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
        let mut manifest: Manifest = toml::from_str(manifest_text)?;
        if manifest.module.category == "core" && manifest.module.role.is_none() {
            return Err(err!(
                "{}: a core module needs `role = \"runtime\" | \"installer\" | \"adapter\" | \"app\"` in [module]",
                manifest.module.id
            ));
        }
        if manifest.fx.is_some() {
            return Err(err!(
                "[[fx]] is no longer read: declare effects with DK_FX in C (sdk/include/dk/fx.h)"
            ));
        }
        for (name, old) in [
            ("machine", &manifest.machine_old),
            ("param", &manifest.param_old),
            ("page", &manifest.page_old),
            ("voice", &manifest.voice_old),
        ] {
            if old.is_some() {
                return Err(err!(
                    "[[{name}]] is no longer read: declare machines with DK_MACHINE in C (sdk/include/dk/machine.h)"
                ));
            }
        }
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
        // the descriptor records of the objects fill the typed sections; a module loaded without all its objects has none
        if objects.len() == manifest.object.len() {
            let d = crate::desc::read_objects(&objects)
                .map_err(|e| err!("{}: {e}", manifest.module.id))?;
            manifest.machine = d.machines;
            manifest.param = d.params;
            manifest.page = d.pages;
            manifest.voice = d.voices;
            manifest.sample_bank = d.sample_bank;
            manifest.sample_tracks = d.sample_tracks;
            // a patched-in module keeps the records out of the image: the objects lose their descriptor sections
            if !manifest.module.dynamic {
                // an object that held only records is removed with its manifest entry (the two lists stay aligned)
                let section = crate::sdkgen::desc_spec().section;
                let mut kept = vec![];
                let mut kept_refs = vec![];
                for (o, r) in objects.drain(..).zip(manifest.object.drain(..)) {
                    if o.sections.iter().any(|s| s.name == section) {
                        match crate::desc::strip(&o) {
                            Some(s) => {
                                kept.push(s);
                                kept_refs.push(r);
                            }
                            None => {}
                        }
                    } else {
                        kept.push(o);
                        kept_refs.push(r);
                    }
                }
                objects = kept;
                manifest.object = kept_refs;
            }
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

    /// Lower the typed sections of every module with the device models (`crate::typed`). A patched-in module with contract voices first gets
    /// the state blocks of its voices (`reserve_voice_state`).
    pub fn lower(&mut self, models: &crate::typed::Models) -> Result<()> {
        for m in &mut self.modules {
            // a module loaded without its objects (`from_parts_lenient`) has no descriptors to read
            if !m.manifest.module.dynamic
                && !m.manifest.lowered
                && m.objects.len() == m.manifest.object.len()
            {
                reserve_voice_state(m).map_err(|e| err!("{}: {e}", m.manifest.module.id))?;
            }
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
        let mut modules = vec![];
        for p in found {
            let dir = p.parent().unwrap().to_path_buf();
            let text = std::fs::read_to_string(&p).map_err(|e| err!("{}: {e}", p.display()))?;
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
    fn generation_reads_cached_declarations_without_building_and_tolerates_missing_cache() {
        let root = std::env::temp_dir().join(format!("digikit-cached-gen-{}", std::process::id()));
        let dir = root.join("modules").join("new-machine");
        let cache = root.join("out/cache/dko/test/new-machine/build/module.dko.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
        // No sources or toolchains exist here: either a freshness check or a
        // build would fail. Generation must use the existing cache only.
        std::fs::write(dir.join("module.toml"), r#"
[module]
id = "test/new-machine"
version = "0.1.0"
name = "New machine"
category = "test"
type = "machine"
author = "digikit"
status = "experimental"
summary = "Cached generation fixture."
dynamic = true
[[object]]
path = "build/module.dko.json"
sources = ["missing.c"]
place = "cf.expansion"
"#).unwrap();
        let spec = crate::sdkgen::desc_spec();
        let rec = spec.records.iter().find(|r| r.name == "dk_desc_machine").unwrap();
        let mut bytes = vec![0u8; rec.size];
        for (field, word) in [("kind", rec.kind.unwrap()), ("size", rec.size as u32)] {
            let at = rec.field(field).unwrap().off;
            bytes[at..at + 4].copy_from_slice(&word.to_be_bytes());
        }
        for (field, value) in [("id", "new"), ("name", "New machine"), ("short_name", "NEW"),
            ("render", "_render"), ("note_on", "_note_on"), ("desc", "_desc")] {
            let at = rec.field(field).unwrap().off;
            bytes[at..at + value.len()].copy_from_slice(value.as_bytes());
        }
        let object = serde_json::json!({
            "format": crate::dko::FORMAT, "arch": "cf", "source_sha256": "stale",
            "sections": [{ "name": spec.section, "kind": "data", "align": 4,
                "size": bytes.len(), "hex": hex(&bytes) }], "symbols": [], "relocs": []
        }).to_string();
        std::fs::write(&cache, &object).unwrap();
        let env = crate::build::toolchain::Env::new(&root);
        let build = Build { root: &root, only: Some(&[]), defines: &[] };
        let models = crate::typed::Models::from_texts([
            include_str!("../../../devices/digitakt-ii.toml"),
            include_str!("../../../devices/digitone-ii.toml"),
        ]).unwrap();
        let load = || Library {
            modules: Library::read_root(&root.join("modules"), true, Some((&env, build))).unwrap(),
        };
        let lib = load();
        let files = crate::modgen::generate(&lib, &models, "test/new-machine").unwrap();
        assert_eq!(files[0].0, "build/gen/gallery.json");
        let gallery: serde_json::Value = serde_json::from_str(&files[0].1).unwrap();
        assert_eq!(gallery["machines"][0]["id"], "new");
        assert_eq!(gallery["machines"][0]["name"], "New machine");
        assert_eq!(std::fs::read_to_string(&cache).unwrap(), object);
        // Excluded modules in a pack still omit objects even if cache exists.
        let excluded = Library::read_root(&root.join("modules"), false, Some((&env, build))).unwrap();
        assert!(excluded[0].objects.is_empty());
        std::fs::remove_file(&cache).unwrap();
        let lib = load();
        assert!(crate::modgen::generate(&lib, &models, "test/new-machine").unwrap().is_empty());
        assert!(!cache.exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
