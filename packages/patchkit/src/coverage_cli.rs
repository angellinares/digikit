//! `patchkit imports` and `patchkit coverage`: which SDK imports the packed modules link, and which `[[import]]` entries of sdk/sdk.toml no module links.
//!
//! A `.dkm` records one thing about the SDK surface it uses: its import table (`Dkm::imports`, the undefined symbols of its ColdFire object, resolved by
//! the runtime at load). The builder calls of `init` (`dk_module_ctx_add_machine`, `dk_machine_add_page`, `dk_page_add_param`, `dk_fx_routes`, ...) are
//! ordinary `[[import]]` entries, so the same table covers them. Not observable from a pack: the values a call passes (glyphs, routes, parameter
//! kinds, the constants of `[machine]`, `[fx]` and `[page]`), whether a call runs, and the handle and callback types.

use crate::dkm::Dkm;
use crate::error::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// The import names a `.dkm` links, sorted (the file is parsed and checked as the runtime does).
pub fn linked_imports(file: &[u8]) -> Result<(String, Vec<String>)> {
    let d = Dkm::parse(file)?;
    let mut v = d.imports;
    v.sort();
    v.dedup();
    Ok((d.name, v))
}

/// One `[[import]]` of sdk/sdk.toml: its name and the group it is reported under (its `service`, else `helper`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SdkImport {
    pub name: String,
    pub group: String,
}

/// The `[[import]]` list of an sdk.toml text, in file order.
pub fn sdk_imports(text: &str) -> Result<Vec<SdkImport>> {
    let v: toml::Value = toml::from_str(text).map_err(|e| err!("sdk.toml: {e}"))?;
    let list = v
        .get("import")
        .and_then(|i| i.as_array())
        .ok_or_else(|| err!("sdk.toml has no [[import]]"))?;
    list.iter()
        .map(|i| {
            let name = i
                .get("name")
                .and_then(|n| n.as_str())
                .ok_or_else(|| err!("an [[import]] has no name"))?;
            let group = i
                .get("service")
                .and_then(|s| s.as_str())
                .unwrap_or("helper");
            Ok(SdkImport {
                name: name.into(),
                group: group.into(),
            })
        })
        .collect()
}

/// The allow-list: `[[allow]]` entries `name` and `reason` (an import that no module links on purpose).
pub fn allow_list(text: &str) -> Result<BTreeMap<String, String>> {
    let v: toml::Value = toml::from_str(text).map_err(|e| err!("allow-list: {e}"))?;
    let mut out = BTreeMap::new();
    for a in v
        .get("allow")
        .and_then(|a| a.as_array())
        .into_iter()
        .flatten()
    {
        let name = a.get("name").and_then(|n| n.as_str());
        let reason = a.get("reason").and_then(|n| n.as_str()).unwrap_or("");
        let (Some(name), false) = (name, reason.trim().is_empty()) else {
            return Err(err!(
                "allow-list: every [[allow]] needs a name and a reason"
            ));
        };
        if out.insert(name.to_string(), reason.to_string()).is_some() {
            return Err(err!("allow-list: {name} is listed twice"));
        }
    }
    Ok(out)
}

/// The coverage of the sdk imports by `linked` (name -> the files that link it), as JSON:
/// `{groups: {G: {total, covered, allowed, uncovered: [..]}}, total, covered, allowed, uncovered, unknown: [..], stale_allow: [..]}`.
pub fn coverage(
    sdk: &[SdkImport],
    linked: &BTreeMap<String, BTreeSet<String>>,
    allow: &BTreeMap<String, String>,
) -> Value {
    let known: BTreeSet<&str> = sdk.iter().map(|i| i.name.as_str()).collect();
    let mut groups: BTreeMap<&str, (u32, u32, u32, Vec<Value>)> = BTreeMap::new();
    for i in sdk {
        let g = groups.entry(i.group.as_str()).or_default();
        g.0 += 1;
        if linked.contains_key(&i.name) {
            g.1 += 1;
        } else if let Some(reason) = allow.get(&i.name) {
            g.2 += 1;
            g.3.push(json!({"name": i.name, "allowed": reason}));
        } else {
            g.3.push(json!({"name": i.name}));
        }
    }
    let sum = |f: fn(&(u32, u32, u32, Vec<Value>)) -> u32| groups.values().map(f).sum::<u32>();
    let (total, covered, allowed) = (sum(|g| g.0), sum(|g| g.1), sum(|g| g.2));
    let unknown: Vec<&String> = linked
        .keys()
        .filter(|n| !known.contains(n.as_str()))
        .collect();
    // an allow-list entry that is linked, or not in the sdk, is out of date
    let stale: Vec<&String> = allow
        .keys()
        .filter(|n| linked.contains_key(*n) || !known.contains(n.as_str()))
        .collect();
    let gj: serde_json::Map<String, Value> = groups
        .iter()
        .map(|(g, (t, c, a, u))| {
            (
                g.to_string(),
                json!({"total": t, "covered": c, "allowed": a, "uncovered": u}),
            )
        })
        .collect();
    json!({
        "groups": gj, "total": total, "covered": covered, "allowed": allowed,
        "uncovered": total - covered - allowed, "unknown": unknown, "stale_allow": stale,
    })
}

