//! Check and apply: resolve the module set, verify the anchors against the firmware, allocate regions,
//! link, apply the operations, repack the changed sections, seal and re-verify.

use crate::dko::Dko;
use crate::dsp::Stream;
use crate::error::*;
use crate::firmware::Firmware;
use crate::link;
use crate::module::{Library, Module, Op};
use crate::port::{Anchor, Port, Registry};
use crate::prim::{self, on};
use crate::record::Record;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

pub const SHARC: &str = "sharc";
pub const CF: &str = "cf";

/// One byte-range change, with provenance.
#[derive(Clone, Debug, Serialize)]
pub struct PlanWrite {
    pub cpu: String,
    pub addr: u64,
    pub len: u64,
    /// `patch` (bytes the firmware had), `place` (new memory) or `record`
    pub kind: String,
    pub owner: String,
    pub what: String,
    pub old_sha256: Option<String>,
    pub new_sha256: String,
    pub new_hex: Option<String>,
    /// Executable byte ranges of a placed object (for `explain`)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub code: Vec<(u64, u64)>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Plan {
    pub base: PlanBase,
    pub modules: Vec<String>,
    pub writes: Vec<PlanWrite>,
    /// region name -> bytes used / capacity
    pub regions: BTreeMap<String, (u64, u64)>,
    pub symbols: BTreeMap<String, u64>,
    /// P2: registries and their entries with the assigned indices
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub registries: Vec<prim::PlanRegistry>,
    /// P1: sections copied from the image
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sections: Vec<prim::PlanSection>,
    /// P4: hooks
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hooks: Vec<prim::PlanHook>,
    pub record: Record,
    pub output: Option<PlanOutput>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PlanBase {
    pub device: String,
    pub version: String,
    pub syx_sha256: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct PlanOutput {
    pub syx_sha256: String,
    pub syx_bytes: u64,
    pub container_bytes: u64,
    pub changed_sections: Vec<(u32, u64, u64)>,
}

pub struct Applied {
    pub syx: Vec<u8>,
    pub plan: Plan,
    /// The image id stamped into the runtime (`imageid`), `None` when the patch has no runtime.
    pub image_id: Option<[u8; 8]>,
}

fn anchor_value(a: &Anchor) -> u64 {
    if a.cpu == SHARC && a.kind == "code" {
        a.pm.unwrap_or(a.addr)
    } else {
        a.addr
    }
}

/// Memory of both CPUs as the firmware loads it.
struct Mem {
    stream: Stream,
    main: Vec<u8>,
    main_base: u64,
    stream_dirty: bool,
    main_dirty: bool,
}

impl Mem {
    fn read(&self, cpu: &str, addr: u64, len: u64) -> Result<Vec<u8>> {
        if cpu == SHARC {
            self.stream
                .read(addr as u32, len as usize)
                .ok_or_else(|| err!("DSP {addr:#x}+{len}: not loaded by the boot stream"))
        } else {
            let o = addr.checked_sub(self.main_base).map(|o| o as usize);
            match o {
                Some(o) if o + len as usize <= self.main.len() => {
                    Ok(self.main[o..o + len as usize].to_vec())
                }
                _ => Err(err!("MAIN {addr:#x}+{len}: outside the MAIN OS image")),
            }
        }
    }
    fn write(&mut self, cpu: &str, addr: u64, data: &[u8]) -> Result<()> {
        if cpu == SHARC {
            self.stream_dirty = true;
            self.stream.write(addr as u32, data)
        } else {
            self.read(cpu, addr, data.len() as u64)?;
            let o = (addr - self.main_base) as usize;
            self.main[o..o + data.len()].copy_from_slice(data);
            self.main_dirty = true;
            Ok(())
        }
    }
}

struct Alloc {
    free: Vec<(u64, u64)>,
    used: u64,
    cap: u64,
}

impl Alloc {
    fn new(ranges: Vec<(u64, u64)>) -> Alloc {
        let cap = ranges.iter().map(|(s, e)| e - s).sum();
        Alloc {
            free: ranges,
            used: 0,
            cap,
        }
    }
    fn take(&mut self, size: u64, align: u64) -> Option<u64> {
        for i in 0..self.free.len() {
            let (s, e) = self.free[i];
            let a = s.div_ceil(align) * align;
            if a + size <= e {
                self.free[i].0 = a + size;
                self.used += a + size - s;
                return Some(a);
            }
        }
        None
    }
}

pub struct Selection {
    pub order: Vec<String>,
}

/// Dependency closure check and canonical order: requires first, then id.
pub fn order_modules(lib: &Library, ids: &[String]) -> Result<Vec<String>> {
    let mut set: BTreeSet<String> = BTreeSet::new();
    for id in ids {
        if lib.get(id).is_none() {
            return Err(err!("unknown module {id}"));
        }
        set.insert(id.clone());
    }
    for id in &set {
        let m = lib.get(id).unwrap();
        for r in &m.manifest.module.requires {
            if !set.contains(r) {
                return Err(err!("{id} requires {r}, which is not selected"));
            }
        }
        for c in &m.manifest.module.conflicts {
            if set.contains(c) {
                return Err(err!("{id} conflicts with {c}"));
            }
        }
    }
    let mut out: Vec<String> = vec![];
    let mut left: BTreeSet<String> = set;
    while !left.is_empty() {
        let next = left
            .iter()
            .find(|id| {
                lib.get(id)
                    .unwrap()
                    .manifest
                    .module
                    .requires
                    .iter()
                    .all(|r| out.contains(r))
            })
            .cloned()
            .ok_or_else(|| err!("requires cycle among {:?}", left))?;
        left.remove(&next);
        out.push(next);
    }
    Ok(out)
}

/// Is the module usable on this port? Lists every missing anchor and region.
pub fn compatible(m: &Module, port: &Port) -> Result<()> {
    let dev = port.port.device.as_str();
    let ports = &m.manifest.module.ports;
    if !ports.is_empty() && !ports.iter().any(|p| *p == port.id()) {
        return Err(err!(
            "{} is not compatible with {}: it is built for {}",
            m.id(),
            port.id(),
            ports.join(", ")
        ));
    }
    let mut missing: Vec<String> = vec![];
    for (o, dko) in m.manifest.object.iter().zip(&m.objects) {
        if !on(&o.devices, dev) {
            continue;
        }
        if port.region(&o.place).is_none() {
            missing.push(format!("region {}", o.place));
        }
        for sy in dko.symbols.iter().filter(|s| s.section.is_none()) {
            let name = o.externs.get(&sy.name).unwrap_or(&sy.name);
            // an export of another object cannot be seen here; only names that look like anchors are checked
            if (name.starts_with("dsp.") || name.starts_with("cf.")) && port.anchor(name).is_none()
            {
                missing.push(format!("anchor {name}"));
            }
        }
    }
    for op in &m.manifest.op {
        if !on(&op.devices, dev) {
            continue;
        }
        let op = &match expand_op(m, port, op) {
            Ok(x) => x,
            Err(_) => {
                missing.push(format!("hook {}", op.hook.as_deref().unwrap_or("?")));
                continue;
            }
        };
        match &op.anchors {
            Some(g) => {
                if glob_anchors(port, g).is_empty() {
                    missing.push(format!("anchors {g}"));
                }
            }
            None if port.anchor(&op.anchor).is_none() => {
                missing.push(format!("anchor {}", op.anchor));
            }
            None => {}
        }
        if op.op == "hook" {
            let place = hook_place(port, op);
            let runs = op.displaced.as_deref() == Some("run");
            if port.region(&place).is_none() && runs {
                missing.push(format!("region {place}"));
            }
        }
    }
    for s in m.manifest.section.iter().filter(|s| on(&s.devices, dev)) {
        if port.anchor(&s.from.anchor).is_none() {
            missing.push(format!("anchor {}", s.from.anchor));
        }
        if port.region(&s.region).is_none() {
            missing.push(format!("region {}", s.region));
        }
    }
    for r in m.manifest.registry.iter().filter(|r| on(&r.devices, dev)) {
        if port.region(&r.region).is_none() {
            missing.push(format!("region {}", r.region));
        }
    }
    missing.sort();
    missing.dedup();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(err!(
            "{} is not compatible with {}: no {}",
            m.id(),
            port.id(),
            missing.join(", ")
        ))
    }
}

/// The anchors `PREFIX.<number>` of a `PREFIX.*` pattern, in numeric order.
fn glob_anchors<'a>(port: &'a Port, pattern: &str) -> Vec<&'a Anchor> {
    let prefix = pattern.strip_suffix('*').unwrap_or(pattern);
    let mut v: Vec<(u64, &Anchor)> = port
        .anchor
        .iter()
        .filter_map(|a| {
            let n = a.name.strip_prefix(prefix)?;
            (!n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                .then(|| n.parse::<u64>().ok().map(|n| (n, a)))
                .flatten()
        })
        .collect();
    v.sort_by_key(|(n, _)| *n);
    v.into_iter().map(|(_, a)| a).collect()
}

