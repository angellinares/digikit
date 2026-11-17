//! `patchkit service-check DIR... [--sdk sdk/sdk.toml]`: the structure check of a service crate (`module:check service/NAME`).
//!
//! A service is a crate `modules/core/services/NAME` with one layout (docs/plans/runtime-and-services.md 5.12, R12-1). The check reads the sources and
//! `sdk.toml`; it builds nothing and runs nothing. It reports every problem, one line each, and the exit status is 1 when there is one.
//!
//! - **Layout.** `Cargo.toml` (package `dkrt-svc-NAME`), `src/lib.rs`, `src/records.rs`, `src/slots.rs` and `tests/` with at least one `.rs` file. A folder
//!   `src/dsp/` has a `mod.rs`, and a service whose `Service` impl has `dsp_*` handlers has the folder. `lib.rs` declares `pub mod records`, `pub mod slots`
//!   (and `pub mod dsp` with the folder). Other modules in `src/` are the service's own.
//! - **The service file.** `dks/` holds `module.toml` (id `core/services/NAME`, `role = "service"`, `service = "NAME"`), `Cargo.toml`, `src/lib.rs` (the
//!   `service!(...)` call and nothing else) and, when needed, `src/hooks.rs` (every hook as assembler; required when the module has `[[hook]]` entries) and
//!   `src/fw.rs` (firmware calls). It is required when the service's `[[service]]` row has no `rust` (the service is a file), and checked whenever it exists.
//!   The `[[hook]]` entries of `dks/module.toml` equal the hooks of the `service!` call, per device (an entry with `devices`, and a call entry `(name, entry, dt2)`).
//!   Assembler (`asm!`, `global_asm!`, `naked`) is only in `dks/src/hooks.rs`, and firmware names (`cf.` hook and anchor names, `dkfw`) only under `dks/`.
//! - **Fills equal the slots.** A `[[fill]]` of `dks/module.toml` names a `[[slot]]` of an image adapter (`modules/core/adapters/**/module.toml`). When the
//!   slot must exist (a typo is an error). When the slot exists only on some devices (`devices` in its `[[slot]]`), the fill names `devices` too and only devices the slot has.
//! - **One entry.** The crate exports at most one entry symbol, `dk_module`: the entry that carries the service's `ServiceRow` (interface B). A service
//!   linked into the runtime exports none yet (the runtime names its row in the generated table); a second export of any other name is a second row
//!   symbol. The state a device test reads by name (`DKRT_FXP`, `DKRT_FXS`: a `static` named `DKRT_*`) is state, not an entry, and is allowed.
//! - **Declarations equal the manifest.** The manifest of a service is its `[[service]]` row in `sdk.toml` (the row's `rust` path is in the crate) and its
//!   `[records.kinds]` entries (`service = "NAME"`). The record kinds that `Service::declare` makes (`d.kind(KIND_X, LEN_X)`) are those entries, no
//!   more and no fewer; `declare` makes no slot, hook, export or resource declaration (the manifest has no place for them yet).

use crate::error::*;
use regex_lite::Regex;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The exported entry of a service.
pub const ENTRY: &str = "dk_module";

/// The `.rs` files under `at` with `//` comments blanked (line structure kept), as (path relative to `base`, text).
fn walk(base: &Path, at: &Path, out: &mut Vec<(String, String)>) {
    let Ok(rd) = std::fs::read_dir(at) else {
        return;
    };
    let mut paths: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        if p.is_dir() {
            walk(base, &p, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            let text = std::fs::read_to_string(&p).unwrap_or_default();
            let rel = p
                .strip_prefix(base)
                .unwrap_or(&p)
                .to_string_lossy()
                .replace('\\', "/");
            out.push((rel, blank_comments(&text)));
        }
    }
}

/// The sources of the neutral crate (`src/`).
fn sources(dir: &Path) -> Vec<(String, String)> {
    let mut out = vec![];
    walk(dir, &dir.join("src"), &mut out);
    out
}

/// The sources of the service file crate (`dks/src/`), paths relative to `dks/`.
fn dks_sources(dir: &Path) -> Vec<(String, String)> {
    let mut out = vec![];
    walk(&dir.join("dks"), &dir.join("dks/src"), &mut out);
    out
}

