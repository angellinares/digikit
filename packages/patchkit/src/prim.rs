//! Generic primitives for framework modules (phase 3b.1): image-copy sections (P1), registries and entries (P2),
//! expressions in manifests (P3, `expr.rs`), the `hook` op (P4), device filters on every table (P5) and
//! multi-site anchors for `write` (P6, with `tools/patchkit/port_resolve.py`). `apply.rs` drives them; this file holds the
//! manifest checks, the byte-level builders and the plan records.

use crate::error::*;
use crate::expr::{self, Env};
use crate::module::*;
use serde::Serialize;
use std::collections::BTreeMap;

/// Active on this device? (an empty filter means all)
pub fn on(devices: &[String], dev: &str) -> bool {
    devices.is_empty() || devices.iter().any(|d| d == dev)
}

fn check_devices(owner: &str, what: &str, d: &[String]) -> Result<()> {
    for x in d {
        if crate::devdata::by_short(x).is_none() {
            return Err(err!(
                "{owner}: {what}: device {x:?} is not one of {}",
                crate::devdata::short_list()
            ));
        }
    }
    Ok(())
}

/// `[a-z][a-z0-9_]*`
fn ident_ok(s: &str) -> bool {
    let mut c = s.chars();
    c.next().is_some_and(|c| c.is_ascii_lowercase())
        && c.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// `u8:EXPR` -> (1, "EXPR"); no prefix: a 32-bit word.
pub fn word_spec(t: &str) -> Result<(usize, &str)> {
    let t = t.trim();
    for (p, w) in [("u8:", 1), ("u16:", 2), ("u32:", 4)] {
        if let Some(r) = t.strip_prefix(p) {
            return Ok((w, r));
        }
    }
    if let Some((p, _)) = t.split_once(':') {
        return Err(err!("row word {t:?}: unknown width {p:?} (u8, u16 or u32)"));
    }
    Ok((4, t))
}

/// Checks of a manifest that need no firmware: operations, sections, registries, entries, device filters.
pub fn validate_manifest(m: &Manifest) -> Result<()> {
    let id = m.module.id.as_str();
    for o in &m.object {
        check_devices(id, "object", &o.devices)?;
    }
    for c in &m.claim {
        check_devices(id, "claim", &c.devices)?;
    }
    for op in &m.op {
        check_devices(id, &format!("op {}", op.op), &op.devices)?;
        match op.op.as_str() {
            "write" => {
                if op.hex.is_some() == op.value.is_some() {
                    return Err(err!("{id}: write needs hex or value (not both)"));
                }
                if let Some(v) = &op.value {
                    match op.width {
                        Some(1 | 2 | 4) => {}
                        w => {
                            return Err(err!(
                                "{id}: write of a value needs width 1, 2 or 4 (got {w:?})"
                            ));
                        }
                    }
                    expr::parse(&v.text()).map_err(|e| err!("{id}: write value: {e}"))?;
                    if !op.reloc.is_empty() {
                        return Err(err!("{id}: a write of a value takes no relocations"));
                    }
                } else if op.width.is_some() {
                    return Err(err!("{id}: width goes with value, not hex"));
                }
                match &op.anchors {
                    Some(g) => {
                        if !op.anchor.is_empty() || !g.ends_with(".*") || g.len() < 3 {
                            return Err(err!(
                                "{id}: write: `anchors` is `PREFIX.*` and replaces `anchor`"
                            ));
                        }
                        if op.value.is_none() {
                            return Err(err!("{id}: write over `anchors` needs value and width"));
                        }
                    }
                    None if op.anchor.is_empty() => {
                        return Err(err!("{id}: write needs an anchor"));
                    }
                    None => {}
                }
            }
            "retarget" => {
                return Err(err!(
                    "{id}: op retarget is gone: use op = \"hook\" with a retarget hook"
                ));
            }
            "hook" => {
                if op.hook.is_none() {
                    return Err(err!("{id}: hook needs hook"));
                }
                let Some(to) = &op.to else {
                    return Err(err!("{id}: hook needs to"));
                };
                expr::parse(to).map_err(|e| err!("{id}: hook to: {e}"))?;
                let set = [
                    ("anchor", !op.anchor.is_empty()),
                    ("anchors", op.anchors.is_some()),
                    ("offset", op.offset != 0),
                    ("displaced", op.displaced.is_some()),
                    ("cpu", op.cpu.is_some()),
                    ("value", op.value.is_some()),
                    ("hex", op.hex.is_some()),
                    ("width", op.width.is_some()),
                ];
                if let Some((field, _)) = set.iter().find(|(_, s)| *s) {
                    return Err(err!(
                        "{id}: a hook op names its hook; {field} comes from the declaration"
                    ));
                }
            }
            o => return Err(err!("{id}: unknown op {o}")),
        }
    }
    for s in &m.section {
        check_devices(id, &format!("section {}", s.name), &s.devices)?;
        if !ident_ok(&s.name) {
            return Err(err!(
                "{id}: section name {:?} is not [a-z][a-z0-9_]*",
                s.name
            ));
        }
        if let Some(a) = s.align
            && !a.is_power_of_two()
        {
            return Err(err!("{id}: section {}: align {a}", s.name));
        }
        if let Some(l) = &s.from.len {
            expr::parse(&l.text()).map_err(|e| err!("{id}: section {} len: {e}", s.name))?;
        }
        if let Some(ed) = &s.edit {
            expr::parse(&ed.at.text())
                .map_err(|e| err!("{id}: section {} edit at: {e}", s.name))?;
            for f in &ed.fields {
                let t = f.value.text();
                let (_, e) = word_spec(&t).map_err(|e| err!("{id}: section {}: {e}", s.name))?;
                expr::parse(e).map_err(|e| err!("{id}: section {} edit: {e}", s.name))?;
            }
        }
        if let Some(a) = &s.append {
            if a.row.is_empty() {
                return Err(err!("{id}: section {}: append needs a row", s.name));
            }
            for w in &a.row {
                let t = w.text();
                let (_, e) = word_spec(&t).map_err(|e| err!("{id}: section {}: {e}", s.name))?;
                expr::parse(e).map_err(|e| err!("{id}: section {} row: {e}", s.name))?;
            }
        }
    }
    for r in &m.registry {
        check_devices(id, &format!("registry {}", r.name), &r.devices)?;
        if !ident_ok(&r.name) {
            return Err(err!(
                "{id}: registry name {:?} is not [a-z][a-z0-9_]*",
                r.name
            ));
        }
        if r.max == 0 {
            return Err(err!("{id}: registry {}: max is 0", r.name));
        }
        if let Some(l) = &r.limit
            && !crate::devdata::LIMIT_NAMES.contains(&l.as_str())
        {
            return Err(err!(
                "{id}: registry {}: limit {l:?} is not a device limit ({})",
                r.name,
                crate::devdata::LIMIT_NAMES.join(", ")
            ));
        }
        expr::parse(&r.first.text()).map_err(|e| err!("{id}: registry {} first: {e}", r.name))?;
        if r.gap.is_some() && r.index_table.is_none() {
            return Err(err!(
                "{id}: registry {}: gap rows only exist with an index_table",
                r.name
            ));
        }
        if let Some(g) = &r.gap {
            for k in g.keys() {
                if !r.field.iter().any(|f| f.name == *k) {
                    return Err(err!("{id}: registry {}: gap names no field {k}", r.name));
                }
            }
        }
        let mut seen: Vec<&str> = vec![];
        for f in &r.field {
            if !ident_ok(&f.name) || matches!(f.name.as_str(), "registry" | "id" | "devices") {
                return Err(err!(
                    "{id}: registry {}: field name {:?} is invalid or reserved",
                    r.name,
                    f.name
                ));
            }
            if seen.contains(&f.name.as_str()) {
                return Err(err!("{id}: registry {}: field {} twice", r.name, f.name));
            }
            seen.push(&f.name);
            let k = FKind::of(f).map_err(|e| err!("{id}: registry {}: {e}", r.name))?;
            if f.max_len.is_some() && k != FKind::Cstr {
                return Err(err!(
                    "{id}: registry {} field {}: max_len is for cstr",
                    r.name,
                    f.name
                ));
            }
            if let Some(d) = &f.default {
                let t = toml_text(d, k).map_err(|e| err!("{id}: field {} default: {e}", f.name))?;
                if k != FKind::Cstr {
                    expr::parse(&t).map_err(|e| err!("{id}: field {} default: {e}", f.name))?;
                }
            }
        }
    }
    for e in &m.entry {
        check_devices(id, &format!("entry {}", e.id), &e.devices)?;
        if !ident_ok(&e.id) {
            return Err(err!("{id}: entry id {:?} is not [a-z][a-z0-9_]*", e.id));
        }
    }
    Ok(())
}

// ---- registries ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FKind {
    U8,
    U16,
    U32,
    Cstr,
    Sym,
}

