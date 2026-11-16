//! Address map output (docs/ADDRMAP.md): the entries of the ship map and the module map, and the `patchkit addr` command that names addresses.
//!
//! `patchkit apply --addrmap FILE` writes layers `our` (the ColdFire code of the runtime, adapters and apps the plan placed) and `anchor`
//! (the code anchors of the port). `patchkit pack --addrmap FILE` writes the module map: the code symbols of one module, link-relative.
//! The module map also holds the code symbols of the module's DSP object (`cpu: sharc`), as short-word offsets from the start of its DSP image;
//! the `dsp_base` of the load table places them.

use crate::dko::Dko;
use crate::error::*;
use crate::link::Linked;
use crate::port::Port;
use dkaddrmap::{AddrMap, Cpu, Entry, Key, Layer, LoadTable, Resolver};
use std::collections::BTreeMap;

/// A readable name: Rust symbols (legacy and v0 mangling) are demangled without their hash, other names stay as the linker has them.
fn readable(name: &str) -> String {
    // a symbol the linker made from a section name: `.sec..text.put_word`
    let name = name.strip_prefix(".sec.").unwrap_or(name);
    let name = name.strip_prefix(".text.").unwrap_or(name);
    if name.starts_with("_R") || name.starts_with("_ZN") {
        format!("{:#}", rustc_demangle::demangle(name))
    } else {
        name.to_string()
    }
}

/// The functions of a linked ColdFire object: every symbol that lies in an executable section. The size runs to the next symbol of the
/// section (the object has no symbol sizes; with one function per section it is the function's size).
/// With `module` the addresses are those of the link (base 0): offsets from the module's load address.
pub fn text_entries(obj: &Dko, l: &Linked, module: Option<&str>) -> Vec<Entry> {
    code_entries(obj, l, module, Cpu::Cf)
}

/// The global data symbols of a ColdFire object (`data` and `bss` sections, e.g. the `DKRT_*` statics of a service file): one entry each, sized as its section. A host that
/// reads a module's state (a test of a service file, a profiler) finds the cell by name. Only a module map has them: the ship map names code.
pub fn data_entries(obj: &Dko, l: &Linked, module: Option<&str>) -> Vec<Entry> {
    let kinds: BTreeMap<&str, (&str, u64)> = obj
        .sections
        .iter()
        .map(|s| (s.name.as_str(), (s.kind.as_str(), s.size as u64)))
        .collect();
    let mut out: Vec<Entry> = obj
        .symbols
        .iter()
        .filter(|s| s.global && !s.name.is_empty())
        .filter_map(|s| {
            let sec = s.section.as_deref()?;
            let (kind, size) = *kinds.get(sec)?;
            if kind != "data" && kind != "bss" {
                return None;
            }
            Some(Entry {
                cpu: Cpu::Cf,
                addr: l.places.get(sec).copied().unwrap_or(0) + s.value,
                size: size.saturating_sub(s.value),
                name: readable(&s.name),
                layer: Layer::Our,
                module: module.map(String::from),
            })
        })
        .collect();
    out.sort_by(|a, b| (a.addr, &a.name).cmp(&(b.addr, &b.name)));
    out.dedup_by(|a, b| a.addr == b.addr && a.name == b.name);
    out
}

/// The functions of a module's DSP object linked at `L2_BYTE_BASE` (as the packer does): `cpu: sharc` entries with short-word offsets from the
/// start of the DSP image (symbol values and sizes are in short words).
pub fn dsp_entries(obj: &Dko, module: &str) -> Result<Vec<Entry>> {
    let linked = link_for_map(obj, crate::dsp::L2_BYTE_BASE as u64)?;
    Ok(code_entries(obj, &linked, Some(module), Cpu::Sharc))
}

/// `obj` linked at `base` with every import at 0: addresses are right, relocated fields are not used.
fn link_for_map(obj: &Dko, base: u64) -> Result<Linked> {
    let defined: std::collections::BTreeSet<&str> = obj
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
    crate::link::link(obj, base, &externs)
}

