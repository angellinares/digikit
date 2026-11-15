//! `patchkit owners RUNTIME.dko.json [--owners owners.toml] --out DIR`: one hash per owner of the runtime object.
//!
//! The runtime is one object, but its items belong to services and areas (docs/plans/test-architecture.md, T1e). `owners.toml` maps
//! the module path prefixes of Rust items to tags (`S:fx`, `C:storage`) and lists the shared set. A device test that is tagged `S:fx`
//! keys on `S:fx.sha256` and `shared.sha256` only, so an edit of another service does not rerun it.
//!
//! Each hash is over the sorted position-free signatures of the owner's items (`objdiff::Item::sig`: bytes with relocated fields zeroed,
//! relocation targets by normalized name, string constants by content), plus the exact name of a C symbol. Moving an item inside
//! the object, or to another module of the same owner, changes no hash; an inlined callee changes its caller's owner; moving an item
//! to another owner changes both.
//!
//! The owner of an item comes from the first module path of its demangled name (`<dkrt::a::T as X>::m` belongs to `dkrt::a::T`,
//! `core::ptr::drop_in_place::<dkrt::a::T>` to `core::`). The longest prefix at a `::` boundary wins. An item that no prefix matches is
//! an error: `owners.toml` stays complete. A C symbol (an exported `#[no_mangle]` function) is shared. An anonymous constant belongs
//! to the owner of the items that use it when all users have one owner, and is shared otherwise.

use crate::dko::Dko;
use crate::error::*;
use crate::objdiff::{Item, load};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

/// The tag of the shared set, and the key of its list in `owners.toml`.
pub const SHARED: &str = "shared";

/// Primitive types: an item whose path starts with one (`<[u8]>::len`) is part of `core`.
const PRIMITIVES: [&str; 17] = [
    "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize", "f32",
    "f64", "bool", "char", "str",
];

#[derive(Debug, Clone)]
pub struct Owners {
    /// (module path prefix, tag)
    prefixes: Vec<(String, String)>,
    /// Every tag of the file, `shared` included.
    tags: Vec<String>,
}

impl Owners {
    /// `owners.toml`: a table per tag (`"S:fx" = ["dkrt_svc_fx"]`) and `shared = [...]`, all arrays of module path prefixes.
    pub fn parse(text: &str) -> Result<Owners> {
        let table: BTreeMap<String, Vec<String>> =
            toml::from_str(text).map_err(|e| err!("owners.toml: {e}"))?;
        let mut prefixes: Vec<(String, String)> = vec![];
        let mut seen: HashMap<String, String> = HashMap::new();
        for (tag, list) in &table {
            let ok = tag == SHARED
                || (tag.len() > 2
                    && ["S:", "C:"].iter().any(|p| tag.starts_with(p))
                    && !tag.contains('/'));
            if !ok {
                return Err(err!(
                    "owners.toml: `{tag}` is not `shared`, `S:<service>` or `C:<area>`"
                ));
            }
            for p in list {
                if p.is_empty() || p.contains(char::is_whitespace) {
                    return Err(err!("owners.toml: bad prefix {p:?} under `{tag}`"));
                }
                if let Some(other) = seen.insert(p.clone(), tag.clone()) {
                    return Err(err!(
                        "owners.toml: prefix `{p}` is under both `{other}` and `{tag}`"
                    ));
                }
                prefixes.push((p.clone(), tag.clone()));
            }
        }
        let mut tags: Vec<String> = table.keys().cloned().collect();
        if !tags.iter().any(|t| t == SHARED) {
            tags.push(SHARED.to_string());
        }
        Ok(Owners { prefixes, tags })
    }

    pub fn read(path: &Path) -> Result<Owners> {
        let text =
            std::fs::read_to_string(path).map_err(|e| Error(format!("{}: {e}", path.display())))?;
        Owners::parse(&text)
    }

    pub fn tags(&self) -> &[String] {
        &self.tags
    }

