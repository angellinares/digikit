//! `patchkit objdiff OLD NEW`: do two objects of the same code differ only in names and placement?
//!
//! Proof for moves of Rust items between modules. Mangled names contain the module path, the function order follows it, and
//! relocations point at section symbols, so a byte compare of the objects fails for a perfect move. Here:
//! - functions and named data are matched by a normalized name (demangled, module path removed; a C symbol keeps its exact name);
//! - an anonymous constant (`.rodata.global.*`) with no relocation inside is a string pool: the compiler merges and orders such constants
//!   as it likes, so it is cut at the places the object refers to (each cut piece is one constant, trailing zero padding removed) and
//!   the pieces are compared as one multiset (content and count) over the whole object. A relocation to a piece is compared by the
//!   piece's content, not by blob or offset. Another anonymous constant (with relocations) is matched by its content;
//! - a panic location (`core::panic::Location`: 16 bytes, one relocation at 0 to its file name) is ignored and counted, unless `--strict`;
//!   the runtime is `panic=abort`, so file, line and column cannot change behaviour. File names only a location refers to are ignored too;
//! - a relocation is compared by its normalized target plus addend, never by section or address;
//! - code is compared byte by byte with the relocated fields masked, and the first difference is shown as an instruction.
//!
//! Items with the same normalized name (two `new` in different modules) are paired by equal content first, then in order of content.

use crate::dko::{Dko, DkoSection};
use crate::error::*;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// One difference. `kind`: `only-old`, `only-new`, `differs`, `c-symbol` (an exact-name symbol was added, removed or changed) or `extern`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Difference {
    pub kind: String,
    pub name: String,
    pub detail: String,
    /// The owner of the item (`--owners`: `S:fx`, `shared`, `S:fx,shared` when old and new differ); empty without owners.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub owner: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Report {
    pub identical: bool,
    pub functions: usize,
    pub data_objects: usize,
    /// Panic locations ignored in the old and the new object (0 with `strict`).
    pub ignored_locations_old: usize,
    pub ignored_locations_new: usize,
    pub differences: Vec<Difference>,
    /// Names that `--erase` merged into one (`old: f, f::<T>`): their pairing is by content, not by name.
    pub warnings: Vec<String>,
}

impl Report {
    pub fn text(&self) -> String {
        let ignored = if self.ignored_locations_old + self.ignored_locations_new == 0 {
            String::new()
        } else if self.ignored_locations_old == self.ignored_locations_new {
            format!("ignored: {} panic locations\n", self.ignored_locations_new)
        } else {
            format!(
                "ignored: {} panic locations (old {})\n",
                self.ignored_locations_new, self.ignored_locations_old
            )
        };
        let warned: String = self
            .warnings
            .iter()
            .map(|w| format!("warning: {w}\n"))
            .collect();
        if self.identical {
            return format!(
                "identical: {} functions, {} data objects\n{ignored}{warned}",
                self.functions, self.data_objects
            );
        }
        let mut s = format!(
            "differences: {} ({} functions and {} data objects match)\n{ignored}{warned}",
            self.differences.len(),
            self.functions,
            self.data_objects
        );
        for d in &self.differences {
            let owner = if d.owner.is_empty() {
                String::new()
            } else {
                format!("  [{}]", d.owner)
            };
            s.push_str(&format!("{:9} {}{owner}\n", d.kind, d.name));
            for l in d.detail.lines() {
                s.push_str(&format!("            {l}\n"));
            }
        }
        s
    }
}

/// Type names erased from every normalized name (`--erase`): the instantiation of a generic with one type is the generic itself.
static ERASE: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();

/// A name without crate and module path: `_RNvNtCs..4dkrt3foo4rd32` is `rd32`, `<dkrt::a::T>::m` is `<T>::m`. A name that is not Rust-mangled
/// (a C symbol, `#[no_mangle]`) stays exact.
pub fn norm_name(name: &str) -> String {
    norm_name_with(name, ERASE.get().map(|v| v.as_slice()).unwrap_or(&[]))
}

