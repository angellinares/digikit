//! Generator for the typed module sections (`typed.rs`): from the machine records of one module, per module,
//!
//! - `build/gen/gallery.json`: machines with their knobs and parameters, per device, for the gallery.
//!
//! The output is committed; `patchkit gen --check` fails when a committed file differs from a fresh one. It carries
//! no firmware bytes: only the numbers the module itself declares and the device models.

use crate::error::*;
use crate::module::Library;
use crate::typed::{DevResolved, Knob, Models, RMachine, resolve};

/// One generated file: path relative to the module directory, and its text.
pub type GenFile = (String, String);

/// The permanent type number of a machine (`core/machines` types.toml), if the framework is in the library.
fn type_number(lib: &Library, dev: &str, module: &str, machine: &str) -> Option<i64> {
    let d = lib
        .get("core/machines")?
        .alloc
        .get("machines")?
        .devices
        .get(dev)?;
    let key = format!("{module}:{machine}");
    d.alloc.get(&key).copied()
}

fn gallery(lib: &Library, module: &str, rs: &[DevResolved]) -> Result<String> {
    use serde_json::{Map, Value, json};
    let mut machines: Vec<Value> = vec![];
    let mut ids: Vec<String> = vec![];
    for r in rs {
        for m in &r.machines {
            if !ids.contains(&m.id) {
                ids.push(m.id.clone());
            }
        }
    }
    for id in &ids {
        let mut devs = Map::new();
        let mut first: Option<&RMachine> = None;
        for r in rs {
            let Some(m) = r.machines.iter().find(|m| &m.id == id) else {
                continue;
            };
            first.get_or_insert(m);
            let knobs: Vec<Value> = m
                .knobs
                .iter()
                .enumerate()
                .map(|(j, k)| match k {
                    Knob::Code(0) => Value::Null,
                    Knob::Code(c) => {
                        json!({"slot": r.model.knobs.slot_base + j as i64, "stock": c})
                    }
                    Knob::Param(p) => {
                        json!({"slot": r.model.knobs.slot_base + j as i64, "param": p})
                    }
                })
                .collect();
            let page = r
                .pages
                .iter()
                .find(|p| &p.machine == id)
                .map(|p| json!({"name": p.name, "title": p.title}));
            devs.insert(
                r.dev.clone(),
                json!({
                    "name": m.name, "short": m.short, "type": type_number(lib, &r.dev, module, id),
                    "clone_of": m.clone_of, "position": Value::Null, "own_dsp": m.own_dsp,
                    "level": m.level, "knobs": knobs, "page": page,
                }),
            );
        }
        let m = first.unwrap();
        machines.push(json!({"id": id, "name": m.name, "short": m.short, "devices": devs}));
    }
    let mut params = Map::new();
    for r in rs {
        let list: Vec<Value> = r
            .params
            .iter()
            .map(|p| {
                json!({"id": p.id, "name": p.name, "short": p.short, "min": p.min, "max": p.max,
                       "default": p.default, "slot": p.slot, "template": p.template})
            })
            .collect();
        params.insert(r.dev.clone(), Value::Array(list));
    }
    Ok(serde_json::to_string_pretty(
        &json!({"module": module, "machines": machines, "params": params}),
    )? + "\n")
}

/// The generated files of one module (empty: the module has no typed sections).
pub fn generate(lib: &Library, models: &Models, module: &str) -> Result<Vec<GenFile>> {
    let m = lib
        .get(module)
        .ok_or_else(|| err!("unknown module {module}"))?;
    if !crate::typed::has_typed(&m.manifest) {
        return Ok(vec![]);
    }
    let rs = resolve(&m.manifest, models)?;
    Ok(vec![(
        "build/gen/gallery.json".to_string(),
        gallery(lib, module, &rs)?,
    )])
}

/// The generated files of several modules, with their full paths. The library must hold the objects of these modules: a content
/// module declares its machines in its objects, so without them it has no typed sections and generates nothing.
pub fn generate_many(
    lib: &Library,
    models: &Models,
    modules: &[String],
) -> Result<Vec<(std::path::PathBuf, String)>> {
    let mut out = vec![];
    for id in modules {
        let m = lib.get(id).ok_or_else(|| err!("unknown module {id}"))?;
        for (rel, text) in generate(lib, models, id)? {
            out.push((m.dir.join(rel), text));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module::Module;
    use crate::typed::{MachineDef, ParamDef, PageDef, PageKnob, VoiceDef};

    const HEAD: &str = "[module]\nid = \"machines/t\"\nversion = \"0.1.0\"\nname = \"T\"\ncategory = \"machines\"\ntype = \"machine\"\nauthor = \"digikit\"\nstatus = \"experimental\"\nsummary = \"t.\"\n";

    fn models() -> Models {
        Models::from_texts([
            include_str!("../../../devices/digitakt-ii.toml"),
            include_str!("../../../devices/digitone-ii.toml"),
        ])
        .unwrap()
    }

    fn lib_of(typed: bool) -> (Library, Models) {
        let mut m = Module::from_parts(HEAD, &|_| Err(err!("no files"))).unwrap();
        if typed {
            // a run-time module: its voice needs no DSP object here (a patched-in one gets its state block from that object)
            m.manifest.module.dynamic = true;
            m.manifest.machine = vec![MachineDef {
                id: "t".into(),
                name: "T".into(),
                short: "T".into(),
                stereo: false,
            }];
            m.manifest.param = (0..2)
                .map(|w| ParamDef {
                    id: format!("t_{w}"),
                    machine: "t".into(),
                    word: w,
                    name: "P".into(),
                    short: "P".into(),
                    min: 0,
                    max: 100,
                    default: 50,
                    action: None,
                    glyph: None,
                })
                .collect();
            m.manifest.page = vec![PageDef {
                id: "t_p0".into(),
                machine: "t".into(),
                name: "T".into(),
                title: "T".into(),
                knobs: vec![
                    PageKnob { word: 0, position: None },
                    PageKnob { word: 1, position: None },
                ],
            }];
            m.manifest.voice = vec![VoiceDef {
                machine: "t".into(),
                render: "_r".into(),
                note_on: "_n".into(),
                desc: "_d".into(),
                stereo: false,
            }];
        }
        let models = models();
        let mut lib = Library { modules: vec![m] };
        lib.lower(&models).unwrap();
        (lib, models)
    }

    #[test]
    fn the_gallery_follows_the_device_models() {
        let (lib, models) = lib_of(true);
        let files = generate(&lib, &models, "machines/t").unwrap();
        assert_eq!(files.len(), 1);
        let g: serde_json::Value = serde_json::from_str(&files[0].1).unwrap();
        assert_eq!(g["machines"][0]["devices"]["dt2"]["knobs"][2]["param"], "t_1");
        assert_eq!(g["machines"][0]["devices"]["dn2"]["page"]["title"], "T");
        // a parameter's slot is the device's slot base plus its knob position from `param_slots`: words 0 and 1 sit on B and C
        assert_eq!(g["params"]["dn2"][0]["slot"], 26);
        assert_eq!(g["params"]["dn2"][1]["slot"], 27);
        assert_eq!(g["params"]["dt2"][1]["slot"], 2);
    }

    #[test]
    fn a_module_without_typed_sections_generates_nothing() {
        let (lib, models) = lib_of(false);
        assert!(generate(&lib, &models, "machines/t").unwrap().is_empty());
    }
}
