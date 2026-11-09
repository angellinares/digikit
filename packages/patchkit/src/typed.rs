//! Typed module sections and their lowering.
//!
//! A machine module declares what it adds in C, not in the manifest: `DK_MACHINE`, `DK_PARAM` and `DK_PAGE` records
//! (sdk/include/dk/machine.h), read by `desc` when the module loads. The values here (`MachineDef`, `ParamDef`, `PageDef`,
//! `VoiceDef`) are what `desc` makes of them; `Manifest` holds them in its `machine`, `param`, `page` and `voice` lists.
//!
//! A run-time effect (`FxDef`) is a `DK_FX` record in C (sdk/include/dk/fx.h), read by `desc`.
//!
//! `lower` turns them, per device, into the `[[entry]]` rows the frameworks (`core/machines`, `core/params`,
//! `core/dsp-voice`) take. What a knob means on a device is device data: `[modules.knobs]` and `[modules.frame]` in
//! `devices/<device>.toml` ([`DeviceModel`]). Nothing here names a device. The same resolved view feeds the
//! generator (`modgen.rs`: gallery JSON).

use crate::error::*;
use crate::module::{EntryDef, Manifest};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ---- device data ---------------------------------------------------------------------------------------------

/// `[modules.knobs]` of a device file: where a machine's knobs go.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnobModel {
    /// Knobs a machine can have (positions 0 ..).
    pub count: usize,
    /// Engine or sound-array slot of knob 0.
    pub slot_base: i64,
    /// `machine`: the knobs are fields of the machine row. `page`: they are fields of a page row (`[[page]]`).
    pub to: String,
    /// The machine type every run-time machine of the device clones.
    pub clone_of: i64,
    /// List position of a patched-in machine row (none: the device lists a row without one).
    #[serde(default)]
    pub list_position: Option<i64>,
    /// Field prefix of knob `i` (`f` gives `f0`, `f1` ...).
    pub knob_field: String,
    /// `page` only: field prefix of the slot of knob `i`.
    #[serde(default)]
    pub slot_field: Option<String>,
    /// Field that holds the level code, and its default.
    pub level_field: String,
    pub level_default: i64,
    /// May a machine render with its own DSP routine (`own_dsp`)?
    #[serde(default)]
    pub own_dsp: bool,
    /// `machine` devices: stock code of the tune row, written to knob `tune_slot` of `[adapter.machine]` (none: no tune knob).
    #[serde(default)]
    pub tune: Option<i64>,
    /// `machine` devices: stock code of the level row, written to knob `level_knob` (none: no level knob).
    #[serde(default)]
    pub level: Option<i64>,
    #[serde(default)]
    pub level_knob: Option<usize>,
    /// Knobs the firmware keeps for itself on a clone of a base machine (`[[modules.knobs.reserved]]`).
    #[serde(default)]
    pub reserved: Vec<ReservedKnobs>,
}

/// The slots of `[adapter.machine]` that place a machine's parameters (the rest of that section is read by `devgen`).
#[derive(Clone, Debug, Default, Deserialize)]
struct AdapterSlots {
    tune_slot: Option<usize>,
    #[serde(default)]
    param_slots: Vec<usize>,
}

#[derive(Deserialize)]
struct AdapterDoc {
    machine: Option<AdapterSlots>,
}

/// Knobs of a base machine that a module parameter must not use: a clone of machine `clone_of` (named `name` in messages) keeps them.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReservedKnobs {
    pub clone_of: i64,
    pub name: String,
    pub knobs: Vec<usize>,
    /// Why, for the message.
    pub why: String,
}

/// `[modules.frame]`: where the DSP finds a knob's word in its copy of the ColdFire frame.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrameModel {
    /// Halfword index of knob 0 of the first track (voice).
    pub base: i64,
    /// Halfwords from one track (voice) to the next.
    pub stride: i64,
    /// A gap of `gap` halfwords follows every `group` knobs (0: no gap).
    #[serde(default)]
    pub group: i64,
    #[serde(default)]
    pub gap: i64,
    /// Index the parameter table gives knob 0 (the "mirror" index); knob `j` is `mirror_base + offset(j)`.
    pub mirror_base: i64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModulesSection {
    pub knobs: KnobModel,
    pub frame: FrameModel,
    /// `[modules.templates]`: stock parameter rows a module parameter copies by name.
    #[serde(default)]
    pub templates: BTreeMap<String, i64>,
}

#[derive(Clone, Debug)]
pub struct DeviceModel {
    pub short: String,
    pub knobs: KnobModel,
    pub frame: FrameModel,
    pub templates: BTreeMap<String, i64>,
    /// Knob position of the stock tune (`[adapter.machine] tune_slot`).
    pub tune_slot: Option<usize>,
    /// Knob positions of a machine's module parameters, in order (`[adapter.machine] param_slots`).
    pub param_slots: Vec<usize>,
}

impl DeviceModel {
    /// Halfword offset of knob `j` from knob 0 of the same track.
    pub fn frame_offset(&self, j: usize) -> i64 {
        let j = j as i64;
        let f = &self.frame;
        if f.group > 0 {
            j + (j / f.group) * f.gap
        } else {
            j
        }
    }
}

#[derive(Deserialize)]
struct DeviceFile {
    device: Option<DeviceHead>,
    modules: Option<ModulesSection>,
    adapter: Option<AdapterDoc>,
}
#[derive(Deserialize)]
struct DeviceHead {
    short: String,
}

/// The device models found in the device files (`devices/*.toml`); a file without `[modules]` is skipped.
#[derive(Clone, Debug, Default)]
pub struct Models {
    pub by_device: BTreeMap<String, DeviceModel>,
}

