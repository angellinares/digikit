//! Descriptor records: what a content module declares in C with the `DK_*` macros of `<dk/fx.h>`, `<dk/machine.h>` and `<dk/page.h>`.
//!
//! The macros put fixed-layout records in the `.dk.desc` section of the module's ColdFire object. `read_objects` decodes them to
//! the typed values of `typed` (`FxDef`, `MachineDef`, `ParamDef`, `PageDef`, `VoiceDef`); `Module` loading puts them in the
//! manifest, and `pack` lowers them. `strip` takes the section out of the object, as the records never reach the device.
//! The layout is `[desc]` of `sdk/sdk.toml` (`sdkgen::desc_spec`); no offset is written here.

use crate::dko::{Dko, ISA_CHECK_KEY};
use crate::error::*;
use crate::module::Module;
use crate::register::EFFECT_KNOBS;
use crate::sdkgen::{self, DescRecord, DescSpec, DescTy};
use crate::typed::{FxDef, FxKnob, MachineDef, PageDef, PageKnob, ParamDef, VoiceDef};

/// The declarations of a set of objects.
#[derive(Debug)]
pub struct Desc {
    pub sample_bank: Option<crate::typed::SampleBankDef>,
    pub sample_tracks: Option<crate::typed::SampleTracksDef>,
    /// The effects, sorted by id.
    pub fx: Vec<FxDef>,
    /// The machines, sorted by id.
    pub machines: Vec<MachineDef>,
    /// The machines' parameters, by machine id, then word.
    pub params: Vec<ParamDef>,
    /// The machines' pages, by machine id, then in the order of the records.
    pub pages: Vec<PageDef>,
    pub native_pages: Vec<crate::typed::NativePageDef>,
    /// The machines' DSP routines, sorted by machine id.
    pub voices: Vec<VoiceDef>,
}

/// One record as it was read, before the records are put together.
enum Rec {
    NativePage { what: String, def: crate::typed::NativePageDef },
    SampleBank(crate::typed::SampleBankDef),
    SampleTracks(crate::typed::SampleTracksDef),
    Fx {
        what: String,
        def: FxDef,
    },
    FxParam {
        what: String,
        fx: String,
        word: u32,
        knob: FxKnob,
    },
    Machine {
        def: MachineDef,
        voice: VoiceDef,
    },
    Param {
        what: String,
        def: ParamDef,
    },
    ParamAction {
        what: String,
        machine: String,
        word: u32,
        func: String,
    },
    ParamGlyph {
        what: String,
        owner: String,
        word: u32,
        glyph: u32,
    },
    ParamLatched {
        what: String,
        owner: String,
        word: u32,
    },
    Page {
        what: String,
        owner: String,
        name: String,
        title: String,
        knobs: Vec<PageKnob>,
    },
}

fn be(b: &[u8], off: usize) -> u32 {
    u32::from_be_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn konst(spec: &DescSpec, name: &str) -> Result<u32> {
    spec.consts
        .get(name)
        .copied()
        .ok_or_else(|| err!("{}: [desc] has no const {name}", sdkgen::SOURCE))
}

fn record<'a>(spec: &'a DescSpec, name: &str) -> Result<&'a DescRecord> {
    spec.records
        .iter()
        .find(|r| r.name == name)
        .ok_or_else(|| err!("{}: [desc] has no record {name}", sdkgen::SOURCE))
}

/// A word field of the record in `b`.
fn word(b: &[u8], rec: &DescRecord, name: &str) -> Result<u32> {
    Ok(be(b, rec.field(name)?.off))
}

/// A signed word field.
fn sword(b: &[u8], rec: &DescRecord, name: &str) -> Result<i64> {
    Ok(i64::from(word(b, rec, name)? as i32))
}

/// A text field of the record in `b`: printable ASCII up to a NUL, NUL-padded to the end.
fn text(what: &str, b: &[u8], rec: &DescRecord, name: &str) -> Result<String> {
    let f = rec.field(name)?;
    let raw = &b[f.off..f.off + f.size];
    let Some(nul) = raw.iter().position(|&c| c == 0) else {
        return Err(err!("{what}: {name}: no NUL in {} bytes", f.size));
    };
    if raw[nul..].iter().any(|&c| c != 0) {
        return Err(err!("{what}: {name}: bytes after the NUL are not zero"));
    }
    if raw[..nul].iter().any(|c| !(0x20..=0x7e).contains(c)) {
        return Err(err!("{what}: {name}: not printable ASCII"));
    }
    Ok(raw[..nul].iter().map(|&c| c as char).collect())
}

/// A text field that must not be empty.
fn req(what: &str, b: &[u8], rec: &DescRecord, name: &str) -> Result<String> {
    let t = text(what, b, rec, name)?;
    if t.is_empty() {
        return Err(err!("{what}: {name} is empty"));
    }
    Ok(t)
}

/// A text field that may be empty.
fn opt(what: &str, b: &[u8], rec: &DescRecord, name: &str) -> Result<Option<String>> {
    Ok(Some(text(what, b, rec, name)?).filter(|t| !t.is_empty()))
}

fn fx_param(what: &str, spec: &DescSpec, b: &[u8], rec: &DescRecord) -> Result<Rec> {
    let display = match word(b, rec, "display")? {
        0 => None,
        d if d == konst(spec, "DK_PERCENT")? => Some("percent".to_string()),
        d if d == konst(spec, "DK_BIPOLAR")? => Some("bipolar".to_string()),
        d => return Err(err!("{what}: display {d} is not DK_PERCENT or DK_BIPOLAR")),
    };
    let float = |name: &str| -> Result<f64> { Ok(f64::from(f32::from_bits(word(b, rec, name)?))) };
    Ok(Rec::FxParam {
        what: what.to_string(),
        fx: req(what, b, rec, "fx")?,
        word: word(b, rec, "word")?,
        knob: FxKnob {
            name: req(what, b, rec, "name")?,
            short: req(what, b, rec, "short_name")?,
            min: float("min")?,
            max: float("max")?,
            default: float("def")?,
            display,
            position: None,
        },
    })
}

fn fx_def(what: &str, spec: &DescSpec, b: &[u8], rec: &DescRecord) -> Result<Rec> {
    let master = konst(spec, "DK_ROUTE_MASTER")?;
    let send = konst(spec, "DK_ROUTE_SEND")?;
    let route = match word(b, rec, "route")? {
        0 => None,
        r if r == master => Some("master".to_string()),
        r if r == send => Some("send".to_string()),
        r => {
            return Err(err!(
                "{what}: route {r} is not DK_ROUTE_MASTER or DK_ROUTE_SEND"
            ));
        }
    };
    let mask = word(b, rec, "routes")?;
    if mask & !(master | send) != 0 {
        return Err(err!(
            "{what}: routes {mask:#x} has bits other than DK_ROUTE_MASTER and DK_ROUTE_SEND"
        ));
    }
    let routes = if mask == 0 {
        None
    } else {
        let mut v = vec![];
        if mask & send != 0 {
            v.push("send".to_string());
        }
        if mask & master != 0 {
            v.push("master".to_string());
        }
        Some(v)
    };
    Ok(Rec::Fx {
        what: what.to_string(),
        def: FxDef {
            id: req(what, b, rec, "id")?,
            process: req(what, b, rec, "process")?,
            desc: req(what, b, rec, "desc")?,
            defaults: vec![],
            devices: vec![],
            tab: None,
            title: None,
            mix: i64::from(word(b, rec, "mix")?),
            knob: vec![],
            route,
            routes,
        },
    })
}