    /// The tag whose longest prefix matches `path` at a `::` boundary.
    fn lookup(&self, path: &str) -> Option<&str> {
        self.prefixes
            .iter()
            .filter(|(p, _)| {
                path == p
                    || path
                        .strip_prefix(p.as_str())
                        .is_some_and(|r| r.starts_with("::"))
            })
            .max_by_key(|(p, _)| p.len())
            .map(|(_, t)| t.as_str())
    }

    /// The owner of a named item by its full name (see the module comment).
    pub(crate) fn owner_of_name(&self, item: &Item) -> Result<&str> {
        if !item.is_rust() {
            return Ok(SHARED); // a C symbol
        }
        let path = first_path(&item.full)
            .ok_or_else(|| err!("item {} has no module path: {}", item.raw, item.full))?;
        if !path.contains("::") && PRIMITIVES.contains(&path.as_str()) {
            return self.lookup("core").ok_or_else(|| {
                err!(
                    "no owner for primitive item {} (add `core` to owners.toml)",
                    item.full
                )
            });
        }
        self.lookup(&path)
            .ok_or_else(|| err!("no owner for module path `{path}` (item {})", item.full))
    }
}

/// The first module path of a demangled name: leading `<`, `&`, `*`, `[`, `(`, `mut`, `const` and `dyn` are skipped, the path ends
/// before generics (`<`) and `::{closure}` / `::<T>`.
fn first_path(full: &str) -> Option<String> {
    let b = full.as_bytes();
    let mut i = 0;
    loop {
        while i < b.len() && !(b[i].is_ascii_alphanumeric() || b[i] == b'_') {
            i += 1;
        }
        if i >= b.len() {
            return None;
        }
        let start = i;
        while i < b.len() {
            if b[i].is_ascii_alphanumeric() || b[i] == b'_' {
                i += 1;
            } else if full[i..].starts_with("::")
                && b.get(i + 2)
                    .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
            {
                i += 2;
            } else {
                break;
            }
        }
        let tok = &full[start..i];
        if matches!(tok, "mut" | "const" | "dyn" | "impl") {
            continue;
        }
        return Some(tok.to_string());
    }
}

/// One owned item: its owner and its line in the owner's hash.
pub struct Owned {
    pub owner: String,
    pub line: String,
}

