//! The `register` section of a `.dkm`: typed machine and parameter records for the digikit runtime's registry
//! (docs/design/digikit-registry.md). The packer lowers a dynamic module's typed sections (`typed.rs`) for one device into
//! these records; the runtime (`modules/core/runtime/src/rs/src/registry.rs`) validates them before it writes any row.
//!
//! All integers are big endian. Layout:
//!
//! ```text
//! 0x00 u32 magic "DKRG"   0x04 u32 version = 1   0x08 u32 device (1 dt2, 2 dn2)   0x0c u32 record count
//! 0x10 records, each: u32 kind, u32 length (bytes, a multiple of 4, this header included), then
//!   kind 1 PARAM   (0x50): u32 index, template, slot; i32 min, max, default; name[32]; short[16]
//!   kind 2 MACHINE (0x6c): u32 key, clone_of, position, own_dsp, level; i32 knobs[8]; name[24]; short[24]
//!   kind 3 PAGE    (0x60): u32 type, knobs[8], level; name[24]; title[24]            (Digitone II)
//!   kind 5 ACTION  (0x10): u32 param (the index of a PARAM record of the same file), image offset of the click callback `void fn(u32 track)`
//!   kind 4 EFFECT  (0x1a4): u32 fx, mix, n_knobs, id; tab[8]; title[24]; 7 knobs of (u32 position, display, f32 min, max, default; name[24]; short[8])
//!                           (the page of a run-time effect, Digitakt II; `fx` is the index of the effect record in the DSP section; `id` is the
//!                           effect's 16-bit ID, `effect_id`: the kit stores it with the effect's values, so a kit load restores values only to the effect they came from)
//! ```
//!
//! `key` (a machine's and a page's first field, a DSP voice's type) is `hash << 8 | type`: the 24-bit hash of the machine's global id
//! (`machine_hash`) above a type byte of 0, which tells the runtime to assign a type number in the run-time range at load (and to remember
//! it on the card). A type byte of 16 to 23 with hash 0 pins that number (tests and hand-made files).
//!
//! A knob is a stock parameter code (0 = none) or `KNOB_PARAM | index` of a PARAM record of the same file. A page's knob
//! is `KNOB_PARAM | index` or 0; its sound-array slot is the device's slot base plus the knob position. On a device that
//! keeps its knobs in pages (the Digitone II) the machine record's knobs are 0.

use crate::error::*;
use crate::typed::{DevResolved, Knob};

pub const MAGIC: u32 = 0x444b_5247; // "DKRG"
pub const VERSION: u32 = 1;
pub const HEADER: usize = 16;
pub const KIND_PARAM: u32 = 1;
pub const KIND_MACHINE: u32 = 2;
pub const KIND_PAGE: u32 = 3;
pub const LEN_PARAM: usize = 0x50;
pub const LEN_MACHINE: usize = 0x6c;
pub const LEN_PAGE: usize = 0x60;
pub const KIND_EFFECT: u32 = 4;
pub const LEN_EFFECT: usize = 0x1a4;
/// kind 5 ACTION (0x10): u32 param, offset. The click callback of the file's PARAM record `param`: `offset` is its image offset (the runtime adds the load address).
pub const KIND_ACTION: u32 = 5;
pub const LEN_ACTION: usize = 0x10;
/// Knobs of an effect page (the 8th word of its parameters is MIX), the text fields, and the MIX default's range (the knob's raw 0 .. 127).
pub const EFFECT_KNOBS: usize = 7;
pub const EFFECT_TAB: usize = 8;
pub const EFFECT_TAB_MAX: usize = 3;
pub const EFFECT_TITLE: usize = 24;
pub const EFFECT_NAME: usize = 24;
pub const EFFECT_SHORT: usize = 8;
pub const EFFECT_SHORT_MAX: usize = 6;
pub const MAX_RECORDS: usize = dkneeds::MAX_RECORDS as usize;
/// Encoders on a page: an array length, so a const (the test `the_record_format_constants_come_from_the_contract_maxima` in packages/patchkit/tests/sdkgen.rs keeps it equal to `DK_PAGE_KNOBS` of sdk/sdk.toml).
pub const KNOBS: usize = 8;
pub const KNOB_PARAM: u32 = 0x8000_0000;
/// `position` of a run-time machine: listed after the built-in machines, in type order.
pub const POS_END: u32 = 255;
pub const MACHINE_NAME: usize = 24;
pub const PARAM_NAME: usize = 32;
pub const PARAM_SHORT: usize = 16;

/// The 24-bit hash of a run-time machine's global id `<module id>:<machine id>`: the low 24 bits of FNV-1a 32 of the UTF-8 string
/// (docs/design/module-sdk.md 11.5 D7, docs/design/digikit-registry.md "Machine keys").
pub fn machine_hash(module: &str, machine: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in format!("{module}:{machine}").bytes() {
        h = (h ^ b as u32).wrapping_mul(0x0100_0193);
    }
    h & 0x00ff_ffff
}

