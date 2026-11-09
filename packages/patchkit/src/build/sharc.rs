//! SHARC+ objects. The source text (`#include` expansion and `#ifdef` selection) and its hash are made here; [`sharc_obj`](super::sharc_obj)
//! assembles it with Selache as a library, applies the encoding-bug guards and re-decodes every instruction. All of it runs in this
//! process and builds for wasm32; nothing is a child process.
//!
//! The text of a native build is read from files ([`source_text`]); the browser passes the texts ([`source_text_from`]).

use super::toolchain as tc;
use crate::dko::Dko;
use crate::error::*;
use std::path::{Path, PathBuf};

/// The include directories of a SHARC+ object of the module: its generated and own sources, then the SDK's. The directories of the
/// sources come after these.
pub fn include_dirs(root: &Path, moddir: &Path) -> Vec<PathBuf> {
    let mut v = vec![moddir.join("build/gen"), moddir.join("src")];
    v.extend(tc::global_include_dirs(false).iter().map(|d| root.join(d)));
    v
}

/// `Path.read_text()`: UTF-8, with `\r\n` and `\r` read as `\n`.
fn read_text(p: &Path) -> Result<String> {
    Ok(normalize(&tc::read_text(p)?))
}

/// Line ends as `\n` (`\r\n` and `\r` are read as `\n`).
fn normalize(t: &str) -> String {
    if t.contains('\r') {
        t.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        t.to_string()
    }
}

/// `#include "name"` alone on a line (trailing blanks allowed): the name.
fn include_name(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("#include")?;
    let t = rest.trim_start_matches(|c: char| c.is_whitespace());
    if t.len() == rest.len() {
        return None;
    }
    let t = t.strip_prefix('"')?;
    let end = t.find('"')?;
    if end == 0 || !t[end + 1..].chars().all(|c| c == ' ' || c == '\t') {
        return None;
    }
    Some(&t[..end])
}

/// Where an `#include "name"` is found: its text, or None.
pub type Finder<'a> = &'a dyn Fn(&str) -> Result<Option<String>>;

/// Replace `#include "name"` lines by the file's text (searched in `dirs`).
pub fn expand_includes(text: &str, dirs: &[PathBuf], depth: u32) -> Result<String> {
    let find = |name: &str| -> Result<Option<String>> {
        match dirs.iter().map(|d| d.join(name)).find(|f| f.is_file()) {
            Some(f) => read_text(&f).map(Some),
            None => Ok(None),
        }
    };
    let places = || {
        format!(
            "{:?}",
            dirs.iter()
                .map(|d| d.display().to_string())
                .collect::<Vec<_>>()
        )
    };
    expand_with(text, &find, &places, depth)
}

/// [`expand_includes`] over any finder; `places` names where it looked, for the error.
fn expand_with(
    text: &str,
    find: Finder,
    places: &dyn Fn() -> String,
    depth: u32,
) -> Result<String> {
    if depth > 4 {
        return Err(err!("#include nests deeper than 4 levels"));
    }
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        match include_name(line) {
            Some(name) => {
                let Some(t) = find(name)? else {
                    return Err(err!("#include \"{name}\" not found in {}", places()));
                };
                out.push_str(&expand_with(&t, find, places, depth + 1)?);
            }
            None => out.push_str(line),
        }
    }
    Ok(out)
}

/// `#ifdef NAME` / `#ifndef NAME` / `#else` / `#endif` lines (alone on their line, nestable). A line outside the taken branches is
/// dropped together with the directive lines. `#define NAME` / `#undef NAME` lines of the taken branches count as defines too
/// (the lines stay in the output).
pub fn apply_conditionals(text: &str, defines: &[String]) -> Result<String> {
    let mut out: Vec<&str> = vec![];
    let mut stack: Vec<bool> = vec![];
    let mut defined: Vec<String> = defines.to_vec();
    for (n, line) in text.split('\n').enumerate() {
        let n = n + 1;
        let Some((kind, name)) = directive(line) else {
            if stack.iter().all(|b| *b) {
                if let Some((def, name)) = define_directive(line) {
                    defined.retain(|d| d != name);
                    if def {
                        defined.push(name.to_string());
                    }
                }
                out.push(line);
            }
            continue;
        };
        match kind {
            "ifdef" | "ifndef" => {
                let Some(name) = name else {
                    return Err(err!("line {n}: #{kind} needs a name"));
                };
                stack.push(defined.iter().any(|d| d == name) == (kind == "ifdef"));
            }
            _ if stack.is_empty() => return Err(err!("line {n}: #{kind} without #ifdef")),
            "else" => {
                let l = stack.len() - 1;
                stack[l] = !stack[l];
            }
            _ => {
                stack.pop();
            }
        }
    }
    if !stack.is_empty() {
        return Err(err!("#ifdef without #endif"));
    }
    Ok(out.join("\n"))
}