fn code_entries(obj: &Dko, l: &Linked, module: Option<&str>, cpu: Cpu) -> Vec<Entry> {
    let sharc = cpu == Cpu::Sharc;
    let sections: BTreeMap<&str, (&str, u64)> = obj
        .sections
        .iter()
        .map(|s| {
            let size = s.size as u64;
            (
                s.name.as_str(),
                (s.kind.as_str(), if sharc { size / 2 } else { size }),
            )
        })
        .collect();
    let mut by_section: BTreeMap<&str, Vec<(u64, &str)>> = BTreeMap::new();
    for s in &obj.symbols {
        let Some(sec) = s.section.as_deref() else {
            continue;
        };
        if s.name.is_empty() || sections.get(sec).map(|x| x.0) != Some("text") {
            continue;
        }
        by_section.entry(sec).or_default().push((s.value, &s.name));
    }
    let mut out = vec![];
    for (sec, mut syms) in by_section {
        syms.sort();
        let at = l.places.get(sec).copied().unwrap_or(0);
        // a SHARC section is placed at an L2 byte address; its symbols are short-word offsets from there
        let at = if sharc {
            at.saturating_sub(crate::dsp::L2_BYTE_BASE as u64) / 2
        } else {
            at
        };
        let end = sections[sec].1;
        for (i, (value, name)) in syms.iter().enumerate() {
            // a second symbol at the same offset (an alias) shares the range
            let next = syms[i + 1..]
                .iter()
                .map(|x| x.0)
                .find(|v| v > value)
                .unwrap_or(end);
            out.push(Entry {
                cpu,
                addr: at + value,
                size: next.saturating_sub(*value),
                name: readable(name),
                layer: Layer::Our,
                module: module.map(String::from),
            });
        }
    }
    // a global symbol and the section's own symbol can name one function twice
    out.sort_by(|a, b| (a.addr, &a.name).cmp(&(b.addr, &b.name)));
    out.dedup_by(|a, b| a.addr == b.addr && a.name == b.name);
    out
}

/// Anchors longer than this name a region (`cf.main` spans the image), not a function or a hook point: they stay out of the map, or they
/// would hide every firmware name under them.
pub const MAX_ANCHOR: u64 = 4096;

/// The code anchors of a port that sit on the ColdFire.
pub fn anchor_entries(port: &Port) -> Vec<Entry> {
    port.anchor
        .iter()
        .filter(|a| a.cpu == "cf" && a.kind == "code" && a.len <= MAX_ANCHOR)
        .map(|a| Entry {
            cpu: Cpu::Cf,
            addr: a.addr,
            size: a.len,
            name: a.name.clone(),
            layer: Layer::Anchor,
            module: None,
        })
        .collect()
}

/// The byte addresses of the symbols a host reads to decode the load table (`DKRT_STATE`, `DKRT_DSP`, `dyndsp_info`; docs/ADDRMAP.md "Load table"),
/// for those the image has exactly once.
pub fn load_symbols(plan: &crate::apply::Plan) -> std::collections::BTreeMap<String, u64> {
    [
        dkaddrmap::snapshot::SYM_STATE,
        dkaddrmap::snapshot::SYM_DSP,
        dkaddrmap::snapshot::SYM_INFO,
        dkaddrmap::snapshot::SYM_PROVIDERS,
    ]
    .iter()
    .filter_map(|s| {
        let mut found = plan
            .symbols
            .iter()
            .filter(|(k, _)| k.rsplit(':').next() == Some(*s));
        match (found.next(), found.next()) {
            (Some((_, a)), None) => Some((s.to_string(), *a)),
            _ => None,
        }
    })
    .collect()
}