/// A manifest `hook` op in the form the later steps read: the site anchor and offset of the declared hook,
/// and `hook` (displaced code runs), `hook` with `displaced = drop` or `retarget` by its mode.
fn expand_op(m: &Module, port: &Port, op: &Op) -> Result<Op> {
    let Some(name) = op.hook.as_deref().filter(|_| op.op == "hook") else {
        return Ok(op.clone());
    };
    let h = port
        .hook(name)
        .ok_or_else(|| err!("{}: no hook {name} on {}", m.id(), port.port.device))?;
    let mut x = op.clone();
    x.anchor = h.site.clone();
    x.offset = h.offset;
    match h.mode.as_str() {
        "before" => x.displaced = Some("run".into()),
        "replace" => x.displaced = Some("drop".into()),
        "retarget" => x.op = "retarget".into(),
        o => return Err(err!("{}: hook {name}: unknown mode {o}", m.id())),
    }
    Ok(x)
}

/// The ops of a module that are active on the port's device, hook ops expanded.
fn device_ops(m: &Module, port: &Port) -> Result<Vec<Op>> {
    m.manifest
        .op
        .iter()
        .filter(|o| on(&o.devices, &port.port.device))
        .map(|o| expand_op(m, port, o))
        .collect()
}

/// Region of a hook's tail: `place`, else the CPU's code region.
fn hook_place(port: &Port, op: &Op) -> String {
    op.place
        .clone()
        .unwrap_or_else(|| match port.anchor(&op.anchor).map(|a| a.cpu.as_str()) {
            Some(SHARC) => "dsp.l2".into(),
            _ => "cf.expansion".into(),
        })
}

fn verify_anchor(mem: &Mem, a: &Anchor) -> Result<()> {
    let at = if a.kind == "value" {
        a.src
            .ok_or_else(|| err!("anchor {}: value without src", a.name))?
    } else {
        a.addr
    };
    let bytes = mem.read(&a.cpu, at, a.len)?;
    let got = sha256_hex(&bytes);
    if got != a.sha256 {
        return Err(err!(
            "anchor {} at {:#x}: bytes hash {} but the port expects {}",
            a.name,
            at,
            got,
            a.sha256
        ));
    }
    Ok(())
}

/// Switches for tests and oracles.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Write the `DKPR` record (always on for real patches; oracles compare against images made without).
    pub record: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options { record: true }
    }
}

pub fn apply(fw_bytes: &[u8], reg: &Registry, lib: &Library, ids: &[String]) -> Result<Applied> {
    apply_with(fw_bytes, reg, lib, ids, Options::default())
}

/// A linked object placed in memory.
struct Chunk {
    owner: String,
    cpu: &'static str,
    region: String,
    addr: u64,
    image: Vec<u8>,
    code: Vec<(u64, u64)>,
    /// ColdFire bss that follows the image (zeroed by the loader)
    bss: u64,
    /// plan text: `object in <region>`, `section <name>`, ...
    what: String,
}

/// An in-place change of firmware bytes.
struct Patch {
    owner: String,
    cpu: &'static str,
    addr: u64,
    bytes: Vec<u8>,
    old_sha256: String,
    what: String,
}

/// The built-in symbols a module may name as an extern.
const SEGMENTS: &str = "pk.segments";
const EXPANSION: &str = "core/expansion";

fn uses_expansion(lib: &Library, ids: &[String], dev: &str, port: &Port) -> bool {
    let exp = "cf.expansion";
    ids.iter().filter_map(|i| lib.get(i)).any(|m| {
        let mf = &m.manifest;
        mf.object
            .iter()
            .any(|o| o.place == exp && on(&o.devices, dev))
            || mf
                .section
                .iter()
                .any(|s| s.region == exp && on(&s.devices, dev))
            || mf
                .registry
                .iter()
                .any(|r| r.region == exp && on(&r.devices, dev))
            || device_ops(m, port).is_ok_and(|ops| {
                ops.iter().any(|o| {
                    o.op == "hook"
                        && o.displaced.as_deref() == Some("run")
                        && hook_place(port, o) == exp
                })
            })
    })
}

/// A value of the stock image inside a verified anchor (`image.u8|u16|u32(anchor + off)`).
fn image_read(mem: &Mem, port: &Port, name: &str, off: i64, w: u8) -> Result<i64> {
    let a = port
        .anchor(name)
        .ok_or_else(|| err!("image.u{}(): no anchor {name}", w as u32 * 8))?;
    if a.kind == "value" || a.kind == "sites" {
        return Err(err!(
            "image.u{}({name}): a {} anchor holds no bytes to read",
            w as u32 * 8,
            a.kind
        ));
    }
    let at = a.addr as i64 + off;
    if at < a.addr as i64 || at + w as i64 > (a.addr + a.len) as i64 {
        return Err(err!(
            "image.u{}({name} {off:+}) lies outside the anchor ({} bytes), so its value is not covered by the port's hash",
            w as u32 * 8,
            a.len
        ));
    }
    let b = mem.read(&a.cpu, at as u64, w as u64)?;
    let mut v = 0u64;
    if a.cpu == SHARC {
        for x in b.iter().rev() {
            v = v << 8 | *x as u64;
        }
    } else {
        for x in &b {
            v = v << 8 | *x as u64;
        }
    }
    Ok(v as i64)
}

/// First-fit allocation in a named region; returns the address and the CPU of the region.
fn take_region(
    allocs: &mut BTreeMap<String, Alloc>,
    port: &Port,
    region: &str,
    size: usize,
    align: u64,
    who: &str,
) -> Result<(u64, &'static str)> {
    let r = port
        .region(region)
        .ok_or_else(|| err!("port {} has no {region} region", port.id()))?;
    let al = allocs.get_mut(region).unwrap();
    let base = al.take((size as u64).max(4), align.max(4)).ok_or_else(|| {
        err!(
            "{who}: {size} bytes do not fit in {region} ({} of {} bytes used)",
            al.used,
            al.cap
        )
    })?;
    Ok((base, if r.cpu == SHARC { SHARC } else { CF }))
}

/// A copied section, sized.
struct SecStage {
    copied: Vec<u8>,
    from_addr: u64,
    total: usize,
}