/// The owner of every item of `items`, in order: named items by path, anonymous constants by their users.
pub(crate) fn assign<'a>(items: &[Item], owners: &'a Owners) -> Result<Vec<&'a str>> {
    let mut out: Vec<Option<&'a str>> = vec![None; items.len()];
    let by_raw: HashMap<&str, usize> = items
        .iter()
        .enumerate()
        .map(|(i, x)| (x.raw.as_str(), i))
        .collect();
    // every unmapped path is listed at once (module path, items), so one run shows what `owners.toml` lacks
    let mut unmapped: BTreeMap<String, usize> = BTreeMap::new();
    for (i, x) in items.iter().enumerate() {
        if x.is_anon() {
            continue;
        }
        match owners.owner_of_name(x) {
            Ok(o) => out[i] = Some(o),
            Err(_) => {
                let path = first_path(&x.full).unwrap_or_else(|| x.raw.clone());
                let depth: Vec<&str> = path.split("::").take(2).collect();
                *unmapped.entry(depth.join("::")).or_default() += 1;
            }
        }
    }
    if !unmapped.is_empty() {
        let shown: Vec<String> = unmapped
            .iter()
            .take(200)
            .map(|(p, n)| format!("  {p} ({n})"))
            .collect();
        return Err(err!(
            "owners.toml maps no owner for {} module paths (to two components):\n{}{}",
            unmapped.len(),
            shown.join("\n"),
            if unmapped.len() > 200 { "\n  ..." } else { "" }
        ));
    }
    // An anonymous constant takes the owner that all its users share. Users that are anonymous too are resolved first (a few passes).
    for _ in 0..8 {
        let mut changed = false;
        for (i, x) in items.iter().enumerate() {
            if out[i].is_some() || !x.is_anon() {
                continue;
            }
            let users: Vec<Option<&str>> = x
                .refs
                .iter()
                .filter_map(|r| by_raw.get(r.as_str()))
                .map(|&j| out[j])
                .collect();
            if users.is_empty() {
                out[i] = Some(SHARED);
                changed = true;
            } else if users.iter().all(|u| u.is_some()) {
                let first = users[0];
                out[i] = if users.iter().all(|u| *u == first) {
                    first
                } else {
                    Some(SHARED)
                };
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    Ok(out.into_iter().map(|o| o.unwrap_or(SHARED)).collect())
}

/// The owner and the hash line of every item of the object.
pub fn owned_items(o: &Dko, owners: &Owners) -> Result<Vec<Owned>> {
    if o.arch != "cf" {
        return Err(err!("owners reads a ColdFire object (got {})", o.arch));
    }
    let loaded = load(o, false)?;
    let who = assign(&loaded.items, owners)?;
    Ok(loaded
        .items
        .iter()
        .zip(who)
        .map(|(x, owner)| Owned {
            owner: owner.to_string(),
            // a Rust item has no name in its line, so a move inside the object changes nothing; a C symbol keeps its exact name
            line: if x.is_rust() || x.is_anon() {
                x.sig()
            } else {
                format!("{}\t{}", x.full, x.sig())
            },
        })
        .collect())
}

/// Per tag: (items, hash). Every tag of `owners.toml` is there, also with no item.
pub fn hashes(o: &Dko, owners: &Owners) -> Result<BTreeMap<String, (usize, String)>> {
    let mut lines: BTreeMap<String, Vec<String>> =
        owners.tags().iter().map(|t| (t.clone(), vec![])).collect();
    for item in owned_items(o, owners)? {
        lines.entry(item.owner).or_default().push(item.line);
    }
    Ok(lines
        .into_iter()
        .map(|(tag, mut l)| {
            l.sort();
            let mut h = Sha256::new();
            for line in &l {
                h.update(line.as_bytes());
                h.update(b"\n");
            }
            (tag, (l.len(), hex(&h.finalize())))
        })
        .collect())
}

pub const DEFAULT_OWNERS: &str = "modules/core/runtime/owners.toml";

/// `patchkit owners RUNTIME.dko.json [--owners owners.toml] --out DIR`: writes `<tag>.sha256` for every tag and `shared.sha256`,
/// and prints one line per tag.
pub fn run(rest: &[String]) -> Result<i32> {
    let usage =
        format!("usage: patchkit owners RUNTIME.dko.json [--owners {DEFAULT_OWNERS}] --out DIR");
    let (mut file, mut owners, mut out) = (None, DEFAULT_OWNERS.to_string(), None);
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--owners" => owners = it.next().ok_or_else(|| Error(usage.clone()))?.clone(),
            "--out" => out = Some(it.next().ok_or_else(|| Error(usage.clone()))?.clone()),
            f if f.starts_with("--") => return Err(Error(usage)),
            f => {
                if file.replace(f.to_string()).is_some() {
                    return Err(Error(usage));
                }
            }
        }
    }
    let (Some(file), Some(out)) = (file, out) else {
        return Err(Error(usage));
    };
    let owners = Owners::read(Path::new(&owners))?;
    let o = Dko::parse(&std::fs::read_to_string(&file).map_err(|e| Error(format!("{file}: {e}")))?)
        .map_err(|e| Error(format!("{file}: {e}")))?;
    let hashes = hashes(&o, &owners)?;
    std::fs::create_dir_all(&out).map_err(|e| Error(format!("{out}: {e}")))?;
    for (tag, (n, h)) in &hashes {
        let path = Path::new(&out).join(format!("{tag}.sha256"));
        std::fs::write(&path, format!("{h}\n"))
            .map_err(|e| Error(format!("{}: {e}", path.display())))?;
        println!("{tag:<14} {n:5} items  {}", &h[..16]);
    }
    Ok(0)
}