/// The ship map of an applied plan.
pub fn ship_map(plan: &crate::apply::Plan) -> AddrMap {
    let has = |name: &str| {
        plan.symbols
            .keys()
            .any(|k| k.rsplit(':').next() == Some(name))
    };
    // how a host reads the load table of this image: the runtime's tables and the L2 bases of the DSP placement
    let loader = crate::devdata::by_short(&plan.base.device)
        .and_then(|d| d.modules)
        .filter(|_| has(dkaddrmap::snapshot::SYM_STATE))
        .map(|modules| dkaddrmap::snapshot::Loader {
            modules,
            l2_byte_base: crate::dsp::L2_BYTE_BASE as u64,
            l2_sw_base: crate::dsp::L2_SW_BASE as u64,
            dsp: has(dkaddrmap::snapshot::SYM_DSP) && has(dkaddrmap::snapshot::SYM_INFO),
            providers: if has(dkaddrmap::snapshot::SYM_PROVIDERS) {
                dkrt_sdk::PROVIDER_MAX as u64
            } else {
                0
            },
        });
    let key = Key {
        loader,
        device: Some(plan.base.device.clone()),
        firmware: Some(plan.base.version.clone()),
        image_sha256: Some(plan.base.syx_sha256.clone()),
        output_sha256: plan.output.as_ref().map(|o| o.syx_sha256.clone()),
        modules: plan.modules.clone(),
        ..Key::default()
    };
    AddrMap::new("ship", key, plan.addr_entries.clone())
}

/// The map of one packed module: link-relative entries. The ColdFire object is linked at base 0 as `pack` does; the DSP object, if any, gives
/// `sharc` entries relative to the start of the DSP image. A data-only module has neither.
pub fn module_map(
    cf: Option<&Dko>,
    dsp: Option<&Dko>,
    id: &str,
    dkm_sha256: &str,
) -> Result<AddrMap> {
    let key = Key {
        module: Some(id.to_string()),
        source_sha256: cf.map(|o| o.source_sha256.clone()),
        dkm_sha256: Some(dkm_sha256.to_string()),
        ..Key::default()
    };
    let mut entries = vec![];
    if let Some(o) = cf {
        let linked = link_for_map(o, 0)?;
        entries.extend(text_entries(o, &linked, Some(id)));
        entries.extend(data_entries(o, &linked, Some(id)));
    }
    if let Some(o) = dsp {
        entries.extend(dsp_entries(o, id)?);
    }
    Ok(AddrMap::new("module", key, entries))
}

const USAGE: &str = "usage: patchkit addr --map FILE... [--loads FILE] [--load ID=BASE]... [--dsp-base ID=PC]... [--snapshot SYMBOL=HEX]... [--cpu cf|sharc] [--ret] [--format json|text|reads|loads] (ADDR... | --stdin)
  ADDR is hex (0x...) or decimal, optionally CPU:ADDR (sharc:0xb80010). --stdin reads one ADDR per line.
  --loads is a JSON load table {\"modules\":[{\"id\":NAME,\"base\":N}]}; --load adds one module (hex base).
  --snapshot gives the raw bytes of a runtime symbol (DKRT_STATE, DKRT_DSP, dyndsp_info) as read from the emulator or device; the load table is decoded from them
  with the map's loader header. --format reads lists the symbols and byte counts to read; --format loads prints the decoded load table.
  --dsp-base sets where the DSP image of a loaded module starts (hex software PC); `sharc` names of the module need it.
  --ret names a return address (the address minus 2).";

fn number(s: &str) -> Result<u64> {
    let t = s.trim();
    match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        Some(h) => u64::from_str_radix(h, 16),
        None => t.parse(),
    }
    .map_err(|_| Error(format!("bad address {s:?}\n{USAGE}")))
}