/// A `hook` op, decoded.
struct HookStage<'a> {
    module: &'a Module,
    op_idx: usize,
    op: Op,
    anchor: &'a Anchor,
    cpu: &'static str,
    site: u64,
    site_len: usize,
    old: Vec<u8>,
    cf: Option<prim::CfSite>,
    run: bool,
    tail_len: usize,
    place: String,
    /// `hook.<anchor>` (with `.<offset>` when not 0)
    base_name: String,
}

#[derive(Default)]
struct Stage<'a> {
    reg_first: Vec<i64>,
    reg_size: Vec<usize>,
    secs: Vec<SecStage>,
    hooks: Vec<HookStage<'a>>,
}

/// Everything about the primitives that needs the stock image but no addresses yet: registry `first`, table and
/// section sizes (expressions with anchors and image reads only), and the decoded hook sites.
fn stage<'a>(
    mem: &Mem,
    port: &'a Port,
    lib: &'a Library,
    order: &[String],
    _dev: &str,
    active: &prim::Active,
) -> Result<Stage<'a>> {
    let anchor_fn = |n: &str| port.anchor(n).map(|a| anchor_value(a) as i64);
    let image_fn = |n: &str, off: i64, w: u8| image_read(mem, port, n, off, w);
    let empty: BTreeMap<String, u64> = BTreeMap::new();
    let sc = prim::Scope {
        syms: &empty,
        anchor: &anchor_fn,
        image: &image_fn,
        this: None,
        fields: None,
    };
    let mut st = Stage::default();
    for r in &active.regs {
        let first = crate::expr::eval_str(&r.def.first.text(), &sc)
            .map_err(|e| err!("{}: registry {} first: {e}", r.owner.id(), r.def.name))?;
        if first < 0 || first + r.def.max as i64 > u32::MAX as i64 {
            return Err(err!(
                "{}: registry {}: first {first} with max {} leaves 32 bits",
                r.owner.id(),
                r.def.name,
                r.def.max
            ));
        }
        st.reg_first.push(first);
        st.reg_size.push(r.size()?);
    }
    for s in &active.sections {
        let who = format!("{}: section {}", s.owner.id(), s.def.name);
        let a = port.anchor(&s.def.from.anchor).unwrap();
        if a.kind == "value" || a.kind == "sites" {
            return Err(err!("{who}: anchor {} holds no bytes", a.name));
        }
        let off = s.def.from.offset;
        let len = match &s.def.from.len {
            Some(l) => crate::expr::eval_str(&l.text(), &sc).map_err(|e| err!("{who} len: {e}"))?,
            None => a.len as i64 - off,
        };
        if off < 0 || len < 0 {
            return Err(err!("{who}: from offset {off} len {len}"));
        }
        let copied = mem.read(&a.cpu, a.addr + off as u64, len as u64)?;
        if off + len > a.len as i64 {
            match &s.def.from.expect_sha256 {
                Some(e) if *e == sha256_hex(&copied) => {}
                Some(_) => {
                    return Err(err!(
                        "{who}: the bytes copied beyond the anchor do not hash to expect_sha256"
                    ));
                }
                None => {
                    return Err(err!(
                        "{who}: copies {} bytes from {} +{off}, past the anchor's {} bytes, and has no expect_sha256",
                        len,
                        a.name,
                        a.len
                    ));
                }
            }
        }
        let rows = match &s.def.append {
            Some(ap) => {
                let n = active
                    .regs
                    .iter()
                    .find(|r| r.def.name == ap.registry)
                    .map_or(0, |r| r.entries.len());
                let mut w = 0;
                for x in &ap.row {
                    w += prim::word_spec(&x.text())?.0;
                }
                n * w
            }
            None => 0,
        };
        st.secs.push(SecStage {
            from_addr: a.addr + off as u64,
            total: copied.len() + rows,
            copied,
        });
    }
    for id in order {
        let m = lib.get(id).unwrap();
        for (oi, op) in device_ops(m, port)?.into_iter().enumerate() {
            if op.op != "hook" {
                continue;
            }
            let who = format!(
                "{}: hook {}",
                m.id(),
                op.hook.as_deref().unwrap_or(&op.anchor)
            );
            let a = port.anchor(&op.anchor).unwrap();
            let cpu: &'static str = if a.cpu == SHARC { SHARC } else { CF };
            if op.cpu.as_deref().is_some_and(|c| c != a.cpu) {
                return Err(err!("{who}: cpu {:?} but the anchor is {}", op.cpu, a.cpu));
            }
            if a.kind != "code" {
                return Err(err!("{who}: needs a code anchor, {} is {}", a.name, a.kind));
            }
            if op.offset < 0 {
                return Err(err!("{who}: negative offset"));
            }
            let run = op.displaced.as_deref() == Some("run");
            let site = a.addr + op.offset as u64;
            let (site_len, cf) = if cpu == SHARC {
                if op.offset % 2 != 0 {
                    return Err(err!("{who}: a SHARC site is parcel aligned"));
                }
                (6usize, None)
            } else {
                let s = prim::cf_site(&mem.main, mem.main_base, a.addr, site)
                    .map_err(|e| err!("{who}: {e}"))?;
                (s.len, Some(s))
            };
            let old = mem.read(cpu, site, site_len as u64)?;
            if op.offset as u64 + site_len as u64 > a.len {
                match &op.expect_sha256 {
                    Some(e) if *e == sha256_hex(&old) => {}
                    Some(_) => {
                        return Err(err!("{who}: the site bytes do not hash to expect_sha256"));
                    }
                    None => {
                        return Err(err!(
                            "{who}: the {site_len}-byte site reaches past the anchor's {} bytes and has no expect_sha256",
                            a.len
                        ));
                    }
                }
            }
            let tail_len = match (&cf, run) {
                (_, false) => 0,
                (Some(s), true) => prim::cf_tail(s, 0, 0)
                    .map_err(|e| err!("{who}: {e}"))?
                    .len(),
                (None, true) => 12,
            };
            let place = hook_place(port, &op);
            let base_name = if op.offset == 0 {
                format!("hook.{}", a.name)
            } else {
                format!("hook.{}.{}", a.name, op.offset)
            };
            st.hooks.push(HookStage {
                module: m,
                op_idx: oi,
                op,
                anchor: a,
                cpu,
                site,
                site_len,
                old,
                cf,
                run,
                tail_len,
                place,
                base_name,
            });
        }
    }
    Ok(st)
}