fn machine_def(what: &str, b: &[u8], rec: &DescRecord) -> Result<Rec> {
    let id = req(what, b, rec, "id")?;
    let stereo = match word(b, rec, "stereo")? {
        0 => false,
        1 => true,
        n => return Err(err!("{what}: stereo {n} is not 0 or 1")),
    };
    Ok(Rec::Machine {
        def: MachineDef {
            id: id.clone(),
            name: req(what, b, rec, "name")?,
            short: req(what, b, rec, "short_name")?,
            stereo,
        },
        voice: VoiceDef {
            machine: id,
            render: req(what, b, rec, "render")?,
            note_on: req(what, b, rec, "note_on")?,
            desc: req(what, b, rec, "desc")?,
            stereo,
        },
    })
}

fn param_def(what: &str, b: &[u8], rec: &DescRecord) -> Result<Rec> {
    Ok(Rec::Param {
        what: what.to_string(),
        def: ParamDef {
            id: req(what, b, rec, "id")?,
            machine: req(what, b, rec, "machine")?,
            word: word(b, rec, "word")?,
            name: req(what, b, rec, "name")?,
            short: req(what, b, rec, "short_name")?,
            min: sword(b, rec, "min")?,
            max: sword(b, rec, "max")?,
            default: sword(b, rec, "def")?,
            action: None,
            glyph: None,
            latched: false,
        },
    })
}

fn param_glyph(what: &str, b: &[u8], rec: &DescRecord) -> Result<Rec> {
    Ok(Rec::ParamGlyph {
        what: what.to_string(),
        owner: req(what, b, rec, "owner")?,
        word: word(b, rec, "word")?,
        glyph: word(b, rec, "glyph")?,
    })
}

fn param_action(what: &str, b: &[u8], rec: &DescRecord) -> Result<Rec> {
    Ok(Rec::ParamAction {
        what: what.to_string(),
        machine: req(what, b, rec, "machine")?,
        word: word(b, rec, "word")?,
        func: req(what, b, rec, "fn")?,
    })
}

fn native_page_def(what: &str, spec: &DescSpec, b: &[u8], rec: &DescRecord, replacement: bool) -> Result<Rec> {
    let version = if replacement { 2 } else { 1 };
    let page = if replacement { word(b, rec, "page")? } else { 1 };
    if word(b, rec, "version")? != version || page > 1 {
        return Err(err!("{what}: invalid native page version/index"));
    }
    let flags = if replacement { word(b, rec, "flags")? } else { 0 };
    if flags & !3 != 0 || (flags & 1 != 0 && page != 0) || (flags & 2 != 0 && page != 1) { return Err(err!("{what}: invalid native page flags")); }
    let field = rec.field("binding")?;
    let DescTy::Rec { rec: name, count } = &field.ty else {
        return Err(err!("{what}: native bindings must be records"));
    };
    let binding = record(spec, name)?;
    let mut bindings = vec![];
    let mut absent = false;
    let mut positions = 0u32;
    for i in 0..*count {
        let bytes = &b[field.off + i * binding.size..field.off + (i + 1) * binding.size];
        let kind = word(bytes, binding, "kind")?;
        if kind == 0 {
            if bytes.iter().any(|&x| x != 0) { return Err(err!("{what}: unused native binding must be zero")); }
            absent = true;
            continue;
        }
        let position = word(bytes, binding, "position")?;
        let value = word(bytes, binding, "value")?;
        if absent || !(1..=2).contains(&kind) || position >= 8 || positions & (1 << position) != 0 || value >= 256 {
            return Err(err!("{what}: native bindings need PARAM/LOCAL kinds, contiguous entries, unique positions 0..7 and values below 256"));
        }
        positions |= 1 << position;
        bindings.push(crate::typed::NativeBinding { kind, value, position });
    }
    if bindings.is_empty() { return Err(err!("{what}: native page needs a binding")); }
    Ok(Rec::NativePage { what: what.into(), def: crate::typed::NativePageDef {
        version, page, flags, owner: req(what, b, rec, "owner")?, title: req(what, b, rec, "title")?,
        draw: req(what, b, rec, "draw")?, event: req(what, b, rec, "event")?, bindings,
    }})
}

fn page_def(what: &str, spec: &DescSpec, b: &[u8], rec: &DescRecord) -> Result<Rec> {
    let kf = rec.field("knob")?;
    let DescTy::Rec { rec: kname, count } = &kf.ty else {
        return Err(err!(
            "{}: [desc] {}.knob is not an array of records",
            sdkgen::SOURCE,
            rec.name
        ));
    };
    let krec = record(spec, kname)?;
    let auto = konst(spec, "DK_POS_AUTO")?;
    let mut knobs = vec![];
    let mut absent = false;
    for i in 0..*count {
        let kb = &b[kf.off + i * krec.size..kf.off + (i + 1) * krec.size];
        let kwhat = format!("{what}: knob {i}");
        match word(kb, krec, "used")? {
            0 => {
                if kb.iter().any(|&c| c != 0) {
                    return Err(err!("{kwhat}: is unused but not all zero"));
                }
                absent = true;
            }
            1 => {
                if absent {
                    return Err(err!("{kwhat}: follows an unused entry (a gap)"));
                }
                let p = word(kb, krec, "position")?;
                let position = if p == auto {
                    None
                } else if (p as usize) < *count {
                    Some(p)
                } else {
                    return Err(err!(
                        "{kwhat}: position {p} is not 0 to {} or DK_POS_AUTO",
                        count - 1
                    ));
                };
                knobs.push(PageKnob {
                    word: word(kb, krec, "word")?,
                    position,
                });
            }
            u => return Err(err!("{kwhat}: used {u} is 0 or 1")),
        }
    }
    Ok(Rec::Page {
        what: what.to_string(),
        owner: req(what, b, rec, "owner")?,
        name: req(what, b, rec, "name")?,
        title: req(what, b, rec, "title")?,
        knobs,
    })
}