impl FKind {
    pub fn of(f: &FieldDef) -> Result<FKind> {
        Ok(match f.kind.as_str() {
            "u8" => FKind::U8,
            "u16" => FKind::U16,
            "u32" => FKind::U32,
            "cstr" => FKind::Cstr,
            "sym" => FKind::Sym,
            k => {
                return Err(err!(
                    "field {}: kind {k:?} is not u8, u16, u32, cstr or sym",
                    f.name
                ));
            }
        })
    }
    /// Bytes in a row (a cstr and a sym are 32-bit words).
    pub fn size(self) -> usize {
        match self {
            FKind::U8 => 1,
            FKind::U16 => 2,
            _ => 4,
        }
    }
}

/// A TOML field value as text: the string of a cstr, else the expression.
fn toml_text(v: &toml::Value, k: FKind) -> Result<String> {
    match (v, k) {
        (toml::Value::String(s), _) => Ok(s.clone()),
        (toml::Value::Integer(i), k) if k != FKind::Cstr => Ok(i.to_string()),
        _ => Err(err!(
            "expected {}",
            if k == FKind::Cstr {
                "a string"
            } else {
                "an integer or an expression string"
            }
        )),
    }
}

pub struct ActiveEntry<'a> {
    pub owner: &'a Module,
    pub def: std::borrow::Cow<'a, EntryDef>,
    /// a filler row of a registry with an `index_table`: a number nobody selected (no symbol, no claim)
    pub gap: bool,
}

