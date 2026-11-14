//! The sections of a run-time module (docs/plans/sdk.md 6.1, 6.3): no `.dk.desc` records. The keys table comes from `module.toml`
//! (`[[machine]]`, `[[fx]]`, `[uses.<device>]`) and the DSP entry points are found by name in the DSP object.
//!
//! The names are the toolchain's symbols, with its leading underscore. The prefix `x` is the id with `-` as `_`, or `dsp` of the `[[machine]]` or
//! `[[fx]]` entry of module.toml when DSP code exists under another prefix (the id stays what the keys hash):
//! machine `x`: `_x_render`, `_x_note_on` (global text), optional `_x_desc` (global data, a `dk_machine_desc`: state bytes, stereo, gate);
//! effect `y`: `_y_process` (global text), `_y_desc` (global data, a `dk_fx_desc`);
//! sample pool of machine `x`: `_x_sample_rec` and `_x_sample_ctl` (both or none; one machine of a module has the pool).

use crate::dkmdsp::{self, FxSpec, VoiceSpec};
use crate::dko::Dko;
use crate::error::*;
use crate::module::Module;
use crate::typed::SampleTracksDef;
use std::collections::BTreeMap;

#[derive(Debug)]
pub struct Sections {
    /// The keys table of the header (`dkneeds::keys`).
    pub keys: Vec<u8>,
    pub dsp: Vec<u8>,
    /// A stereo machine (a desc with `DESC_STEREO`): machine contract 1.2.
    pub stereo: bool,
    /// A machine that reads `gate`: machine contract 1.6.
    pub gate: bool,
}

fn defined(obj: &Dko, name: &str, kind: &str) -> bool {
    obj.symbols.iter().any(|s| {
        s.name == name
            && s.global
            && s.section.as_ref().is_some_and(|sec| {
                obj.sections
                    .iter()
                    .any(|x| &x.name == sec && x.kind == kind)
            })
    })
}

fn require(obj: &Dko, module: &str, what: &str, name: &str, kind: &str) -> Result<()> {
    if defined(obj, name, kind) {
        Ok(())
    } else {
        Err(err!(
            "{module}: {what} needs the DSP symbol {name} (global {kind} of the DSP object), which is not defined"
        ))
    }
}

/// The contract word of a `dk_machine_desc` in the linked DSP object: stereo.
fn desc_flags(obj: &Dko, desc: &str) -> Result<bool> {
    let mut imports = std::collections::BTreeSet::new();
    for r in &obj.relocs {
        if !obj
            .symbols
            .iter()
            .any(|s| s.name == r.symbol && s.section.is_some())
        {
            imports.insert(r.symbol.clone());
        }
    }
    let externs: BTreeMap<String, u64> = imports.into_iter().map(|n| (n, 0)).collect();
    let linked = crate::link::link(obj, crate::dsp::L2_BYTE_BASE as u64, &externs)?;
    let at = linked
        .symbols
        .get(desc)
        .and_then(|a| a.checked_sub(crate::dsp::L2_BYTE_BASE as u64))
        .ok_or_else(|| err!("{desc}: not in the DSP image"))? as usize;
    let b = linked
        .image
        .get(at + 4..at + 8)
        .ok_or_else(|| err!("{desc}: shorter than a machine descriptor"))?;
    let w = u32::from_le_bytes(b.try_into().unwrap());
    Ok(w & dkmdsp::DESC_STEREO != 0)
}