impl Models {
    pub fn from_texts<'a>(texts: impl IntoIterator<Item = &'a str>) -> Result<Models> {
        let mut by_device = BTreeMap::new();
        for t in texts {
            let f: DeviceFile = toml::from_str(t)?;
            let (Some(d), Some(m)) = (f.device, f.modules) else {
                continue;
            };
            let slots = f.adapter.and_then(|a| a.machine).unwrap_or_default();
            if by_device
                .insert(
                    d.short.clone(),
                    DeviceModel {
                        short: d.short.clone(),
                        knobs: m.knobs,
                        frame: m.frame,
                        templates: m.templates,
                        tune_slot: slots.tune_slot,
                        param_slots: slots.param_slots,
                    },
                )
                .is_some()
            {
                return Err(err!("two device files describe [modules] of {}", d.short));
            }
        }
        Ok(Models { by_device })
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn from_dir(dir: &std::path::Path) -> Result<Models> {
        let mut texts = vec![];
        if let Ok(rd) = std::fs::read_dir(dir) {
            let mut paths: Vec<_> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
            paths.sort();
            for p in paths {
                if p.extension().is_some_and(|e| e == "toml") {
                    texts.push(
                        std::fs::read_to_string(&p).map_err(|e| err!("{}: {e}", p.display()))?,
                    );
                }
            }
        }
        Models::from_texts(texts.iter().map(|s| s.as_str()))
    }
}

// ---- the typed sections --------------------------------------------------------------------------------------

/// One knob of a machine on a device: a stock parameter code (`0` = none) or a parameter of this module (its id).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Knob {
    Code(i64),
    Param(String),
}

/// A machine (a `DK_MACHINE` record).
#[derive(Clone, Debug, Serialize)]
pub struct MachineDef {
    pub id: String,
    pub name: String,
    pub short: String,
    /// Declared stereo (`.stereo = 1`, machine contract 1.2).
    pub stereo: bool,
}

/// A parameter of a machine (a `DK_PARAM` record). `min`, `max` and `default` are raw: the knob unit times 256.
#[derive(Clone, Debug, Serialize)]
pub struct ParamDef {
    pub id: String,
    /// The id of the machine.
    pub machine: String,
    /// The place in the machine's parameters, from 0 without gaps.
    pub word: u32,
    pub name: String,
    pub short: String,
    pub min: i64,
    pub max: i64,
    pub default: i64,
    /// The symbol of the click callback (a `DK_PARAM_ACTION` record), `void fn(u32 track)`.
    pub action: Option<String>,
    /// The glyph (a `DK_PARAM_GLYPH` record, `dk_glyph`); `None`: the encoder.
    pub glyph: Option<u32>,
    /// The adapter snapshots this word at actual note_on (machine contract 1.3).
    pub latched: bool,
}

/// The template name and the allowed knob range (min, max) of a `dk_glyph` value.
fn glyph_info(glyph: Option<u32>) -> Option<(&'static str, i64, i64)> {
    Some(match glyph.unwrap_or(0) {
        0 => ("encoder", 0, 127),
        1 => ("fader", 0, 127),
        2 => ("pan", 0, 127),
        3 => ("onoff", 0, 1),
        4 => ("tune", 4, 124),
        5 => ("play", 0, 3),
        _ => return None,
    })
}

/// One encoder of a machine page (a `DK_KNOB` or `DK_KNOB_AT` entry of a `DK_PAGE` record).
#[derive(Clone, Debug, Serialize)]
pub struct PageKnob {
    /// The parameter word of the machine.
    pub word: u32,
    /// The encoder 0 to 7; `None`: the lowest free one.
    pub position: Option<u32>,
}

/// A page of a machine (a `DK_PAGE` record). `id` is made up from the machine id and the place among its pages.
#[derive(Clone, Debug, Serialize)]
pub struct PageDef {
    pub id: String,
    pub machine: String,
    pub name: String,
    pub title: String,
    pub knobs: Vec<PageKnob>,
}

#[derive(Clone, Debug, Serialize)]
pub struct NativeBinding {
    pub kind: u32,
    pub value: u32,
    pub position: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct NativePageDef {
    pub version: u32,
    pub page: u32,
    pub flags: u32,
    pub owner: String,
    pub title: String,
    pub draw: String,
    pub event: String,
    pub bindings: Vec<NativeBinding>,
}

/// The DSP routines of a machine (from its `DK_MACHINE` record): `render` and `note_on` are the symbols of the routines, `desc` the
/// `dk_machine_desc` data symbol: voices, state and clear bytes come from it.
#[derive(Clone, Debug, Serialize)]
pub struct VoiceDef {
    pub machine: String,
    pub render: String,
    pub note_on: String,
    pub desc: String,
    /// Declared stereo; the packer checks it against `DK_MACHINE_STEREO` in the `dk_machine_desc`.
    pub stereo: bool,
}

/// A run-time effect (a `DK_FX` record, DSP section version 4 of a `.dkm`; docs/design/digikit-dkm.md): `process` is the global text symbol of its process routine
/// (<dk/fx.h>), `desc` the data symbol of its `dk_fx_desc` (n_params and state_bytes come from it), `defaults` the words p0 ... of its row (at most
/// `n_params`, the words the DSP reads as they are; none: all 0). Only the packer reads it: a patched-in effect is an `[[entry]]` row of the registry `fx`.
///
/// An effect with a `tab` has a page of its own in the FX view (docs/design/digikit-registry.md "Effect pages"): `tab` is the label of the page's tab
/// (1 to 3 characters), `title` the page title (up to 23), `mix` the default of the MIX knob on the level slot (0 to 127; the adapter blends dry and wet, the
/// effect writes only the wet signal), `route` and `routes` the default route and the allowed ones (`"master"`, `"send"`) and `knob` the knobs, up to 7: knob i is parameter word p_i, shown at `position` of the 8 knob positions of the page
/// (default: i). A knob's value reaches the effect as a float in [`min`, `max`]; its default fills p_i of the row (so `defaults` names no word a knob has).
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FxDef {
    pub id: String,
    pub process: String,
    pub desc: String,
    #[serde(default)]
    pub defaults: Vec<u32>,
    #[serde(default)]
    pub devices: Vec<String>,
    #[serde(default)]
    pub tab: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    /// The MIX knob's default, 0 to 127.
    #[serde(default = "default_mix")]
    pub mix: i64,
    #[serde(default)]
    pub knob: Vec<FxKnob>,
    /// Where the effect runs when the settings do not say otherwise: `"master"` (the default) or `"send"` (a send bus: one amount for each track).
    #[serde(default)]
    pub route: Option<String>,
    /// The routes the user may choose in DIGIKIT > FX ROUTES, a non-empty list of `"master"` and `"send"`. Left out: `["master"]`, or `["send", "master"]`
    /// when `route = "send"` (see `register::fx_routes`).
    #[serde(default)]
    pub routes: Option<Vec<String>>,
}

fn default_mix() -> i64 {
    127
}

/// One knob of an effect page (a `DK_FX_PARAM` record of a `DK_FX`).
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FxKnob {
    /// The long name shown while the knob is turned (1 to 23 characters).
    pub name: String,
    /// The label under the knob (1 to 6 characters).
    pub short: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    /// `percent` (0 .. 127, the default) or `bipolar` (-64 .. +63).
    #[serde(default)]
    pub display: Option<String>,
    /// The knob position on the page, 0 to 7 (default: the knob's place in the list).
    #[serde(default)]
    pub position: Option<u32>,
}

pub fn has_typed(m: &Manifest) -> bool {
    !(m.machine.is_empty() && m.param.is_empty() && m.page.is_empty() && m.voice.is_empty())
}

// ---- resolution ----------------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct RMachine {
    pub id: String,
    pub name: String,
    pub short: String,
    pub clone_of: i64,
    /// List position of a patched-in row (`[modules.knobs] list_position`).
    pub position: Option<i64>,
    pub own_dsp: bool,
    pub knobs: Vec<Knob>,
    pub level: i64,
}