/// `patchkit addr`: names for addresses. JSON on stdout: `{"unloaded": [...], "results": [{"cpu", "addr", "name", "text", "layer", "sym_addr",
/// "offset", "size", "module"}]}`; an address no layer holds has `name` null.
pub fn run(rest: &[String]) -> Result<()> {
    let mut maps = vec![];
    let mut loads = LoadTable::default();
    let mut cpu = Cpu::Cf;
    let (mut ret, mut stdin) = (false, false);
    let mut format = String::from("json");
    let mut snapshot = dkaddrmap::snapshot::Snapshot::new();
    let mut addrs: Vec<String> = vec![];
    let mut dsp_bases: Vec<(String, u64)> = vec![];
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        let mut value = |what: &str| -> Result<&String> {
            it.next()
                .ok_or_else(|| Error(format!("{what} needs a value\n{USAGE}")))
        };
        match a.as_str() {
            "--map" => {
                let p = value("--map")?;
                let t = std::fs::read_to_string(p).map_err(|e| Error(format!("{p}: {e}")))?;
                maps.push(AddrMap::parse(&t).map_err(|e| Error(format!("{p}: {e}")))?);
            }
            "--loads" => {
                let p = value("--loads")?;
                let t = std::fs::read_to_string(p).map_err(|e| Error(format!("{p}: {e}")))?;
                let l: LoadTable =
                    serde_json::from_str(&t).map_err(|e| Error(format!("{p}: {e}")))?;
                loads.modules.extend(l.modules);
            }
            "--load" => {
                let v = value("--load")?;
                let (id, base) = v
                    .split_once('=')
                    .ok_or_else(|| Error(format!("--load ID=BASE\n{USAGE}")))?;
                loads.modules.push(dkaddrmap::Load {
                    id: id.into(),
                    base: number(&format!("0x{}", base.trim_start_matches("0x")))?,
                    size: 0,
                    dsp_base: None,
                    fini: None,
                });
            }
            "--dsp-base" => {
                let v = value("--dsp-base")?;
                let (id, pc) = v
                    .split_once('=')
                    .ok_or_else(|| Error(format!("--dsp-base ID=PC\n{USAGE}")))?;
                dsp_bases.push((
                    id.into(),
                    number(&format!("0x{}", pc.trim_start_matches("0x")))?,
                ));
            }
            "--cpu" => {
                let v = value("--cpu")?;
                cpu = Cpu::parse(v).ok_or_else(|| Error(format!("--cpu cf|sharc\n{USAGE}")))?;
            }
            "--format" => format = value("--format")?.clone(),
            "--snapshot" => {
                let v = value("--snapshot")?;
                let (sym, hex) = v
                    .split_once('=')
                    .ok_or_else(|| Error(format!("--snapshot SYMBOL=HEX\n{USAGE}")))?;
                let bytes = dkaddrmap::snapshot::parse_hex(hex)
                    .map_err(|e| Error(format!("--snapshot {sym}: {e}")))?;
                snapshot.insert(sym.into(), bytes);
            }
            "--ret" => ret = true,
            "--stdin" => stdin = true,
            _ => addrs.push(a.clone()),
        }
    }
    if stdin {
        let mut b = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut b)
            .map_err(|e| Error(format!("stdin: {e}")))?;
        addrs.extend(b.lines().filter(|l| !l.trim().is_empty()).map(String::from));
    }
    let text = format == "text";
    if !["json", "text", "reads", "loads"].contains(&format.as_str()) {
        return Err(Error(format!("--format json|text|reads|loads\n{USAGE}")));
    }
    if format == "reads" {
        let l = dkaddrmap::loader_of(&maps).ok_or_else(|| {
            Error(
                "no map describes the load table (a ship map from `patchkit apply --addrmap` does)"
                    .into(),
            )
        })?;
        println!("{}", serde_json::json!(l.reads()));
        return Ok(());
    }
    if !snapshot.is_empty() {
        let l = dkaddrmap::loader_of(&maps).ok_or_else(|| {
            Error("--snapshot needs a ship map (it holds the loader header)".into())
        })?;
        let t = dkaddrmap::snapshot::decode_loads(l, &snapshot).map_err(Error)?;
        loads.modules.extend(t.modules);
    }
    if format == "loads" {
        println!(
            "{}",
            serde_json::to_string(&loads).expect("load table serialises")
        );
        return Ok(());
    }
    if maps.is_empty() || addrs.is_empty() {
        return Err(Error(USAGE.into()));
    }
    for (id, pc) in dsp_bases {
        match loads.modules.iter_mut().find(|l| l.id == id) {
            Some(l) => l.dsp_base = Some(pc),
            None => {
                return Err(Error(format!(
                    "--dsp-base {id}: the module is not loaded\n{USAGE}"
                )));
            }
        }
    }
    let r = Resolver::new(&maps, &loads).map_err(Error)?;
    let mut results = vec![];
    for a in &addrs {
        let (c, v) = match a.split_once(':') {
            Some((c, v)) => (
                Cpu::parse(c).ok_or_else(|| Error(format!("bad CPU in {a:?}\n{USAGE}")))?,
                v,
            ),
            None => (cpu, a.as_str()),
        };
        let addr = number(v)?;
        let hit = if ret {
            r.lookup_return(c, addr)
        } else {
            r.lookup(c, addr)
        };
        if text {
            println!(
                "{addr:#x} {}",
                hit.as_ref().map_or("?".to_string(), |h| h.text())
            );
        }
        results.push(serde_json::json!({
            "cpu": c, "addr": addr,
            "name": hit.as_ref().map(|h| &h.name),
            "text": hit.as_ref().map(|h| h.text()),
            "layer": hit.as_ref().map(|h| h.layer),
            "sym_addr": hit.as_ref().map(|h| h.sym_addr),
            "offset": hit.as_ref().map(|h| h.offset),
            "size": hit.as_ref().map(|h| h.size),
            "module": hit.as_ref().and_then(|h| h.module.clone()),
        }));
    }
    if !text {
        println!(
            "{}",
            serde_json::json!({"unloaded": r.unloaded, "results": results})
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dko::{DkoSection, DkoSymbol};

    fn section(name: &str, kind: &str, size: u32) -> DkoSection {
        DkoSection {
            name: name.into(),
            kind: kind.into(),
            align: 4,
            size,
            hex: if kind == "bss" {
                String::new()
            } else {
                "00".repeat(size as usize)
            },
        }
    }

    fn symbol(name: &str, section: &str, value: u64) -> DkoSymbol {
        DkoSymbol {
            name: name.into(),
            section: Some(section.into()),
            value,
            global: true,
        }
    }

    /// One function per section (`-ffunction-sections`), a second function sharing a section, a data symbol and an import.
    fn object() -> Dko {
        Dko {
            format: crate::dko::FORMAT,
            arch: "cf".into(),
            source_sha256: "ab".repeat(32),
            toolchain: Default::default(),
            sections: vec![
                section(".text.first", "text", 16),
                section(".text.pair", "text", 32),
                section(".data.table", "data", 8),
            ],
            symbols: vec![
                symbol("first", ".text.first", 0),
                symbol("first", ".text.first", 0),
                symbol("pair_a", ".text.pair", 0),
                symbol("pair_b", ".text.pair", 12),
                symbol("table", ".data.table", 0),
                symbol("_RNvNtCsdWzhuXZnywa_4dkrt4card4gate", ".text.first", 8),
                DkoSymbol {
                    name: "dk_import".into(),
                    section: None,
                    value: 0,
                    global: true,
                },
            ],
            relocs: vec![],
        }
    }

    #[test]
    fn code_symbols_get_sizes_and_readable_names() {
        let o = object();
        let l = crate::link::link(&o, 0x2000_0000, &BTreeMap::new()).unwrap();
        let e = text_entries(&o, &l, None);
        let at = |n: &str| e.iter().find(|x| x.name == n).map(|x| (x.addr, x.size));
        let first = l.places[".text.first"];
        let pair = l.places[".text.pair"];
        assert_eq!(at("first"), Some((first, 8)));
        assert_eq!(
            at("dkrt::card::gate"),
            Some((first + 8, 8)),
            "a Rust name is demangled, without its hash"
        );
        assert_eq!(at("pair_a"), Some((pair, 12)));
        assert_eq!(at("pair_b"), Some((pair + 12, 20)));
        assert_eq!(e.len(), 4, "no data symbol and no import");
        assert_eq!(readable(".sec..text.put_word"), "put_word");
        assert!(first >= 0x2000_0000);
    }

    #[test]
    fn a_module_map_is_link_relative_and_resolves_after_rebasing() {
        let o = object();
        let map = module_map(Some(&o), None, "test/demo", "cd").unwrap();
        assert_eq!(map.key.module.as_deref(), Some("test/demo"));
        assert!(
            map.entries
                .iter()
                .all(|e| e.module.as_deref() == Some("test/demo"))
        );
        let loads = LoadTable {
            modules: vec![dkaddrmap::Load {
                id: "test/demo".into(),
                base: 0x2100_0000,
                size: 0x100,
                dsp_base: None,
                fini: None,
            }],
        };
        let r = Resolver::new(&[map.clone()], &loads).unwrap();
        let pair_b = map.entries.iter().find(|e| e.name == "pair_b").unwrap();
        let hit = r.lookup(Cpu::Cf, 0x2100_0000 + pair_b.addr + 4).unwrap();
        assert_eq!((hit.name.as_str(), hit.offset), ("pair_b", 4));
    }

    /// A SHARC object: two functions in one text section (parcels), a data symbol.
    fn dsp_object() -> Dko {
        Dko {
            format: crate::dko::FORMAT,
            arch: "sharc".into(),
            source_sha256: "ef".repeat(32),
            toolchain: Default::default(),
            sections: vec![
                section("seg_pmco", "text", 40),
                section("seg_dmda", "data", 8),
            ],
            symbols: vec![
                symbol("_pk_init", "seg_pmco", 0),
                symbol("_pk_render", "seg_pmco", 6),
                symbol("_pk_table", "seg_dmda", 0),
            ],
            relocs: vec![],
        }
    }

    #[test]
    fn a_module_map_names_the_global_data_symbols_of_the_colfire_object() {
        let map = module_map(Some(&object()), None, "test/demo", "cd").unwrap();
        let table = map.entries.iter().find(|e| e.name == "table").unwrap();
        assert_eq!((table.cpu, table.size), (Cpu::Cf, 8));
        assert!(
            map.entries.iter().all(|e| e.name != "dk_import"),
            "an import has no address"
        );
    }

    #[test]
    fn dsp_symbols_are_short_word_offsets_from_the_image_start() {
        let map = module_map(Some(&object()), Some(&dsp_object()), "test/demo", "cd").unwrap();
        let dsp: Vec<_> = map.entries.iter().filter(|e| e.cpu == Cpu::Sharc).collect();
        let at = |n: &str| dsp.iter().find(|e| e.name == n).map(|e| (e.addr, e.size));
        // 40 bytes of text are 20 parcels: init runs 6 parcels, render to the end
        assert_eq!(at("_pk_init"), Some((0, 6)));
        assert_eq!(at("_pk_render"), Some((6, 14)));
        assert_eq!(dsp.len(), 2, "no data symbol");
        assert!(dsp.iter().all(|e| e.module.as_deref() == Some("test/demo")));
        assert!(map.entries.iter().any(|e| e.cpu == Cpu::Cf));
        let loads = LoadTable {
            modules: vec![dkaddrmap::Load {
                id: "test/demo".into(),
                base: 0x2100_0000,
                size: 0x100,
                dsp_base: Some(0xb8_1000),
                fini: None,
            }],
        };
        let r = Resolver::new(&[map], &loads).unwrap();
        let hit = r.lookup(Cpu::Sharc, 0xb8_1000 + 6 + 3).unwrap();
        assert_eq!((hit.name.as_str(), hit.offset), ("_pk_render", 3));
    }
}