/// `norm_name` with an explicit list of erased type names.
fn norm_name_with(name: &str, erase: &[String]) -> String {
    if !name.starts_with("_R") {
        return name.to_string();
    }
    let Ok(d) = rustc_demangle::try_demangle(name) else {
        return name.to_string();
    };
    let s = format!("{d:#}");
    let b = s.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let start = c.is_ascii_alphabetic() || c == b'_';
        let prev_ok = i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_');
        if start && prev_ok {
            let mut j = i;
            while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                j += 1;
            }
            // `module::` before another identifier: a path prefix. Before `<` or `{` it names an item (`f::{closure#0}`): kept.
            if s[j..].starts_with("::")
                && b.get(j + 2)
                    .is_some_and(|n| n.is_ascii_alphabetic() || *n == b'_')
            {
                i = j + 2;
                continue;
            }
            out.push_str(&s[i..j]);
            i = j;
            continue;
        }
        out.push(c as char);
        i += 1;
    }
    for t in erase {
        // `f::<T>` is `f`, `Fx<T>` is `Fx` (the module path of `T` is already gone)
        out = out
            .replace(&format!("::<{t}>"), "")
            .replace(&format!("<{t}>"), "");
    }
    out
}

/// A name with its module path: `_RNvNtCs..4dkrt3foo4rd32` is `dkrt::foo::rd32`. A name that is not Rust-mangled stays exact.
pub(crate) fn full_name(name: &str) -> String {
    if !name.starts_with("_R") {
        return name.to_string();
    }
    match rustc_demangle::try_demangle(name) {
        Ok(d) => format!("{d:#}"),
        Err(_) => name.to_string(),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Text,
    Named,
    Anon,
}

pub(crate) struct Item {
    /// normalized name (`Anon`: empty)
    name: String,
    /// the section's name in the object (for messages)
    pub(crate) raw: String,
    class: Class,
    align: u32,
    size: u32,
    bss: bool,
    /// bytes with every relocated field zeroed
    bytes: Vec<u8>,
    /// (offset, type, normalized target + addend)
    relocs: Vec<(u64, String, String)>,
    /// The demangled name with its module path (`dkrt::fx::send::apply`); a C symbol keeps its exact name; an anonymous
    /// constant has none.
    pub(crate) full: String,
    /// The names of the sections whose relocations point into this item.
    pub(crate) refs: Vec<String>,
}

impl Item {
    pub(crate) fn is_anon(&self) -> bool {
        self.class == Class::Anon
    }

    /// A Rust item (its section name carries a mangled path), not a C symbol or an anonymous constant.
    pub(crate) fn is_rust(&self) -> bool {
        self.class != Class::Anon && split_prefix(&self.raw).1.starts_with("_R")
    }

    /// Hash of the item's content without its position: alignment, size, bytes with relocated fields zeroed, and the normalized
    /// relocation targets.
    pub(crate) fn sig(&self) -> String {
        let mut h = Sha256::new();
        h.update(self.align.to_be_bytes());
        h.update(self.size.to_be_bytes());
        h.update([self.bss as u8]);
        h.update(&self.bytes);
        for (o, t, x) in &self.relocs {
            h.update(o.to_be_bytes());
            h.update(t.as_bytes());
            h.update([0]);
            h.update(x.as_bytes());
            h.update([0]);
        }
        hex(&h.finalize()[..12])
    }
}

const PREFIXES: [&str; 4] = [".text.", ".rodata.", ".data.", ".bss."];

fn split_prefix(name: &str) -> (&str, &str) {
    for p in PREFIXES {
        if let Some(r) = name.strip_prefix(p) {
            return (p, r);
        }
    }
    ("", name)
}

fn is_anon(rest: &str) -> bool {
    ["global.", ".L", "anon.", "str.", "alloc_"]
        .iter()
        .any(|p| rest.starts_with(p))
}

fn class_of(s: &DkoSection) -> Class {
    let (_, rest) = split_prefix(&s.name);
    if s.kind == "text" {
        Class::Text
    } else if is_anon(rest) {
        Class::Anon
    } else {
        Class::Named
    }
}

/// One constant of a string pool: how often it occurs and a short text for messages.
#[derive(Default)]
struct Piece {
    count: usize,
    size: usize,
    shown: String,
}

pub(crate) struct Loaded {
    pub(crate) items: Vec<Item>,
    externs: BTreeSet<String>,
    /// content hash -> piece (the string constants of the object as one multiset)
    pieces: BTreeMap<String, Piece>,
    /// panic locations that were left out
    locations: usize,
}

fn printable(b: &[u8], n: usize) -> String {
    b.iter()
        .take(n)
        .map(|&c| {
            if (32..127).contains(&c) {
                c as char
            } else {
                '.'
            }
        })
        .collect()
}

fn trim_zeros(b: &[u8]) -> &[u8] {
    let n = b.iter().rposition(|&c| c != 0).map_or(0, |i| i + 1);
    &b[..n]
}

pub(crate) fn load(o: &Dko, strict: bool) -> Result<Loaded> {
    let secs: HashMap<&str, &DkoSection> =
        o.sections.iter().map(|s| (s.name.as_str(), s)).collect();
    let defined: HashMap<&str, (&str, u64)> = o
        .symbols
        .iter()
        .filter(|y| !y.name.starts_with(".sec."))
        .filter_map(|y| {
            y.section
                .as_deref()
                .map(|s| (y.name.as_str(), (s, y.value)))
        })
        .collect();
    // the section and offset a relocation target names (None: an undefined symbol)
    let resolve = |sym: &str, addend: i64| -> Option<(&DkoSection, i64)> {
        if let Some(s) = sym.strip_prefix(".sec.").and_then(|sn| secs.get(sn)) {
            return Some((s, addend));
        }
        let (sn, v) = defined.get(sym)?;
        Some((secs.get(sn)?, *v as i64 + addend))
    };
    let mut reloc_in: HashMap<&str, Vec<&crate::dko::DkoReloc>> = HashMap::new();
    for r in &o.relocs {
        reloc_in.entry(r.section.as_str()).or_default().push(r);
    }
    let anon = |s: &DkoSection| class_of(s) == Class::Anon;
    // a panic location: 16 bytes, one relocation at 0 to a constant (its file name)
    let is_location = |s: &DkoSection| {
        !strict
            && anon(s)
            && s.size == 16
            && match reloc_in.get(s.name.as_str()).map(|v| v.as_slice()) {
                Some([r]) => {
                    r.offset == 0 && resolve(&r.symbol, r.addend).is_some_and(|(t, _)| anon(t))
                }
                _ => false,
            }
    };
    let locations: HashSet<&str> = o
        .sections
        .iter()
        .filter(|s| is_location(s))
        .map(|s| s.name.as_str())
        .collect();
    // a string pool: an anonymous constant with no relocation inside
    let pool: HashSet<&str> = o
        .sections
        .iter()
        .filter(|s| {
            anon(s)
                && !locations.contains(s.name.as_str())
                && !reloc_in.contains_key(s.name.as_str())
        })
        .map(|s| s.name.as_str())
        .collect();
    // where the object refers into a pool: `real` (by anything but a location) and `from_loc`
    let mut real: HashMap<&str, BTreeSet<i64>> = HashMap::new();
    // who refers to (pool, offset)
    let mut referrers: HashMap<(&str, i64), HashSet<&str>> = HashMap::new();
    let mut from_loc: HashMap<&str, BTreeSet<i64>> = HashMap::new();
    for r in &o.relocs {
        if let Some((t, off)) = resolve(&r.symbol, r.addend) {
            if pool.contains(t.name.as_str()) {
                let m = if locations.contains(r.section.as_str()) {
                    &mut from_loc
                } else {
                    &mut real
                };
                m.entry(t.name.as_str()).or_default().insert(off);
                if !locations.contains(r.section.as_str()) {
                    referrers
                        .entry((t.name.as_str(), off))
                        .or_default()
                        .insert(r.section.as_str());
                }
            }
        }
    }
    for (sn, v) in defined.values() {
        if pool.contains(sn) {
            real.entry(sn).or_default().insert(*v as i64);
            referrers.entry((sn, *v as i64)).or_default().insert("");
        }
    }
    // the pieces of each pool: cut at every place something refers to (and at 0)
    let mut cuts: HashMap<&str, Vec<i64>> = HashMap::new();
    let mut pieces: BTreeMap<String, Piece> = BTreeMap::new();
    let mut piece_key: HashMap<(&str, i64), String> = HashMap::new();
    for s in o.sections.iter().filter(|s| pool.contains(s.name.as_str())) {
        let bytes = unhex(&s.hex)?;
        let mut at: BTreeSet<i64> = BTreeSet::new();
        at.insert(0);
        at.extend(real.get(s.name.as_str()).into_iter().flatten().copied());
        at.extend(from_loc.get(s.name.as_str()).into_iter().flatten().copied());
        let at: Vec<i64> = at
            .into_iter()
            .filter(|&c| (0..=bytes.len() as i64).contains(&c))
            .collect();
        for (k, &c) in at.iter().enumerate() {
            let end = at.get(k + 1).map_or(bytes.len(), |&e| e as usize);
            let body = trim_zeros(&bytes[c as usize..end]);
            let mut h = Sha256::new();
            h.update(body);
            let key = hex(&h.finalize()[..8]);
            piece_key.insert((s.name.as_str(), c), key.clone());
            let loc_only = from_loc
                .get(s.name.as_str())
                .is_some_and(|f| f.contains(&c))
                && !real.get(s.name.as_str()).is_some_and(|f| f.contains(&c));
            if !loc_only && !body.is_empty() {
                let p = pieces.entry(key).or_default();
                p.count += 1;
                p.size = body.len();
                p.shown = printable(body, 24);
            }
        }
        cuts.insert(s.name.as_str(), at);
    }
    // the content of another anonymous section (a target label must not depend on other labels)
    let content = |s: &DkoSection| -> Result<String> {
        let b = unhex(&s.hex)?;
        let mut h = Sha256::new();
        h.update(s.align.to_be_bytes());
        h.update(s.size.to_be_bytes());
        h.update(&b);
        Ok(format!(
            "{}\"{}\"",
            hex(&h.finalize()[..6]),
            printable(&b, 20).replace('"', ".")
        ))
    };
    let target = |sym: &str, addend: i64, from: &str| -> Result<String> {
        let Some((s, off)) = resolve(sym, addend) else {
            return Ok(format!("{}{:+}", norm_name(sym), addend));
        };
        let n = s.name.as_str();
        if locations.contains(n) {
            return Ok("panic-location".into());
        }
        if pool.contains(n) {
            // the piece that starts at `off` (an offset inside a piece keeps its distance from the start)
            let at = &cuts[n];
            let start = at.iter().copied().rfind(|&c| c <= off).unwrap_or(0);
            let key = &piece_key[&(n, start)];
            // The end pointer of the piece the same item refers to just before it (`cmp` against the end of a table): what follows is
            // whatever the pool placed there, so the target is "the end of that piece". An adjacent piece that the item reads next is
            // compared by this label too: a difference in it is caught by the piece multiset and by the other users of that piece.
            let prev = at.iter().copied().rfind(|&c| c < off);
            if let Some(a) = prev {
                if referrers.get(&(n, a)).is_some_and(|f| f.contains(from)) {
                    return Ok(format!("str-end:{}", piece_key[&(n, a)]));
                }
            }
            return Ok(format!("str:{key}{:+}", off - start));
        }
        let (p, rest) = split_prefix(&s.name);
        Ok(match class_of(s) {
            Class::Anon => format!("anon:{}{:+}", content(s)?, off),
            _ => format!("{}{}{:+}", p, norm_name(rest), off),
        })
    };
    // who refers into each section (the owner of an anonymous constant is the owner of its users)
    let mut referred_by: HashMap<&str, BTreeSet<&str>> = HashMap::new();
    for r in &o.relocs {
        if let Some((t, _)) = resolve(&r.symbol, r.addend) {
            if t.name != r.section {
                referred_by
                    .entry(t.name.as_str())
                    .or_default()
                    .insert(r.section.as_str());
            }
        }
    }
    let mut items = vec![];
    for s in &o.sections {
        if locations.contains(s.name.as_str()) || pool.contains(s.name.as_str()) {
            continue;
        }
        let mut bytes = if s.kind == "bss" {
            vec![]
        } else {
            unhex(&s.hex)?
        };
        let mut relocs = vec![];
        for r in reloc_in.get(s.name.as_str()).into_iter().flatten() {
            let w = match r.rtype.as_str() {
                "cf.pc8" => 1,
                "cf.pc16" => 2,
                _ => 4,
            };
            let at = r.offset as usize;
            if at + w <= bytes.len() {
                bytes[at..at + w].fill(0);
            }
            relocs.push((
                r.offset,
                r.rtype.clone(),
                target(&r.symbol, r.addend, &s.name)?,
            ));
        }
        relocs.sort();
        let class = class_of(s);
        let (_, rest) = split_prefix(&s.name);
        items.push(Item {
            name: if class == Class::Anon {
                String::new()
            } else {
                norm_name(rest)
            },
            raw: s.name.clone(),
            class,
            align: s.align,
            size: s.size,
            bss: s.kind == "bss",
            bytes,
            relocs,
            full: if class == Class::Anon {
                String::new()
            } else {
                full_name(rest)
            },
            refs: referred_by
                .get(s.name.as_str())
                .into_iter()
                .flatten()
                .map(|n| n.to_string())
                .collect(),
        });
    }
    let externs = o
        .symbols
        .iter()
        .filter(|y| y.section.is_none())
        .map(|y| norm_name(&y.name))
        .collect();
    Ok(Loaded {
        items,
        externs,
        pieces,
        locations: locations.len(),
    })
}

fn describe(i: &Item) -> String {
    if i.class == Class::Anon {
        let shown: String = i
            .bytes
            .iter()
            .take(24)
            .map(|&b| {
                if (32..127).contains(&b) {
                    b as char
                } else {
                    '.'
                }
            })
            .collect();
        format!("anonymous constant of {} bytes {shown:?}", i.size)
    } else {
        i.name.clone()
    }
}

/// The names that `--erase` merged within one object: two items of one class whose names differ without the erase and agree with it.
fn erase_collisions(side: &str, items: &[Item]) -> Vec<String> {
    let mut by: BTreeMap<(u8, String), BTreeSet<String>> = BTreeMap::new();
    for i in items.iter().filter(|i| i.class != Class::Anon) {
        let (_, rest) = split_prefix(&i.raw);
        by.entry((i.class as u8, i.name.clone()))
            .or_default()
            .insert(norm_name_with(rest, &[]));
    }
    by.into_iter()
        .filter(|(_, v)| v.len() > 1)
        .map(|((_, name), v)| {
            let all: Vec<String> = v.into_iter().collect();
            format!(
                "{side}: --erase merged {} names into `{name}`: {}",
                all.len(),
                all.join(", ")
            )
        })
        .collect()
}

fn c_visible(i: &Item) -> bool {
    i.class != Class::Anon && !i.raw.contains("._R")
}

/// The first difference of two items with equal normalized names, as text.
fn explain(old: &Item, new: &Item) -> String {
    let mut out = vec![];
    if old.align != new.align {
        out.push(format!("align {} -> {}", old.align, new.align));
    }
    if old.size != new.size {
        out.push(format!("size {} -> {} bytes", old.size, new.size));
    }
    if old.bss {
        return out.join("\n");
    }
    // first differing byte or relocation
    let n = old.bytes.len().min(new.bytes.len());
    let mut at = (0..n).find(|&k| old.bytes[k] != new.bytes[k]);
    let mut why = "bytes";
    for (a, b) in old.relocs.iter().zip(&new.relocs) {
        if a != b {
            if at.is_none_or(|x| (a.0.min(b.0) as usize) < x) {
                at = Some(a.0.min(b.0) as usize);
                why = "relocation";
            }
            break;
        }
    }
    if at.is_none() && old.relocs.len() != new.relocs.len() {
        let l = old.relocs.len().min(new.relocs.len());
        let o = old
            .relocs
            .get(l)
            .or(new.relocs.get(l))
            .map(|r| r.0 as usize);
        at = o;
        why = "relocation count";
    }
    let Some(at) = at else {
        return out.join("\n");
    };
    out.push(format!(
        "first difference at +{at:#x} ({why}); old, then new"
    ));
    if old.class == Class::Text {
        // instruction starts of the old side up to `at`
        let mut starts = vec![];
        let mut p = 0usize;
        while p <= at && p < old.bytes.len() {
            starts.push(p);
            let l = coldfire::decode_at(&old.bytes, 0, p).map_or(2, |i| (i.len as usize).max(2));
            p += l;
        }
        let first = starts.last().copied().unwrap_or(0);
        for &s in starts.iter().rev().take(3).rev() {
            if s != first {
                out.push(format!("  {}", insn_line(old, s)));
            }
        }
        out.push(format!("- {}", insn_line(old, first)));
        out.push(format!("+ {}", insn_line(new, first)));
    } else {
        let w = |i: &Item| {
            let e = (at + 8).min(i.bytes.len());
            hex(i.bytes.get(at..e).unwrap_or(&[]))
        };
        out.push(format!("- {}", w(old)));
        out.push(format!("+ {}", w(new)));
        for r in old.relocs.iter().filter(|r| r.0 as usize == at) {
            out.push(format!("- reloc {} {}", r.1, r.2));
        }
        for r in new.relocs.iter().filter(|r| r.0 as usize == at) {
            out.push(format!("+ reloc {} {}", r.1, r.2));
        }
    }
    out.join("\n")
}

fn insn_line(i: &Item, at: usize) -> String {
    let (len, text) = match coldfire::decode_at(&i.bytes, 0, at) {
        Some(x) => (
            (x.len as usize).max(2),
            format!("{}{}", x.mnemonic(), x.size.suffix()),
        ),
        None => (2, "??".to_string()),
    };
    let end = (at + len).min(i.bytes.len());
    let mut s = format!("+{at:#06x}  {:<12} {}", hex(&i.bytes[at..end]), text);
    for r in i
        .relocs
        .iter()
        .filter(|r| (r.0 as usize) < end && r.0 as usize + 4 > at)
    {
        s.push_str(&format!("  [reloc +{:#x} {}]", r.0 as usize - at, r.2));
    }
    s
}

/// Compare two objects (panic locations ignored).
pub fn diff(old: &Dko, new: &Dko) -> Result<Report> {
    diff_with(old, new, false)
}

/// Compare two objects; with `strict` a panic location (file, line, column) is compared like any constant.
pub fn diff_with(old: &Dko, new: &Dko, strict: bool) -> Result<Report> {
    diff_owned(old, new, strict, None)
}

/// `diff_with`, and with `owners` each difference names the owner of its item (`owners.toml`, see `crate::owners`).
pub fn diff_owned(
    old: &Dko,
    new: &Dko,
    strict: bool,
    owners: Option<&crate::owners::Owners>,
) -> Result<Report> {
    if old.arch != "cf" || new.arch != "cf" {
        return Err(err!(
            "objdiff compares ColdFire objects (got {} and {})",
            old.arch,
            new.arch
        ));
    }
    let Loaded {
        items: a,
        externs: ae,
        pieces: pa,
        locations: la,
    } = load(old, strict)?;
    let Loaded {
        items: b,
        externs: be,
        pieces: pb,
        locations: lb,
    } = load(new, strict)?;
    let mut rep = Report {
        ignored_locations_old: la,
        ignored_locations_new: lb,
        ..Report::default()
    };
    if ERASE.get().is_some_and(|e| !e.is_empty()) {
        rep.warnings.extend(erase_collisions("old", &a));
        rep.warnings.extend(erase_collisions("new", &b));
    }
    // raw section name -> owner, per side
    let who = |items: &[Item]| -> Result<HashMap<String, String>> {
        let Some(owners) = owners else {
            return Ok(HashMap::new());
        };
        let w = crate::owners::assign(items, owners)?;
        Ok(items
            .iter()
            .zip(w)
            .map(|(i, o)| (i.raw.clone(), o.to_string()))
            .collect())
    };
    let (wa, wb) = (who(&a)?, who(&b)?);
    let owner_of = |old: Option<&Item>, new: Option<&Item>| -> String {
        let mut v: Vec<&str> = vec![];
        for (i, w) in [(old, &wa), (new, &wb)] {
            if let Some(o) = i.and_then(|i| w.get(&i.raw)) {
                if !v.contains(&o.as_str()) {
                    v.push(o);
                }
            }
        }
        v.join(",")
    };
    let mut push = |kind: &str, name: String, detail: String, owner: String| {
        rep.differences.push(Difference {
            kind: kind.into(),
            name,
            detail,
            owner,
        })
    };
    // group by (class, name)
    let key = |i: &Item| (i.class as u8, i.name.clone());
    let mut ga: BTreeMap<(u8, String), Vec<&Item>> = BTreeMap::new();
    let mut gb: BTreeMap<(u8, String), Vec<&Item>> = BTreeMap::new();
    a.iter().for_each(|i| ga.entry(key(i)).or_default().push(i));
    b.iter().for_each(|i| gb.entry(key(i)).or_default().push(i));
    let (mut funcs, mut datas) = (0usize, 0usize);
    let keys: BTreeSet<_> = ga.keys().chain(gb.keys()).cloned().collect();
    let flag = |kind_only: &str,
                i: &Item,
                is_old: bool,
                push: &mut dyn FnMut(&str, String, String, String)| {
        let kind = if c_visible(i) { "c-symbol" } else { kind_only };
        let detail = if c_visible(i) {
            format!("exact-name symbol {} ({kind_only})", i.raw)
        } else {
            String::new()
        };
        let owner = if is_old {
            owner_of(Some(i), None)
        } else {
            owner_of(None, Some(i))
        };
        push(kind, describe(i), detail, owner);
    };
    for k in keys {
        let mut xs = ga.remove(&k).unwrap_or_default();
        let mut ys = gb.remove(&k).unwrap_or_default();
        let count = |n: usize, c: u8, f: &mut usize, d: &mut usize| {
            if c == Class::Text as u8 {
                *f += n
            } else {
                *d += n
            }
        };
        // equal content first
        let mut i = 0;
        while i < xs.len() {
            let s = xs[i].sig();
            if let Some(j) = ys.iter().position(|y| y.sig() == s) {
                ys.remove(j);
                xs.remove(i);
                count(1, k.0, &mut funcs, &mut datas);
            } else {
                i += 1;
            }
        }
        if k.0 == Class::Anon as u8 {
            xs.iter().for_each(|x| flag("only-old", x, true, &mut push));
            ys.iter()
                .for_each(|y| flag("only-new", y, false, &mut push));
            continue;
        }
        xs.sort_by_key(|x| x.sig());
        ys.sort_by_key(|y| y.sig());
        let n = xs.len().min(ys.len());
        for j in 0..n {
            if c_visible(xs[j]) && xs[j].raw != ys[j].raw {
                push(
                    "c-symbol",
                    describe(xs[j]),
                    format!("renamed {} -> {}", xs[j].raw, ys[j].raw),
                    owner_of(Some(xs[j]), Some(ys[j])),
                );
            }
            push(
                "differs",
                describe(xs[j]),
                explain(xs[j], ys[j]),
                owner_of(Some(xs[j]), Some(ys[j])),
            );
        }
        xs[n..]
            .iter()
            .for_each(|x| flag("only-old", x, true, &mut push));
        ys[n..]
            .iter()
            .for_each(|y| flag("only-new", y, false, &mut push));
    }
    // the string constants as one multiset
    let mut strings = 0usize;
    for (k, x) in &pa {
        let n = pb.get(k).map_or(0, |y| y.count);
        strings += x.count.min(n);
        if x.count > n {
            push(
                "only-old",
                format!("string constant {:?} of {} bytes", x.shown, x.size),
                format!("old {} times, new {} times", x.count, n),
                String::new(),
            );
        }
    }
    for (k, y) in &pb {
        let n = pa.get(k).map_or(0, |x| x.count);
        if y.count > n {
            push(
                "only-new",
                format!("string constant {:?} of {} bytes", y.shown, y.size),
                format!("old {} times, new {} times", n, y.count),
                String::new(),
            );
        }
    }
    datas += strings;
    for e in ae.difference(&be) {
        push(
            "extern",
            e.clone(),
            "undefined symbol only in old".into(),
            String::new(),
        );
    }
    for e in be.difference(&ae) {
        push(
            "extern",
            e.clone(),
            "undefined symbol only in new".into(),
            String::new(),
        );
    }
    rep.functions = funcs;
    rep.data_objects = datas;
    rep.identical = rep.differences.is_empty();
    Ok(rep)
}

/// `patchkit objdiff OLD NEW [--format text|json] [--strict] [--erase TYPE]... [--owners owners.toml]`; returns the exit code.
pub fn run(rest: &[String]) -> Result<i32> {
    const U: &str = "usage: patchkit objdiff OLD.dko.json NEW.dko.json [--format text|json] [--strict] [--erase TYPE]... [--owners owners.toml]";
    let mut strict = false;
    let mut erase: Vec<String> = vec![];
    let mut files = vec![];
    let mut format = "text".to_string();
    let mut owners_file: Option<String> = None;
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        if a == "--format" {
            format = it.next().ok_or_else(|| Error(U.into()))?.clone();
        } else if a == "--strict" {
            strict = true;
        } else if a == "--owners" {
            owners_file = Some(it.next().ok_or_else(|| Error(U.into()))?.clone());
        } else if a == "--erase" {
            erase.push(it.next().ok_or_else(|| Error(U.into()))?.clone());
        } else if a.starts_with("--") {
            return Err(Error(U.into()));
        } else {
            files.push(a.clone());
        }
    }
    if files.len() != 2 || !matches!(format.as_str(), "text" | "json") {
        return Err(Error(U.into()));
    }
    let _ = ERASE.set(erase);
    let read = |f: &str| -> Result<Dko> {
        Dko::parse(&std::fs::read_to_string(f).map_err(|e| Error(format!("{f}: {e}")))?)
            .map_err(|e| Error(format!("{f}: {e}")))
    };
    let owners = owners_file
        .map(|f| crate::owners::Owners::read(std::path::Path::new(&f)))
        .transpose()?;
    let rep = diff_owned(
        &read(&files[0])?,
        &read(&files[1])?,
        strict,
        owners.as_ref(),
    )?;
    if format == "json" {
        println!("{}", serde_json::to_string_pretty(&rep)?);
    } else {
        print!("{}", rep.text());
    }
    Ok(if rep.identical { 0 } else { 1 })
}