#[derive(Clone, Debug)]
pub struct RParam {
    pub id: String,
    pub name: String,
    pub short: String,
    pub template: i64,
    pub min: i64,
    pub max: i64,
    pub default: i64,
    /// Knob position, slot (`slot_base + knob`), halfword offset in the frame, parameter-table mirror index.
    pub knob: usize,
    pub slot: i64,
    pub frame_off: i64,
    pub mirror: i64,
    /// The symbol of the click callback of the knob.
    pub action: Option<String>,
    pub latched_word: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct RPage {
    pub id: String,
    pub machine: String,
    pub name: String,
    pub title: String,
    pub knobs: Vec<Knob>,
    pub level: i64,
}

#[derive(Clone, Debug)]
pub struct RVoice {
    pub machine: String,
    /// The render routine's symbol.
    pub entry: String,
    pub note_on: Option<String>,
    pub desc: Option<String>,
    pub stereo: bool,
    pub latched: u32,
    /// A patched-in contract voice: the state block `Library::lower` reserved for it (`desc` read from the object).
    pub state: Option<VoiceState>,
}

/// The state of a patched-in contract voice (`dk_machine_desc`): the global bss symbol that `Library::lower` added to the module's DSP object
/// (`voices` x `stride` bytes), the bytes between two voices' states and the bytes the adapter zeroes at note-on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VoiceState {
    pub symbol: String,
    pub stride: u32,
    pub clear: u32,
}

/// The typed sections of one module as one device sees them.
#[derive(Clone, Debug)]
pub struct DevResolved {
    pub dev: String,
    pub model: DeviceModel,
    pub machines: Vec<RMachine>,
    pub params: Vec<RParam>,
    pub pages: Vec<RPage>,
    pub voices: Vec<RVoice>,
    pub native_pages: Vec<NativePageDef>,
}

/// Where the parameters of one machine sit on a device: `pos[i]` is the knob position of parameter `i` (of the machine's list);
/// `pages` (page devices only) are the pages with their (parameter, position) entries.
struct Placed {
    pos: Vec<usize>,
    pages: Vec<(String, String, Vec<(usize, usize)>)>,
}

/// Place the parameters of a machine on a device: word w sits at knob position `param_slots[w]`, where the DSP adapter reads it. For now a
/// machine has at most `param_slots.len()` parameters, one page that lists the words in order, and no explicit positions. A page device
/// puts the same positions on its page.
fn place(
    who: &str,
    what: &str,
    dev: &str,
    model: &DeviceModel,
    machine: &MachineDef,
    mine: &[&ParamDef],
    pages: &[&PageDef],
) -> Result<Placed> {
    let kn = &model.knobs;
    // the entries of each page as (parameter index, position)
    let mut entries: Vec<Vec<(usize, Option<usize>)>> = vec![];
    let mut on_page = vec![false; mine.len()];
    for pg in pages {
        let mut list = vec![];
        for e in &pg.knobs {
            let Some(i) = mine.iter().position(|p| p.word == e.word) else {
                return Err(err!(
                    "{who}: {what}: page {}: knob names word {}, but the machine has no such parameter",
                    pg.name,
                    e.word
                ));
            };
            if on_page[i] {
                return Err(err!(
                    "{who}: {what}: parameter word {} is on a page twice",
                    e.word
                ));
            }
            on_page[i] = true;
            list.push((i, e.position.map(|p| p as usize)));
        }
        entries.push(list);
    }
    // the DSP adapter passes word w from knob position `param_slots[w]`, and the runtime of a page device writes page knob j to slot
    // `slot_base + j` for every page: until the adapter has per-machine maps, a machine has one page and its words sit at the adapter positions
    let free = &model.param_slots;
    if let Some(&s) = free.iter().find(|&&s| s >= kn.count) {
        return Err(err!(
            "{who}: {what}: param_slots of {dev} has knob {s}, but {dev} has {}",
            kn.count
        ));
    }
    if mine.len() > free.len() {
        return Err(err!(
            "{who}: {what}: {} parameters, but {dev} has only {} free knob positions {free:?} (param_slots)",
            mine.len(),
            free.len()
        ));
    }
    if pages.len() > 1 {
        return Err(err!("{who}: {what}: a machine has one page for now"));
    }
    for (k, (i, p)) in entries.iter().flatten().enumerate() {
        if p.is_some() {
            return Err(err!(
                "{who}: {what}: a machine knob has its adapter position for now (DK_KNOB_AT)"
            ));
        }
        if *i != k {
            return Err(err!(
                "{who}: {what}: machine knobs follow the DSP word order for now"
            ));
        }
    }
    if mine.iter().enumerate().any(|(i, q)| q.word as usize != i) {
        return Err(err!(
            "{who}: {what}: machine knobs follow the DSP word order for now (words must be 0 to {})",
            mine.len().saturating_sub(1)
        ));
    }
    let pos: Vec<usize> = free[..mine.len()].to_vec();
    if kn.to == "machine" {
        return Ok(Placed { pos, pages: vec![] });
    }
    // a page device: a machine without a page gets one named after it
    let (name, title) = match pages.first() {
        Some(p) => (p.name.clone(), p.title.clone()),
        None => (machine.name.to_ascii_uppercase(), machine.name.clone()),
    };
    let mut placed: Vec<(usize, usize)> = pos.iter().copied().enumerate().collect();
    placed.sort_by_key(|&(_, p)| p);
    Ok(Placed {
        pos,
        pages: vec![(name, title, placed)],
    })
}