/// The 16-bit ID of a run-time effect (1 to 0xffff; 0 means "no effect" in a kit's storage): the low and high halves of FNV-1a 32 of the UTF-8 string `<module id>:<effect id>`
/// folded together, 0 turned into 1. The runtime keeps the effect's values in the kit under this number (docs/findings/61-fx-pages.md "Persistence"); it is
/// defined here and nowhere else: the record carries it. Two effects of one image can share an ID (1 in 65535 for a pair): the later load then takes the values of the other.
pub fn effect_id(module: &str, effect: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in format!("{module}:{effect}").bytes() {
        h = (h ^ b as u32).wrapping_mul(0x0100_0193);
    }
    let f = (h >> 16) ^ (h & 0xffff);
    if f == 0 { 1 } else { f }
}

/// The machine key a `.dkm` carries where a stock machine has its type: the hash in the upper 24 bits and a low byte of 0 ("assign a type
/// at load"). A page and a DSP voice name their machine by the same key.
pub fn machine_key(module: &str, machine: &str) -> u32 {
    machine_hash(module, machine) << 8
}

/// A type field as `dkm-info` prints it: a machine key as `h<hash>` (six hex digits), a plain type number (hash 0) as decimal.
pub fn fmt_type(t: u32) -> String {
    match t >> 8 {
        0 => t.to_string(),
        h => format!("h{h:06x}"),
    }
}

/// The id digikit gives a device (`[digikit] id` of the device file).
pub fn device_id(dev: &str) -> Result<u32> {
    crate::devdata::by_short(dev)
        .map(|d| d.digikit_id)
        .ok_or_else(|| err!("no run-time registry for device {dev:?}"))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Param {
    pub index: u32,
    pub template: u32,
    pub slot: u32,
    pub min: i32,
    pub max: i32,
    pub default: i32,
    pub name: String,
    pub short: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Machine {
    pub type_no: u32,
    pub clone_of: u32,
    pub position: u32,
    pub own_dsp: u32,
    pub level: u32,
    pub knobs: [u32; KNOBS],
    pub name: String,
    pub short: String,
}

/// A parameter page of a machine (Digitone II): the knobs A .. H and the level code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page {
    pub type_no: u32,
    pub knobs: [u32; KNOBS],
    pub level: u32,
    pub name: String,
    pub title: String,
}

/// One knob of an effect page. `min`, `max` and `default` are IEEE single floats (their bits): the value the effect gets is a float in that range.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectKnob {
    pub position: u32,
    /// 0 percent (0 .. 127), 1 bipolar (-64 .. +63)
    pub display: u32,
    pub min: u32,
    pub max: u32,
    pub default: u32,
    pub name: String,
    pub short: String,
}

/// The page of a run-time effect (Digitakt II): the effect is record `fx` of the module's DSP section. `mix` is the default of the MIX knob (raw 0 .. 127).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectPage {
    pub fx: u32,
    /// The effect's ID in a kit's storage (`effect_id`, 1 to 0xffff).
    pub id: u32,
    pub mix: u32,
    pub tab: String,
    pub title: String,
    pub knobs: Vec<EffectKnob>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Register {
    pub device: u32,
    pub params: Vec<Param>,
    /// Click callbacks: (the PARAM record's index, the image offset of the function), by parameter.
    pub actions: Vec<(u32, u32)>,
    pub machines: Vec<Machine>,
    pub pages: Vec<Page>,
    pub effects: Vec<EffectPage>,
}

/// The f32 of a manifest number, checked finite.
fn f32_bits(what: &str, v: f64) -> Result<u32> {
    let f = v as f32;
    if !f.is_finite() {
        return Err(err!("{what}: {v} is not a finite float"));
    }
    Ok(f.to_bits())
}

/// The effect page of effect number `fx` (its index in the device's effect list), or None when it declares no `tab`. A knob's default is a float in
/// [min, max]; positions are distinct, 0 to 7.
pub fn effect_page(module: &str, fx: usize, d: &crate::typed::FxDef) -> Result<Option<EffectPage>> {
    let Some(tab) = &d.tab else {
        if d.title.is_some() || !d.knob.is_empty() {
            return Err(err!(
                "{module}: fx {}: a title or knobs need a `tab` (the page's tab label)",
                d.id
            ));
        }
        return Ok(None);
    };
    let who = format!("{module}: fx {}", d.id);
    let Some(title) = &d.title else {
        return Err(err!("{who}: a page needs a `title`"));
    };
    if !(0..=127).contains(&d.mix) {
        return Err(err!("{who}: mix {} is 0 to 127", d.mix));
    }
    if d.knob.len() > EFFECT_KNOBS {
        return Err(err!(
            "{who}: {} knobs; a page has at most {EFFECT_KNOBS} (and MIX)",
            d.knob.len()
        ));
    }
    let mut knobs = vec![];
    for (i, k) in d.knob.iter().enumerate() {
        let w = format!("{who}: knob {i} ({})", k.name);
        let position = k.position.unwrap_or(i as u32);
        let display = match k.display.as_deref() {
            None | Some("percent") => 0,
            Some("bipolar") => 1,
            Some(o) => return Err(err!("{w}: display {o:?} is percent or bipolar")),
        };
        if !(k.min < k.max) || !(k.min..=k.max).contains(&k.default) {
            return Err(err!(
                "{w}: needs min < max and a default inside (min {}, max {}, default {})",
                k.min,
                k.max,
                k.default
            ));
        }
        knobs.push(EffectKnob {
            position,
            display,
            min: f32_bits(&w, k.min)?,
            max: f32_bits(&w, k.max)?,
            default: f32_bits(&w, k.default)?,
            name: k.name.clone(),
            short: k.short.clone(),
        });
    }
    let e = EffectPage {
        fx: fx as u32,
        id: effect_id(module, &d.id),
        mix: d.mix as u32,
        tab: tab.clone(),
        title: title.clone(),
        knobs,
    };
    e.check().map_err(|x| err!("{who}: {}", x.0))?;
    Ok(Some(e))
}