pub fn apply_with(
    fw_bytes: &[u8],
    reg: &Registry,
    lib: &Library,
    ids: &[String],
    opts: Options,
) -> Result<Applied> {
    let mut fw = Firmware::parse(fw_bytes)?;
    let port = reg.find(&fw.syx_sha256).ok_or_else(|| {
        err!("no port for firmware {}: it is not a stock image patchkit knows (a patched image is refused too)", fw.syx_sha256)
    })?;
    let dev = port.port.device.clone();
    let main_sec = fw
        .section(3)
        .ok_or_else(|| err!("image has no MAIN OS section"))?;
    let dsp_sec = fw
        .section(7)
        .ok_or_else(|| err!("image has no DSP section"))?;
    if sha256_hex(&main_sec.data) != port.port.main_sha256
        || sha256_hex(&dsp_sec.data) != port.port.dsp_sha256
    {
        return Err(err!(
            "port {} does not match the sections of this image",
            port.id()
        ));
    }
    let mut mem = Mem {
        stream: Stream::parse(dsp_sec.data.clone())?,
        main: main_sec.data.clone(),
        main_base: main_sec.dest as u64,
        stream_dirty: false,
        main_dirty: false,
    };
    let stock_main_len = mem.main.len() as u64;
    let segments_addr = mem.main_base + stock_main_len;

    // The module set: the selection, plus core/expansion when anything is placed in the expansion region.
    let mut selected: Vec<String> = ids.to_vec();
    if uses_expansion(lib, &selected, &dev, port) && !selected.iter().any(|i| i == EXPANSION) {
        if lib.get(EXPANSION).is_none() {
            return Err(err!(
                "a module needs cf.expansion but {EXPANSION} is not in the library"
            ));
        }
        selected.push(EXPANSION.to_string());
    }
    let order = order_modules(lib, &selected)?;
    for id in &order {
        compatible(lib.get(id).unwrap(), port)?;
    }
    let in_expansion = order.iter().any(|i| i == EXPANSION);

    // Registries, entries and sections of the primitives (P1, P2), for this device.
    let mut active = prim::collect(lib, &order, &dev)?;
    {
        let anchor_fn = |n: &str| port.anchor(n).map(|a| anchor_value(a) as i64);
        let image_fn = |n: &str, off: i64, w: u8| image_read(&mem, port, n, off, w);
        let empty: BTreeMap<String, u64> = BTreeMap::new();
        let sc = prim::Scope {
            syms: &empty,
            anchor: &anchor_fn,
            image: &image_fn,
            this: None,
            fields: None,
        };
        let mut firsts = vec![];
        for r in &active.regs {
            firsts.push(
                crate::expr::eval_str(&r.def.first.text(), &sc)
                    .map_err(|e| err!("{}: registry {} first: {e}", r.owner.id(), r.def.name))?,
            );
        }
        active.layout(&firsts, &dev)?;
    }

    // Claims: one owner per resource; every registry entry claims `<registry>.<id>`.
    let mut claims: BTreeMap<String, String> = BTreeMap::new();
    for id in &order {
        for c in lib
            .get(id)
            .unwrap()
            .manifest
            .claim
            .iter()
            .filter(|c| on(&c.devices, &dev))
        {
            if let Some(prev) = claims.insert(c.resource.clone(), id.clone()) {
                return Err(err!("{id} and {prev} both claim {}", c.resource));
            }
        }
    }
    for r in &active.regs {
        for e in r.entries.iter().filter(|e| !e.gap) {
            let res = format!("{}.{}", r.def.name, e.def.id);
            if let Some(prev) = claims.insert(res.clone(), e.owner.id().to_string()) {
                return Err(err!("{} and {prev} both claim {res}", e.owner.id()));
            }
        }
    }

    // Every anchor a selected module uses must hold the bytes the port recorded.
    let mut used: BTreeSet<String> = BTreeSet::new();
    for id in &order {
        let m = lib.get(id).unwrap();
        for (o, dko) in m.manifest.object.iter().zip(&m.objects) {
            if on(&o.devices, &dev) {
                for sy in dko.symbols.iter().filter(|s| s.section.is_none()) {
                    let name = o.externs.get(&sy.name).unwrap_or(&sy.name);
                    if port.anchor(name).is_some() {
                        used.insert(name.clone());
                    }
                }
            }
        }
        for op in &device_ops(m, port)? {
            match &op.anchors {
                Some(g) => used.extend(glob_anchors(port, g).iter().map(|a| a.name.clone())),
                None => {
                    used.insert(op.anchor.clone());
                }
            }
        }
    }
    // anchors the primitives and expressions name (an anchor is verified before anything is read from it)
    {
        let mut names: Vec<String> = vec![];
        fn add(names: &mut Vec<String>, t: &str) -> Result<()> {
            crate::expr::parse(t)?.names(names);
            Ok(())
        }
        for id in &order {
            let m = lib.get(id).unwrap();
            for op in &device_ops(m, port)? {
                if let Some(v) = &op.value {
                    add(&mut names, &v.text())?;
                }
                if op.op == "hook" {
                    add(&mut names, op.to.as_deref().unwrap())?;
                }
            }
        }
        for s in &active.sections {
            names.push(s.def.from.anchor.clone());
            if let Some(l) = &s.def.from.len {
                add(&mut names, &l.text())?;
            }
            for w in s.def.append.iter().flat_map(|a| a.row.iter()) {
                let t = w.text();
                add(&mut names, prim::word_spec(&t)?.1)?;
            }
        }
        for r in &active.regs {
            add(&mut names, &r.def.first.text())?;
            for (fi, f) in r.def.field.iter().enumerate() {
                if r.kinds[fi] != prim::FKind::Cstr {
                    for e in &r.entries {
                        if let Some(v) = e.def.fields.get(&f.name).or(f.default.as_ref()) {
                            match v {
                                toml::Value::String(t) => add(&mut names, t)?,
                                toml::Value::Integer(i) => add(&mut names, &i.to_string())?,
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
        for n in names {
            if port.anchor(&n).is_some() {
                used.insert(n);
            }
        }
    }
    for name in &used {
        let a = port.anchor(name).unwrap();
        if a.kind == "sites" {
            continue; // checked by the parameter framework against its own scan
        }
        verify_anchor(&mem, a)?;
    }
    // Sizes of the primitives' sections and tables, and the decoded hook sites.
    let stg = stage(&mem, port, lib, &order, &dev, &active)?;
    let mut sec_at: Vec<(u64, &'static str)> = vec![(0, CF); active.sections.len()];
    let mut reg_at: Vec<(u64, &'static str)> = vec![(0, CF); active.regs.len()];
    let mut tail_at: Vec<Option<(u64, &'static str)>> = vec![None; stg.hooks.len()];

    // Allocate every object first-fit in its region, then link with everyone's exports visible.
    let mut allocs: BTreeMap<String, Alloc> = BTreeMap::new();
    for r in &port.region {
        allocs.insert(r.name.clone(), Alloc::new(port.region_ranges(&r.name)));
    }
    struct Placed<'a> {
        owner: String,
        dko: &'a Dko,
        externs: &'a BTreeMap<String, String>,
        optional: &'a [String],
        place: String,
        base: u64,
        cpu: &'static str,
    }
    let no_externs: BTreeMap<String, String> = BTreeMap::new();
    let no_optional: Vec<String> = vec![];
    let mut placed: Vec<Placed> = vec![];
    for id in &order {
        let m = lib.get(id).unwrap();
        let fw_objects: &[(Dko, &'static str)] = &[];
        {
            for (dko, region) in fw_objects {
                let (size, align) = link::footprint(dko);
                let al = allocs
                    .get_mut(*region)
                    .ok_or_else(|| err!("port {} has no {region} region", port.id()))?;
                let base = al.take(size.max(4), align.max(4)).ok_or_else(|| {
                    err!(
                        "{id}: {size} bytes do not fit in {region} ({} of {} bytes used)",
                        al.used,
                        al.cap
                    )
                })?;
                placed.push(Placed {
                    owner: m.tag(),
                    dko,
                    externs: &no_externs,
                    optional: &no_optional,
                    place: (*region).into(),
                    base,
                    cpu: if dko.arch == "sharc" { SHARC } else { CF },
                });
            }
        }
        for (o, dko) in m.manifest.object.iter().zip(&m.objects) {
            if !on(&o.devices, &dev) {
                continue;
            }
            let (size, align) = link::footprint(dko);
            let cpu = if dko.arch == "sharc" { SHARC } else { CF };
            let region = port.region(&o.place).unwrap();
            if region.cpu != cpu {
                return Err(err!(
                    "{id}: a {} object cannot be placed in {} ({})",
                    dko.arch,
                    o.place,
                    region.cpu
                ));
            }
            let al = allocs.get_mut(&o.place).unwrap();
            let base = al.take(size.max(4), align.max(4)).ok_or_else(|| {
                err!(
                    "{id}: {size} bytes do not fit in {} ({} of {} bytes used)",
                    o.place,
                    al.used,
                    al.cap
                )
            })?;
            placed.push(Placed {
                owner: m.tag(),
                dko,
                externs: &o.externs,
                optional: &o.optional,
                place: o.place.clone(),
                base,
                cpu,
            });
        }
        // sections, registry tables and hook tails of this module
        for (si, s) in active.sections.iter().enumerate() {
            if s.owner.id() == id {
                let who = format!("{id}: section {}", s.def.name);
                sec_at[si] = take_region(
                    &mut allocs,
                    port,
                    &s.def.region,
                    stg.secs[si].total,
                    s.def.align.unwrap_or(4),
                    &who,
                )?;
            }
        }
        for (ri, r) in active.regs.iter().enumerate() {
            if r.owner.id() == id {
                let who = format!("{id}: registry {}", r.def.name);
                reg_at[ri] =
                    take_region(&mut allocs, port, &r.def.region, stg.reg_size[ri], 4, &who)?;
            }
        }
        for (hi, h) in stg.hooks.iter().enumerate() {
            if h.module.id() == id && h.run {
                let who = format!("{id}: hook {} tail", h.anchor.name);
                tail_at[hi] = Some(take_region(
                    &mut allocs,
                    port,
                    &h.place,
                    h.tail_len,
                    if h.cpu == SHARC { 8 } else { 4 },
                    &who,
                )?);
            }
        }
    }
    let mut globals: BTreeMap<String, u64> = BTreeMap::new();
    for p in &placed {
        let l = link::define(p.dko, p.base)?;
        for g in &l.globals {
            if globals.insert(g.clone(), l.symbols[g]).is_some() {
                return Err(err!("symbol {g} is exported twice"));
            }
        }
    }
    // Symbols of the primitives: sections, registries (rows, count, indices), hook tails and returns.
    let mut prim_syms: BTreeMap<String, u64> = BTreeMap::new();
    let mut prim_owner: Vec<(String, String, u64)> = vec![];
    {
        let mut add = |owner: &Module, name: String, v: u64| -> Result<()> {
            if globals.contains_key(&name) || prim_syms.insert(name.clone(), v).is_some() {
                return Err(err!("symbol {name} is defined twice"));
            }
            prim_owner.push((owner.tag(), name, v));
            Ok(())
        };
        for (si, s) in active.sections.iter().enumerate() {
            add(s.owner, s.def.name.clone(), sec_at[si].0)?;
            add(
                s.owner,
                format!("{}.size", s.def.name),
                stg.secs[si].total as u64,
            )?;
        }
        for (ri, r) in active.regs.iter().enumerate() {
            let n = &r.def.name;
            add(r.owner, format!("{n}.first"), stg.reg_first[ri] as u64)?;
            add(r.owner, format!("{n}.count"), r.entries.len() as u64)?;
            add(r.owner, format!("{n}.entries"), r.live as u64)?;
            add(r.owner, format!("{n}.rows"), reg_at[ri].0)?;
            add(r.owner, format!("{n}.row_size"), r.row_size as u64)?;
            for (k, e) in r.entries.iter().enumerate().filter(|(_, e)| !e.gap) {
                add(
                    e.owner,
                    format!("{n}.{}.index", e.def.id),
                    (stg.reg_first[ri] + k as i64) as u64,
                )?;
            }
        }
        for (hi, h) in stg.hooks.iter().enumerate() {
            let ret = if h.cpu == SHARC {
                h.anchor.pm.map_or_else(
                    || crate::dsp::l2_byte_to_pc(h.site as u32) as u64,
                    |pm| pm + h.op.offset as u64 / 2,
                ) + 3
            } else {
                h.site + h.site_len as u64
            };
            add(h.module, format!("{}.return", h.base_name), ret)?;
            if let Some((addr, _)) = tail_at[hi] {
                let v = if h.cpu == SHARC {
                    crate::dsp::l2_byte_to_pc(addr as u32) as u64
                } else {
                    addr
                };
                add(h.module, format!("{}.tail", h.base_name), v)?;
            }
        }
    }
    // every symbol a manifest expression can name
    let mut all_syms: BTreeMap<String, u64> = globals.clone();
    all_syms.extend(prim_syms.iter().map(|(k, v)| (k.clone(), *v)));

    let mut writes: Vec<PlanWrite> = vec![];
    let mut all_symbols: BTreeMap<String, u64> = BTreeMap::new();
    for (owner, name, v) in &prim_owner {
        all_symbols.insert(format!("{owner}:{name}"), *v);
    }
    let mut chunks: Vec<Chunk> = vec![];
    for p in &placed {
        let mut ext: BTreeMap<String, u64> = globals.clone();
        for sy in p.dko.symbols.iter().filter(|s| s.section.is_none()) {
            let aname = p
                .externs
                .get(&sy.name)
                .cloned()
                .unwrap_or_else(|| sy.name.clone());
            if aname == SEGMENTS {
                ext.insert(sy.name.clone(), segments_addr);
            } else if let Some(v) = prim_syms.get(&aname) {
                ext.insert(sy.name.clone(), *v);
            } else if let Some(a) = port.anchor(&aname) {
                ext.insert(sy.name.clone(), anchor_value(a));
            } else if !ext.contains_key(&sy.name) && p.optional.contains(&sy.name) {
                // an optional link to a module that is not selected
                ext.insert(sy.name.clone(), 0);
            }
        }
        let l = link::link(p.dko, p.base, &ext)?;
        for (k, v) in &l.symbols {
            all_symbols.insert(format!("{}:{k}", p.owner), *v);
        }
        chunks.push(Chunk {
            owner: p.owner.clone(),
            cpu: p.cpu,
            region: p.place.clone(),
            addr: p.base,
            image: l.image,
            code: l.text_ranges,
            bss: l.bss_size.div_ceil(4) * 4,
            what: format!("object in {}", p.place),
        });
    }

    // Bytes of the primitives' memory: registry tables, copied sections with appended rows, hook tails.
    let mut plan_regs: Vec<prim::PlanRegistry> = vec![];
    let mut plan_secs: Vec<prim::PlanSection> = vec![];
    {
        let anchor_fn = |n: &str| port.anchor(n).map(|a| anchor_value(a) as i64);
        let image_fn = |n: &str, off: i64, w: u8| image_read(&mem, port, n, off, w);
        let scope = prim::Scope {
            syms: &all_syms,
            anchor: &anchor_fn,
            image: &image_fn,
            this: None,
            fields: None,
        };
        let pad = |mut v: Vec<u8>, cpu: &str| {
            v.resize(v.len().max(4).div_ceil(4) * 4, 0);
            let _ = cpu;
            v
        };
        let mut built: Vec<Vec<BTreeMap<String, i64>>> = vec![];
        for (ri, r) in active.regs.iter().enumerate() {
            let (addr, cpu) = reg_at[ri];
            let (bytes, vals) =
                prim::build_registry(r, addr, cpu == CF, stg.reg_first[ri], &scope)?;
            plan_regs.push(prim::PlanRegistry {
                name: r.def.name.clone(),
                module: r.owner.tag(),
                region: r.def.region.clone(),
                addr,
                bytes: bytes.len() as u64,
                first: stg.reg_first[ri] as u64,
                count: r.entries.len() as u64,
                max: r.def.max as u64,
                row_size: r.row_size as u64,
                fields: r
                    .def
                    .field
                    .iter()
                    .enumerate()
                    .map(|(i, f)| (f.name.clone(), f.kind.clone(), r.offsets[i] as u64))
                    .collect(),
                gaps: r
                    .entries
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| e.gap)
                    .map(|(k, _)| (stg.reg_first[ri] + k as i64) as u64)
                    .collect(),
                entries: r
                    .entries
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| !e.gap)
                    .map(|(k, e)| prim::PlanEntry {
                        id: e.def.id.clone(),
                        module: e.owner.tag(),
                        index: (stg.reg_first[ri] + k as i64) as u64,
                    })
                    .collect(),
            });
            chunks.push(Chunk {
                owner: r.owner.tag(),
                cpu,
                region: r.def.region.clone(),
                addr,
                image: pad(bytes, cpu),
                code: vec![],
                bss: 0,
                what: format!("registry {} ({} entries)", r.def.name, r.entries.len()),
            });
            built.push(vals);
        }
        for (si, s) in active.sections.iter().enumerate() {
            let (addr, cpu) = sec_at[si];
            let sp = &stg.secs[si];
            let mut bytes = sp.copied.clone();
            let mut rows = 0u64;
            if let Some(ap) = &s.def.append {
                let ri = active
                    .regs
                    .iter()
                    .position(|r| r.def.name == ap.registry)
                    .unwrap();
                bytes.extend(prim::build_rows(s, &built[ri], cpu == CF, &scope)?);
                rows = built[ri].len() as u64;
            }
            debug_assert_eq!(bytes.len(), sp.total);
            plan_secs.push(prim::PlanSection {
                name: s.def.name.clone(),
                module: s.owner.tag(),
                region: s.def.region.clone(),
                addr,
                bytes: bytes.len() as u64,
                from_anchor: s.def.from.anchor.clone(),
                from_addr: sp.from_addr,
                copied: sp.copied.len() as u64,
                appended_rows: rows,
            });
            chunks.push(Chunk {
                owner: s.owner.tag(),
                cpu,
                region: s.def.region.clone(),
                addr,
                image: pad(bytes, cpu),
                code: vec![],
                bss: 0,
                what: format!("section {}", s.def.name),
            });
        }
        for (hi, h) in stg.hooks.iter().enumerate() {
            let Some((addr, cpu)) = tail_at[hi] else {
                continue;
            };
            let who = format!(
                "{}: hook {} {:+}",
                h.module.id(),
                h.anchor.name,
                h.op.offset
            );
            let ret = prim_syms[&format!("{}.return", h.base_name)];
            let bytes = match &h.cf {
                Some(site) => prim::cf_tail(site, addr, ret).map_err(|e| err!("{who}: {e}"))?,
                None => {
                    let mut b = h.old.clone();
                    b.extend(prim::sharc_jump(ret).map_err(|e| err!("{who}: {e}"))?);
                    b
                }
            };
            debug_assert_eq!(bytes.len(), h.tail_len);
            chunks.push(Chunk {
                owner: h.module.tag(),
                cpu,
                region: h.place.clone(),
                addr,
                code: vec![(addr, addr + bytes.len() as u64)],
                image: pad(bytes, cpu),
                bss: 0,
                what: format!("hook tail {}", h.base_name),
            });
        }
    }

    // Operations.
    let mut patches: Vec<Patch> = vec![];
    let mut plan_hooks: Vec<prim::PlanHook> = vec![];
    {
        let anchor_fn = |n: &str| port.anchor(n).map(|a| anchor_value(a) as i64);
        let image_fn = |n: &str, off: i64, w: u8| image_read(&mem, port, n, off, w);
        let scope = prim::Scope {
            syms: &all_syms,
            anchor: &anchor_fn,
            image: &image_fn,
            this: None,
            fields: None,
        };
        for id in &order {
            let m = lib.get(id).unwrap();
            for (oi, op) in device_ops(m, port)?.iter().enumerate() {
                if op.op == "hook" {
                    let hi = stg
                        .hooks
                        .iter()
                        .position(|h| h.module.id() == id && h.op_idx == oi)
                        .unwrap();
                    let h = &stg.hooks[hi];
                    let who = format!(
                        "{id}: hook {}",
                        op.hook.as_deref().unwrap_or(&h.anchor.name)
                    );
                    let to = crate::expr::eval_str(op.to.as_deref().unwrap(), &scope)
                        .map_err(|e| err!("{who}: to: {e}"))?;
                    let to =
                        u64::try_from(to).map_err(|_| err!("{who}: target {to} is negative"))?;
                    let new = match &h.cf {
                        Some(site) => {
                            prim::cf_site_bytes(site, to).map_err(|e| err!("{who}: {e}"))?
                        }
                        None => prim::sharc_jump(to).map_err(|e| err!("{who}: {e}"))?,
                    };
                    patches.push(Patch {
                        owner: m.tag(),
                        cpu: h.cpu,
                        addr: h.site,
                        old_sha256: sha256_hex(&h.old),
                        what: format!("hook {}{:+}", h.anchor.name, op.offset),
                        bytes: new,
                    });
                    plan_hooks.push(prim::PlanHook {
                        module: m.tag(),
                        anchor: h.anchor.name.clone(),
                        hook: op.hook.clone(),
                        offset: op.offset,
                        cpu: h.anchor.cpu.clone(),
                        addr: h.site,
                        site_len: h.site_len as u64,
                        displaced: if h.run { "run" } else { "drop" }.into(),
                        to,
                        tail: prim_syms.get(&format!("{}.tail", h.base_name)).copied(),
                        ret: prim_syms[&format!("{}.return", h.base_name)],
                        displaced_insns: h
                            .cf
                            .iter()
                            .flat_map(|s| s.insns.iter())
                            .map(|d| format!("{:#010x}  {}  {}", d.addr, hex(&d.raw), d.insn))
                            .collect(),
                    });
                    continue;
                }
                let anchors: Vec<&Anchor> = match &op.anchors {
                    Some(g) => glob_anchors(port, g),
                    None => vec![port.anchor(&op.anchor).unwrap()],
                };
                for a in anchors {
                    let cpu: &'static str = if a.cpu == SHARC { SHARC } else { CF };
                    // a value anchor names its instruction by `src` (its `addr` is the number the instruction holds)
                    let site = if a.kind == "value" {
                        a.src.unwrap_or(a.addr)
                    } else {
                        a.addr
                    };
                    let addr = (site as i64 + op.offset) as u64;
                    let new = match op.op.as_str() {
                        "retarget" => retarget(&mem, a, op, &all_syms)?,
                        "write" => match &op.value {
                            Some(v) => {
                                let sc = prim::Scope {
                                    this: Some(&a.name),
                                    ..scope
                                };
                                let what = format!("{id}: write {} {:+}", a.name, op.offset);
                                let x = crate::expr::eval_str(&v.text(), &sc)
                                    .map_err(|e| err!("{what}: {e}"))?;
                                prim::pack(x, op.width.unwrap() as usize, cpu == CF, &what)?
                            }
                            None => write_bytes(a, op, addr, &all_syms)?,
                        },
                        _ => unreachable!(),
                    };
                    let old = mem.read(cpu, addr, new.len() as u64)?;
                    if op.op == "write" {
                        match &op.expect_sha256 {
                            Some(e) if *e != sha256_hex(&old) => {
                                return Err(err!(
                                    "{id}: write at {} +{}: old bytes hash {} but the module expects {e}",
                                    a.name,
                                    op.offset,
                                    sha256_hex(&old)
                                ));
                            }
                            None if op.offset < 0
                                || (op.offset as u64 + new.len() as u64) > a.len =>
                            {
                                return Err(err!(
                                    "{id}: write at {} +{} lies outside the anchor and has no expect_sha256",
                                    a.name,
                                    op.offset
                                ));
                            }
                            _ => {}
                        }
                    }
                    patches.push(Patch {
                        owner: m.tag(),
                        cpu,
                        addr,
                        bytes: new,
                        old_sha256: sha256_hex(&old),
                        what: format!("{} {}{:+}", op.op, a.name, op.offset),
                    });
                }
            }
        }
    }

    // The patch record: first in the segment chain when the expansion region is in use, else a DSP block.
    let rec = Record {
        base_sha256: fw.syx_sha256.clone(),
        modules: order.iter().map(|i| lib.get(i).unwrap().tag()).collect(),
    };
    let rec_bytes = rec.encode();
    let (rec_region, rec_addr) = if !opts.record {
        ("", 0)
    } else if in_expansion {
        let a = allocs
            .get_mut("cf.expansion")
            .and_then(|al| al.take(rec_bytes.len() as u64, 4))
            .ok_or_else(|| err!("no room for the patch record in cf.expansion"))?;
        ("cf.expansion", a)
    } else {
        let a = allocs
            .get_mut("dsp.l2")
            .ok_or_else(|| {
                err!(
                    "port {} has no dsp.l2 region for the patch record",
                    port.id()
                )
            })?
            .take(rec_bytes.len() as u64, 4)
            .ok_or_else(|| err!("no room for the patch record in dsp.l2"))?;
        ("dsp.l2", a)
    };

    // Overlaps among in-place patches and new memory.
    let mut ranges: Vec<(String, u64, u64, String)> = vec![];
    for p in &patches {
        ranges.push((
            p.cpu.to_string(),
            p.addr,
            p.addr + p.bytes.len() as u64,
            format!("{} {}", p.owner, p.what),
        ));
    }
    for c in &chunks {
        ranges.push((
            c.cpu.to_string(),
            c.addr,
            c.addr + c.image.len() as u64 + c.bss,
            format!("{} object in {}", c.owner, c.region),
        ));
    }
    ranges.sort();
    for w in ranges.windows(2) {
        if w[0].0 == w[1].0 && w[1].1 < w[0].2 {
            return Err(err!(
                "overlapping writes: {} [{:#x},{:#x}) and {} [{:#x},{:#x})",
                w[0].3,
                w[0].1,
                w[0].2,
                w[1].3,
                w[1].1,
                w[1].2
            ));
        }
    }

    // Apply in-place patches first (appended memory must not be visible to them).
    for p in &patches {
        mem.write(p.cpu, p.addr, &p.bytes)?;
        writes.push(PlanWrite {
            cpu: p.cpu.to_string(),
            addr: p.addr,
            len: p.bytes.len() as u64,
            kind: "patch".into(),
            owner: p.owner.clone(),
            what: p.what.clone(),
            old_sha256: Some(p.old_sha256.clone()),
            new_sha256: sha256_hex(&p.bytes),
            new_hex: (p.bytes.len() <= 32).then(|| hex(&p.bytes)),
            code: vec![],
        });
    }
    // Placed objects: DSP blocks, flash-cave code, and the expansion segment chain.
    let mut segs: Vec<(u64, Vec<u8>, u64)> = vec![]; // dest, payload, bss
    if opts.record && in_expansion {
        segs.push((rec_addr, rec_bytes.clone(), 0));
    }
    for c in &chunks {
        match (c.cpu, c.region.as_str()) {
            (SHARC, _) => {
                mem.stream.append_block(c.addr as u32, &c.image)?;
                mem.stream_dirty = true;
            }
            (_, "cf.flash_cave") => {
                let old = mem.read(CF, c.addr, c.image.len() as u64)?;
                if old.iter().any(|&b| b != 0) {
                    return Err(err!(
                        "{}: cf.flash_cave at {:#x} is not zero in this image",
                        c.owner,
                        c.addr
                    ));
                }
                mem.write(CF, c.addr, &c.image)?;
            }
            (_, "cf.expansion") => {
                let mut img = c.image.clone();
                img.resize(img.len().div_ceil(4) * 4, 0);
                segs.push((c.addr, img, c.bss));
            }
            (_, r) => return Err(err!("{}: no placement rule for region {r}", c.owner)),
        }
        writes.push(PlanWrite {
            cpu: c.cpu.to_string(),
            addr: c.addr,
            len: c.image.len() as u64 + c.bss,
            kind: "place".into(),
            owner: c.owner.clone(),
            what: c.what.clone(),
            old_sha256: None,
            new_sha256: sha256_hex(&c.image),
            new_hex: None,
            code: c.code.clone(),
        });
    }
    if opts.record {
        if rec_region == "dsp.l2" {
            mem.stream.append_block(rec_addr as u32, &rec_bytes)?;
            mem.stream_dirty = true;
        }
        writes.push(PlanWrite {
            cpu: if rec_region == "dsp.l2" { SHARC } else { CF }.into(),
            addr: rec_addr,
            len: rec_bytes.len() as u64,
            kind: "record".into(),
            owner: "patchkit".into(),
            what: format!("DKPR in {rec_region}"),
            old_sha256: None,
            new_sha256: sha256_hex(&rec_bytes),
            new_hex: None,
            code: vec![],
        });
    }
    let mut image_id = None;
    if in_expansion {
        if !mem.main.len().is_multiple_of(4) {
            return Err(err!(
                "MAIN OS length {} is not a multiple of 4",
                mem.main.len()
            ));
        }
        // where the id field lies in MAIN OS once the segments are appended (its symbol's address inside a segment payload)
        let id_addr = globals.get(crate::imageid::SYMBOL).copied();
        let mut id_at = None;
        for (dest, payload, bss) in &segs {
            mem.main
                .extend_from_slice(&(payload.len() as u32).to_be_bytes());
            mem.main.extend_from_slice(&(*dest as u32).to_be_bytes());
            mem.main.extend_from_slice(&(*bss as u32).to_be_bytes());
            if let Some(a) = id_addr
                && (*dest..*dest + payload.len() as u64).contains(&a)
            {
                id_at = Some(mem.main.len() + (a - dest) as usize);
            }
            mem.main.extend_from_slice(payload);
        }
        mem.main.extend_from_slice(&u32::MAX.to_be_bytes());
        mem.main_dirty = true;
        if let Some(at) = id_at {
            if at < crate::imageid::MARKER.len()
                || &mem.main[at - crate::imageid::MARKER.len()..at] != crate::imageid::MARKER
                || mem.main[at..at + 8] != [0u8; 8]
            {
                return Err(err!(
                    "{}: the id field is not the marker and 8 zero bytes",
                    crate::imageid::SYMBOL
                ));
            }
            if crate::imageid::locate(&mem.main)? != Some(at) {
                return Err(err!("the image id marker is not unique in MAIN OS"));
            }
            let id = crate::imageid::compute(&unhex(&fw.syx_sha256)?, &mem.main, &mem.stream.blob);
            mem.main[at..at + 8].copy_from_slice(&id);
            image_id = Some(id);
        }
    }

    // Limits, repack, build.
    let lim = &port.limits;
    if mem.stream_dirty {
        if mem.stream.blob.len() as u64 > lim.dsp_depacked_max {
            return Err(err!(
                "DSP stream is {} bytes, over the {} the ColdFire boot routine sends",
                mem.stream.blob.len(),
                lim.dsp_depacked_max
            ));
        }
        fw.replace(7, mem.stream.blob.clone())?;
        let stored = fw.section(7).unwrap().stored.len() as u64;
        if stored > lim.dsp_stored_max {
            return Err(err!(
                "packed DSP section is {stored} bytes, over the {} limit",
                lim.dsp_stored_max
            ));
        }
    }
    if mem.main_dirty {
        if mem.main.len() as u64 > lim.main_staged_max {
            return Err(err!(
                "MAIN OS is {} bytes, over the {} staging limit",
                mem.main.len(),
                lim.main_staged_max
            ));
        }
        fw.replace(3, mem.main.clone())?;
    }
    let syx = fw.build(Some(lim.container_max as usize))?;
    let regions = allocs
        .iter()
        .map(|(k, v)| (k.clone(), (v.used, v.cap)))
        .collect();
    let plan = Plan {
        base: PlanBase {
            device: dev,
            version: port.port.version.clone(),
            syx_sha256: fw.syx_sha256.clone(),
        },
        modules: order.iter().map(|i| lib.get(i).unwrap().tag()).collect(),
        writes,
        regions,
        symbols: all_symbols,
        registries: plan_regs,
        sections: plan_secs,
        hooks: plan_hooks,
        record: rec,
        output: None,
    };
    let mut applied = Applied {
        syx,
        plan,
        image_id,
    };
    let report = crate::verify::verify(&applied.syx, Some(reg), Some(fw_bytes))?;
    applied.plan.output = Some(PlanOutput {
        syx_sha256: sha256_hex(&applied.syx),
        syx_bytes: applied.syx.len() as u64,
        container_bytes: report.container_bytes,
        changed_sections: fw
            .sections
            .iter()
            .filter(|s| s.changed)
            .map(|s| (s.id, s.data.len() as u64, s.stored.len() as u64))
            .collect(),
    });
    Ok(applied)
}

/// Rewrite the target of a direct call or jump at an anchor.
///
/// SHARC+: only the unconditional direct CALL (`25a_direct`, opcode 0x180400) in v1; the 24-bit target is the
/// short-word pc. ColdFire: `jsr abs.l` / `jmp abs.l` (absolute target), `jsr d16(pc)`, `bsr.w` and `bsr.l`
/// (pc-relative, range checked).
fn retarget(mem: &Mem, a: &Anchor, op: &Op, globals: &BTreeMap<String, u64>) -> Result<Vec<u8>> {
    let to = op.to.as_deref().unwrap();
    let target = *globals
        .get(to)
        .ok_or_else(|| err!("retarget: module symbol {to} is not defined"))?;
    let addr = (a.addr as i64 + op.offset) as u64;
    if a.cpu == SHARC {
        let b = mem.read(SHARC, addr, 6)?;
        let p0 = u16::from_le_bytes([b[0], b[1]]);
        let p1 = u16::from_le_bytes([b[2], b[3]]);
        if p0 != 0x1804 || (p1 >> 8) != 0 {
            return Err(err!(
                "retarget {}: the instruction at {addr:#x} is not a direct CALL (words {} )",
                a.name,
                hex(&b)
            ));
        }
        if target >= 1 << 24 {
            return Err(err!(
                "retarget: {to} = {target:#x} does not fit a 24-bit target"
            ));
        }
        let n1 = (p1 & 0xff00) | ((target >> 16) as u16 & 0xff);
        let n2 = (target & 0xffff) as u16;
        let mut out = vec![];
        out.extend_from_slice(&p0.to_le_bytes());
        out.extend_from_slice(&n1.to_le_bytes());
        out.extend_from_slice(&n2.to_le_bytes());
        return Ok(out);
    }
    let b = mem.read(CF, addr, 6)?;
    let w = u16::from_be_bytes([b[0], b[1]]);
    let rel = |bits: u32, ext_at: u64| -> Result<i64> {
        let d = target as i64 - (addr + ext_at) as i64;
        let lim = 1i64 << (bits - 1);
        if !(-lim..lim).contains(&d) {
            return Err(err!(
                "retarget {}: {to} is {d} bytes from the call, out of range for {bits} bits",
                a.name
            ));
        }
        Ok(d)
    };
    match w {
        0x4eb9 | 0x4ef9 => {
            let mut out = w.to_be_bytes().to_vec();
            out.extend_from_slice(&(target as u32).to_be_bytes());
            Ok(out)
        }
        0x4eba => {
            let mut out = w.to_be_bytes().to_vec();
            out.extend_from_slice(&(rel(16, 2)? as i16).to_be_bytes());
            Ok(out)
        }
        0x6100 => {
            let mut out = w.to_be_bytes().to_vec();
            out.extend_from_slice(&(rel(16, 2)? as i16).to_be_bytes());
            Ok(out)
        }
        0x61ff => {
            let mut out = w.to_be_bytes().to_vec();
            out.extend_from_slice(&(rel(32, 2)? as i32).to_be_bytes());
            Ok(out)
        }
        _ => Err(err!(
            "retarget {}: the instruction at {addr:#x} is not a jsr/jmp/bsr with a 16- or 32-bit operand (words {})",
            a.name,
            hex(&b)
        )),
    }
}

fn write_bytes(a: &Anchor, op: &Op, addr: u64, globals: &BTreeMap<String, u64>) -> Result<Vec<u8>> {
    if a.cpu != CF && !op.reloc.is_empty() {
        return Err(err!(
            "write: relocations are only supported for ColdFire writes"
        ));
    }
    apply_relocs(unhex(op.hex.as_deref().unwrap())?, &op.reloc, addr, globals)
}

/// ColdFire relocations of a write of `bytes` at `addr`.
fn apply_relocs(
    mut bytes: Vec<u8>,
    relocs: &[crate::module::OpReloc],
    addr: u64,
    globals: &BTreeMap<String, u64>,
) -> Result<Vec<u8>> {
    for r in relocs {
        if r.rtype == "sharc.abs32" {
            // the low 32 bits of a 48-bit instruction (parcels little endian), as the linker writes them
            let v = (*globals
                .get(&r.symbol)
                .ok_or_else(|| err!("write: symbol {} is not defined", r.symbol))?
                as i64
                + r.addend) as u64
                & 0xffff_ffff;
            let at = r.offset as usize;
            if at + 6 > bytes.len() {
                return Err(err!("write: relocation at {at} runs past the bytes"));
            }
            bytes[at + 2..at + 4].copy_from_slice(&((v >> 16) as u16).to_le_bytes());
            bytes[at + 4..at + 6].copy_from_slice(&((v & 0xffff) as u16).to_le_bytes());
            continue;
        }
        let s = *globals
            .get(&r.symbol)
            .ok_or_else(|| err!("write: symbol {} is not defined", r.symbol))?
            as i64
            + r.addend;
        let (size, pcrel) = match r.rtype.as_str() {
            "cf.32" => (4usize, false),
            "cf.pc32" => (4, true),
            "cf.pc16" => (2, true),
            "cf.pc8" => (1, true),
            t => {
                return Err(err!(
                    "write: relocation type {t} is not supported in a write"
                ));
            }
        };
        let v = if pcrel {
            s - (addr + r.offset) as i64
        } else {
            s
        };
        let at = r.offset as usize;
        if at + size > bytes.len() {
            return Err(err!(
                "write: relocation at {at} runs past the {} bytes",
                bytes.len()
            ));
        }
        bytes[at..at + size].copy_from_slice(&(v as u64).to_be_bytes()[8 - size..]);
    }
    Ok(bytes)
}