/// Resolve the typed sections against the device models: one [`DevResolved`] per device that has something.
pub fn resolve(m: &Manifest, models: &Models) -> Result<Vec<DevResolved>> {
    if !m.native_page.is_empty() && !m.module.dynamic {
        return Err(err!("native callback pages require a run-time machine"));
    }
    let who = m.module.id.as_str();
    let mut out = vec![];
    if m.machine.is_empty() {
        return Ok(out);
    }
    for (dev, model) in &models.by_device {
        let kn = &model.knobs;
        if kn.to != "machine" && kn.to != "page" {
            return Err(err!(
                "devices/{dev}.toml: [modules.knobs] to = {:?} is neither machine nor page",
                kn.to
            ));
        }
        let mut machines = vec![];
        let mut params = vec![];
        let mut pages = vec![];
        for d in &m.machine {
            let what = format!("machine {}", d.id);
            let mine: Vec<&ParamDef> = m.param.iter().filter(|p| p.machine == d.id).collect();
            let mpages: Vec<&PageDef> = m.page.iter().filter(|p| p.machine == d.id).collect();
            let placed = place(who, &what, dev, model, d, &mine, &mpages)?;
            // the machine's knob list: positions to parameters, plus the stock rows of a machine-row device
            let mut knobs = vec![Knob::Code(0); kn.count];
            if kn.to == "machine" {
                if let (Some(code), Some(slot)) = (kn.tune, model.tune_slot)
                    && let Some(k) = knobs.get_mut(slot)
                {
                    *k = Knob::Code(code);
                }
                if let (Some(code), Some(j)) = (kn.level, kn.level_knob)
                    && let Some(k) = knobs.get_mut(j)
                {
                    *k = Knob::Code(code);
                }
            }
            for (p, &at) in mine.iter().zip(&placed.pos) {
                knobs[at] = Knob::Param(p.id.clone());
            }
            if kn.to == "page" {
                while matches!(knobs.last(), Some(Knob::Code(0))) {
                    knobs.pop();
                }
            }
            // knobs the firmware keeps on a clone of the base machine: no module parameter there
            for r in kn.reserved.iter().filter(|r| r.clone_of == kn.clone_of) {
                for &j in &r.knobs {
                    if let Some(Knob::Param(p)) = knobs.get(j) {
                        return Err(err!(
                            "{who}: machine {}: param {p:?} sits at knob {j}, but knob {j} of a {} clone is {} \
                             (docs/findings/02-machines-and-parameters.md)",
                            d.id,
                            r.name,
                            r.why
                        ));
                    }
                }
            }
            let own_dsp = kn.own_dsp && m.voice.iter().any(|v| v.machine == d.id);
            machines.push(RMachine {
                id: d.id.clone(),
                name: d.name.clone(),
                short: d.short.clone(),
                clone_of: kn.clone_of,
                position: kn.list_position,
                own_dsp,
                knobs,
                level: kn.level_default,
            });
            for (p, &at) in mine.iter().zip(&placed.pos) {
                let Some((gname, gmin, gmax)) = glyph_info(p.glyph) else {
                    return Err(err!("{who}: param {}: unknown glyph {:?}", p.id, p.glyph));
                };
                if p.min < gmin * 256 || p.max > gmax * 256 {
                    return Err(err!(
                        "{who}: param {}: glyph {gname} needs knob values {gmin} .. {gmax}, the param has {} .. {}",
                        p.id,
                        p.min as f64 / 256.0,
                        p.max as f64 / 256.0
                    ));
                }
                let template = *model
                    .templates
                    .get(gname)
                    .ok_or_else(|| err!("{who}: devices/{dev}.toml has no `{gname}` template"))?;
                params.push(RParam {
                    id: p.id.clone(),
                    name: p.name.clone(),
                    short: p.short.clone(),
                    template,
                    min: p.min,
                    max: p.max,
                    default: p.default,
                    knob: at,
                    slot: kn.slot_base + at as i64,
                    frame_off: model.frame_offset(at),
                    mirror: model.frame.mirror_base + model.frame_offset(at),
                    action: p.action.clone(),
                    latched_word: p.latched.then_some(p.word),
                });
            }
            for (k, (name, title, list)) in placed.pages.iter().enumerate() {
                let mut pk = vec![Knob::Code(0); kn.count];
                for &(i, at) in list {
                    pk[at] = Knob::Param(mine[i].id.clone());
                }
                while matches!(pk.last(), Some(Knob::Code(0))) {
                    pk.pop();
                }
                pages.push(RPage {
                    id: mpages
                        .get(k)
                        .map_or_else(|| format!("{}_p0", d.id), |p| p.id.clone()),
                    machine: d.id.clone(),
                    name: name.clone(),
                    title: title.clone(),
                    knobs: pk,
                    level: kn.level_default,
                });
            }
        }
        let voices = m
            .voice
            .iter()
            .map(|v| RVoice {
                machine: v.machine.clone(),
                entry: v.render.clone(),
                note_on: Some(v.note_on.clone()),
                desc: Some(v.desc.clone()),
                stereo: v.stereo,
                latched: m
                    .param
                    .iter()
                    .filter(|p| p.machine == v.machine && p.latched)
                    .fold(0, |mask, p| mask | (1 << p.word)),
                state: m.voice_state.get(&v.machine).cloned(),
            })
            .collect();
        let native_pages = m
            .native_page
            .iter()
            .map(|page| {
                let mut page = page.clone();
                for binding in &mut page.bindings {
                    if binding.kind == 1 {
                        let p = m
                            .param
                            .iter()
                            .find(|p| p.machine == page.owner && p.word == binding.value)
                            .ok_or_else(|| {
                                err!("native page parameter word {} is missing", binding.value)
                            })?;
                        binding.value =
                            params.iter().position(|r| r.id == p.id).ok_or_else(|| {
                                err!("native page parameter {} unavailable on {dev}", p.id)
                            })? as u32;
                    }
                }
                Ok(page)
            })
            .collect::<Result<Vec<_>>>()?;
        out.push(DevResolved {
            dev: dev.clone(),
            model: model.clone(),
            machines,
            params,
            pages,
            voices,
            native_pages,
        });
    }
    Ok(out)
}