/// The routes of effect `d` as (mask of `dkmdsp::ROUTE_*`, default route). `route` is `"master"` (the default when left out) or `"send"`. `routes` lists the routes the user may
/// choose in DIGIKIT > FX ROUTES; left out it is `["master"]`, and `["send", "master"]` when `route = "send"` (so a send effect falls back to the master when the send slots
/// are taken, and a master effect is a master effect). A send-only effect says `routes = ["send"]`. Refused: a name that is not `master` or `send`, an empty `routes`, a route twice,
/// and a `route` that `routes` does not list.
pub fn fx_routes(module: &str, d: &crate::typed::FxDef) -> Result<(u32, u32)> {
    use crate::dkmdsp::{ROUTE_MASTER, ROUTE_SEND};
    let who = format!("{module}: fx {}", d.id);
    let one = |what: &str, name: &str| match name {
        "master" => Ok(ROUTE_MASTER),
        "send" => Ok(ROUTE_SEND),
        o => Err(err!("{who}: {what} {o:?} is master or send")),
    };
    let route = match d.route.as_deref() {
        None => ROUTE_MASTER,
        Some(r) => one("route", r)?,
    };
    let routes = match &d.routes {
        None if route == ROUTE_SEND => ROUTE_SEND | ROUTE_MASTER,
        None => ROUTE_MASTER,
        Some(l) if l.is_empty() => {
            return Err(err!("{who}: routes is empty; list master, send or both"));
        }
        Some(l) => {
            let mut mask = 0;
            for r in l {
                let b = one("routes entry", r)?;
                if mask & b != 0 {
                    return Err(err!("{who}: routes lists {r:?} twice"));
                }
                mask |= b;
            }
            mask
        }
    };
    if routes & route == 0 {
        return Err(err!(
            "{who}: route {:?} is not in routes",
            d.route.as_deref().unwrap_or("master")
        ));
    }
    // the send knob and the FX ROUTES row are labelled with the page's tab: an effect without a page is an insert
    if d.tab.is_none() && routes != ROUTE_MASTER {
        return Err(err!(
            "{who}: a send route needs a `tab` (the label of the effect's page, which also labels its send knob and its FX ROUTES row)"
        ));
    }
    Ok((routes, route))
}

/// The words p0 ... of an effect's row: with a page, knob i's default as a float for p_i and 0 after them (`defaults` must then be empty: a word has one
/// source); without a page, `defaults`.
pub fn fx_defaults(module: &str, d: &crate::typed::FxDef) -> Result<Vec<u32>> {
    if d.tab.is_none() {
        return Ok(d.defaults.clone());
    }
    if !d.defaults.is_empty() {
        return Err(err!(
            "{module}: fx {}: an effect with a page takes its defaults from the knobs; drop `defaults`",
            d.id
        ));
    }
    d.knob
        .iter()
        .map(|k| {
            f32_bits(
                &format!("{module}: fx {}: knob {}", d.id, k.name),
                k.default,
            )
        })
        .collect()
}

impl EffectPage {
    pub fn check(&self) -> Result<()> {
        text_ok("effect tab", &self.tab, EFFECT_TAB_MAX)?;
        text_ok("effect page title", &self.title, EFFECT_TITLE - 1)?;
        if self.mix > 127 {
            return Err(err!("mix {} is 0 to 127", self.mix));
        }
        if self.id == 0 || self.id > 0xffff {
            return Err(err!("effect id {} is 1 to 65535", self.id));
        }
        if self.knobs.len() > EFFECT_KNOBS {
            return Err(err!("{} knobs; at most {EFFECT_KNOBS}", self.knobs.len()));
        }
        let mut seen = 0u32;
        for k in &self.knobs {
            text_ok("knob name", &k.name, EFFECT_NAME - 1)?;
            text_ok("knob short", &k.short, EFFECT_SHORT_MAX)?;
            if k.position >= KNOBS as u32 || seen & (1 << k.position) != 0 || k.display > 1 {
                return Err(err!(
                    "knob {}: a position 0 to 7 that no other knob has, display 0 or 1",
                    k.name
                ));
            }
            seen |= 1 << k.position;
            let (lo, hi, d) = (
                f32::from_bits(k.min),
                f32::from_bits(k.max),
                f32::from_bits(k.default),
            );
            if !(lo.is_finite() && hi.is_finite() && d.is_finite())
                || !(lo < hi)
                || !(lo..=hi).contains(&d)
            {
                return Err(err!("knob {}: min < max and default inside", k.name));
            }
        }
        Ok(())
    }
}