/// True when the report has an uncovered import, an unknown import or a stale allow-list entry.
pub fn failed(report: &Value) -> bool {
    report["uncovered"].as_u64() != Some(0)
        || !report["unknown"].as_array().is_none_or(|a| a.is_empty())
        || !report["stale_allow"]
            .as_array()
            .is_none_or(|a| a.is_empty())
}

fn print_report(r: &Value, files: usize) {
    println!(
        "sdk imports: {} total, {} covered, {} allowed, {} uncovered ({files} modules)",
        r["total"], r["covered"], r["allowed"], r["uncovered"]
    );
    for (g, v) in r["groups"].as_object().into_iter().flatten() {
        println!(
            "  {g:<10} {:>3}/{:<3} covered, {} allowed, {} uncovered",
            v["covered"],
            v["total"],
            v["allowed"],
            v["total"].as_u64().unwrap_or(0)
                - v["covered"].as_u64().unwrap_or(0)
                - v["allowed"].as_u64().unwrap_or(0)
        );
        for u in v["uncovered"].as_array().into_iter().flatten() {
            match u["allowed"].as_str() {
                Some(why) => println!(
                    "    allowed    {} ({why})",
                    u["name"].as_str().unwrap_or("")
                ),
                None => println!("    UNCOVERED  {}", u["name"].as_str().unwrap_or("")),
            }
        }
    }
    for n in r["unknown"].as_array().into_iter().flatten() {
        println!(
            "  UNKNOWN    {} (linked, not in sdk.toml)",
            n.as_str().unwrap_or("")
        );
    }
    for n in r["stale_allow"].as_array().into_iter().flatten() {
        println!(
            "  STALE      {} (allow-list entry that is linked or not in sdk.toml)",
            n.as_str().unwrap_or("")
        );
    }
}

fn flag(rest: &[String], name: &str) -> Option<String> {
    rest.windows(2).find(|w| w[0] == name).map(|w| w[1].clone())
}

/// The file arguments: everything that is not a flag or the value of `--sdk` or `--allow`.
fn files(rest: &[String]) -> Vec<&String> {
    let mut out = vec![];
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--sdk" | "--allow" => {
                it.next();
            }
            s if s.starts_with("--") => {}
            _ => out.push(a),
        }
    }
    out
}

fn load(paths: &[&String]) -> Result<Vec<(String, String, Vec<String>)>> {
    paths
        .iter()
        .map(|p| {
            let raw = std::fs::read(p.as_str()).map_err(|e| err!("{p}: {e}"))?;
            let (name, imps) = linked_imports(&raw).map_err(|e| err!("{p}: {e}"))?;
            Ok((p.to_string(), name, imps))
        })
        .collect()
}

/// `patchkit imports [--json] FILE.dkm...`
pub fn run_imports(rest: &[String]) -> Result<()> {
    let paths = files(rest);
    if paths.is_empty() {
        return Err(err!("usage: patchkit imports [--json] FILE.dkm..."));
    }
    let mods = load(&paths)?;
    if rest.iter().any(|a| a == "--json") {
        let o: serde_json::Map<String, Value> = mods
            .into_iter()
            .map(|(p, name, imps)| (p, json!({"module": name, "imports": imps})))
            .collect();
        println!("{}", Value::Object(o));
    } else {
        for (p, name, imps) in mods {
            println!("{p} ({name}): {} imports", imps.len());
            for i in imps {
                println!("  {i}");
            }
        }
    }
    Ok(())
}