// ---- lowering ------------------------------------------------------------------------------------------------

fn kv(k: &str, v: toml::Value) -> (String, toml::Value) {
    (k.to_string(), v)
}

fn ent(id: &str, registry: &str, dev: &str, fields: Vec<(String, toml::Value)>) -> EntryDef {
    EntryDef {
        registry: registry.into(),
        id: id.into(),
        devices: vec![dev.into()],
        fields: fields.into_iter().collect(),
    }
}

fn int(i: i64) -> toml::Value {
    toml::Value::Integer(i)
}
fn s(t: &str) -> toml::Value {
    toml::Value::String(t.into())
}

/// The value a knob puts in a row field: a code, or the symbol `params.<id>.index`.
fn knob_value(k: Option<&Knob>) -> toml::Value {
    match k {
        None => int(0),
        Some(Knob::Code(c)) => int(*c),
        Some(Knob::Param(p)) => s(&format!("params.{p}.index")),
    }
}

/// The declared ranges of a machine's eight knobs for the voice row (core/dsp-voice's runtime guard): knob j of the machine is a `Param` whose
/// `min`/`max` become `min | max << 16`; a stock code, an empty knob or a range outside 0 ..= 0xffff stays 0 (undeclared: the guard
/// clamps to 0 .. 0x7f00). Both devices: the adapter of a contract voice clamps the parameter in knob position j (the Digitone II's page knobs
/// are the machine's knob list too) to the range of knob j.
pub fn voice_ranges(r: &DevResolved, machine: &str) -> [u32; 8] {
    let mut out = [0u32; 8];
    let Some(m) = r.machines.iter().find(|m| m.id == machine) else {
        return out;
    };
    for (j, k) in m.knobs.iter().take(8).enumerate() {
        if let Knob::Param(id) = k {
            if let Some(p) = r.params.iter().find(|p| &p.id == id) {
                if (0..=0xffff).contains(&p.min) && (0..=0xffff).contains(&p.max) && p.min <= p.max
                {
                    out[j] = (p.min as u32) | (p.max as u32) << 16;
                }
            }
        }
    }
    out
}

/// The `[[entry]]` rows of the typed sections of one module, in the order the hand-written form had them:
/// voices, machines, params, pages, per device.
pub fn entries(resolved: &[DevResolved]) -> Vec<EntryDef> {
    let mut out = vec![];
    for r in resolved {
        let kn = &r.model.knobs;
        let dev = r.dev.as_str();
        for v in &r.voices {
            let mut f = vec![
                kv("type", s(&format!("machines.{}.index", v.machine))),
                kv("pc", s(&v.entry)),
            ];
            for (j, w) in voice_ranges(r, &v.machine).iter().enumerate() {
                if *w != 0 {
                    f.push(kv(&format!("rng{j}"), int(*w as i64)));
                }
            }
            if let Some(n) = &v.note_on {
                f.push(kv("note_on", s(n)));
            }
            if v.latched != 0 {
                f.push(kv("latched", int(v.latched as i64)));
            }
            if let Some(st) = &v.state {
                f.push(kv("state_base", s(&st.symbol)));
                f.push(kv("stride", int(st.stride as i64)));
                f.push(kv("clear", int(st.clear as i64)));
            }
            out.push(ent(&v.machine, "voices", dev, f));
        }
        for m in &r.machines {
            let mut f = vec![
                kv("name", s(&m.name)),
                kv("short", s(&m.short)),
                kv("clone_of", int(m.clone_of)),
            ];
            if let Some(p) = m.position {
                f.push(kv("position", int(p)));
            }
            if m.own_dsp {
                f.push(kv("own_dsp", int(1)));
            }
            if kn.to == "machine" {
                let names: Vec<String> = (0..kn.count)
                    .map(|i| format!("{}{i}", kn.knob_field))
                    .collect();
                for (i, n) in names.iter().enumerate() {
                    f.push(kv(n, knob_value(m.knobs.get(i))));
                }
                f.push(kv(&kn.level_field, int(m.level)));
            }
            out.push(ent(&m.id, "machines", dev, f));
        }
        for p in &r.params {
            out.push(ent(
                &p.id,
                "params",
                dev,
                vec![
                    kv("name", s(&p.name)),
                    kv("short", s(&p.short)),
                    kv("template", int(p.template)),
                    kv("slot", int(p.slot)),
                    kv("min", int(p.min)),
                    kv("max", int(p.max)),
                    kv("default", int(p.default)),
                ],
            ));
        }
        for g in &r.pages {
            let mut f = vec![
                kv("machine", s(&format!("machines.{}.index", g.machine))),
                kv("name", s(&g.name)),
                kv("title", s(&g.title)),
            ];
            let c: Vec<String> = (0..kn.count)
                .map(|i| format!("{}{i}", kn.knob_field))
                .collect();
            let sl: Vec<String> = (0..kn.count)
                .map(|i| format!("{}{i}", kn.slot_field.as_deref().unwrap_or("s")))
                .collect();
            for (i, k) in g.knobs.iter().enumerate() {
                f.push(kv(&c[i], knob_value(Some(k))));
                if matches!(k, Knob::Param(_)) {
                    f.push(kv(&sl[i], int(kn.slot_base + i as i64)));
                }
            }
            f.push(kv(&kn.level_field, int(g.level)));
            out.push(ent(&g.id, "pages", dev, f));
        }
    }
    out
}