fn text_ok(what: &str, s: &str, max: usize) -> Result<()> {
    if s.is_empty() || s.len() > max {
        return Err(err!("{what} {s:?}: 1 to {max} characters"));
    }
    if !s.bytes().all(|c| (0x20..0x7f).contains(&c)) {
        return Err(err!("{what} {s:?}: printable ASCII only"));
    }
    Ok(())
}

fn put_text(out: &mut Vec<u8>, s: &str, field: usize) {
    let mut b = s.as_bytes().to_vec();
    b.resize(field, 0);
    out.extend_from_slice(&b);
}

fn p32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}

impl Register {
    /// Lower one device's resolved typed sections of module `module`. A machine's `type_no` is its machine key (`machine_key`):
    /// the runtime assigns the type number at load.
    pub fn lower(r: &DevResolved, module: &str) -> Result<Register> {
        let key = |machine: &str| machine_key(module, machine);
        let mut reg = Register {
            device: device_id(&r.dev)?,
            ..Default::default()
        };
        for (i, p) in r.params.iter().enumerate() {
            reg.params.push(Param {
                index: i as u32,
                template: p.template as u32,
                slot: p.slot as u32,
                min: p.min as i32,
                max: p.max as i32,
                default: p.default as i32,
                name: p.name.clone(),
                short: p.short.clone(),
            });
        }
        for m in &r.machines {
            let t = key(&m.id);
            if t == 0 {
                return Err(err!("machine {}: its id hashes to 0; rename it", m.id));
            }
            let mut knobs = [0u32; KNOBS];
            if r.model.knobs.to == "machine" {
                for (j, k) in m.knobs.iter().enumerate() {
                    knobs[j] = match k {
                        Knob::Code(c) if (0..=0xffff).contains(c) => *c as u32,
                        Knob::Code(c) => return Err(err!("machine {}: knob code {c}", m.id)),
                        Knob::Param(id) => {
                            let i = r.params.iter().position(|p| &p.id == id).ok_or_else(|| {
                                err!("machine {}: knob names {id:?}, not a param", m.id)
                            })?;
                            KNOB_PARAM | i as u32
                        }
                    };
                }
            }
            // otherwise the device keeps its knobs in pages: the machine record's knobs stay 0
            reg.machines.push(Machine {
                type_no: t,
                clone_of: m.clone_of as u32,
                position: POS_END,
                own_dsp: u32::from(m.own_dsp),
                level: m.level as u32,
                knobs,
                name: m.name.clone(),
                short: m.short.clone(),
            });
        }
        for g in &r.pages {
            let t = key(&g.machine);
            let mut knobs = [0u32; KNOBS];
            for (j, k) in g.knobs.iter().enumerate() {
                knobs[j] = match k {
                    Knob::Code(0) => 0,
                    Knob::Code(c) => return Err(err!("page {}: knob {j}: stock code {c}", g.id)),
                    Knob::Param(id) => {
                        let i =
                            r.params.iter().position(|p| &p.id == id).ok_or_else(|| {
                                err!("page {}: knob names {id:?}, not a param", g.id)
                            })?;
                        KNOB_PARAM | i as u32
                    }
                };
            }
            reg.pages.push(Page {
                type_no: t,
                knobs,
                level: g.level as u32,
                name: g.name.clone(),
                title: g.title.clone(),
            });
        }
        reg.check()?;
        Ok(reg)
    }

    /// The click callbacks of the module's parameters (`DK_PARAM_ACTION`): the image offset of each one's symbol. `symbols` are the
    /// module's global ColdFire symbols with their offsets in the loaded image (`dkm::image_symbols`).
    pub fn set_actions(
        &mut self,
        r: &DevResolved,
        symbols: &std::collections::BTreeMap<String, u32>,
    ) -> Result<()> {
        self.actions.clear();
        for (i, p) in r.params.iter().enumerate() {
            let Some(func) = &p.action else { continue };
            let off = symbols.get(func).ok_or_else(|| {
                err!(
                    "param {}: the click callback {func} is not a global function of the module (a static function has no symbol)",
                    p.id
                )
            })?;
            self.actions.push((i as u32, *off));
        }
        self.check()
    }