pub struct ActiveReg<'a> {
    pub owner: &'a Module,
    pub def: &'a RegistryDef,
    pub entries: Vec<ActiveEntry<'a>>,
    pub kinds: Vec<FKind>,
    /// the registry's allocation table (`index_table`)
    pub table: Option<&'a AllocTable>,
    /// entries that are not gaps
    pub live: usize,
    /// Byte offset of each field in a row (natural alignment).
    pub offsets: Vec<usize>,
    pub row_size: usize,
}

pub struct ActiveSection<'a> {
    pub owner: &'a Module,
    pub def: &'a SectionDef,
}

#[derive(Default)]
pub struct Active<'a> {
    pub regs: Vec<ActiveReg<'a>>,
    pub sections: Vec<ActiveSection<'a>>,
}

impl ActiveReg<'_> {
    /// The text of a field of an entry: its value, else the field's default.
    fn text(&self, fi: usize, e: &ActiveEntry) -> Result<String> {
        let f = &self.def.field[fi];
        let v = e.def.fields.get(&f.name).or(f.default.as_ref()).ok_or_else(|| {
            err!(
                "{}: entry {} of registry {} has no field {} (and the registry gives no default)",
                e.owner.id(),
                e.def.id,
                self.def.name,
                f.name
            )
        })?;
        toml_text(v, self.kinds[fi])
            .map_err(|m| err!("{}: entry {} field {}: {m}", e.owner.id(), e.def.id, f.name))
    }

    /// Bytes of the table: the rows, then the strings.
    pub fn size(&self) -> Result<usize> {
        let mut n = (self.entries.len() + usize::from(self.def.terminator)) * self.row_size;
        for e in &self.entries {
            for (fi, k) in self.kinds.iter().enumerate() {
                if *k == FKind::Cstr {
                    n += self.text(fi, e)?.len() + 1;
                }
            }
        }
        Ok(n)
    }
}

/// The registries, entries and sections active on `dev`, in canonical module order, checked against each other.
pub fn collect<'a>(lib: &'a Library, order: &[String], dev: &str) -> Result<Active<'a>> {
    let mut act = Active::default();
    for id in order {
        let m = lib.get(id).unwrap();
        if m.manifest.module.dynamic {
            return Err(err!(
                "{}: is a run-time module; pack it with `patchkit pack` and load it with the digikit runtime",
                m.id()
            ));
        }
        if crate::typed::has_typed(&m.manifest) && !m.manifest.lowered {
            return Err(err!(
                "{}: typed sections were not lowered (load the library with the device models)",
                m.id()
            ));
        }
        for d in m.manifest.registry.iter().filter(|r| on(&r.devices, dev)) {
            if act.regs.iter().any(|r| r.def.name == d.name) {
                return Err(err!("registry {} is declared twice", d.name));
            }
            let kinds = d
                .field
                .iter()
                .map(FKind::of)
                .collect::<Result<Vec<_>>>()
                .map_err(|e| err!("{}: registry {}: {e}", m.id(), d.name))?;
            let (mut offsets, mut at, mut widest) = (vec![], 0usize, 1usize);
            for k in &kinds {
                at = at.div_ceil(k.size()) * k.size();
                offsets.push(at);
                at += k.size();
                widest = widest.max(k.size());
            }
            act.regs.push(ActiveReg {
                owner: m,
                def: d,
                entries: vec![],
                kinds,
                table: m.alloc.get(&d.name),
                live: 0,
                offsets,
                row_size: at.div_ceil(widest) * widest,
            });
        }
    }
    for id in order {
        let m = lib.get(id).unwrap();
        for e in m.manifest.entry.iter().filter(|e| on(&e.devices, dev)) {
            let r = act
                .regs
                .iter_mut()
                .find(|r| r.def.name == e.registry)
                .ok_or_else(|| {
                    err!(
                        "{}: entry {}: no registry {} on {dev} (the framework that declares it is not selected)",
                        m.id(),
                        e.id,
                        e.registry
                    )
                })?;
            for k in e.fields.keys() {
                if !r.def.field.iter().any(|f| f.name == *k) {
                    return Err(err!(
                        "{}: entry {}: registry {} has no field {k}",
                        m.id(),
                        e.id,
                        e.registry
                    ));
                }
            }
            r.entries.push(ActiveEntry {
                owner: m,
                def: std::borrow::Cow::Borrowed(e),
                gap: false,
            });
        }
        for s in m.manifest.section.iter().filter(|s| on(&s.devices, dev)) {
            if act.sections.iter().any(|x| x.def.name == s.name) {
                return Err(err!("section {} is declared twice", s.name));
            }
            act.sections.push(ActiveSection { owner: m, def: s });
        }
    }
    for r in &mut act.regs {
        r.live = r.entries.len();
    }
    for r in &act.regs {
        if r.entries.len() > r.def.max as usize {
            return Err(err!(
                "registry {} is full: {} entries, at most {}",
                r.def.name,
                r.entries.len(),
                r.def.max
            ));
        }
        // a device limit below the framework's maximum (`[limits]` of the device file)
        if let Some(l) = &r.def.limit
            && let Some(cap) = crate::devdata::by_short(dev).and_then(|d| d.limit(l))
            && r.entries.len() as u64 > cap
        {
            return Err(err!(
                "registry {} is full on {dev}: {} entries, the device file's [limits] {l} is {cap}",
                r.def.name,
                r.entries.len()
            ));
        }
        // fields without a value anywhere are an error here, so the sizes below cannot fail later
        for e in &r.entries {
            for fi in 0..r.def.field.len() {
                r.text(fi, e)?;
            }
        }
    }
    for s in &act.sections {
        if let Some(ed) = &s.def.edit
            && !act.regs.iter().any(|r| r.def.name == ed.registry)
        {
            return Err(err!(
                "{}: section {} edits rows of registry {}, which is not declared on {dev}",
                s.owner.id(),
                s.def.name,
                ed.registry
            ));
        }
        if let Some(a) = &s.def.append
            && !act.regs.iter().any(|r| r.def.name == a.registry)
        {
            return Err(err!(
                "{}: section {} appends rows of registry {}, which is not declared on {dev}",
                s.owner.id(),
                s.def.name,
                a.registry
            ));
        }
    }
    Ok(act)
}