/// The header table (`dkneeds::keys`) of a module for `device`: the `[uses]` counts, the machine and effect keys and the keys of the `[[record]]` entries (generic
/// kinds), grouped by kind in the order of the kind numbers. The packer adds the needs of the linked imports.
fn header_table(
    mf: &crate::module::Manifest,
    device: &str,
    mk: Vec<u32>,
    fk: Vec<u32>,
) -> Result<Vec<u8>> {
    let id = &mf.module.id;
    let u = mf.uses.for_device(device);
    let rows = (
        u32::from(u.machine_rows),
        u32::from(u.param_rows),
        u32::from(u.page_rows),
    );
    // the keys by kind: machines, effects, and the records of generic kinds (`[[record]]`), in the order of the kind numbers
    let mut groups: std::collections::BTreeMap<u32, Vec<u32>> = std::collections::BTreeMap::new();
    if !mk.is_empty() {
        groups.insert(dkneeds::KIND_MACHINE, mk);
    }
    if !fk.is_empty() {
        groups.insert(dkneeds::KIND_EFFECT, fk);
    }
    for r in &mf.records {
        let kind = dkneeds::KIND_NAMES
            .iter()
            .find(|(n, _)| *n == r.kind)
            .map(|k| k.1)
            .ok_or_else(|| {
                err!(
                    "{id}: [[record]] kind {:?} is not in [records.kinds] of sdk/sdk.toml",
                    r.kind
                )
            })?;
        if !dkneeds::GENERIC_KINDS.contains(&kind) {
            return Err(err!(
                "{id}: [[record]] kind {:?} is not generic: its records come from builder calls",
                r.kind
            ));
        }
        let keys = groups.entry(kind).or_default();
        if keys.contains(&r.key) {
            return Err(err!(
                "{id}: [[record]] kind {:?} key {} twice",
                r.kind,
                r.key
            ));
        }
        keys.push(r.key);
    }
    let key_groups: Vec<dkneeds::keys::KeyGroup> = groups
        .iter()
        .map(|(kind, keys)| dkneeds::keys::KeyGroup { kind: *kind, keys })
        .collect();
    let uses: Vec<dkneeds::keys::Use> = [
        (dkneeds::RES_MACHINE_ROWS, rows.0),
        (dkneeds::RES_PARAM_ROWS, rows.1),
        (dkneeds::RES_PAGE_ROWS, rows.2),
        (dkneeds::RES_RECORD_BYTES, u32::from(u.record_bytes)),
        (dkneeds::RES_DSP_COST_VOICE, u.dsp_cost_voice),
        (dkneeds::RES_DSP_COST_FX, u.dsp_cost_fx),
    ]
    .into_iter()
    .filter(|(_, count)| *count != 0)
    .map(|(resource, count)| dkneeds::keys::Use { resource, count })
    .collect();
    let table = dkneeds::keys::Table {
        uses: &uses,
        keys: &key_groups,
        ..Default::default()
    };
    let mut keys = vec![0u8; dkneeds::keys::table_size(&table)];
    dkneeds::keys::write_table(&table, &mut keys).map_err(|e| err!("{id}: header table: {e:?}"))?;
    Ok(keys)
}