    /// The checks the runtime repeats on the file.
    pub fn check(&self) -> Result<()> {
        let max = crate::sdkgen::limits_max();
        let (max_params, max_machines, max_pages) = (
            max.param_rows as usize,
            max.machine_rows as usize,
            max.page_rows as usize,
        );
        if self.params.len() > max_params
            || self.machines.len() > max_machines
            || self.pages.len() > max_pages
        {
            return Err(err!(
                "at most {max_params} params, {max_machines} machines and {max_pages} pages per module"
            ));
        }
        for p in &self.params {
            text_ok("param name", &p.name, PARAM_NAME - 1)?;
            text_ok("param short", &p.short, PARAM_SHORT - 1)?;
            if p.min > p.max {
                return Err(err!("param {}: min above max", p.name));
            }
        }
        let mut seen = vec![];
        for m in &self.machines {
            text_ok("machine name", &m.name, MACHINE_NAME - 1)?;
            text_ok("machine short", &m.short, MACHINE_NAME - 1)?;
            if seen.contains(&m.type_no) {
                return Err(err!("two machines with type {}", m.type_no));
            }
            seen.push(m.type_no);
            for k in m.knobs {
                if k & KNOB_PARAM != 0 && ((k & !KNOB_PARAM) as usize) >= self.params.len() {
                    return Err(err!("machine {}: knob names a missing param", m.name));
                }
            }
        }
        let mut acted = vec![];
        for &(param, off) in &self.actions {
            if param as usize >= self.params.len() || acted.contains(&param) {
                return Err(err!(
                    "click action of param {param}: no such param in the file, or two actions"
                ));
            }
            if off == u32::MAX {
                return Err(err!("click action of param {param}: no callback offset"));
            }
            acted.push(param);
        }
        let max_effects = max.fx_slots as usize;
        if self.effects.len() > max_effects
            || self.params.len()
                + self.actions.len()
                + self.machines.len()
                + self.pages.len()
                + self.effects.len()
                > MAX_RECORDS
        {
            return Err(err!(
                "at most {max_effects} effect pages and {MAX_RECORDS} records per module"
            ));
        }
        let mut fxs = vec![];
        for e in &self.effects {
            e.check()?;
            if fxs.contains(&e.fx) {
                return Err(err!("two pages for effect {}", e.fx));
            }
            fxs.push(e.fx);
        }
        for g in &self.pages {
            text_ok("page name", &g.name, MACHINE_NAME - 1)?;
            text_ok("page title", &g.title, MACHINE_NAME - 1)?;
            if !self.machines.iter().any(|m| m.type_no == g.type_no) {
                return Err(err!(
                    "page {}: no machine of type {} in the file",
                    g.name,
                    g.type_no
                ));
            }
            for k in g.knobs {
                if k != 0
                    && (k & KNOB_PARAM == 0 || ((k & !KNOB_PARAM) as usize) >= self.params.len())
                {
                    return Err(err!("page {}: knob is not a param of the file", g.name));
                }
            }
        }
        Ok(())
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut o = vec![];
        p32(&mut o, MAGIC);
        p32(&mut o, VERSION);
        p32(&mut o, self.device);
        p32(
            &mut o,
            (self.params.len()
                + self.actions.len()
                + self.machines.len()
                + self.pages.len()
                + self.effects.len()) as u32,
        );
        for p in &self.params {
            p32(&mut o, KIND_PARAM);
            p32(&mut o, LEN_PARAM as u32);
            p32(&mut o, p.index);
            p32(&mut o, p.template);
            p32(&mut o, p.slot);
            p32(&mut o, p.min as u32);
            p32(&mut o, p.max as u32);
            p32(&mut o, p.default as u32);
            put_text(&mut o, &p.name, PARAM_NAME);
            put_text(&mut o, &p.short, PARAM_SHORT);
        }
        for &(param, off) in &self.actions {
            p32(&mut o, KIND_ACTION);
            p32(&mut o, LEN_ACTION as u32);
            p32(&mut o, param);
            p32(&mut o, off);
        }
        for m in &self.machines {
            p32(&mut o, KIND_MACHINE);
            p32(&mut o, LEN_MACHINE as u32);
            for v in [m.type_no, m.clone_of, m.position, m.own_dsp, m.level] {
                p32(&mut o, v);
            }
            for k in m.knobs {
                p32(&mut o, k);
            }
            put_text(&mut o, &m.name, MACHINE_NAME);
            put_text(&mut o, &m.short, MACHINE_NAME);
        }
        for g in &self.pages {
            p32(&mut o, KIND_PAGE);
            p32(&mut o, LEN_PAGE as u32);
            p32(&mut o, g.type_no);
            for k in g.knobs {
                p32(&mut o, k);
            }
            p32(&mut o, g.level);
            put_text(&mut o, &g.name, MACHINE_NAME);
            put_text(&mut o, &g.title, MACHINE_NAME);
        }
        for e in &self.effects {
            p32(&mut o, KIND_EFFECT);
            p32(&mut o, LEN_EFFECT as u32);
            for v in [e.fx, e.mix, e.knobs.len() as u32, e.id] {
                p32(&mut o, v);
            }
            put_text(&mut o, &e.tab, EFFECT_TAB);
            put_text(&mut o, &e.title, EFFECT_TITLE);
            for j in 0..EFFECT_KNOBS {
                match e.knobs.get(j) {
                    Some(k) => {
                        for v in [k.position, k.display, k.min, k.max, k.default] {
                            p32(&mut o, v);
                        }
                        put_text(&mut o, &k.name, EFFECT_NAME);
                        put_text(&mut o, &k.short, EFFECT_SHORT);
                    }
                    None => o.extend_from_slice(&[0u8; 20 + EFFECT_NAME + EFFECT_SHORT]),
                }
            }
        }
        o
    }