/// The records of one object, in order. Only a ColdFire object can hold them.
fn records(obj: &Dko, spec: &DescSpec) -> Result<Vec<Rec>> {
    let mut out = vec![];
    for sec in obj.sections.iter().filter(|s| s.name == spec.section) {
        let sname = &sec.name;
        if obj.arch != "cf" {
            return Err(err!(
                "{sname}: only a ColdFire object holds descriptor records, this one is {}",
                obj.arch
            ));
        }
        if let Some(r) = obj.relocs.iter().find(|r| r.section == *sname) {
            return Err(err!(
                "{sname}: relocation at {:#x} ({}): a record holds no addresses",
                r.offset,
                r.symbol
            ));
        }
        let b = unhex(&sec.hex)?;
        let head = record(spec, "dk_desc_fx")?;
        let (kind_off, size_off) = (head.field("kind")?.off, head.field("size")?.off);
        let mut at = 0;
        let mut index = 0;
        while at < b.len() {
            let what = format!("{sname}: record {index}");
            if b.len() - at < kind_off.max(size_off) + 4 {
                return Err(err!("{what}: the section ends inside the header"));
            }
            let kind = be(&b, at + kind_off);
            let size = be(&b, at + size_off) as usize;
            let rec = spec
                .records
                .iter()
                .find(|r| r.kind == Some(kind))
                .ok_or_else(|| err!("{what}: unknown record kind {kind}"))?;
            // A machine record of contract 1.1 ends before `stereo`: mono.
            let legacy = rec.name == "dk_desc_machine" && size + 4 == rec.size;
            if size != rec.size && !legacy {
                return Err(err!(
                    "{what}: size {size} is not the {} bytes of {}",
                    rec.size,
                    rec.name
                ));
            }
            if size > b.len() - at {
                return Err(err!(
                    "{what}: {} bytes do not fit in the {} left",
                    size,
                    b.len() - at
                ));
            }
            let mut padded;
            let rb = if legacy {
                padded = b[at..at + size].to_vec();
                padded.resize(rec.size, 0);
                &padded[..]
            } else {
                &b[at..at + size]
            };
            out.push(match rec.name.as_str() {
                "dk_desc_sample_bank" => Rec::SampleBank(crate::typed::SampleBankDef {
                    local_id: word(rb, rec, "local_id")?,
                    bank: req(&what, rb, rec, "bank")?,
                    descriptor: req(&what, rb, rec, "descriptor")?,
                    capacity_frames: word(rb, rec, "capacity_frames")?,
                }),
                "dk_desc_sample_tracks" => Rec::SampleTracks(crate::typed::SampleTracksDef {
                    local_id: word(rb, rec, "local_id")?,
                    records: req(&what, rb, rec, "records")?,
                    control: req(&what, rb, rec, "control")?,
                }),
                "dk_desc_fx" => fx_def(&what, spec, rb, rec)?,
                "dk_desc_fx_param" => fx_param(&what, spec, rb, rec)?,
                "dk_desc_machine" => machine_def(&what, rb, rec)?,
                "dk_desc_param" => param_def(&what, rb, rec)?,
                "dk_desc_param_action" => param_action(&what, rb, rec)?,
                "dk_desc_param_glyph" => param_glyph(&what, rb, rec)?,
                "dk_desc_param_latched" => Rec::ParamLatched {
                    what: what.clone(),
                    owner: req(&what, rb, rec, "owner")?,
                    word: word(rb, rec, "word")?,
                },
                "dk_desc_page" => page_def(&what, spec, rb, rec)?,
                "dk_desc_native_page" => native_page_def(&what, spec, rb, rec, false)?,
                "dk_desc_native_page_v2" => native_page_def(&what, spec, rb, rec, true)?,
                _ => return Err(err!("{what}: no reader for {}", rec.name)),
            });
            at += size;
            index += 1;
        }
    }
    Ok(out)
}

/// Check that `words` are 0, 1, 2 ... once the list is sorted; `who` names the owner in the message.
fn gapless(who: &str, what: &str, words: &mut [u32]) -> Result<()> {
    words.sort_unstable();
    for (i, w) in words.iter().enumerate() {
        if *w as usize != i {
            return Err(if i > 0 && words[i - 1] == *w {
                err!("{who}: {what} word {w} is declared twice")
            } else {
                err!("{who}: {what} words have a gap: word {i} is missing")
            });
        }
    }
    Ok(())
}

/// The page positions of the entries `(word, position)`: explicit ones stay, the others take the lowest free position in order.
fn positions(what: &str, entries: &[PageKnob]) -> Result<Vec<u32>> {
    let mut taken: Vec<u32> = vec![];
    for e in entries {
        if let Some(p) = e.position {
            if taken.contains(&p) {
                return Err(err!("{what}: two knobs at position {p}"));
            }
            taken.push(p);
        }
    }
    let mut out = vec![];
    for e in entries {
        out.push(match e.position {
            Some(p) => p,
            None => {
                let p = (0..8)
                    .find(|p| !taken.contains(p))
                    .ok_or_else(|| err!("{what}: more than 8 knobs"))?;
                taken.push(p);
                p
            }
        });
    }
    Ok(out)
}