/// The keys table and the DSP section of a new-style module for `device`. `name` is the packed module name (`pack --name`, else the module id): the keys hash it with
/// the `[[machine]]` and `[[fx]]` ids, so an alias made with another name has keys of its own. `dsp_obj` is its DSP object (none: refused, a machine or an effect
/// needs one).
pub fn sections(m: &Module, name: &str, device: &str, dsp_obj: Option<&Dko>) -> Result<Sections> {
    let mf = &m.manifest;
    let id = &mf.module.id;
    let Some(obj) = dsp_obj else {
        if mf.machines.is_empty() && mf.fxs.is_empty() {
            // a tool or a service: no machine or effect keys, no DSP section (a header table with the records and the uses it has)
            return Ok(Sections {
                keys: header_table(mf, device, vec![], vec![])?,
                dsp: vec![],
                stereo: false,
                gate: false,
            });
        }
        return Err(err!(
            "{id}: a machine or an effect needs a DSP object for {device}"
        ));
    };
    let mut voices = vec![];
    let mut fxs = vec![];
    let mut stereo = false;
    let mut tracks: Option<SampleTracksDef> = None;
    for x in &mf.machines {
        let (c, src) = x.dsp_prefix();
        let what = format!("machine {:?} (DSP symbol prefix {c:?} from {src})", x.id);
        let render = format!("_{c}_render");
        let note_on = format!("_{c}_note_on");
        require(obj, id, &what, &render, "text")?;
        require(obj, id, &what, &note_on, "text")?;
        let desc = format!("_{c}_desc");
        let desc = defined(obj, &desc, "data").then_some(desc);
        let mut st = false;
        if let Some(d) = &desc {
            st = desc_flags(obj, d)?;
            stereo |= st;
        }
        voices.push(VoiceSpec {
            type_no: crate::register::machine_key(name, &x.id),
            render,
            note_on: Some(note_on),
            desc,
            stereo: st,
            latched: 0,
            ranges: [0; 8],
        });
        let (rec, ctl) = (format!("_{c}_sample_rec"), format!("_{c}_sample_ctl"));
        if defined(obj, &rec, "bss")
            || defined(obj, &rec, "data")
            || defined(obj, &ctl, "bss")
            || defined(obj, &ctl, "data")
        {
            let kind = |n: &str| {
                if defined(obj, n, "bss") {
                    "bss"
                } else {
                    "data"
                }
            };
            require(
                obj,
                id,
                &format!("the sample pool of {what}"),
                &rec,
                kind(&rec),
            )?;
            require(
                obj,
                id,
                &format!("the sample pool of {what}"),
                &ctl,
                kind(&ctl),
            )?;
            if tracks.is_some() {
                return Err(err!("{id}: only one machine of a module has a sample pool"));
            }
            // `local_id` named a table of the module; a module has one table (`dk_sample_pool_open` takes the lowest id), so it is 0
            tracks = Some(SampleTracksDef {
                local_id: 0,
                records: rec,
                control: ctl,
            });
        }
    }
    for y in &mf.fxs {
        let (c, src) = y.dsp_prefix();
        let what = format!("effect {:?} (DSP symbol prefix {c:?} from {src})", y.id);
        let process = format!("_{c}_process");
        let desc = format!("_{c}_desc");
        require(obj, id, &what, &process, "text")?;
        require(obj, id, &what, &desc, "data")?;
        // the routes and the defaults come from `dk_fx_routes` and the page parameters at commit; the section needs a valid pair here
        fxs.push(FxSpec {
            process,
            desc,
            defaults: vec![],
            routes: dkmdsp::ROUTE_MASTER | dkmdsp::ROUTE_SEND,
            route: dkmdsp::ROUTE_MASTER,
        });
    }
    let (dsp, gate) = dkmdsp::pack_with_gate(
        obj,
        crate::register::device_id(device)?,
        &voices,
        &fxs,
        tracks.as_ref(),
    )?;
    let mk: Vec<u32> = mf
        .machines
        .iter()
        .map(|x| crate::register::machine_key(name, &x.id))
        .collect();
    let fk: Vec<u32> = mf
        .fxs
        .iter()
        .map(|y| crate::register::effect_id(name, &y.id))
        .collect();
    let keys = header_table(mf, device, mk, fk)?;
    Ok(Sections {
        keys,
        dsp,
        stereo,
        gate,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dko::{DkoSection, DkoSymbol};

    const HEAD: &str = "[module]\nid = \"test/m\"\nversion = \"0.1.0\"\nname = \"M\"\ncategory = \"test\"\ntype = \"machine\"\nauthor = \"digikit\"\nstatus = \"experimental\"\nsummary = \"s\"\n";

    fn module(extra: &str) -> Module {
        let manifest: crate::module::Manifest = toml::from_str(&format!("{HEAD}{extra}")).unwrap();
        Module {
            manifest,
            objects: vec![],
            alloc: Default::default(),
            digest: String::new(),
            dir: Default::default(),
        }
    }

    fn sym(name: &str, section: &str, value: u64) -> DkoSymbol {
        DkoSymbol {
            name: name.into(),
            section: Some(section.into()),
            value,
            global: true,
        }
    }

    /// A DSP object with the given global text symbols (one parcel apart), a data section holding a machine desc with `contract` and
    /// the given global data symbols, and bss.
    fn dsp_object(text: &[&str], data: &[&str], bss: &[&str], contract: u32) -> Dko {
        let words = [dkmdsp::DESC_MAGIC, contract, 1, 1, 8, 8, 0, 0];
        let desc: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
        let mut symbols = vec![];
        for (i, n) in text.iter().enumerate() {
            symbols.push(sym(n, "t", 3 * i as u64));
        }
        for n in data {
            symbols.push(sym(n, "d", 0));
        }
        for (i, n) in bss.iter().enumerate() {
            symbols.push(sym(n, "b", 64 * i as u64));
        }
        Dko {
            format: crate::dko::FORMAT,
            arch: "sharc".into(),
            source_sha256: "0".repeat(64),
            toolchain: Default::default(),
            sections: vec![
                DkoSection {
                    name: "t".into(),
                    kind: "text".into(),
                    align: 8,
                    size: 48,
                    hex: hex(&[0u8; 48]),
                },
                DkoSection {
                    name: "d".into(),
                    kind: "data".into(),
                    align: 4,
                    size: desc.len() as u32,
                    hex: hex(&desc),
                },
                DkoSection {
                    name: "b".into(),
                    kind: "bss".into(),
                    align: 4,
                    size: 256,
                    hex: String::new(),
                },
            ],
            symbols,
            relocs: vec![],
        }
    }

    const MACHINE: &str = "[[machine]]\nid = \"foo\"\n[uses.dn2]\nmachine_rows = 1\nparam_rows = 4\npage_rows = 1\nrecord_bytes = 560\ndsp_cost_voice = 6239\n";

    #[test]
    fn a_tool_without_tables_has_a_head_only_header_table_and_no_dsp() {
        let s = sections(&module(""), "test/m", "dn2", None).unwrap();
        assert_eq!(s.keys.len(), dkneeds::keys::HEAD);
        assert!(s.dsp.is_empty());
    }

    #[test]
    fn a_record_entry_puts_its_key_in_the_keys_entry_and_a_typed_or_unknown_kind_is_refused() {
        let m = module("[[record]]\nkind = \"effect\"\nkey = 1\n");
        let e = sections(&m, "test/m", "dn2", None).unwrap_err().to_string();
        assert!(e.contains("not generic"), "{e}");
        let m = module("[[record]]\nkind = \"nothing\"\nkey = 1\n");
        let e = sections(&m, "test/m", "dn2", None).unwrap_err().to_string();
        assert!(e.contains("not in [records.kinds]"), "{e}");
    }

    #[test]
    fn the_keys_table_and_the_dsp_entries_come_from_the_manifest_and_the_names() {
        let m = module(&format!("{MACHINE}[[fx]]\nid = \"bar-x\"\n"));
        let obj = dsp_object(
            &["_foo_render", "_foo_note_on", "_bar_x_process"],
            &["_foo_desc", "_bar_x_desc"],
            &[],
            dkmdsp::DESC_CONTRACT | dkmdsp::DESC_STEREO,
        );
        // the effect's desc must be a dk_fx_desc: the machine desc is not
        let e = sections(&m, "test/m", "dn2", Some(&obj))
            .unwrap_err()
            .to_string();
        assert!(e.contains("dk_fx_desc") || e.contains("DKFX"), "{e}");
        let m = module(MACHINE);
        let s = sections(&m, "test/m", "dn2", Some(&obj)).unwrap();
        assert!(s.stereo);
        let k = dkneeds::keys::Keys::parse(&s.keys).unwrap();
        assert_eq!((k.machine_rows(), k.param_rows(), k.page_rows()), (1, 4, 1));
        assert_eq!(k.record_bytes(), 560);
        assert_eq!((k.dsp_cost_voice(), k.dsp_cost_fx()), (6239, 0));
        assert!(
            k.machine_keys()
                .eq([crate::register::machine_key("test/m", "foo")])
        );
        assert_eq!(k.n_fx(), 0);
        let d = dkmdsp::DspSection::parse(&s.dsp).unwrap();
        assert_eq!(d.voices.len(), 1);
        assert_eq!(
            d.voices[0].type_no,
            crate::register::machine_key("test/m", "foo")
        );
    }

    #[test]
    fn the_keys_follow_the_packed_name_not_the_module_id() {
        let m = module(MACHINE);
        let obj = dsp_object(&["_foo_render", "_foo_note_on"], &[], &[], 0);
        let s = sections(&m, "test/m-copy", "dn2", Some(&obj)).unwrap();
        let k = dkneeds::keys::Keys::parse(&s.keys).unwrap();
        let key = crate::register::machine_key("test/m-copy", "foo");
        assert!(k.machine_keys().eq([key]));
        assert_ne!(key, crate::register::machine_key("test/m", "foo"));
        let d = dkmdsp::DspSection::parse(&s.dsp).unwrap();
        assert_eq!(
            d.voices[0].type_no, key,
            "the voice record names the same key"
        );
    }

    #[test]
    fn the_packed_file_has_the_keys_table_and_the_needed_runtime() {
        let m = module(MACHINE);
        let obj = dsp_object(&["_foo_render", "_foo_note_on"], &[], &[], 0);
        let s = sections(&m, "test/m", "dn2", Some(&obj)).unwrap();
        let o = crate::dkm::Options {
            name: "test/m".into(),
            version: 0x010203,
            trace: false,
            stereo: s.stereo,
            gate: false,
        };
        let f = crate::dkm::pack_keys(None, &s.keys, &s.dsp, &o).unwrap();
        // the bytes of the old contract pairs are reserved zero; the packer links no import here, so the file needs the version of the header table
        assert_eq!(&f[0x38..0x40], &[0u8; 8]);
        assert_eq!(
            &f[0x30..0x34],
            &crate::sdkgen::table_runtime().to_be_bytes()
        );
        let off = u32::from_be_bytes(f[0x90..0x94].try_into().unwrap()) as usize;
        let size = u32::from_be_bytes(f[0x94..0x98].try_into().unwrap()) as usize;
        assert_eq!(&f[off..off + size], &s.keys[..]);
        let n = crate::dkm::needs(&f).unwrap();
        assert_eq!((n.machine_rows, n.param_rows, n.page_rows), (1, 4, 1));
        assert_eq!(n.voice_rows, 1);
        crate::dkm::Dkm::parse(&f).unwrap();
        // a table that is not a keys table is refused
        assert!(crate::dkm::pack_keys(None, &[1, 2, 3], &s.dsp, &o).is_err());
    }

    #[test]
    fn a_missing_entry_symbol_is_refused_by_name() {
        let m = module(MACHINE);
        let obj = dsp_object(&["_foo_render"], &[], &[], 0);
        let e = sections(&m, "test/m", "dn2", Some(&obj))
            .unwrap_err()
            .to_string();
        assert!(e.contains("_foo_note_on"), "{e}");
        let obj = dsp_object(&["_foo_note_on"], &[], &[], 0);
        let e = sections(&m, "test/m", "dn2", Some(&obj))
            .unwrap_err()
            .to_string();
        assert!(e.contains("_foo_render"), "{e}");
        let e = sections(&m, "test/m", "dn2", None).unwrap_err().to_string();
        assert!(e.contains("DSP object"), "{e}");
        let fx = module("[[fx]]\nid = \"bar\"\n");
        let obj = dsp_object(&["_bar_process"], &[], &[], 0);
        let e = sections(&fx, "test/m", "dn2", Some(&obj))
            .unwrap_err()
            .to_string();
        assert!(e.contains("_bar_desc"), "{e}");
        let obj = dsp_object(&[], &["_bar_desc"], &[], 0);
        let e = sections(&fx, "test/m", "dn2", Some(&obj))
            .unwrap_err()
            .to_string();
        assert!(e.contains("_bar_process"), "{e}");
    }

    #[test]
    fn a_dsp_prefix_replaces_the_id_in_the_symbols_and_not_in_the_keys() {
        let m = module(
            "[[machine]]\nid = \"foo\"\ndsp = \"sn\"\n[[fx]]\nid = \"bar-x\"\ndsp = \"bx\"\n",
        );
        let obj = dsp_object(
            &["_sn_render", "_sn_note_on", "_bx_process"],
            &["_sn_desc"],
            &[],
            dkmdsp::DESC_CONTRACT,
        );
        // the id's own names are not looked for
        let e = sections(&m, "test/m", "dn2", Some(&obj))
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("_bx_desc") && e.contains("\"bx\"") && e.contains("module.toml"),
            "{e}"
        );
        let m = module("[[machine]]\nid = \"foo\"\ndsp = \"sn\"\n");
        let s = sections(&m, "test/m", "dn2", Some(&obj)).unwrap();
        let k = dkneeds::keys::Keys::parse(&s.keys).unwrap();
        assert!(
            k.machine_keys()
                .eq([crate::register::machine_key("test/m", "foo")])
        );
        let e = sections(&module(MACHINE), "test/m", "dn2", Some(&obj))
            .unwrap_err()
            .to_string();
        assert!(e.contains("_foo_render") && e.contains("its id"), "{e}");
    }

    #[test]
    fn a_sample_pool_needs_both_symbols() {
        let m = module(MACHINE);
        let obj = dsp_object(
            &["_foo_render", "_foo_note_on"],
            &[],
            &["_foo_sample_rec"],
            0,
        );
        let e = sections(&m, "test/m", "dn2", Some(&obj))
            .unwrap_err()
            .to_string();
        assert!(e.contains("_foo_sample_ctl"), "{e}");
    }
}