impl<'a> Active<'a> {
    /// Registries with an `index_table`: give every entry its permanent number and fill the numbers nobody selected
    /// with gap rows, so `entries[k]` has index `first + k`. `firsts` is `first` of each registry (from the image).
    pub fn layout(&mut self, firsts: &[i64], dev: &str) -> Result<()> {
        for (ri, r) in self.regs.iter_mut().enumerate() {
            let Some(table) = r.table else { continue };
            if r.entries.is_empty() {
                continue;
            }
            let first = firsts[ri];
            let who = r.owner.id().to_string();
            let da = table.devices.get(dev).ok_or_else(|| {
                err!(
                    "{who}: registry {}: its allocation table has no [{dev}] section",
                    r.def.name
                )
            })?;
            let mut placed: Vec<(i64, ActiveEntry<'a>)> = vec![];
            for e in std::mem::take(&mut r.entries) {
                let key = format!("{}:{}", e.owner.id(), e.def.id);
                let n = *da.alloc.get(&key).ok_or_else(|| {
                    err!(
                        "{key} has no number in the allocation table of registry {} ({}, [{dev}]): the module is not built for {dev}, or add `\"{key}\" = <free number>`",
                        r.def.name,
                        r.def.index_table.as_deref().unwrap_or("?")
                    )
                })?;
                if n < first {
                    return Err(err!(
                        "{key}: number {n} is below {first}, the first number that is not a stock one (registry {})",
                        r.def.name
                    ));
                }
                if n > da.last {
                    return Err(err!(
                        "{key}: number {n} is above the framework's last supported number {} ([{dev}], registry {})",
                        da.last,
                        r.def.name
                    ));
                }
                placed.push((n, e));
            }
            placed.sort_by_key(|(n, _)| *n);
            for w in placed.windows(2) {
                if w[0].0 == w[1].0 {
                    return Err(err!(
                        "registry {}: {}:{} and {}:{} have the same number {}",
                        r.def.name,
                        w[0].1.owner.id(),
                        w[0].1.def.id,
                        w[1].1.owner.id(),
                        w[1].1.def.id,
                        w[0].0
                    ));
                }
            }
            let span = (placed.last().unwrap().0 - first + 1) as usize;
            if span > r.def.max as usize {
                return Err(err!(
                    "registry {}: the highest number {} spans {span} rows, at most {}",
                    r.def.name,
                    placed.last().unwrap().0,
                    r.def.max
                ));
            }
            let gap_def = EntryDef {
                registry: r.def.name.clone(),
                id: "(gap)".into(),
                devices: vec![],
                fields: r.def.gap.clone().unwrap_or_default(),
            };
            let mut it = placed.into_iter().peekable();
            for slot in 0..span {
                let n = first + slot as i64;
                if it.peek().is_some_and(|(k, _)| *k == n) {
                    r.entries.push(it.next().unwrap().1);
                } else {
                    r.entries.push(ActiveEntry {
                        owner: r.owner,
                        def: std::borrow::Cow::Owned(gap_def.clone()),
                        gap: true,
                    });
                }
            }
            for fi in 0..r.def.field.len() {
                for e in r.entries.iter().filter(|e| e.gap) {
                    r.text(fi, e).map_err(|e| {
                        err!(
                            "{e} (a gap row takes its fields from [registry.gap] and the defaults)"
                        )
                    })?;
                }
            }
        }
        Ok(())
    }
}

/// The scope an expression is evaluated in.
#[derive(Clone, Copy)]
pub struct Scope<'a> {
    /// module exports, section, registry and hook symbols (natural addresses)
    pub syms: &'a BTreeMap<String, u64>,
    /// an anchor's value (its address, the pc of SHARC code)
    pub anchor: &'a dyn Fn(&str) -> Option<i64>,
    /// a value of the stock image inside an anchor
    pub image: &'a dyn Fn(&str, i64, u8) -> Result<i64>,
    /// the anchor `this` names
    pub this: Option<&'a str>,
    /// `$name` of a registry entry
    pub fields: Option<&'a BTreeMap<String, i64>>,
}

