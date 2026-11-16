//! `patchkit service-check DIR... [--sdk sdk/sdk.toml]`: the structure check of a service crate (`module:check service/NAME`).
//!
//! A service is a crate `modules/core/services/NAME` with one layout (docs/plans/runtime-and-services.md 5.12, R12-1). The check reads the sources and
//! `sdk.toml`; it builds nothing and runs nothing. It reports every problem, one line each, and the exit status is 1 when there is one.
//!
//! - **Layout.** `Cargo.toml` (package `dkrt-svc-NAME`), `src/lib.rs`, `src/records.rs`, `src/slots.rs` and `tests/` with at least one `.rs` file. A folder
//!   `src/dsp/` has a `mod.rs`, and a service whose `Service` impl has `dsp_*` handlers has the folder. `lib.rs` declares `pub mod records`, `pub mod slots`
//!   (and `pub mod dsp` with the folder). Other modules in `src/` are the service's own.
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

/// The sources of the crate with `//` comments blanked (line structure kept), as (path relative to `dir`, text).
fn sources(dir: &Path) -> Vec<(String, String)> {
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
    let mut out = vec![];
    walk(dir, &dir.join("src"), &mut out);
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
fn manifest(sdk: &Path, name: &str) -> Result<(BTreeSet<String>, Option<String>)> {
    let text = std::fs::read_to_string(sdk).map_err(|e| err!("{}: {e}", sdk.display()))?;
    let v: toml::Table = text.parse().map_err(|e| err!("{}: {e}", sdk.display()))?;
    let row = v
        .get("service")
        .and_then(|s| s.as_array())
        .into_iter()
        .flatten()
        .filter_map(|r| r.as_table())
        .find(|r| r.get("name").and_then(|n| n.as_str()) == Some(name))
        .and_then(|r| r.get("rust"))
        .and_then(|r| r.as_str())
        .map(str::to_string);
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

/// Every problem of the service crate at `dir`; empty when it is as the layout says.
pub fn check(dir: &Path, sdk: &Path) -> Result<Vec<String>> {
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
    match row {
        None => problem(format!(
            "{} has no [[service]] row named `{name}`",
            sdk.display()
        )),
        Some(r) if !r.starts_with(&format!("{}::", want_pkg.replace('-', "_"))) => {
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