/// Put the records together: ids, owners and words are checked here.
fn assemble(recs: Vec<Rec>) -> Result<Desc> {
    let mut sample_bank = None;
    let mut sample_tracks = None;
    let mut fxs: Vec<(String, FxDef)> = vec![];
    let mut fx_params: Vec<(String, String, u32, FxKnob)> = vec![];
    let mut machines: Vec<(MachineDef, VoiceDef)> = vec![];
    let mut params: Vec<(String, ParamDef)> = vec![];
    let mut actions: Vec<(String, String, u32, String)> = vec![];
    let mut glyphs: Vec<(String, String, u32, u32)> = vec![];
    let mut latched = vec![];
    let mut pages: Vec<(String, String, String, String, Vec<PageKnob>)> = vec![];
    let mut native_pages = vec![];
    for r in recs {
        match r {
            Rec::NativePage { what, def } => native_pages.push((what, def)),
            Rec::SampleBank(b) => {
                if sample_bank.is_some() {
                    return Err(err!("at most one sample bank per module"));
                }
                if !(1..=8192).contains(&b.capacity_frames) {
                    return Err(err!("sample bank capacity must be 1 .. 8192 frames"));
                }
                sample_bank = Some(b);
            }
            Rec::SampleTracks(t) => {
                if sample_tracks.is_some() {
                    return Err(err!("at most one sample track table per module"));
                }
                if sample_bank.is_some() {
                    return Err(err!("a module declares a sample bank or a sample track table, not both"));
                }
                sample_tracks = Some(t);
            }
            Rec::Fx { what, def } => fxs.push((what, def)),
            Rec::FxParam {
                what,
                fx,
                word,
                knob,
            } => fx_params.push((what, fx, word, knob)),
            Rec::Machine { def, voice } => machines.push((def, voice)),
            Rec::Param { what, def } => params.push((what, def)),
            Rec::ParamAction {
                what,
                machine,
                word,
                func,
            } => actions.push((what, machine, word, func)),
            Rec::ParamGlyph {
                what,
                owner,
                word,
                glyph,
            } => glyphs.push((what, owner, word, glyph)),
            Rec::ParamLatched { what, owner, word } => latched.push((what, owner, word)),
            Rec::Page {
                what,
                owner,
                name,
                title,
                knobs,
            } => pages.push((what, owner, name, title, knobs)),
        }
    }
    fxs.sort_by(|a, b| a.1.id.cmp(&b.1.id));
    if let Some(w) = fxs.windows(2).find(|w| w[0].1.id == w[1].1.id) {
        return Err(err!("effect {} is declared twice", w[0].1.id));
    }
    machines.sort_by(|a, b| a.0.id.cmp(&b.0.id));
    if let Some(w) = machines.windows(2).find(|w| w[0].0.id == w[1].0.id) {
        return Err(err!("machine {} is declared twice", w[0].0.id));
    }
    if let Some((_, f)) = fxs
        .iter()
        .find(|(_, f)| machines.iter().any(|(m, _)| m.id == f.id))
    {
        return Err(err!("{} is declared as a machine and as an effect", f.id));
    }

    // the machines' parameters
    params.sort_by(|a, b| (&a.1.machine, a.1.word).cmp(&(&b.1.machine, b.1.word)));
    let mut ids: Vec<&str> = vec![];
    for (what, p) in &params {
        if !machines.iter().any(|(m, _)| m.id == p.machine) {
            return Err(err!("{what}: machine {} is not declared", p.machine));
        }
        if ids.contains(&p.id.as_str()) {
            return Err(err!("{what}: param {} is declared twice", p.id));
        }
        ids.push(&p.id);
    }
    for (what, machine, word, func) in actions {
        let Some((_, p)) = params
            .iter_mut()
            .find(|(_, p)| p.machine == machine && p.word == word)
        else {
            return Err(err!(
                "{what}: machine {machine} has no param at word {word} for a click action"
            ));
        };
        if p.action.is_some() {
            return Err(err!(
                "{what}: param {} (word {word}) has two click actions",
                p.id
            ));
        }
        p.action = Some(func);
    }
    for (what, owner, word) in latched {
        let Some((_, p)) = params
            .iter_mut()
            .find(|(_, p)| p.machine == owner && p.word == word)
        else {
            return Err(err!(
                "{what}: machine {owner} has no param at word {word} for latching"
            ));
        };
        if p.latched {
            return Err(err!(
                "{what}: param {} (word {word}) has two latched declarations",
                p.id
            ));
        }
        p.latched = true;
    }
    for (what, owner, word, glyph) in glyphs {
        if glyph > 5 {
            return Err(err!(
                "{what}: unknown glyph {glyph} (DK_GLYPH_ENCODER .. DK_GLYPH_PLAY is 0 to 5)"
            ));
        }
        if fxs.iter().any(|(_, f)| f.id == owner) {
            return Err(err!(
                "{what}: effect {owner}: glyphs on effect parameters are not supported yet"
            ));
        }
        let Some((_, p)) = params
            .iter_mut()
            .find(|(_, p)| p.machine == owner && p.word == word)
        else {
            return Err(err!(
                "{what}: machine {owner} has no param at word {word} for a glyph"
            ));
        };
        if p.glyph.is_some() {
            return Err(err!(
                "{what}: param {} (word {word}) has two glyphs",
                p.id
            ));
        }
        p.glyph = Some(glyph);
    }
    for (m, _) in &machines {
        let mut words: Vec<u32> = params
            .iter()
            .filter(|(_, p)| p.machine == m.id)
            .map(|(_, p)| p.word)
            .collect();
        gapless(&format!("machine {}", m.id), "param", &mut words)?;
    }

    // the pages
    let mut machine_pages: Vec<PageDef> = vec![];
    let mut fx_pages: Vec<(String, String, String, Vec<PageKnob>)> = vec![];
    for (what, owner, name, title, knobs) in pages {
        if machines.iter().any(|(m, _)| m.id == owner) {
            let mut seen: Vec<u32> = vec![];
            for (i, k) in knobs.iter().enumerate() {
                if !params
                    .iter()
                    .any(|(_, p)| p.machine == owner && p.word == k.word)
                {
                    return Err(err!(
                        "{what}: knob {i} names word {}, but machine {owner} has no such param",
                        k.word
                    ));
                }
                if seen.contains(&k.word) {
                    return Err(err!("{what}: knob {i}: word {} is on the page twice", k.word));
                }
                seen.push(k.word);
            }
            let n = machine_pages.iter().filter(|p| p.machine == owner).count();
            machine_pages.push(PageDef {
                id: format!("{owner}_p{n}"),
                machine: owner,
                name,
                title,
                knobs,
            });
        } else if fxs.iter().any(|(_, f)| f.id == owner) {
            if fx_pages.iter().any(|p| p.0 == owner) {
                return Err(err!("{what}: an effect has one page for now"));
            }
            fx_pages.push((owner, name, title, knobs));
        } else {
            return Err(err!(
                "{what}: owner {owner} is neither a machine nor an effect of the module"
            ));
        }
    }
    machine_pages.sort_by(|a, b| a.machine.cmp(&b.machine));

    // the effects: parameters, then the page
    for (what, fx, _, _) in &fx_params {
        if !fxs.iter().any(|(_, f)| &f.id == fx) {
            return Err(err!("{what}: effect {fx} is not declared"));
        }
    }
    let mut fx_out = vec![];
    for (_, mut f) in fxs {
        let mut mine: Vec<(u32, FxKnob)> = fx_params
            .iter()
            .filter(|p| p.1 == f.id)
            .map(|p| (p.2, p.3.clone()))
            .collect();
        let who = format!("effect {}", f.id);
        if mine.len() > EFFECT_KNOBS {
            return Err(err!(
                "{who}: {} params; at most {EFFECT_KNOBS} (and MIX)",
                mine.len()
            ));
        }
        gapless(
            &who,
            "param",
            &mut mine.iter().map(|m| m.0).collect::<Vec<_>>(),
        )?;
        mine.sort_by_key(|m| m.0);
        match fx_pages.iter().find(|p| p.0 == f.id) {
            Some((_, name, title, knobs)) => {
                for k in knobs {
                    if k.word as usize >= mine.len() {
                        return Err(err!(
                            "{who}: page {name}: a knob names word {}, but the effect has no such param",
                            k.word
                        ));
                    }
                }
                for w in 0..mine.len() as u32 {
                    match knobs.iter().filter(|k| k.word == w).count() {
                        1 => {}
                        0 => {
                            return Err(err!("{who}: page {name}: param word {w} is not on the page"));
                        }
                        _ => {
                            return Err(err!("{who}: page {name}: param word {w} is on the page twice"));
                        }
                    }
                }
                let pos = positions(&format!("{who}: page {name}"), knobs)?;
                f.tab = Some(name.clone());
                f.title = Some(title.clone());
                f.knob = mine
                    .into_iter()
                    .map(|(w, mut k)| {
                        let at = knobs.iter().position(|e| e.word == w).unwrap();
                        k.position = Some(pos[at]);
                        k
                    })
                    .collect();
            }
            None => {
                f.defaults = mine
                    .iter()
                    .map(|(_, k)| (k.default as f32).to_bits())
                    .collect();
            }
        }
        fx_out.push(f);
    }
    let mut native_owners = vec![];
    for (what, page) in &native_pages {
        if !machines.iter().any(|(m, _)| m.id == page.owner) || native_owners.contains(&(page.owner.clone(), page.page)) {
            return Err(err!("{what}: native page needs a declared machine and unique native page indexes"));
        }
        native_owners.push((page.owner.clone(), page.page));
        let owner_pages: Vec<_> = native_pages.iter().filter(|(_, p)| p.owner == page.owner).collect();
        if owner_pages.iter().filter(|(_, p)| p.page == page.page).count() > 1 {
            return Err(err!("{what}: unique native page indexes required"));
        }
        if owner_pages.iter().any(|(_, p)| p.version != page.version) ||
            (page.version == 1 && owner_pages.len() != 1) ||
            (page.version == 2 && (owner_pages.len() != 2 || !owner_pages.iter().any(|(_, p)| p.page == 0) || !owner_pages.iter().any(|(_, p)| p.page == 1))) {
            return Err(err!("{what}: native v2 requires exactly pages 0 and 1; cannot mix versions"));
        }
        for binding in &page.bindings {
            if binding.kind == 1 && !params.iter().any(|(_, p)| p.machine == page.owner && p.word == binding.value) {
                return Err(err!("{what}: machine {} has no native-page parameter word {}", page.owner, binding.value));
            }
        }
    }
    Ok(Desc {
        sample_bank,
        sample_tracks,
        fx: fx_out,
        params: params.into_iter().map(|p| p.1).collect(),
        pages: machine_pages,
        native_pages: native_pages.into_iter().map(|(_, page)| page).collect(),
        voices: machines.iter().map(|m| m.1.clone()).collect(),
        machines: machines.into_iter().map(|m| m.0).collect(),
    })
}