impl Env for Scope<'_> {
    fn symbol(&self, name: &str) -> Option<i64> {
        if name == "this"
            && let Some(t) = self.this
        {
            return (self.anchor)(t);
        }
        self.syms
            .get(name)
            .map(|v| *v as i64)
            .or_else(|| (self.anchor)(name))
    }
    fn field(&self, name: &str) -> Option<i64> {
        self.fields.and_then(|f| f.get(name)).copied()
    }
    fn image(&self, anchor: &str, off: i64, width: u8) -> Result<i64> {
        let a = match (anchor, self.this) {
            ("this", Some(t)) => t,
            (a, _) => a,
        };
        (self.image)(a, off, width)
    }
}

/// Integer bytes with a range check (signed or unsigned values of that width are accepted).
pub fn pack(v: i64, width: usize, be: bool, what: &str) -> Result<Vec<u8>> {
    let bits = 8 * width as u32;
    let (lo, hi) = (-(1i64 << (bits - 1)), (1i64 << bits) - 1);
    if !(lo..=hi).contains(&v) {
        return Err(err!("{what}: {v} ({v:#x}) does not fit {width} byte(s)"));
    }
    let b = (v as u64).to_le_bytes();
    let mut out = b[..width].to_vec();
    if be {
        out.reverse();
    }
    Ok(out)
}