/// `^#(define|undef)[ \t]+(\w+)`: (is define, name).
fn define_directive(line: &str) -> Option<(bool, &str)> {
    let rest = line.strip_prefix('#')?;
    let (def, after) = match rest.strip_prefix("define") {
        Some(a) => (true, a),
        None => (false, rest.strip_prefix("undef")?),
    };
    let name = after
        .strip_prefix([' ', '\t'])?
        .trim_start_matches([' ', '\t']);
    let end = name
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(name.len());
    (end > 0).then(|| (def, &name[..end]))
}

/// `^#(ifdef|ifndef|else|endif)(?:[ \t]+(\w+))?[ \t]*$`
fn directive(line: &str) -> Option<(&'static str, Option<&str>)> {
    let rest = line.strip_prefix('#')?;
    for k in ["ifdef", "ifndef", "else", "endif"] {
        let Some(after) = rest.strip_prefix(k) else {
            continue;
        };
        let t = after.trim_end_matches([' ', '\t']);
        if t.is_empty() {
            return Some((k, None));
        }
        let name = t.trim_start_matches([' ', '\t']);
        if name.len() < t.len()
            && !name.is_empty()
            && name.chars().all(|c| c.is_alphanumeric() || c == '_')
        {
            return Some((k, Some(name)));
        }
    }
    None
}

/// The text of a SHARC+ object: the sources joined, `#include`s expanded, the `#ifdef` alternatives selected.
pub fn source_text(
    root: &Path,
    moddir: &Path,
    sources: &[PathBuf],
    defines: &[String],
) -> Result<String> {
    source_text_in(include_dirs(root, moddir), sources, defines)
}

/// [`source_text`] with the include directories given; the directories of the sources come after them.
pub fn source_text_in(
    mut dirs: Vec<PathBuf>,
    sources: &[PathBuf],
    defines: &[String],
) -> Result<String> {
    for p in sources {
        let d = p.parent().unwrap_or(Path::new(".")).to_path_buf();
        if !dirs.contains(&d) {
            dirs.push(d);
        }
    }
    let mut texts = vec![];
    for p in sources {
        texts.push(read_text(p)?);
    }
    apply_conditionals(&expand_includes(&texts.join("\n"), &dirs, 0)?, defines)
}

/// The `source_sha256` of a SHARC+ object: the SHA-256 of that text.
pub fn source_hash(text: &str) -> String {
    sha256_hex(text.as_bytes())
}

/// The text of SHARC+ sources given as texts: the sources joined, `#include`s taken from `includes` (name -> text; the browser has no
/// directories), the `#ifdef` alternatives selected.
pub fn source_text_from(
    sources: &[String],
    includes: &std::collections::BTreeMap<String, String>,
    defines: &[String],
) -> Result<String> {
    let find = |name: &str| -> Result<Option<String>> { Ok(includes.get(name).cloned()) };
    let places = || format!("the {} include files given", includes.len());
    let joined = sources
        .iter()
        .map(|t| normalize(t))
        .collect::<Vec<_>>()
        .join("\n");
    apply_conditionals(&expand_with(&joined, &find, &places, 0)?, defines)
}

/// Assemble and verify `text`: the object (`sharc_obj::object_from_text`).
pub fn sharc_object(text: &str) -> Result<Dko> {
    super::sharc_obj::object_from_text(text)
}