/// The declarations in a set of objects (a module's).
pub fn read_objects<'a>(objs: impl IntoIterator<Item = &'a Dko>) -> Result<Desc> {
    let spec = sdkgen::desc_spec();
    let mut recs = vec![];
    for o in objs {
        recs.extend(records(o, &spec)?);
    }
    assemble(recs)
}

/// The declarations in one object.
pub fn read(obj: &Dko) -> Result<Desc> {
    read_objects([obj])
}

/// A copy of the object without the descriptor sections, their symbols and their relocations; `None` when nothing is left in it.
/// The ISA check record, if it matched the original, is renewed for the remaining bytes.
pub fn strip(obj: &Dko) -> Option<Dko> {
    let section = sdkgen::desc_spec().section;
    let mut o = obj.clone();
    o.sections.retain(|s| s.name != section);
    o.symbols
        .retain(|s| s.section.as_deref() != Some(section.as_str()));
    o.relocs.retain(|r| r.section != section);
    if !o.sections.iter().any(|s| s.size > 0) {
        return None;
    }
    if obj.toolchain.contains_key(ISA_CHECK_KEY) && obj.require_isa_checked().is_ok() {
        o.stamp_isa();
    }
    Some(o)
}

/// The effects of a module: the records of its ColdFire objects.
pub fn module_fx(m: &Module) -> Result<Vec<FxDef>> {
    Ok(read_objects(&m.objects)?.fx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dko::{DkoReloc, DkoSection, DkoSymbol};

    /// A record under construction: fields are set by name.
    struct Rb {
        rec: DescRecord,
        krec: DescRecord,
        b: Vec<u8>,
    }

    impl Rb {
        fn new(name: &str) -> Rb {
            let spec = sdkgen::desc_spec();
            let rec = record(&spec, name).unwrap().clone();
            let mut r = Rb {
                krec: record(&spec, "dk_desc_page_knob").unwrap().clone(),
                b: vec![0u8; rec.size],
                rec,
            };
            let (kind, size) = (r.rec.kind.unwrap(), r.rec.size as u32);
            r.num("kind", kind).num("size", size);
            r
        }
        fn num(&mut self, f: &str, v: u32) -> &mut Rb {
            let off = self.rec.field(f).unwrap().off;
            self.b[off..off + 4].copy_from_slice(&v.to_be_bytes());
            self
        }
        fn text(&mut self, f: &str, s: &str) -> &mut Rb {
            let f = self.rec.field(f).unwrap();
            self.b[f.off..f.off + f.size].fill(0);
            self.b[f.off..f.off + s.len()].copy_from_slice(s.as_bytes());
            self
        }
        /// Page entry `i`.
        fn knob(&mut self, i: usize, word: u32, position: u32) -> &mut Rb {
            let base = self.rec.field("knob").unwrap().off + i * self.krec.size;
            for (f, v) in [("used", 1), ("word", word), ("position", position)] {
                let off = base + self.krec.field(f).unwrap().off;
                self.b[off..off + 4].copy_from_slice(&v.to_be_bytes());
            }
            self
        }
        fn done(&self) -> Vec<u8> {
            self.b.clone()
        }
    }

    #[test]
    fn sample_bank_declarations_are_numeric_single_and_capacity_bounded() {
        let mut r = Rb::new("dk_desc_sample_bank");
        r.num("local_id", 17)
            .text("bank", "_bank")
            .text("descriptor", "_sample")
            .num("capacity_frames", 8192);
        let d = read(&obj(&r.done())).unwrap();
        let b = d.sample_bank.unwrap();
        assert_eq!((b.local_id, b.capacity_frames), (17, 8192));
        assert_eq!(
            (b.bank.as_str(), b.descriptor.as_str()),
            ("_bank", "_sample")
        );
        assert!(err_of(&[&r, &r]).contains("one sample bank"));
        for n in [0, 8193, u32::MAX] {
            r.num("capacity_frames", n);
            assert!(err_of(&[&r]).contains("capacity"));
        }
        r.num("capacity_frames", 1).text("bank", "");
        assert!(err_of(&[&r]).contains("bank is empty"));
    }

    const AUTO: u32 = 255;

    fn fx(id: &str) -> Rb {
        let mut r = Rb::new("dk_desc_fx");
        r.text("id", id)
            .text("process", "_fs_process")
            .text("desc", "_fs_desc")
            .num("mix", 127)
            .num("route", 2)
            .num("routes", 3);
        r
    }

    fn fx_param(fx: &str, word: u32, name: &str) -> Rb {
        let mut r = Rb::new("dk_desc_fx_param");
        r.text("fx", fx)
            .num("word", word)
            .num("display", 2)
            .num("min", (-64.0f32).to_bits())
            .num("max", 63.0f32.to_bits())
            .num("def", 0.0f32.to_bits())
            .text("name", name)
            .text("short_name", "SHIFT");
        r
    }

    fn machine(id: &str) -> Rb {
        let mut r = Rb::new("dk_desc_machine");
        r.text("id", id)
            .text("name", "Pluck")
            .text("short_name", "PLK")
            .text("render", "_pk_render")
            .text("note_on", "_pk_note_on")
            .text("desc", "_pk_desc")
            .num("stereo", 0);
        r
    }

    fn param(machine: &str, id: &str, word: u32) -> Rb {
        let mut r = Rb::new("dk_desc_param");
        r.text("machine", machine)
            .text("id", id)
            .num("word", word)
            .num("min", 0)
            .num("max", 0x7f00)
            .num("def", 0x4000)
            .text("name", "Decay")
            .text("short_name", "DECAY");
        r
    }

    fn action(machine: &str, word: u32, func: &str) -> Rb {
        let mut r = Rb::new("dk_desc_param_action");
        r.text("machine", machine).num("word", word).text("fn", func);
        r
    }

    fn glyph(owner: &str, word: u32, glyph: u32) -> Rb {
        let mut r = Rb::new("dk_desc_param_glyph");
        r.text("owner", owner).num("word", word).num("glyph", glyph);
        r
    }

    fn page(owner: &str) -> Rb {
        let mut r = Rb::new("dk_desc_page");
        r.text("owner", owner).text("name", "PLUCK").text("title", "Pluck");
        r
    }

    fn obj(bytes: &[u8]) -> Dko {
        Dko {
            format: 1,
            arch: "cf".into(),
            source_sha256: String::new(),
            toolchain: Default::default(),
            sections: vec![DkoSection {
                name: sdkgen::desc_spec().section,
                kind: "data".into(),
                align: 4,
                size: bytes.len() as u32,
                hex: hex(bytes),
            }],
            symbols: vec![],
            relocs: vec![],
        }
    }

    fn all(recs: &[&Rb]) -> Vec<u8> {
        recs.iter().flat_map(|r| r.done()).collect()
    }

    fn err_of(recs: &[&Rb]) -> String {
        read(&obj(&all(recs))).unwrap_err().0
    }

    /// An effect with one parameter and a page for it.
    fn fshift() -> (Rb, Rb, Rb) {
        let mut pg = page("fshift");
        pg.text("name", "FSH").text("title", "Freq Shifter").knob(0, 0, 0);
        (fx("fshift"), fx_param("fshift", 0, "Freq Shift"), pg)
    }

    #[test]
    fn an_effect_with_a_page_reads_to_its_fx_def() {
        let (f, p, g) = fshift();
        let d = read(&obj(&all(&[&f, &p, &g]))).unwrap();
        let [f] = d.fx.as_slice() else {
            panic!("one effect")
        };
        assert_eq!(
            (f.id.as_str(), f.process.as_str(), f.desc.as_str()),
            ("fshift", "_fs_process", "_fs_desc")
        );
        assert_eq!(
            (f.tab.as_deref(), f.title.as_deref()),
            (Some("FSH"), Some("Freq Shifter"))
        );
        assert_eq!(f.mix, 127);
        assert_eq!(f.route.as_deref(), Some("send"));
        assert_eq!(
            f.routes,
            Some(vec!["send".to_string(), "master".to_string()])
        );
        assert!(f.defaults.is_empty() && f.devices.is_empty());
        let [k] = f.knob.as_slice() else {
            panic!("one knob")
        };
        assert_eq!((k.name.as_str(), k.short.as_str()), ("Freq Shift", "SHIFT"));
        assert_eq!((k.min, k.max, k.default), (-64.0, 63.0, 0.0));
        assert_eq!(
            (k.display.as_deref(), k.position),
            (Some("bipolar"), Some(0))
        );
        assert!(d.machines.is_empty() && d.pages.is_empty());
    }

    #[test]
    fn an_effect_without_a_page_keeps_the_defaults_of_its_params() {
        let mut p1 = fx_param("fshift", 1, "Other");
        p1.num("def", 2.0f32.to_bits()).num("max", 3.0f32.to_bits());
        let d = read(&obj(&all(&[
            &fx("fshift"),
            &p1,
            &fx_param("fshift", 0, "A"),
        ])))
        .unwrap();
        let f = &d.fx[0];
        assert!(f.tab.is_none() && f.title.is_none() && f.knob.is_empty());
        assert_eq!(f.defaults, [0.0f32.to_bits(), 2.0f32.to_bits()]);
    }

    #[test]
    fn auto_positions_take_the_lowest_free_one_in_entry_order() {
        let mut g = page("fshift");
        g.knob(0, 0, AUTO).knob(1, 1, 0).knob(2, 2, AUTO);
        let recs = [
            &fx("fshift"),
            &fx_param("fshift", 0, "A"),
            &fx_param("fshift", 1, "B"),
            &fx_param("fshift", 2, "C"),
            &g,
        ];
        let d = read(&obj(&all(&recs))).unwrap();
        let pos: Vec<_> = d.fx[0].knob.iter().map(|k| k.position).collect();
        assert_eq!(pos, [Some(1), Some(0), Some(2)]);
    }

    #[test]
    fn effects_are_sorted_and_an_id_twice_is_refused() {
        let mut b = fx("zeta").done();
        b.extend(fx("alpha").done());
        let ids: Vec<_> = read(&obj(&b)).unwrap().fx.into_iter().map(|f| f.id).collect();
        assert_eq!(ids, ["alpha", "zeta"]);
        assert!(err_of(&[&fx("same"), &fx("same")]).contains("twice"));
    }

    #[test]
    fn machine_records_read_to_machines_params_pages_and_voices() {
        let mut g = page("pluck");
        g.knob(0, 1, AUTO).knob(1, 0, 3);
        let recs = [
            &machine("pluck"),
            &param("pluck", "pk_b", 1),
            &param("pluck", "pk_a", 0),
            &g,
        ];
        let d = read(&obj(&all(&recs))).unwrap();
        assert_eq!(d.machines.len(), 1);
        assert_eq!((d.machines[0].name.as_str(), d.machines[0].short.as_str()), ("Pluck", "PLK"));
        assert_eq!(d.voices[0].render, "_pk_render");
        assert_eq!(d.voices[0].desc, "_pk_desc");
        // in word order
        let ids: Vec<_> = d.params.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["pk_a", "pk_b"]);
        assert_eq!((d.params[0].min, d.params[0].max, d.params[0].default), (0, 0x7f00, 0x4000));
        let [pg] = d.pages.as_slice() else {
            panic!("one page")
        };
        assert_eq!((pg.machine.as_str(), pg.name.as_str(), pg.title.as_str()), ("pluck", "PLUCK", "Pluck"));
        assert_eq!(pg.knobs[0].word, 1);
        assert_eq!(pg.knobs[0].position, None);
        assert_eq!(pg.knobs[1].position, Some(3));
    }

    #[test]
    fn a_click_action_joins_the_param_at_its_word() {
        let m = machine("pluck");
        let recs = [
            &m,
            &param("pluck", "pk_b", 1),
            &param("pluck", "pk_a", 0),
            &action("pluck", 1, "_pk_click"),
        ];
        let d = read(&obj(&all(&recs))).unwrap();
        assert_eq!(d.params[0].action, None);
        assert_eq!(d.params[1].action.as_deref(), Some("_pk_click"));
        // no param at that word, and two actions on one
        let e = err_of(&[&m, &param("pluck", "a", 0), &action("pluck", 1, "f")]);
        assert!(e.contains("no param at word 1"), "{e}");
        let e = err_of(&[&m, &param("pluck", "a", 0), &action("nope", 0, "f")]);
        assert!(e.contains("no param at word 0"), "{e}");
        let e = err_of(&[
            &m,
            &param("pluck", "a", 0),
            &action("pluck", 0, "f"),
            &action("pluck", 0, "g"),
        ]);
        assert!(e.contains("two click actions"), "{e}");
        let mut empty = action("pluck", 0, "");
        empty.text("fn", "");
        assert!(err_of(&[&m, &param("pluck", "a", 0), &empty]).contains("fn is empty"));
    }

    #[test]
    fn native_callback_page_validates_version_owner_words_and_positions() {
        let native = |owner: &str| {
            let mut page = Rb::new("dk_desc_native_page");
            page.num("version", 1).text("owner", owner).text("title", "Native proof")
                .text("draw", "native_draw").text("event", "native_event");
            let at = page.rec.field("binding").unwrap().off;
            for (i, values) in [[1u32, 0, 0], [2, 0, 2], [1, 1, 3]].iter().enumerate() {
                for (j, value) in values.iter().enumerate() {
                    page.b[at + i * 12 + j * 4..at + i * 12 + j * 4 + 4].copy_from_slice(&value.to_be_bytes());
                }
            }
            page
        };
        let m = machine("proof");
        let p0 = param("proof", "start", 0);
        let p1 = param("proof", "end", 1);
        let page = native("proof");
        let d = read(&obj(&all(&[&m, &p0, &p1, &page]))).unwrap();
        assert_eq!(d.native_pages[0].bindings.iter().map(|b| (b.kind,b.position)).collect::<Vec<_>>(), [(1,0),(2,2),(1,3)]);
        assert!(err_of(&[&m, &p0, &p1, &native("absent")]).contains("declared machine"));
        assert!(err_of(&[&m, &p0, &page]).contains("parameter word 1"));
        assert!(err_of(&[&m, &p0, &p1, &page, &page]).contains("unique native page indexes"));
        let mut bad = native("proof"); bad.num("version", 2);
        assert!(err_of(&[&m, &p0, &p1, &bad]).contains("version/index"));
        let mut bad = native("proof");
        let at = bad.rec.field("binding").unwrap().off;
        bad.b[at + 20..at + 24].copy_from_slice(&0u32.to_be_bytes()); // LOCAL duplicates encoder A
        assert!(err_of(&[&m, &p0, &p1, &bad]).contains("unique positions"));
    }

    #[test]
    fn native_replacement_pages_require_a_complete_unmixed_pair() {
        let replacement = |index: u32| {
            let mut page = Rb::new("dk_desc_native_page_v2");
            page.num("version", 2).num("page", index).text("owner", "proof")
                .text("title", "Replacement").text("draw", "draw").text("event", "event");
            let at = page.rec.field("binding").unwrap().off;
            for (j, value) in [1u32, 0, 7].iter().enumerate() {
                page.b[at + j * 4..at + j * 4 + 4].copy_from_slice(&value.to_be_bytes());
            }
            page
        };
        let m = machine("proof"); let p = param("proof", "start", 0);
        let a = replacement(0); let b = replacement(1);
        let pages = read(&obj(&all(&[&m, &p, &a, &b]))).unwrap().native_pages;
        assert_eq!(pages.iter().map(|p| (p.version,p.page)).collect::<Vec<_>>(), [(2,0),(2,1)]);
        assert!(err_of(&[&m,&p,&a]).contains("exactly pages 0 and 1"));
        assert!(err_of(&[&m,&p,&a,&a]).contains("unique native page indexes"));
        assert!(err_of(&[&m,&p,&replacement(2),&b]).contains("version/index"));
        let mut overlay = replacement(0); overlay.num("flags", 1);
        assert_eq!(read(&obj(&all(&[&m,&p,&overlay,&b]))).unwrap().native_pages[0].flags, 1);
        let mut wide = replacement(1); wide.num("flags", 2);
        assert_eq!(read(&obj(&all(&[&m,&p,&a,&wide]))).unwrap().native_pages[1].flags, 2);
        for (index, flags) in [(0,2), (1,1), (0,3), (1,3), (0,4)] {
            let mut bad = replacement(index); bad.num("flags", flags);
            assert!(err_of(&[&m,&p,&bad,&b]).contains("invalid native page flags"));
        }
        let mut legacy = Rb::new("dk_desc_native_page");
        legacy.num("version",1).text("owner","proof").text("title","Legacy").text("draw","draw").text("event","event");
        let at = legacy.rec.field("binding").unwrap().off;
        legacy.b[at..at+4].copy_from_slice(&2u32.to_be_bytes());
        assert!(err_of(&[&m,&p,&a,&legacy]).contains("cannot mix"));
    }

    #[test]
    fn latched_parameters_join_declared_machine_words_only() {
        let latched = |owner: &str, word: u32| {
            let mut r = Rb::new("dk_desc_param_latched");
            r.text("owner", owner).num("word", word);
            r
        };
        let m = machine("pluck");
        let a = param("pluck", "a", 0);
        let b = param("pluck", "b", 1);
        let latch = latched("pluck", 0);
        let d = read(&obj(&all(&[&m, &a, &b, &latch]))).unwrap();
        assert!(d.params[0].latched);
        assert!(!d.params[1].latched);
        assert!(err_of(&[&m, &a, &latch, &latch]).contains("two latched declarations"));
        assert!(err_of(&[&m, &a, &latched("absent", 0)]).contains("no param at word 0"));
        assert!(err_of(&[&m, &a, &latched("pluck", 1)]).contains("no param at word 1"));
    }

    #[test]
    fn a_glyph_joins_the_param_at_its_word() {
        let m = machine("pluck");
        let recs = [
            &m,
            &param("pluck", "pk_b", 1),
            &param("pluck", "pk_a", 0),
            &glyph("pluck", 1, 4),
        ];
        let d = read(&obj(&all(&recs))).unwrap();
        assert_eq!(d.params[0].glyph, None);
        assert_eq!(d.params[1].glyph, Some(4));
        // an effect owner, no param at that word, two glyphs on one, a value that is no glyph
        let e = err_of(&[
            &m,
            &fx("fshift"),
            &param("pluck", "a", 0),
            &glyph("fshift", 0, 1),
        ]);
        assert!(e.contains("not supported yet"), "{e}");
        let e = err_of(&[&m, &param("pluck", "a", 0), &glyph("pluck", 1, 1)]);
        assert!(e.contains("no param at word 1"), "{e}");
        let e = err_of(&[
            &m,
            &param("pluck", "a", 0),
            &glyph("pluck", 0, 1),
            &glyph("pluck", 0, 2),
        ]);
        assert!(e.contains("two glyphs"), "{e}");
        let e = err_of(&[&m, &param("pluck", "a", 0), &glyph("pluck", 0, 9)]);
        assert!(e.contains("unknown glyph 9"), "{e}");
    }

    #[test]
    fn mistakes_in_the_declarations_are_refused() {
        let m = machine("pluck");
        // a gap in the words of a machine
        let e = err_of(&[&m, &param("pluck", "a", 0), &param("pluck", "b", 2)]);
        assert!(e.contains("gap") && e.contains("word 1"), "{e}");
        // a word twice
        let e = err_of(&[&m, &param("pluck", "a", 0), &param("pluck", "b", 0)]);
        assert!(e.contains("declared twice"), "{e}");
        // a param id twice, and a param of no machine
        let e = err_of(&[&m, &param("pluck", "a", 0), &param("pluck", "a", 1)]);
        assert!(e.contains("param a is declared twice"), "{e}");
        let e = err_of(&[&m, &param("nope", "a", 0)]);
        assert!(e.contains("machine nope is not declared"), "{e}");
        // a page of an owner nobody declares
        let e = err_of(&[&m, &page("ghost")]);
        assert!(e.contains("owner ghost"), "{e}");
        // a page entry for a word the machine has no param for
        let mut g = page("pluck");
        g.knob(0, 5, AUTO);
        let e = err_of(&[&m, &param("pluck", "a", 0), &g]);
        assert!(e.contains("names word 5"), "{e}");
        // a machine and an effect with one id
        let e = err_of(&[&m, &fx("pluck")]);
        assert!(e.contains("machine and as an effect"), "{e}");
        // a machine without its routines
        let mut bare = machine("pluck");
        bare.text("render", "");
        assert!(err_of(&[&bare]).contains("render is empty"));
    }

    #[test]
    fn mistakes_in_an_effect_page_are_refused() {
        let (f, p, g) = fshift();
        // two pages
        let e = err_of(&[&f, &p, &g, &g]);
        assert!(e.contains("one page for now"), "{e}");
        // a param that is not on the page
        let e = err_of(&[&f, &p, &fx_param("fshift", 1, "B"), &g]);
        assert!(e.contains("word 1 is not on the page"), "{e}");
        // an entry for a missing word
        let mut g2 = page("fshift");
        g2.knob(0, 0, 0).knob(1, 3, 1);
        let e = err_of(&[&f, &p, &g2]);
        assert!(e.contains("word 3"), "{e}");
        // two entries at one position
        let mut g3 = page("fshift");
        g3.knob(0, 0, 2).knob(1, 1, 2);
        let e = err_of(&[&f, &p, &fx_param("fshift", 1, "B"), &g3]);
        assert!(e.contains("two knobs at position 2"), "{e}");
        // a position that is no encoder; an entry after a gap; a gap in the effect's words
        let mut g4 = page("fshift");
        g4.knob(0, 0, 9);
        assert!(err_of(&[&f, &p, &g4]).contains("position 9"));
        let mut g5 = page("fshift");
        g5.knob(2, 0, 0);
        assert!(err_of(&[&f, &p, &g5]).contains("gap"));
        let e = err_of(&[&f, &fx_param("fshift", 1, "B")]);
        assert!(e.contains("gap"), "{e}");
        // display that is neither percent nor bipolar
        let mut bad = fx_param("fshift", 0, "A");
        bad.num("display", 3);
        assert!(err_of(&[&f, &bad]).contains("display 3"));
    }

    #[test]
    fn a_malformed_text_field_is_refused() {
        let id = |b: &mut Vec<u8>, f: &dyn Fn(&mut [u8])| {
            let off = record(&sdkgen::desc_spec(), "dk_desc_fx").unwrap().field("id").unwrap().off;
            f(&mut b[off..off + 16]);
        };
        let broken = |f: &dyn Fn(&mut [u8])| {
            let mut b = fx("fshift").done();
            id(&mut b, f);
            read(&obj(&b)).unwrap_err().0
        };
        assert!(broken(&|s| s.fill(b'a')).contains("no NUL"));
        assert!(broken(&|s| s[10] = b'x').contains("after the NUL"));
        assert!(broken(&|s| s[0] = 0x7f).contains("printable"));
    }

    #[test]
    fn a_malformed_record_is_refused() {
        let mut b = fx("fshift").done();
        b[3] = 99;
        assert!(read(&obj(&b)).unwrap_err().0.contains("unknown record kind"));
        let mut b = fx("fshift").done();
        b[7] += 4;
        assert!(read(&obj(&b)).unwrap_err().0.contains("size"));
        // a record cut short
        let b = fx("fshift").done();
        assert!(read(&obj(&b[..b.len() - 4])).unwrap_err().0.contains("do not fit"));
        // routes with a bit that is no route
        let mut r = fx("fshift");
        r.num("routes", 8);
        assert!(err_of(&[&r]).contains("routes"));
    }

    #[test]
    fn a_relocation_or_a_sharc_object_is_refused() {
        let mut o = obj(&fx("fshift").done());
        o.relocs.push(DkoReloc {
            section: o.sections[0].name.clone(),
            offset: 0,
            rtype: "abs32".into(),
            symbol: "_x".into(),
            addend: 0,
        });
        assert!(read(&o).unwrap_err().0.contains("relocation"));
        let mut o = obj(&fx("fshift").done());
        o.arch = "sharc".into();
        assert!(read(&o).unwrap_err().0.contains("ColdFire"));
    }

    #[test]
    fn strip_takes_the_records_out() {
        // a records-only object has nothing left
        assert!(strip(&obj(&fx("fshift").done())).is_none());
        // the other sections stay; the records' symbols and relocations go
        let mut o = obj(&fx("fshift").done());
        let desc = o.sections[0].name.clone();
        o.sections.push(DkoSection {
            name: ".text".into(),
            kind: "text".into(),
            align: 2,
            size: 2,
            hex: "4e75".into(),
        });
        for (name, section) in [
            ("_desc", Some(desc.clone())),
            ("_main", Some(".text".to_string())),
        ] {
            o.symbols.push(DkoSymbol {
                name: name.into(),
                section,
                value: 0,
                global: true,
            });
        }
        o.relocs.push(DkoReloc {
            section: desc,
            offset: 0,
            rtype: "abs32".into(),
            symbol: "_main".into(),
            addend: 0,
        });
        let s = strip(&o).unwrap();
        assert_eq!(s.sections.len(), 1);
        assert_eq!(s.sections[0].name, ".text");
        assert_eq!(s.symbols.len(), 1);
        assert_eq!(s.symbols[0].name, "_main");
        assert!(s.relocs.is_empty());
    }
}