/// Lower the typed sections of a manifest into `[[entry]]` rows (once). A manifest with typed sections that is
/// never lowered is refused when it is applied (`prim.rs`).
pub fn lower(m: &mut Manifest, models: &Models) -> Result<()> {
    if m.lowered || !has_typed(m) {
        return Ok(());
    }
    if m.module.dynamic {
        // loaded at run time: the packer lowers it into `.dkm` records, nothing is patched
        m.lowered = true;
        return Ok(());
    }
    if let Some(p) = m.param.iter().find(|p| p.action.is_some()) {
        return Err(err!(
            "{}: param {} has a click action (DK_PARAM_ACTION); only a run-time module (dynamic = true) can declare one",
            m.module.id,
            p.id
        ));
    }
    if models.by_device.is_empty() {
        return Err(err!(
            "{}: has machine records, but no device file with [modules] is available",
            m.module.id
        ));
    }
    let rows = entries(&resolve(m, models)?);
    m.entry.extend(rows);
    m.lowered = true;
    crate::prim::validate_manifest(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub const DT2: &str = r#"
[device]
short = "dt2"
[modules.knobs]
count = 8
slot_base = 0
to = "machine"
clone_of = 0
knob_field = "f"
level_field = "f8"
level_default = 10
tune = 0xca
level = 0xd1
level_knob = 7
[modules.templates]
encoder = 0xcc
fader = 0xd1
pan = 0x47
onoff = 0x32
tune = 0xca
play = 0xcb
[[modules.knobs.reserved]]
clone_of = 0
name = "SAMPLE"
knobs = [3]
why = "the sample slot: only the sample number bits reach the DSP"
[modules.frame]
base = 0x6d
stride = 48
group = 4
gap = 2
mirror_base = 25
[adapter.machine]
tune_slot = 0
param_slots = [1, 2, 4, 5]
"#;
    pub const DN2: &str = r#"
[device]
short = "dn2"
[modules.knobs]
count = 8
slot_base = 25
to = "page"
clone_of = 0
list_position = 4
knob_field = "c"
slot_field = "s"
level_field = "c8"
level_default = 10
own_dsp = true
[modules.templates]
encoder = 0xd5
fader = 0xd7
pan = 0x47
onoff = 0xdd
tune = 0xee
play = 0xd5
[modules.frame]
base = 0x6d
stride = 0x49
mirror_base = 25
[adapter.machine]
param_slots = [0, 1, 2, 3]
"#;
    const HEAD: &str = r#"
[module]
id = "machines/t"
version = "0.1.0"
name = "T"
category = "machines"
type = "machine"
author = "digikit"
status = "experimental"
summary = "t."
"#;

    fn models() -> Models {
        Models::from_texts([DT2, DN2]).unwrap()
    }
    /// A module with machine `t` of `n` parameters (t_0 ..; range 0 .. 100) and the given pages (entries are (word, position)).
    fn manifest(n: u32, pages: &[&[(u32, Option<u32>)]]) -> Manifest {
        let mut m: Manifest = toml::from_str(HEAD).unwrap();
        m.machine = vec![MachineDef {
            id: "t".into(),
            name: "Tee".into(),
            short: "T".into(),
            stereo: false,
        }];
        m.param = (0..n)
            .map(|w| ParamDef {
                id: format!("t_{w}"),
                machine: "t".into(),
                word: w,
                name: format!("P{w}"),
                short: format!("P{w}"),
                min: 0,
                max: 100,
                default: 50,
                action: None,
                glyph: None,
                latched: false,
            })
            .collect();
        m.page = pages
            .iter()
            .enumerate()
            .map(|(k, es)| PageDef {
                id: format!("t_p{k}"),
                machine: "t".into(),
                name: format!("PG{k}"),
                title: format!("Page {k}"),
                knobs: es
                    .iter()
                    .map(|&(word, position)| PageKnob { word, position })
                    .collect(),
            })
            .collect();
        m.voice = vec![VoiceDef {
            machine: "t".into(),
            render: "_t_render".into(),
            note_on: "_t_note_on".into(),
            desc: "_t_desc".into(),
            stereo: false,
        }];
        m
    }
    fn field<'a>(e: &'a EntryDef, k: &str) -> Option<&'a toml::Value> {
        e.fields.get(k)
    }
    fn p(n: &str) -> Knob {
        Knob::Param(n.into())
    }
    fn knobs_of(m: &Manifest, dev: &str) -> Vec<Knob> {
        let r = resolve(m, &models()).unwrap();
        r.iter().find(|x| x.dev == dev).unwrap().machines[0]
            .knobs
            .clone()
    }
    fn bad(m: &Manifest) -> String {
        resolve(m, &models()).unwrap_err().0
    }

    #[test]
    fn dt2_lowering_puts_knobs_in_the_machine_row() {
        let mut m = manifest(2, &[&[(0, None), (1, None)]]);
        lower(&mut m, &models()).unwrap();
        let on = |dev: &str, reg: &str| -> Vec<&EntryDef> {
            m.entry
                .iter()
                .filter(|e| e.devices == [dev] && e.registry == reg)
                .collect()
        };
        let e = on("dt2", "machines")[0];
        assert_eq!(field(e, "f0"), Some(&int(0xca)));
        assert_eq!(field(e, "f1"), Some(&s("params.t_0.index")));
        assert_eq!(field(e, "f2"), Some(&s("params.t_1.index")));
        assert_eq!(field(e, "f3"), Some(&int(0)));
        assert_eq!(field(e, "f7"), Some(&int(0xd1)));
        assert_eq!(field(e, "f8"), Some(&int(10)));
        assert_eq!(field(e, "clone_of"), Some(&int(0)));
        assert!(field(e, "position").is_none() && field(e, "own_dsp").is_none());
        // slots are the knob positions
        let q = on("dt2", "params");
        assert_eq!(field(q[0], "slot"), Some(&int(1)));
        assert_eq!(field(q[0], "template"), Some(&int(0xcc)));
        assert_eq!(field(q[1], "slot"), Some(&int(2)));
        assert!(on("dt2", "pages").is_empty());
        // the runtime guard's ranges: knobs 1 and 2 are params (0 .. 100 = 100 << 16)
        let v = on("dt2", "voices")[0];
        assert_eq!(field(v, "pc"), Some(&s("_t_render")));
        assert_eq!(field(v, "rng1"), Some(&int(100 << 16)));
        assert!(field(v, "rng0").is_none() && field(v, "rng3").is_none());
        let r = resolve(&m, &models()).unwrap();
        let dt2 = r.iter().find(|x| x.dev == "dt2").unwrap();
        assert_eq!(
            voice_ranges(dt2, "t"),
            [0, 100 << 16, 100 << 16, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn dn2_lowering_puts_knobs_in_a_page_and_slots_after_25() {
        let mut m = manifest(2, &[&[(0, None), (1, None)]]);
        lower(&mut m, &models()).unwrap();
        let on = |reg: &str| -> Vec<&EntryDef> {
            m.entry
                .iter()
                .filter(|e| e.devices == ["dn2"] && e.registry == reg)
                .collect()
        };
        let mach = on("machines");
        assert_eq!(field(mach[0], "own_dsp"), Some(&int(1)));
        assert_eq!(field(mach[0], "position"), Some(&int(4)));
        assert!(field(mach[0], "c0").is_none());
        let q = on("params");
        assert_eq!(field(q[0], "slot"), Some(&int(25)));
        assert_eq!(field(q[1], "template"), Some(&int(0xd5)));
        assert_eq!(field(q[1], "slot"), Some(&int(26)));
        let g = on("pages");
        assert_eq!(field(g[0], "machine"), Some(&s("machines.t.index")));
        assert_eq!(field(g[0], "name"), Some(&s("PG0")));
        assert_eq!(field(g[0], "c0"), Some(&s("params.t_0.index")));
        assert_eq!(field(g[0], "s1"), Some(&int(26)));
        assert_eq!(field(g[0], "c8"), Some(&int(10)));
    }

    #[test]
    fn a_machine_without_a_page_gets_one_named_after_it_on_the_digitone() {
        let m = manifest(2, &[]);
        let r = resolve(&m, &models()).unwrap();
        assert!(r.iter().find(|x| x.dev == "dt2").unwrap().pages.is_empty());
        let dn2 = r.iter().find(|x| x.dev == "dn2").unwrap();
        assert_eq!(dn2.pages.len(), 1);
        assert_eq!(
            (dn2.pages[0].name.as_str(), dn2.pages[0].title.as_str()),
            ("TEE", "Tee")
        );
        assert_eq!(dn2.pages[0].knobs, [p("t_0"), p("t_1")]);
    }

    #[test]
    fn placement_follows_the_device_profile() {
        let m = manifest(4, &[&[(0, None), (1, None), (2, None), (3, None)]]);
        let x = Knob::Code(0);
        assert_eq!(
            knobs_of(&m, "dt2"),
            [
                Knob::Code(0xca),
                p("t_0"),
                p("t_1"),
                x.clone(),
                p("t_2"),
                p("t_3"),
                x,
                Knob::Code(0xd1)
            ]
        );
        assert_eq!(
            knobs_of(&m, "dn2"),
            [p("t_0"), p("t_1"), p("t_2"), p("t_3")]
        );
    }

    #[test]
    fn a_machine_knob_has_its_adapter_position_on_both_devices() {
        let m = manifest(3, &[&[(0, None), (1, None), (2, None)]]);
        let r = resolve(&m, &models()).unwrap();
        let dt2 = r.iter().find(|x| x.dev == "dt2").unwrap();
        assert_eq!(
            dt2.params.iter().map(|q| q.knob).collect::<Vec<_>>(),
            [1, 2, 4]
        );
        let dn2 = r.iter().find(|x| x.dev == "dn2").unwrap();
        assert_eq!(
            dn2.params.iter().map(|q| q.knob).collect::<Vec<_>>(),
            [0, 1, 2]
        );
    }

    #[test]
    fn machine_page_rules_are_refused() {
        // an explicit position
        let e = bad(&manifest(2, &[&[(0, Some(5)), (1, None)]]));
        assert!(e.contains("adapter position"), "{e}");
        // words out of order
        let e = bad(&manifest(2, &[&[(1, None), (0, None)]]));
        assert!(e.contains("DSP word order"), "{e}");
        // two pages
        let e = bad(&manifest(2, &[&[(0, None)], &[(1, None)]]));
        assert!(e.contains("one page for now"), "{e}");
    }

    #[test]
    fn placement_mistakes_are_refused() {
        // more parameters than a device has free positions
        let e = bad(&manifest(5, &[]));
        assert!(
            e.contains("5 parameters") && e.contains("free knob positions"),
            "{e}"
        );
        // a page entry names a word the machine does not have
        let e = bad(&manifest(2, &[&[(0, None), (7, None)]]));
        assert!(e.contains("word 7"), "{e}");
        // a parameter on two pages
        let e = bad(&manifest(2, &[&[(0, None)], &[(0, None), (1, None)]]));
        assert!(e.contains("on a page twice"), "{e}");
    }

    #[test]
    fn frame_offsets_follow_the_device_model() {
        let ms = models();
        let dt2 = &ms.by_device["dt2"];
        let off: Vec<i64> = (0..8).map(|j| dt2.frame_offset(j)).collect();
        assert_eq!(off, [0, 1, 2, 3, 6, 7, 8, 9]);
        let r = resolve(&manifest(4, &[]), &ms).unwrap();
        let q = |dev: &str, id: &str| {
            r.iter()
                .find(|x| x.dev == dev)
                .unwrap()
                .params
                .iter()
                .find(|q| q.id == id)
                .unwrap()
                .clone()
        };
        assert_eq!((q("dt2", "t_2").frame_off, q("dt2", "t_2").mirror), (6, 31));
        assert_eq!((q("dn2", "t_2").frame_off, q("dn2", "t_2").mirror), (2, 27));
    }

    #[test]
    fn own_dsp_follows_the_voice_routine() {
        let own = |m: &Manifest, dev: &str| {
            let r = resolve(m, &models()).unwrap();
            r.iter().find(|x| x.dev == dev).unwrap().machines[0].own_dsp
        };
        let m = manifest(1, &[]);
        assert!(own(&m, "dn2"));
        assert!(!own(&m, "dt2"));
        let mut novoice = m.clone();
        novoice.voice.clear();
        assert!(!own(&novoice, "dn2"));
    }

    #[test]
    fn a_click_action_reaches_the_register_record_of_a_run_time_module_only() {
        let mut m = manifest(3, &[]);
        m.param[1].action = Some("_t_click".into());
        // a patched-in module has no registry rows for it
        let e = lower(&mut m.clone(), &models()).unwrap_err().0;
        assert!(e.contains("click action") && e.contains("t_1"), "{e}");
        // a run-time one: the resolved parameter carries the symbol and the register record the image offset
        let res = resolve(&m, &models()).unwrap();
        for r in &res {
            assert_eq!(
                r.params
                    .iter()
                    .map(|p| p.action.as_deref())
                    .collect::<Vec<_>>(),
                [None, Some("_t_click"), None]
            );
            let mut reg = crate::register::Register::lower(r, "machines/t").unwrap();
            let syms = std::collections::BTreeMap::from([("_t_click".to_string(), 0x40u32)]);
            reg.set_actions(r, &syms).unwrap();
            assert_eq!(reg.actions, [(1, 0x40)]);
            let e = reg.set_actions(r, &Default::default()).unwrap_err().0;
            assert!(e.contains("_t_click") && e.contains("static"), "{e}");
        }
    }

    #[test]
    fn a_glyph_selects_the_template_row_of_the_device() {
        let templates = |m: &Manifest, dev: &str| -> Vec<i64> {
            let r = resolve(m, &models()).unwrap();
            r.iter()
                .find(|x| x.dev == dev)
                .unwrap()
                .params
                .iter()
                .map(|q| q.template)
                .collect()
        };
        let mut m = manifest(2, &[]);
        assert_eq!(templates(&m, "dt2"), [0xcc, 0xcc]);
        assert_eq!(templates(&m, "dn2"), [0xd5, 0xd5]);
        m.param[1].glyph = Some(1);
        assert_eq!(templates(&m, "dt2"), [0xcc, 0xd1]);
        assert_eq!(templates(&m, "dn2"), [0xd5, 0xd7]);
        // the range of the glyph: ONOFF is 0 .. 1, the param is 0 .. 127
        m.param[1].glyph = Some(3);
        m.param[1].max = 127 * 256;
        let e = bad(&m);
        assert!(e.contains("t_1") && e.contains("onoff"), "{e}");
        m.param[1].max = 256;
        assert_eq!(templates(&m, "dt2"), [0xcc, 0x32]);
        // TUNE is 4 .. 124
        m.param[1].glyph = Some(4);
        let e = bad(&m);
        assert!(e.contains("t_1") && e.contains("tune"), "{e}");
        // PLAY is 0 .. 3: DT2 has the PLAY row, DN2 falls back to the encoder row
        m.param[1].glyph = Some(5);
        m.param[1].max = 127 * 256;
        let e = bad(&m);
        assert!(e.contains("t_1") && e.contains("play"), "{e}");
        m.param[1].max = 3 * 256;
        assert_eq!(templates(&m, "dt2"), [0xcc, 0xcb]);
        assert_eq!(templates(&m, "dn2"), [0xd5, 0xd5]);
    }

    #[test]
    fn lowering_runs_once_and_needs_models() {
        let mut m = manifest(2, &[]);
        assert!(lower(&mut m, &Models::default()).is_err());
        lower(&mut m, &models()).unwrap();
        let n = m.entry.len();
        lower(&mut m, &models()).unwrap();
        assert_eq!(m.entry.len(), n);
    }
}

/// A module-owned table of 16 per-track sample records, declared by DK_SAMPLE_TRACKS (<dk/sample_track.h>, API 1.5): the symbols of the
/// records and of the control sets in the module's DSP data.
#[derive(Clone, Debug, serde::Serialize)]
pub struct SampleTracksDef {
    pub local_id: u32,
    pub records: String,
    pub control: String,
}

/// A module-owned sample bank, declared by DK_SAMPLE_BANK.
#[derive(Clone, Debug, serde::Serialize)]
pub struct SampleBankDef {
    pub local_id: u32,
    pub bank: String,
    pub descriptor: String,
    pub capacity_frames: u32,
}