#[derive(Clone, Debug, Serialize)]
pub struct PlanEntry {
    pub id: String,
    pub module: String,
    pub index: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct PlanRegistry {
    pub name: String,
    pub module: String,
    pub region: String,
    pub addr: u64,
    pub bytes: u64,
    pub first: u64,
    pub count: u64,
    pub max: u64,
    pub row_size: u64,
    /// (field, kind, byte offset in the row)
    pub fields: Vec<(String, String, u64)>,
    pub entries: Vec<PlanEntry>,
    /// indexes of the gap rows (registries with an `index_table`)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub gaps: Vec<u64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PlanSection {
    pub name: String,
    pub module: String,
    pub region: String,
    pub addr: u64,
    /// bytes in all (copied plus appended rows)
    pub bytes: u64,
    pub from_anchor: String,
    pub from_addr: u64,
    pub copied: u64,
    pub appended_rows: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct PlanHook {
    pub module: String,
    pub anchor: String,
    /// the declared hook the manifest op names
    pub hook: Option<String>,
    pub offset: i64,
    pub cpu: String,
    pub addr: u64,
    /// bytes of the site replaced
    pub site_len: u64,
    pub displaced: String,
    pub to: u64,
    /// `hook.<anchor>.tail` (displaced = run)
    pub tail: Option<u64>,
    /// `hook.<anchor>.return`
    pub ret: u64,
    /// displaced instructions (ColdFire) with their original and re-encoded bytes
    pub displaced_insns: Vec<String>,
}

/// The values of one entry's fields (and `index`), as evaluated.
pub type EntryValues = BTreeMap<String, i64>;

/// Build the byte image of a registry's table (rows, then strings). `base` is the address of the table, `first`
/// the index of entry 0; `scope` has everything but `$fields`.
pub fn build_registry(
    r: &ActiveReg,
    base: u64,
    be: bool,
    first: i64,
    scope: &Scope,
) -> Result<(Vec<u8>, Vec<EntryValues>)> {
    let rows = (r.entries.len() + usize::from(r.def.terminator)) * r.row_size;
    let mut out = vec![0u8; rows];
    let mut pool: Vec<u8> = vec![];
    let mut all = vec![];
    for (k, e) in r.entries.iter().enumerate() {
        let mut vals: BTreeMap<String, i64> = BTreeMap::new();
        vals.insert("index".into(), first + k as i64);
        for (fi, f) in r.def.field.iter().enumerate() {
            let t = r.text(fi, e)?;
            let what = format!("{}: entry {} field {}", e.owner.id(), e.def.id, f.name);
            let kind = r.kinds[fi];
            let v = if kind == FKind::Cstr {
                if let Some(m) = f.max_len
                    && t.len() > m
                {
                    return Err(err!("{what}: {:?} is {} bytes, max_len is {m}", t, t.len()));
                }
                let at = base + (rows + pool.len()) as u64;
                pool.extend_from_slice(t.as_bytes());
                pool.push(0);
                at as i64
            } else {
                expr::eval_str(&t, scope).map_err(|e| err!("{what}: {e}"))?
            };
            let w = kind.size();
            let bytes = pack(v, w, be, &what)?;
            let at = k * r.row_size + r.offsets[fi];
            out[at..at + w].copy_from_slice(&bytes);
            vals.insert(f.name.clone(), v);
        }
        all.push(vals);
    }
    out.extend_from_slice(&pool);
    Ok((out, all))
}

/// Rows appended to a copied section: one per entry, the words of `append.row` evaluated per entry.
pub fn build_rows(
    sec: &ActiveSection,
    entries: &[EntryValues],
    be: bool,
    scope: &Scope,
) -> Result<Vec<u8>> {
    let Some(a) = &sec.def.append else {
        return Ok(vec![]);
    };
    let mut out = vec![];
    for (k, vals) in entries.iter().enumerate() {
        let sc = Scope {
            fields: Some(vals),
            ..*scope
        };
        for (wi, w) in a.row.iter().enumerate() {
            let t = w.text();
            let (width, e) = word_spec(&t)?;
            let what = format!("section {} row {k} word {wi}", sec.def.name);
            let v = expr::eval_str(e, &sc).map_err(|e| err!("{what}: {e}"))?;
            out.extend(pack(v, width, be, &what)?);
        }
    }
    Ok(out)
}

/// The `edit` of a copied section: each entry of its registry rewrites words of one copied row in place.
pub fn apply_edits(
    sec: &ActiveSection,
    entries: &[EntryValues],
    be: bool,
    scope: &Scope,
    bytes: &mut [u8],
) -> Result<()> {
    let Some(ed) = &sec.def.edit else {
        return Ok(());
    };
    for (k, vals) in entries.iter().enumerate() {
        let sc = Scope {
            fields: Some(vals),
            ..*scope
        };
        let what = format!("section {} edit {k}", sec.def.name);
        let at = expr::eval_str(&ed.at.text(), &sc).map_err(|e| err!("{what}: {e}"))?;
        for f in &ed.fields {
            let t = f.value.text();
            let (width, e) = word_spec(&t)?;
            let v = expr::eval_str(e, &sc).map_err(|e| err!("{what}: {e}"))?;
            let b = pack(v, width, be, &what)?;
            let o = (at as u64 + f.offset) as usize;
            let dst = bytes
                .get_mut(o..o + b.len())
                .ok_or_else(|| err!("{what}: offset {o:#x} is outside the copied bytes"))?;
            dst.copy_from_slice(&b);
        }
    }
    Ok(())
}

// ---- hooks --------------------------------------------------------------------------------------------------

/// SHARC+ unconditional non-delayed `JUMP` to a short-word pc (the instruction the `jump` op writes).
pub fn sharc_jump(target: u64) -> Result<Vec<u8>> {
    if target >= 1 << 24 {
        return Err(err!("{target:#x} does not fit a 24-bit jump target"));
    }
    let mut out = vec![];
    out.extend_from_slice(&0x063eu16.to_le_bytes());
    out.extend_from_slice(&(((target >> 16) & 0xff) as u16).to_le_bytes());
    out.extend_from_slice(&((target & 0xffff) as u16).to_le_bytes());
    Ok(out)
}

pub const CF_JMP_L: u16 = 0x4ef9;
pub const CF_NOP: u16 = 0x4e71;
/// Bytes of `jmp.l abs32`.
pub const CF_JMP_LEN: usize = 6;

#[derive(Clone, Debug)]
pub struct DispInsn {
    pub addr: u64,
    pub raw: Vec<u8>,
    pub insn: coldfire::Insn,
}

#[derive(Clone, Debug)]
pub struct CfSite {
    pub addr: u64,
    /// bytes of whole instructions that cover the jump (at least 6)
    pub len: usize,
    pub insns: Vec<DispInsn>,
}

/// The whole instructions at `site` that the 6-byte jump covers. The instruction chain is decoded from `start`
/// (the anchor's first byte) so a `site` inside an instruction is found and refused.
pub fn cf_site(main: &[u8], base: u64, start: u64, site: u64) -> Result<CfSite> {
    let at = |a: u64| -> Result<coldfire::Insn> {
        let off = a
            .checked_sub(base)
            .filter(|o| (*o as usize) < main.len())
            .ok_or_else(|| err!("{a:#x} is outside the MAIN OS image"))?;
        coldfire::decode_at(main, base as u32, off as usize)
            .filter(|i| i.len >= 2)
            .ok_or_else(|| err!("no ColdFire instruction decodes at {a:#x}"))
    };
    let mut a = start;
    while a < site {
        a += at(a)?.len as u64;
    }
    if a != site {
        return Err(err!(
            "{site:#x} is not on an instruction boundary (the instruction before it ends at {a:#x}, chain from {start:#x})"
        ));
    }
    let (mut insns, mut len) = (vec![], 0usize);
    while len < CF_JMP_LEN {
        let i = at(site + len as u64)?;
        let off = (site + len as u64 - base) as usize;
        let n = i.len as usize;
        if off + n > main.len() {
            return Err(err!(
                "instruction at {:#x} runs past the image",
                site + len as u64
            ));
        }
        insns.push(DispInsn {
            addr: site + len as u64,
            raw: main[off..off + n].to_vec(),
            insn: i,
        });
        len += n;
    }
    Ok(CfSite {
        addr: site,
        len,
        insns,
    })
}

/// The bytes at the site: `jmp.l to`, NOP padded to the whole instructions.
pub fn cf_site_bytes(s: &CfSite, to: u64) -> Result<Vec<u8>> {
    if to > u32::MAX as u64 {
        return Err(err!("hook target {to:#x} is not a 32-bit address"));
    }
    let mut b = CF_JMP_L.to_be_bytes().to_vec();
    b.extend_from_slice(&(to as u32).to_be_bytes());
    while b.len() < s.len {
        b.extend_from_slice(&CF_NOP.to_be_bytes());
    }
    Ok(b)
}

/// Re-encode one displaced instruction to run at `new_addr`. Position independent instructions are copied.
/// `(d16,PC)` becomes `abs.l` (the same target), branches become the 32-bit form; `(d8,PC,Xn)` is refused.
fn relocate_insn(d: &DispInsn, new_addr: u64) -> Result<Vec<u8>> {
    use coldfire::{Ea, Form, Operand};
    let ops = d.insn.operands();
    let mut out = d.raw.clone();
    for op in ops.iter() {
        match op {
            Operand::Ea(Ea::PcIdx { .. }) => {
                return Err(err!(
                    "displaced instruction at {:#x} ({}) uses (d8,PC,Xn): refused",
                    d.addr,
                    d.insn
                ));
            }
            Operand::Ea(Ea::PcDisp { base, d16 }) => {
                let w0 = u16::from_be_bytes([d.raw[0], d.raw[1]]);
                let pos = (*base as u64).wrapping_sub(d.addr) as usize;
                if w0 & 0x3f != 0x3a || pos < 2 || pos + 2 > d.raw.len() {
                    return Err(err!(
                        "displaced instruction at {:#x} ({}) has a PC-relative operand in a form patchkit cannot re-encode",
                        d.addr,
                        d.insn
                    ));
                }
                let target = (*base as i64 + *d16 as i64) as u32;
                out[0..2].copy_from_slice(&((w0 & !0x3f) | 0x39).to_be_bytes());
                out.splice(pos..pos + 2, target.to_be_bytes());
            }
            Operand::Target(t) => {
                if !matches!(d.insn.form, Form::Bra | Form::Bsr | Form::Bcc) {
                    return Err(err!(
                        "displaced instruction at {:#x} ({}) is a PC-relative branch patchkit cannot re-encode",
                        d.addr,
                        d.insn
                    ));
                }
                let disp = *t as i64 - (new_addr as i64 + 2);
                if disp < i32::MIN as i64 || disp > i32::MAX as i64 {
                    return Err(err!(
                        "displaced branch at {:#x}: target {t:#x} is out of 32-bit range from {new_addr:#x}",
                        d.addr
                    ));
                }
                out = vec![d.raw[0], 0xff];
                out.extend_from_slice(&(disp as i32).to_be_bytes());
            }
            _ => {}
        }
    }
    Ok(out)
}

/// `hook.<anchor>.tail`: the displaced instructions as they run at `base`, then `jmp.l ret`.
pub fn cf_tail(s: &CfSite, base: u64, ret: u64) -> Result<Vec<u8>> {
    let mut out: Vec<u8> = vec![];
    for d in &s.insns {
        let r = relocate_insn(d, base + out.len() as u64)?;
        out.extend(r);
    }
    out.extend_from_slice(&CF_JMP_L.to_be_bytes());
    out.extend_from_slice(&(ret as u32).to_be_bytes());
    Ok(out)
}

// ---- tests of the byte-level builders -----------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn site(code: &[u8], at: u64) -> Result<CfSite> {
        cf_site(code, 0x4000_0000, 0x4000_0000, at)
    }