/// The text with `//` comments and `#[cfg(test)]` items (a block or a line) removed.
fn blank_comments(text: &str) -> String {
    let plain = text
        .lines()
        .map(|l| match l.find("//") {
            Some(i) => l[..i].to_string(),
            None => l.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut out = String::new();
    let mut rest = plain.as_str();
    while let Some(at) = rest.find("#[cfg(test)]") {
        out.push_str(&rest[..at]);
        let after = &rest[at + "#[cfg(test)]".len()..];
        // the gated item ends at its `;` or at the `}` that closes its first `{`
        let (semi, open) = (after.find(';'), after.find('{'));
        let end = match (semi, open) {
            (Some(s), Some(o)) if s < o => s + 1,
            (Some(s), None) => s + 1,
            (_, Some(o)) => {
                let mut depth = 0usize;
                let mut end = after.len();
                for (i, c) in after[o..].char_indices() {
                    match c {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                end = o + i + 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                end
            }
            (None, None) => after.len(),
        };
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

/// The names of the symbols a source exports: the item after `#[no_mangle]`, `#[unsafe(no_mangle)]` or `#[export_name = ".."]`.
fn exports(text: &str) -> Vec<String> {
    let attr = Regex::new(
        r#"#\[\s*(?:unsafe\s*\(\s*)?(?:no_mangle|export_name\s*=\s*"([^"]*)")\s*\)?\s*\]"#,
    )
    .unwrap();
    let item = Regex::new(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:unsafe\s+)?(?:extern\s+\S+\s+)?(?:fn|static(?:\s+mut)?|const)\s+([A-Za-z_]\w*)").unwrap();
    let mut out = vec![];
    for m in attr.captures_iter(text) {
        if let Some(n) = m.get(1) {
            out.push(n.as_str().to_string());
            continue;
        }
        let rest = &text[m.get(0).unwrap().end()..];
        // other attributes may sit between the attribute and the item
        let rest = rest.trim_start();
        let mut rest = rest;
        while rest.starts_with("#[") {
            match rest.find(']') {
                Some(i) => rest = rest[i + 1..].trim_start(),
                None => break,
            }
        }
        if let Some(c) = item.captures(rest.lines().next().unwrap_or("")) {
            out.push(c[1].to_string());
        } else {
            out.push("(unnamed)".to_string());
        }
    }
    out
}

/// The text of the body of `fn declare(` in `text`, if there is one.
fn declare_body(text: &str) -> Option<&str> {
    let at = text.find("fn declare(")?;
    let open = at + text[at..].find('{')?;
    let mut depth = 0usize;
    for (i, c) in text[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[open + 1..open + i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// What `declare` declares: the names `X` of its `d.kind(KIND_X, LEN_X)` calls, and the calls that have no manifest entry.
fn declared(text: &str) -> (BTreeSet<String>, Vec<String>) {
    let mut kinds = BTreeSet::new();
    let mut other = vec![];
    let Some(body) = declare_body(text) else {
        return (kinds, other);
    };
    let call = Regex::new(r"\bd\s*\.\s*(kind|slot|hook|export|resource)\s*\(([^)]*)\)").unwrap();
    for c in call.captures_iter(body) {
        if &c[1] != "kind" {
            other.push(c[1].to_string());
            continue;
        }
        let args: Vec<&str> = c[2].split(',').map(str::trim).collect();
        let name = |a: Option<&&str>, prefix: &str| {
            a.and_then(|s| s.rsplit("::").next())
                .and_then(|s| s.strip_prefix(prefix))
                .map(str::to_string)
        };
        match (name(args.first(), "KIND_"), name(args.get(1), "LEN_")) {
            (Some(k), Some(l)) if k == l => {
                kinds.insert(k);
            }
            _ => other.push(format!("kind({})", c[2].trim())),
        }
    }
    (kinds, other)
}

/// The record kinds the manifest gives to service `name`, and whether it has a `[[service]]` row in this crate.
/// The record kinds of the service and its `[[service]]` row: `None` without a row, `Some(None)` for a row without `rust` (a service file, `.dks`).
fn manifest(sdk: &Path, name: &str) -> Result<(BTreeSet<String>, Option<Option<String>>)> {
    let text = std::fs::read_to_string(sdk).map_err(|e| err!("{}: {e}", sdk.display()))?;
    let v: toml::Table = text.parse().map_err(|e| err!("{}: {e}", sdk.display()))?;
    let row = v
        .get("service")
        .and_then(|s| s.as_array())
        .into_iter()
        .flatten()
        .filter_map(|r| r.as_table())
        .find(|r| r.get("name").and_then(|n| n.as_str()) == Some(name))
        .map(|r| r.get("rust").and_then(|r| r.as_str()).map(str::to_string));
    let kinds = v
        .get("records")
        .and_then(|r| r.get("kinds"))
        .and_then(|k| k.as_table())
        .into_iter()
        .flatten()
        .filter(|(_, k)| k.get("service").and_then(|s| s.as_str()) == Some(name))
        .map(|(n, _)| n.clone())
        .collect();
    Ok((kinds, row))
}

/// The hooks of the `service!` call in `dks/src/lib.rs`: (hook name, device filter: `None` for every device, or `dt2` / `dn2`).
fn service_hooks(lib: &str) -> Option<Vec<(String, Option<String>)>> {
    let at = lib.find("service!(")?;
    let body = &lib[at..];
    let open = Regex::new(r"\bhooks\s*=\s*\[").unwrap().find(body)?.end() - 1;
    let mut depth = 0usize;
    let mut end = body.len();
    for (i, c) in body[open..].char_indices() {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    end = open + i;
                    break;
                }
            }
            _ => {}
        }
    }
    let entry =
        Regex::new(r#"\(\s*"([^"]+)"\s*,\s*[A-Za-z_][\w:]*\s*(?:,\s*([A-Za-z0-9_]+)\s*)?\)"#)
            .unwrap();
    Some(
        entry
            .captures_iter(&body[open + 1..end])
            .map(|c| (c[1].to_string(), c.get(2).map(|d| d.as_str().to_string())))
            .collect(),
    )
}

/// Does `text` (the lib.rs of `dks/src`, comments blanked) hold the `service!` call and nothing else? Attributes, `use` and `mod` lines are allowed.
fn only_service_call(text: &str) -> bool {
    let Some(at) = text.find("service!(") else {
        return false;
    };
    let mut depth = 0usize;
    let mut end = None;
    for (i, c) in text[at..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(at + i + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(end) = end else {
        return false;
    };
    let head = &text[..at];
    let tail = text[end..].trim_start().trim_start_matches(';');
    // the path before `service!` (dkrt_svc_rt::) is part of the call
    let head = head.trim_end_matches(|c: char| c.is_alphanumeric() || c == '_' || c == ':');
    let allowed =
        Regex::new(r"^\s*(?:#!?\[[^\]]*\]|(?:pub\s+)?use\s+[^;]*;|(?:pub\s+)?mod\s+\w+\s*;)\s*$")
            .unwrap();
    // `use`, `mod` and attributes: split at `;` and `]` boundaries into statements
    let mut stmts: Vec<String> = vec![];
    let mut cur = String::new();
    for c in head.chars().chain(['\n']).chain(tail.chars()) {
        cur.push(c);
        if c == ';' || c == ']' {
            stmts.push(std::mem::take(&mut cur));
        }
    }
    if !cur.trim().is_empty() {
        stmts.push(cur);
    }
    stmts.iter().all(|s| allowed.is_match(s))
}

/// The device a `[[hook]]` entry or a `service!` entry is for; an entry for no device named is for all.
fn hook_set(entries: &[(String, Vec<String>)], device: &str) -> BTreeSet<String> {
    entries
        .iter()
        .filter(|(_, d)| d.is_empty() || d.iter().any(|x| x == device))
        .map(|(n, _)| n.clone())
        .collect()
}

/// The problems of the service file crate `dks/` (when it exists or the service is a file) and of the neutral crate's sources against the file rules.
fn check_dks(dir: &Path, needs_dks: bool, neutral: &[(String, String)]) -> Vec<String> {
    let mut out = vec![];
    let asm = Regex::new(
        r"\b(?:global_asm|asm|naked_asm)\s*!|#\[\s*naked\s*\]|#\[\s*unsafe\s*\(\s*naked\s*\)\s*\]",
    )
    .unwrap();
    let fw = Regex::new(r#""cf\.[a-z0-9_]+(?:\.[a-z0-9_]+)+"|\bdkfw\b"#).unwrap();
    for (path, text) in neutral {
        if asm.is_match(text) {
            out.push(format!(
                "{path}: assembler is only in dks/src/hooks.rs (the neutral crate has none)"
            ));
        }
        if let Some(m) = fw.find(text) {
            out.push(format!("{path}: names firmware (`{}`): firmware names (cf.* hooks and anchors, dkfw) belong under dks/", m.as_str()));
        }
    }
    let dks = dir.join("dks");
    if !dks.is_dir() {
        if needs_dks {
            out.push("dks/ is missing: a service file has dks/module.toml, dks/Cargo.toml and dks/src/lib.rs".into());
        }
        return out;
    }
    for f in ["module.toml", "Cargo.toml", "src/lib.rs"] {
        if !dks.join(f).is_file() {
            out.push(format!("dks/{f} is missing"));
        }
    }
    let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let manifest: Option<toml::Table> = std::fs::read_to_string(dks.join("module.toml"))
        .ok()
        .and_then(|t| t.parse().ok());
    let mut hooks: Vec<(String, Vec<String>)> = vec![];
    if let Some(m) = &manifest {
        let module = m.get("module").and_then(|t| t.as_table());
        let get = |k: &str| module.and_then(|t| t.get(k)).and_then(|v| v.as_str());
        let want = format!("core/services/{name}");
        if get("id") != Some(want.as_str()) {
            out.push(format!(
                "dks/module.toml: the id is `{}`, the layout wants `{want}`",
                get("id").unwrap_or("")
            ));
        }
        if get("role") != Some("service") {
            out.push("dks/module.toml: `role` is not \"service\"".into());
        }
        for h in m
            .get("hook")
            .and_then(|h| h.as_array())
            .into_iter()
            .flatten()
        {
            let Some(t) = h.as_table() else { continue };
            let n = t
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let d = t
                .get("devices")
                .and_then(|d| d.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            hooks.push((n, d));
        }
    } else if dks.join("module.toml").is_file() {
        out.push("dks/module.toml does not parse".into());
    }
    let srcs = dks_sources(dir);
    let get = |rel: &str| srcs.iter().find(|(p, _)| p == rel).map(|(_, t)| t.as_str());
    if !hooks.is_empty() && get("src/hooks.rs").is_none() {
        out.push("dks/module.toml has [[hook]] entries but dks/src/hooks.rs is missing (every hook is assembler in that file)".into());
    }
    for (path, text) in &srcs {
        if path != "src/hooks.rs" && asm.is_match(text) {
            out.push(format!("dks/{path}: assembler is only in dks/src/hooks.rs"));
        }
    }
    if let Some(lib) = get("src/lib.rs") {
        if !only_service_call(lib) {
            out.push("dks/src/lib.rs is not the `service!(...)` call alone (attributes, `use` and `mod` lines are allowed)".into());
        }
        let called: Vec<(String, Vec<String>)> = service_hooks(lib)
            .unwrap_or_default()
            .into_iter()
            .map(|(n, d)| (n, d.into_iter().collect()))
            .collect();
        for dev in ["dt2", "dn2"] {
            let (m, c) = (hook_set(&hooks, dev), hook_set(&called, dev));
            for n in c.difference(&m) {
                out.push(format!("dks/src/lib.rs: service! takes the hook `{n}` on {dev}, but dks/module.toml has no [[hook]] for it"));
            }
            for n in m.difference(&c) {
                out.push(format!(
                    "dks/module.toml: [[hook]] `{n}` on {dev} is not in the service! hook list"
                ));
            }
        }
    }
    out
}

/// The `devices` of every `[[slot]]` of the `module.toml` files under `at` (an image adapter's pointer slots), by slot name; an empty list is every device.
/// A slot name declared in two places for a common device (or for every device) is listed in `twice`; on disjoint devices the lists are joined.
fn adapter_slots(
    at: &Path,
    out: &mut std::collections::BTreeMap<String, Vec<String>>,
    twice: &mut Vec<String>,
) {
    let Ok(rd) = std::fs::read_dir(at) else {
        return;
    };
    let mut paths: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        if p.is_dir() {
            let skip = p
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| matches!(n, "target" | "build" | "dist" | "node_modules"));
            if !skip {
                adapter_slots(&p, out, twice);
            }
        } else if p.file_name().is_some_and(|n| n == "module.toml") {
            let Some(t) = std::fs::read_to_string(&p)
                .ok()
                .and_then(|t| t.parse::<toml::Table>().ok())
            else {
                continue;
            };
            for sl in t
                .get("slot")
                .and_then(|s| s.as_array())
                .into_iter()
                .flatten()
            {
                let Some(sl) = sl.as_table() else { continue };
                let Some(name) = sl.get("name").and_then(|n| n.as_str()) else {
                    continue;
                };
                let devices = strings(sl.get("devices"));
                match out.get_mut(name) {
                    None => {
                        out.insert(name.to_string(), devices);
                    }
                    // the same name on disjoint devices is one slot per device; an empty list is every device
                    Some(have)
                        if have.is_empty()
                            || devices.is_empty()
                            || devices.iter().any(|d| have.contains(d)) =>
                    {
                        twice.push(name.to_string());
                    }
                    Some(have) => have.extend(devices),
                }
            }
        }
    }
}

/// The strings of a TOML array (none for anything else).
fn strings(v: Option<&toml::Value>) -> Vec<String> {
    v.and_then(|d| d.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The `[[fill]]` entries of `dks/module.toml` whose devices differ from the devices of the adapter slot they fill. A slot that exists on every device (no
/// `devices`) takes any fill; a slot limited to some devices takes only fills that name `devices`, all of them among the slot's.
fn fill_problems(dir: &Path, adapters: &Path) -> Vec<String> {
    let mut out = vec![];
    // an unparsable dks/module.toml is reported by `check_dks`
    let Some(m) = std::fs::read_to_string(dir.join("dks/module.toml"))
        .ok()
        .and_then(|t| t.parse::<toml::Table>().ok())
    else {
        return out;
    };
    // the fills cannot be compared without the adapters folder: that is an error, not a skip
    if m.get("fill").is_some() && !adapters.is_dir() {
        out.push(format!(
            "the adapters folder {} is missing: the [[fill]] entries of dks/module.toml cannot be checked",
            adapters.display()
        ));
        return out;
    }
    let mut slots = std::collections::BTreeMap::new();
    let mut twice = vec![];
    adapter_slots(adapters, &mut slots, &mut twice);
    for name in twice {
        out.push(format!(
            "the slot `{name}` is declared twice ([[slot]] in the adapter module.toml files)"
        ));
    }
    for f in m
        .get("fill")
        .and_then(|f| f.as_array())
        .into_iter()
        .flatten()
    {
        let Some(t) = f.as_table() else { continue };
        let Some(name) = t.get("slot").and_then(|n| n.as_str()) else {
            continue;
        };
        let Some(have) = slots.get(name) else {
            out.push(format!(
                "dks/module.toml: [[fill]] `{name}` names a slot that no image adapter declares ([[slot]] in an adapter module.toml)"
            ));
            continue;
        };
        if have.is_empty() {
            continue;
        }
        let want = strings(t.get("devices"));
        if want.is_empty() {
            out.push(format!(
                "dks/module.toml: [[fill]] `{name}` has no `devices`, but the image adapter has the slot only on {}",
                have.join(", ")
            ));
        }
        for d in want.iter().filter(|d| !have.contains(d)) {
            out.push(format!(
                "dks/module.toml: [[fill]] `{name}` is for {d}, but the image adapter has the slot only on {}",
                have.join(", ")
            ));
        }
    }
    out
}

/// Every problem of the service crate at `dir`; empty when it is as the layout says. The image adapters are at `../../adapters` of the service folder
/// (`modules/core/adapters`).
pub fn check(dir: &Path, sdk: &Path) -> Result<Vec<String>> {
    check_with_adapters(dir, sdk, &dir.join("../../adapters"))
}

/// `check` with the folder of the image adapters named (the fills of the service file are compared with the `[[slot]]` entries under it).
pub fn check_with_adapters(dir: &Path, sdk: &Path, adapters: &Path) -> Result<Vec<String>> {
    let name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| err!("{}: not a service folder", dir.display()))?
        .to_string();
    let mut out = vec![];
    let mut problem = |s: String| out.push(format!("{name}: {s}"));

    // layout
    let want_pkg = format!("dkrt-svc-{}", name.replace('_', "-"));
    match std::fs::read_to_string(dir.join("Cargo.toml")) {
        Err(_) => problem("Cargo.toml is missing".into()),
        Ok(t) => {
            let pkg = t
                .parse::<toml::Table>()
                .ok()
                .and_then(|t| t.get("package")?.get("name")?.as_str().map(str::to_string));
            if pkg.as_deref() != Some(&want_pkg) {
                problem(format!(
                    "Cargo.toml: the package is `{}`, the layout wants `{want_pkg}`",
                    pkg.unwrap_or_default()
                ));
            }
        }
    }
    for f in ["src/lib.rs", "src/records.rs", "src/slots.rs"] {
        if !dir.join(f).is_file() {
            problem(format!("{f} is missing"));
        }
    }
    let has_tests = std::fs::read_dir(dir.join("tests"))
        .map(|rd| {
            rd.flatten()
                .any(|e| e.path().extension().is_some_and(|x| x == "rs"))
        })
        .unwrap_or(false);
    if !has_tests {
        problem("tests/ has no .rs file".into());
    }
    let srcs = sources(dir);
    let get = |rel: &str| srcs.iter().find(|(p, _)| p == rel).map(|(_, t)| t.as_str());
    let dsp_dir = dir.join("src/dsp");
    if dsp_dir.is_dir() && !dsp_dir.join("mod.rs").is_file() {
        problem("src/dsp/ has no mod.rs".into());
    }
    let records = get("src/records.rs").unwrap_or("");
    if records.contains("fn dsp_") && !dsp_dir.is_dir() {
        problem("records.rs has dsp_* handlers but the crate has no src/dsp/ folder".into());
    }
    if let Some(lib) = get("src/lib.rs") {
        let mut mods = vec!["records", "slots"];
        if dsp_dir.is_dir() {
            mods.push("dsp");
        }
        for m in mods {
            if !Regex::new(&format!(r"\bpub\s+mod\s+{m}\s*;"))
                .unwrap()
                .is_match(lib)
            {
                problem(format!("src/lib.rs does not declare `pub mod {m};`"));
            }
        }
    }

    // one entry
    let mut names: Vec<(String, String)> = vec![];
    for (p, t) in &srcs {
        for n in exports(t) {
            names.push((p.clone(), n));
        }
    }
    let entries = names.iter().filter(|(_, n)| n == ENTRY).count();
    let state = |n: &str| {
        n.starts_with("DKRT_")
            && n.chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    };
    for (p, n) in &names {
        if n != ENTRY && !state(n) {
            problem(format!(
                "{p} exports `{n}`: the only exported entry of a service is `{ENTRY}`, which carries its ServiceRow (no second row symbol)"
            ));
        }
    }
    if entries > 1 {
        problem(format!("`{ENTRY}` is defined {entries} times"));
    }

    // declarations equal the manifest
    let (kinds, row) = manifest(sdk, &name)?;
    for p in check_dks(dir, matches!(row, Some(None)), &srcs) {
        problem(p);
    }
    for p in fill_problems(dir, adapters) {
        problem(p);
    }
    match row {
        None => problem(format!(
            "{} has no [[service]] row named `{name}`",
            sdk.display()
        )),
        Some(Some(r)) if !r.starts_with(&format!("{}::", want_pkg.replace('-', "_"))) => {
            problem(format!(
                "the [[service]] row `{name}` has rust = \"{r}\": the type is not in the crate `{}`",
                want_pkg.replace('-', "_")
            ))
        }
        _ => {}
    }
    let (decl, other) = declared(records);
    for o in other {
        problem(format!(
            "records.rs declares `{o}`: the manifest has no place for it"
        ));
    }
    for k in decl.difference(&kinds) {
        problem(format!(
            "records.rs declares the record kind {k}, which the manifest does not give to `{name}` ([records.kinds] in sdk.toml)"
        ));
    }
    for k in kinds.difference(&decl) {
        problem(format!(
            "the manifest gives the record kind {k} to `{name}`, but records.rs does not declare it"
        ));
    }
    Ok(out)
}

/// `patchkit service-check DIR... [--sdk FILE]`.
pub fn run(args: &[String]) -> Result<()> {
    let mut dirs = vec![];
    let mut sdk = PathBuf::from("sdk/sdk.toml");
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--sdk" {
            sdk = PathBuf::from(args.get(i + 1).ok_or_else(|| err!("--sdk needs a file"))?);
            i += 2;
        } else {
            dirs.push(PathBuf::from(&args[i]));
            i += 1;
        }
    }
    if dirs.is_empty() {
        return Err(err!(
            "usage: patchkit service-check DIR... [--sdk sdk/sdk.toml]"
        ));
    }
    let mut bad = 0;
    for d in &dirs {
        let found = check(d, &sdk)?;
        for p in &found {
            eprintln!("{p}");
        }
        if found.is_empty() {
            println!("{}: ok", d.display());
        }
        bad += found.len();
    }
    if bad > 0 {
        return Err(err!(
            "{bad} problem{} in the service structure",
            if bad == 1 { "" } else { "s" }
        ));
    }
    Ok(())
}