/// `patchkit coverage --sdk sdk/sdk.toml [--allow FILE] [--json] FILE.dkm...`; exit 1 (an error) when an import is uncovered.
pub fn run_coverage(rest: &[String]) -> Result<()> {
    let sdk_path = flag(rest, "--sdk").ok_or_else(|| {
        err!("usage: patchkit coverage --sdk sdk/sdk.toml [--allow FILE] [--json] FILE.dkm...")
    })?;
    let sdk =
        sdk_imports(&std::fs::read_to_string(&sdk_path).map_err(|e| err!("{sdk_path}: {e}"))?)?;
    let allow = match flag(rest, "--allow") {
        Some(p) => allow_list(&std::fs::read_to_string(&p).map_err(|e| err!("{p}: {e}"))?)?,
        None => BTreeMap::new(),
    };
    let paths = files(rest);
    if paths.is_empty() {
        return Err(err!("coverage: no .dkm files"));
    }
    let mods = load(&paths)?;
    let mut linked: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (p, _, imps) in &mods {
        for i in imps {
            linked.entry(i.clone()).or_default().insert(p.clone());
        }
    }
    let report = coverage(&sdk, &linked, &allow);
    if rest.iter().any(|a| a == "--json") {
        println!("{report}");
    } else {
        print_report(&report, mods.len());
    }
    if failed(&report) {
        return Err(err!("coverage: the SDK surface is not covered (see above)"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dko::*;

    fn sec(name: &str, kind: &str, bytes: &[u8]) -> DkoSection {
        DkoSection {
            name: name.into(),
            kind: kind.into(),
            align: 4,
            size: bytes.len() as u32,
            hex: hex(bytes),
        }
    }

    /// A module whose `init` calls `imports` (one `jsr abs.l` each) and whose `dk_module` record has `init` set.
    fn module(imports: &[&str]) -> Vec<u8> {
        let mut text = vec![];
        for _ in imports {
            text.extend([0x4e, 0xb9, 0, 0, 0, 0]);
        }
        text.extend([0x4e, 0x75]);
        let sym = |name: &str, section: Option<&str>, value: u64| DkoSymbol {
            name: name.into(),
            section: section.map(String::from),
            value,
            global: true,
        };
        let mut symbols = vec![
            sym("init_fn", Some(".text"), 0),
            sym("dk_module", Some(".data"), 0),
        ];
        let mut relocs = vec![DkoReloc {
            section: ".data".into(),
            offset: 0,
            rtype: "cf.32".into(),
            symbol: "init_fn".into(),
            addend: 0,
        }];
        for (i, n) in imports.iter().enumerate() {
            symbols.push(sym(n, None, 0));
            relocs.push(DkoReloc {
                section: ".text".into(),
                offset: 2 + 6 * i as u64,
                rtype: "cf.32".into(),
                symbol: n.to_string(),
                addend: 0,
            });
        }
        let mut o = Dko {
            format: FORMAT,
            arch: "cf".into(),
            source_sha256: "0".repeat(64),
            toolchain: Default::default(),
            sections: vec![sec(".text", "text", &text), sec(".data", "data", &[0; 16])],
            symbols,
            relocs,
        };
        o.stamp_isa();
        crate::dkm::pack(
            &o,
            &crate::dkm::Options {
                name: "test/x".into(),
                version: 0x010000,
                trace: false,
                stereo: false,
                gate: false,
                service: None,
            },
        )
        .unwrap()
    }

    const SDK: &str = r#"
[[import]]
name = "dk_alloc"
service = "runtime"
[[import]]
name = "dk_page_add_param"
service = "machines"
[[import]]
name = "dk_fx_routes"
service = "fx"
[[import]]
name = "memcpy"
kind = "helper"
"#;

    #[test]
    fn a_packed_file_lists_its_imports_once_and_sorted() {
        let f = module(&["memcpy", "dk_alloc", "memcpy"]);
        let (name, imps) = linked_imports(&f).unwrap();
        assert_eq!(name, "test/x");
        assert_eq!(imps, ["dk_alloc", "memcpy"]);
        assert!(linked_imports(&f[..f.len() - 1]).is_err());
    }

    #[test]
    fn coverage_groups_by_service_and_counts_allowed_unknown_and_stale() {
        let sdk = sdk_imports(SDK).unwrap();
        assert_eq!(sdk.iter().filter(|i| i.group == "helper").count(), 1);
        let mut linked = BTreeMap::new();
        for n in ["dk_alloc", "memcpy", "dk_bogus"] {
            linked.insert(n.to_string(), BTreeSet::from(["a.dkm".to_string()]));
        }
        let allow =
            allow_list("[[allow]]\nname = \"dk_fx_routes\"\nreason = \"reserved\"\n").unwrap();
        let r = coverage(&sdk, &linked, &allow);
        assert_eq!(
            (
                r["total"].as_u64(),
                r["covered"].as_u64(),
                r["allowed"].as_u64(),
                r["uncovered"].as_u64()
            ),
            (Some(4), Some(2), Some(1), Some(1))
        );
        assert_eq!(
            r["groups"]["machines"]["uncovered"][0]["name"],
            "dk_page_add_param"
        );
        assert_eq!(r["groups"]["fx"]["uncovered"][0]["allowed"], "reserved");
        assert_eq!(r["unknown"][0], "dk_bogus");
        assert!(failed(&r));
        // linking an allowed import makes its allow-list entry stale
        linked.insert("dk_fx_routes".into(), BTreeSet::new());
        linked.insert("dk_page_add_param".into(), BTreeSet::new());
        linked.remove("dk_bogus");
        let r = coverage(&sdk, &linked, &allow);
        assert_eq!(r["stale_allow"][0], "dk_fx_routes");
        assert!(failed(&r));
        let r = coverage(&sdk, &linked, &BTreeMap::new());
        assert!(!failed(&r));
    }

    #[test]
    fn an_allow_entry_needs_a_reason() {
        assert!(allow_list("[[allow]]\nname = \"x\"\n").is_err());
        assert!(
            allow_list(
                "[[allow]]\nname = \"x\"\nreason = \"r\"\n[[allow]]\nname = \"x\"\nreason = \"r\"\n"
            )
            .is_err()
        );
    }
}