    #[test]
    fn a_site_is_padded_to_whole_instructions() {
        // moveq #1,d0 (2) ; move.l #0x12345678,d1 (6) ; rts
        let code = [
            0x70, 0x01, 0x22, 0x3c, 0x12, 0x34, 0x56, 0x78, 0x4e, 0x75, 0x4e, 0x71,
        ];
        let s = site(&code, 0x4000_0000).unwrap();
        // moveq (2) + move.l #imm (6) = 8 >= 6
        assert_eq!((s.len, s.insns.len()), (8, 2));
        let b = cf_site_bytes(&s, 0x47e8_b100).unwrap();
        assert_eq!(b, [0x4e, 0xf9, 0x47, 0xe8, 0xb1, 0x00, 0x4e, 0x71]);
        // the tail runs the two instructions unchanged, then jumps back
        let t = cf_tail(&s, 0x47e8_c000, 0x4000_0008).unwrap();
        assert_eq!(&t[..8], &code[..8]);
        assert_eq!(&t[8..], [0x4e, 0xf9, 0x40, 0x00, 0x00, 0x08]);
        // exactly six bytes need no padding
        let code2 = [0x22, 0x3c, 0x00, 0x00, 0x00, 0x01, 0x4e, 0x75];
        let s2 = site(&code2, 0x4000_0000).unwrap();
        assert_eq!(s2.len, 6);
        assert_eq!(cf_site_bytes(&s2, 0x4700_0000).unwrap().len(), 6);
    }