    pub fn parse(b: &[u8]) -> Result<Register> {
        let g = |off: usize| -> Result<u32> {
            b.get(off..off + 4)
                .map(|x| u32::from_be_bytes(x.try_into().unwrap()))
                .ok_or_else(|| err!("register section: read past the end"))
        };
        if g(0)? != MAGIC || g(4)? != VERSION {
            return Err(err!("register section: magic or version"));
        }
        let mut reg = Register {
            device: g(8)?,
            ..Default::default()
        };
        let n = g(12)? as usize;
        let mut at = HEADER;
        let txt = |off: usize, len: usize| -> Result<String> {
            let f = b
                .get(off..off + len)
                .ok_or_else(|| err!("register section: text past the end"))?;
            let l = f.iter().position(|c| *c == 0).unwrap_or(len);
            if f[l..].iter().any(|c| *c != 0) || l == len {
                return Err(err!("register section: text field not NUL-terminated"));
            }
            String::from_utf8(f[..l].to_vec()).map_err(|_| err!("register section: text"))
        };
        for _ in 0..n {
            let (kind, len) = (g(at)?, g(at + 4)? as usize);
            let rec = at + 8;
            match (kind, len) {
                (KIND_PARAM, LEN_PARAM) => reg.params.push(Param {
                    index: g(rec)?,
                    template: g(rec + 4)?,
                    slot: g(rec + 8)?,
                    min: g(rec + 12)? as i32,
                    max: g(rec + 16)? as i32,
                    default: g(rec + 20)? as i32,
                    name: txt(rec + 24, PARAM_NAME)?,
                    short: txt(rec + 24 + PARAM_NAME, PARAM_SHORT)?,
                }),
                (KIND_ACTION, LEN_ACTION) => reg.actions.push((g(rec)?, g(rec + 4)?)),
                (KIND_MACHINE, LEN_MACHINE) => {
                    let mut knobs = [0u32; KNOBS];
                    for (j, k) in knobs.iter_mut().enumerate() {
                        *k = g(rec + 20 + 4 * j)?;
                    }
                    reg.machines.push(Machine {
                        type_no: g(rec)?,
                        clone_of: g(rec + 4)?,
                        position: g(rec + 8)?,
                        own_dsp: g(rec + 12)?,
                        level: g(rec + 16)?,
                        knobs,
                        name: txt(rec + 52, MACHINE_NAME)?,
                        short: txt(rec + 52 + MACHINE_NAME, MACHINE_NAME)?,
                    })
                }
                (KIND_PAGE, LEN_PAGE) => {
                    let mut knobs = [0u32; KNOBS];
                    for (j, k) in knobs.iter_mut().enumerate() {
                        *k = g(rec + 4 + 4 * j)?;
                    }
                    reg.pages.push(Page {
                        type_no: g(rec)?,
                        knobs,
                        level: g(rec + 36)?,
                        name: txt(rec + 40, MACHINE_NAME)?,
                        title: txt(rec + 40 + MACHINE_NAME, MACHINE_NAME)?,
                    })
                }
                (KIND_EFFECT, LEN_EFFECT) => {
                    let n = g(rec + 8)? as usize;
                    if n > EFFECT_KNOBS {
                        return Err(err!("register section: effect record knob count"));
                    }
                    let mut knobs = vec![];
                    for j in 0..n {
                        let k = rec
                            + 16
                            + EFFECT_TAB
                            + EFFECT_TITLE
                            + j * (20 + EFFECT_NAME + EFFECT_SHORT);
                        knobs.push(EffectKnob {
                            position: g(k)?,
                            display: g(k + 4)?,
                            min: g(k + 8)?,
                            max: g(k + 12)?,
                            default: g(k + 16)?,
                            name: txt(k + 20, EFFECT_NAME)?,
                            short: txt(k + 20 + EFFECT_NAME, EFFECT_SHORT)?,
                        });
                    }
                    reg.effects.push(EffectPage {
                        fx: g(rec)?,
                        id: g(rec + 12)?,
                        mix: g(rec + 4)?,
                        tab: txt(rec + 16, EFFECT_TAB)?,
                        title: txt(rec + 16 + EFFECT_TAB, EFFECT_TITLE)?,
                        knobs,
                    })
                }
                _ => return Err(err!("register section: record kind {kind} length {len}")),
            }
            at += len;
        }
        if at != b.len() {
            return Err(err!(
                "register section: {} bytes after the records",
                b.len() - at
            ));
        }
        reg.check()?;
        Ok(reg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Register {
        Register {
            device: 1,
            params: vec![Param {
                index: 0,
                template: 0xd1,
                slot: 1,
                min: 0,
                max: 0x7f00,
                default: 0x4000,
                name: "Decay".into(),
                short: "DECAY".into(),
            }],
            actions: vec![],
            machines: vec![Machine {
                type_no: 16,
                clone_of: 6,
                position: POS_END,
                own_dsp: 0,
                level: 10,
                knobs: [0xf8, KNOB_PARAM, 0xfa, 0xfb, 0xfc, 0xfd, 0, 0xfe],
                name: "XSlice-D".into(),
                short: "XSD".into(),
            }],
            pages: vec![],
            effects: vec![],
        }
    }

    fn with_page() -> Register {
        let mut r = sample();
        r.device = 2;
        r.machines[0].own_dsp = 1;
        r.pages.push(Page {
            type_no: 16,
            knobs: [KNOB_PARAM, 0, 0, 0, 0, 0, 0, 0],
            level: 10,
            name: "SYN".into(),
            title: "Syn".into(),
        });
        r
    }

    #[test]
    fn the_machine_key_is_the_fnv_hash_of_the_global_id() {
        // low 24 bits of FNV-1a 32 of "<module id>:<machine id>" (docs/design/module-sdk.md 11.5 D7); the runtime and the test tools compute the same
        assert_eq!(machine_hash("a", "b"), 0xbd8540);
        assert_eq!(machine_hash("test/sine", "sine"), 0x44feae);
        assert_eq!(machine_key("test/sine", "sine"), 0x44fe_ae00);
        assert_eq!(fmt_type(machine_key("a", "b")), "hbd8540");
        assert_eq!(fmt_type(18), "18");
    }

    #[test]
    fn encode_parse_round_trip() {
        let r = sample();
        let b = r.encode();
        assert_eq!(b.len(), HEADER + LEN_PARAM + LEN_MACHINE);
        assert_eq!(Register::parse(&b).unwrap(), r);
    }

    #[test]
    fn page_round_trip() {
        let r = with_page();
        let b = r.encode();
        assert_eq!(b.len(), HEADER + LEN_PARAM + LEN_MACHINE + LEN_PAGE);
        assert_eq!(Register::parse(&b).unwrap(), r);
        let mut bad = with_page();
        bad.pages[0].type_no = 17;
        assert!(bad.check().unwrap_err().0.contains("no machine of type"));
        let mut bad = with_page();
        bad.pages[0].knobs[2] = 0xd5;
        assert!(bad.check().is_err());
    }

    fn effect() -> EffectPage {
        let f = |x: f32| x.to_bits();
        EffectPage {
            fx: 0,
            id: effect_id("m", "gain"),
            mix: 127,
            tab: "GAN".into(),
            title: "Test Gain".into(),
            knobs: vec![
                EffectKnob {
                    position: 0,
                    display: 0,
                    min: f(0.0),
                    max: f(2.0),
                    default: f(1.0),
                    name: "Gain".into(),
                    short: "GAIN".into(),
                },
                EffectKnob {
                    position: 5,
                    display: 1,
                    min: f(-1.0),
                    max: f(1.0),
                    default: f(0.0),
                    name: "Tilt".into(),
                    short: "TILT".into(),
                },
            ],
        }
    }

    #[test]
    fn an_effect_page_round_trips_and_has_its_length() {
        let mut r = sample();
        r.effects.push(effect());
        let b = r.encode();
        assert_eq!(b.len(), HEADER + LEN_PARAM + LEN_MACHINE + LEN_EFFECT);
        assert_eq!(Register::parse(&b).unwrap(), r);
        // the manifest's view of it
        let d = crate::typed::FxDef {
            id: "gain".into(),
            process: "p".into(),
            desc: "d".into(),
            defaults: vec![],
            devices: vec![],
            tab: Some("GAN".into()),
            title: Some("Test Gain".into()),
            mix: 127,
            knob: vec![crate::typed::FxKnob {
                name: "Gain".into(),
                short: "GAIN".into(),
                min: 0.0,
                max: 2.0,
                default: 1.0,
                display: None,
                position: None,
            }],
            route: None,
            routes: None,
        };
        let p = effect_page("m", 0, &d).unwrap().unwrap();
        assert_eq!(p.knobs[0], effect().knobs[0]);
        assert_eq!(p.id, effect().id);
        let mut none = d.clone();
        none.tab = None;
        assert!(effect_page("m", 0, &none).is_err()); // a title and knobs without a tab
        none.title = None;
        none.knob.clear();
        assert!(effect_page("m", 0, &none).unwrap().is_none());
    }

    fn fx_with(route: Option<&str>, routes: Option<&[&str]>) -> crate::typed::FxDef {
        crate::typed::FxDef {
            id: "gain".into(),
            process: "p".into(),
            desc: "d".into(),
            defaults: vec![],
            devices: vec![],
            tab: None,
            title: None,
            mix: 127,
            knob: vec![],
            route: route.map(String::from),
            routes: routes.map(|l| l.iter().map(|s| s.to_string()).collect()),
        }
    }

    #[test]
    fn the_routes_of_an_effect_follow_the_manifest_rule_and_bad_ones_are_refused() {
        use crate::dkmdsp::{ROUTE_MASTER as M, ROUTE_SEND as S};
        let r = |route, routes: Option<&[&str]>| {
            let mut d = fx_with(route, routes);
            d.tab = Some("GAN".into());
            fx_routes("m", &d)
        };
        // nothing said: a master effect that cannot move
        assert_eq!(r(None, None).unwrap(), (M, M));
        assert_eq!(r(Some("master"), None).unwrap(), (M, M));
        // a send default without `routes` may fall back to the master
        assert_eq!(r(Some("send"), None).unwrap(), (S | M, S));
        // explicit lists, in any order
        assert_eq!(r(None, Some(&["send", "master"])).unwrap(), (S | M, M));
        assert_eq!(
            r(Some("send"), Some(&["master", "send"])).unwrap(),
            (S | M, S)
        );
        assert_eq!(r(Some("send"), Some(&["send"])).unwrap(), (S, S));
        assert_eq!(r(None, Some(&["master"])).unwrap(), (M, M));
        // refused: unknown names, an empty list, a repeated route, a default that is not in the list
        let e = |route, routes: Option<&[&str]>| r(route, routes).unwrap_err().0;
        assert!(e(Some("aux"), None).contains("route \"aux\" is master or send"));
        assert!(e(None, Some(&["master", "bus"])).contains("routes entry \"bus\""));
        assert!(e(None, Some(&[])).contains("routes is empty"));
        assert!(e(None, Some(&["send", "send"])).contains("twice"));
        assert!(e(None, Some(&["send"])).contains("route \"master\" is not in routes"));
        assert!(e(Some("send"), Some(&["master"])).contains("route \"send\" is not in routes"));
        // an effect without a page is an insert: a send route needs the tab that labels it
        let no_tab = |route, routes: Option<&[&str]>| fx_routes("m", &fx_with(route, routes));
        assert_eq!(no_tab(None, None).unwrap(), (M, M));
        assert!(
            no_tab(Some("send"), None)
                .unwrap_err()
                .0
                .contains("needs a `tab`")
        );
        assert!(
            no_tab(None, Some(&["master", "send"]))
                .unwrap_err()
                .0
                .contains("needs a `tab`")
        );
        // the TOML form of an effect takes the two keys and no other spelling
        let m: crate::typed::FxDef = toml::from_str(
            "id = \"g\"\nprocess = \"p\"\ndesc = \"d\"\ntab = \"GAN\"\ntitle = \"T\"\nroute = \"send\"\nroutes = [\"send\"]\n",
        )
        .unwrap();
        assert_eq!(fx_routes("t/x", &m).unwrap(), (S, S));
        assert!(
            toml::from_str::<crate::typed::FxDef>(
                "id = \"g\"\nprocess = \"p\"\ndesc = \"d\"\nroutez = \"send\"\n"
            )
            .is_err()
        );
    }

    #[test]
    fn an_effect_id_is_16_bits_never_0_and_follows_the_module_and_the_effect() {
        let a = effect_id("test/dyn-fx-gain", "gain");
        assert!((1..=0xffff).contains(&a));
        assert_eq!(a, effect_id("test/dyn-fx-gain", "gain"));
        assert_ne!(a, effect_id("test/dyn-fx-gain", "gain2"));
        assert_ne!(a, effect_id("test/other", "gain"));
        // pinned: the ID is stored in kits, so its definition must not drift
        assert_eq!(effect_id("m", "gain"), 0xbb58);
        assert_eq!(effect_id("test/dyn-fx-gain", "gain"), 0x4625);
    }

    #[test]
    fn effect_page_mistakes_are_refused() {
        let ok = |f: &dyn Fn(&mut EffectPage)| {
            let mut e = effect();
            f(&mut e);
            e.check().is_err()
        };
        assert!(ok(&|e| e.tab = "GAIN".into()));
        assert!(ok(&|e| e.tab = "".into()));
        assert!(ok(&|e| e.knobs[0].short = "TOOLONG".into()));
        assert!(ok(&|e| e.knobs[1].position = 0));
        assert!(ok(&|e| e.knobs[1].position = 8));
        assert!(ok(&|e| e.knobs[0].default = 3.0f32.to_bits()));
        assert!(ok(&|e| e.knobs[0].min = e.knobs[0].max));
        assert!(ok(&|e| e.mix = 128));
        assert!(ok(&|e| e.id = 0));
        assert!(ok(&|e| e.id = 0x1_0000));
        let big: Vec<_> = (0..8).map(|_| effect().knobs[0].clone()).collect();
        assert!(ok(&|e| e.knobs = big.clone()));
    }

    #[test]
    fn click_actions_round_trip_and_are_checked() {
        let mut r = sample();
        r.actions = vec![(0, 0x1c4)];
        r.check().unwrap();
        let b = r.encode();
        assert_eq!(Register::parse(&b).unwrap(), r);
        // the record is 0x10 bytes: kind, length, param, offset
        let at = b.len() - 0x10 - (LEN_MACHINE);
        assert_eq!(&b[at..at + 16], &[0, 0, 0, 5, 0, 0, 0, 0x10, 0, 0, 0, 0, 0, 0, 1, 0xc4]);
        // a param the file has not, two actions on one param, no offset
        for bad in [vec![(1, 4)], vec![(0, 4), (0, 8)], vec![(0, u32::MAX)]] {
            let mut r = sample();
            r.actions = bad;
            assert!(r.check().is_err());
        }
    }

    #[test]
    fn mistakes_are_refused() {
        let mut r = sample();
        r.machines[0].knobs[1] = KNOB_PARAM | 3;
        assert!(r.check().unwrap_err().0.contains("missing param"));
        let mut r = sample();
        r.machines[0].name = "x".repeat(24);
        assert!(r.check().is_err());
        let mut b = sample().encode();
        b.push(0);
        assert!(Register::parse(&b).is_err());
        let mut b = sample().encode();
        b[0] ^= 1;
        assert!(Register::parse(&b).is_err());
    }
}