    #[test]
    fn a_site_inside_an_instruction_is_refused() {
        let code = [
            0x22, 0x3c, 0x00, 0x00, 0x00, 0x01, 0x4e, 0x75, 0x4e, 0x71, 0x4e, 0x71, 0x4e, 0x71,
        ];
        let e = site(&code, 0x4000_0002).unwrap_err();
        assert!(e.0.contains("not on an instruction boundary"), "{e}");
        assert!(site(&code, 0x4000_0006).is_ok());
        // an offset that runs off the image
        assert!(site(&code, 0x4000_0100).is_err());
    }

    #[test]
    fn pc_relative_operands_are_re_encoded_or_refused() {
        // lea (0x10,pc),a0 at 0x40000000: target = 0x40000002 + 0x10; then nop nop
        let code = [0x41, 0xfa, 0x00, 0x10, 0x4e, 0x71, 0x4e, 0x71];
        let s = site(&code, 0x4000_0000).unwrap();
        assert_eq!(s.insns.len(), 2);
        let t = cf_tail(&s, 0x47e8_c000, 0x4000_0006).unwrap();
        // lea abs.l,a0 (41f9 + target), nop, jmp.l
        assert_eq!(&t[..6], [0x41, 0xf9, 0x40, 0x00, 0x00, 0x12]);
        assert_eq!(&t[6..8], [0x4e, 0x71]);
        assert_eq!(&t[8..], [0x4e, 0xf9, 0x40, 0x00, 0x00, 0x06]);
        // a conditional branch (bne.w, 0x20 forward) becomes bne.l
        let code = [0x66, 0x00, 0x00, 0x20, 0x4e, 0x71, 0x4e, 0x71, 0x4e, 0x71];
        let s = site(&code, 0x4000_0000).unwrap();
        assert_eq!(s.insns.len(), 2);
        let t = cf_tail(&s, 0x4000_1000, 0x4000_0006).unwrap();
        // target = 0x40000002 + 0x20 = 0x40000022; new pc 0x40001000: disp = 0x40000022 - 0x40001002
        let disp = (0x4000_0022i64 - 0x4000_1002) as i32;
        assert_eq!(&t[..2], [0x66, 0xff]);
        assert_eq!(&t[2..6], disp.to_be_bytes());
        // (d8,PC,Xn) is refused: move.l (2,pc,d1.l*1),d0 = 203b 1802 ; nop nop
        let code = [0x20, 0x3b, 0x18, 0x02, 0x4e, 0x71, 0x4e, 0x71];
        let s = site(&code, 0x4000_0000).unwrap();
        let e = cf_tail(&s, 0x47e8_c000, 0x4000_0008).unwrap_err();
        assert!(e.0.contains("refused"), "{e}");
    }

    #[test]
    fn packing_checks_the_range() {
        assert_eq!(pack(0x1234, 2, true, "x").unwrap(), [0x12, 0x34]);
        assert_eq!(pack(0x1234, 2, false, "x").unwrap(), [0x34, 0x12]);
        assert_eq!(pack(-1, 1, true, "x").unwrap(), [0xff]);
        assert_eq!(pack(255, 1, true, "x").unwrap(), [0xff]);
        assert!(
            pack(256, 1, true, "x")
                .unwrap_err()
                .0
                .contains("does not fit")
        );
        assert!(pack(-129, 1, true, "x").is_err());
        assert!(pack(1 << 32, 4, true, "x").is_err());
        assert_eq!(pack(0xffff_ffff, 4, true, "x").unwrap(), [0xff; 4]);
    }

    #[test]
    fn sharc_jump_encodes_a_24_bit_pc() {
        assert_eq!(
            sharc_jump(0xb9_1234).unwrap(),
            [0x3e, 0x06, 0xb9, 0x00, 0x34, 0x12]
        );
        assert!(sharc_jump(1 << 24).is_err());
    }
}
