//! Typed module sections (the `DK_MACHINE`, `DK_PARAM` and `DK_PAGE` records of a module) over the real device models, ports and
//! stock images (the image tests are skipped when the images are absent).
//!
//! The pins are the SHA-256 of every section except 7 of the images that `test/patched-pluck` produces; its records must lower to
//! the same rows on every port the module supports. Section 7 (the SHARC DSP stream) is left out: it moves with every DSP edit to pluck
//! or core/dsp-voice, which the conformance, PCM and scenario tests guard (the whole-image pins moved seven times with section 7 only).

use patchkit::apply;
use patchkit::error::sha256_hex;
use patchkit::firmware::Firmware;
use patchkit::modgen;
use patchkit::module::Library;
use patchkit::port::Registry;
use patchkit::typed::Models;
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn library() -> Option<Library> {
    Library::scan_repo_or_skip(&root())
}

/// SHA-256 over the id, length and data of every section except 7 (the SHARC DSP stream), in container order.
fn digest_without_dsp(syx: &[u8]) -> String {
    let fw = Firmware::parse(syx).unwrap();
    let mut buf = Vec::new();
    for s in fw.sections.iter().filter(|s| s.id != 7) {
        buf.extend_from_slice(&s.id.to_be_bytes());
        buf.extend_from_slice(&(s.data.len() as u64).to_be_bytes());
        buf.extend_from_slice(&s.data);
    }
    sha256_hex(&buf)
}

/// (stock image, modules, digest_without_dsp of the patched image); moved 2026-10-20 with the id renames (P3c-3a) and 2026-10-21 with the move of a plucked-string machine to test/patched-pluck (P3e-1): patch record only; DN2 pin moved 2026-11-02 with the shared knob map (param_slots [1, 2, 4, 5, 6, 0, 3, 7]): the page knobs sit on B C E F; both pins moved 2026-11-05 with native machine pages (the machines adapter patches the native-page hooks)
const PINS: &[(&str, &str, &str)] = &[
    (
        "Digitakt_II_OS1.17.syx",
        "test/patched-pluck",
        "6dfcfde3d23ee91b65b6bdf3ad1b129cd8aa1b948a25e574951d78330826a970",
    ),
    (
        "Digitone_II_OS1.12.syx",
        "test/patched-pluck",
        "f17f811559afebb4a17ce77e48980977a893118155215f23acf139d4878b8e2e",
    ),
];

#[test]
fn declared_machines_patch_to_the_pinned_images() {
    let Some(lib) = library() else {
        return;
    };
    let reg = Registry::from_dir(&root().join("devices/ports")).unwrap();
    let mut ran = 0;
    for (file, mods, want) in PINS {
        let Ok(raw) = std::fs::read(root().join("firmware").join(file)) else {
            continue;
        };
        let ids: Vec<String> = mods.split(',').map(String::from).collect();
        let (ids, _) = lib.with_requires(&ids).unwrap();
        let out = apply::apply(&raw, &reg, &lib, &ids).unwrap();
        assert_eq!(
            &digest_without_dsp(&out.syx),
            want,
            "{file} + {mods}: a section other than 7 changed; if intended, set the pin to the left value"
        );
        ran += 1;
    }
    eprintln!("typed oracle: {ran} of {} pins ran", PINS.len());
}

/// The committed `build/gen/` files equal fresh ones (`patchkit gen` rewrites them).
#[test]
fn generated_files_are_current() {
    let Some(lib) = library() else {
        return;
    };
    let models = Models::from_dir(&root().join("devices")).unwrap();
    let mut n = 0;
    for m in &lib.modules {
        for (rel, text) in modgen::generate(&lib, &models, m.id()).unwrap() {
            let path = m.dir.join(&rel);
            let have = std::fs::read_to_string(&path).unwrap_or_default();
            assert_eq!(
                have,
                text,
                "{} is stale: run `patchkit gen`",
                path.display()
            );
            n += 1;
        }
    }
    assert!(n >= 1, "pluck generates a gallery, got {n}");
}

/// What pluck's DSP sources hard-code must be what the device models say (finding 04: frame words of the machine
/// slots; pluck.s: `0x6d + 48 t`, offsets 1, 2, 6, 7; pluck_dn2.s: `0x6d + 0x49 v`, offsets 1, 2, 4, 5).
#[test]
fn device_models_match_the_hard_coded_frame_layout_of_pluck() {
    let Some(lib) = library() else {
        return;
    };
    let models = Models::from_dir(&root().join("devices")).unwrap();
    let m = lib.get("test/patched-pluck").unwrap();
    let res = patchkit::typed::resolve(&m.manifest, &models).unwrap();
    let layout = |dev: &str| {
        let r = res.iter().find(|r| r.dev == dev).unwrap();
        let p: Vec<_> = r
            .params
            .iter()
            .map(|p| (p.id.as_str(), p.frame_off, p.mirror, p.slot))
            .collect();
        (r.model.frame.base, r.model.frame.stride, p)
    };
    let (base, stride, p) = layout("dt2");
    assert_eq!((base, stride), (109, 48));
    let offs: Vec<_> = p.iter().map(|x| (x.0, x.1)).collect();
    assert_eq!(
        offs,
        [
            ("pluck_decay", 1),
            ("pluck_pick", 2),
            ("pluck_bright", 6),
            ("pluck_color", 7)
        ]
    );
    assert_eq!((p[0].2, p[3].2), (26, 32));
    let (_, stride, p) = layout("dn2");
    assert_eq!(stride, 73);
    assert_eq!((p[0].1, p[3].1), (1, 5)); // knobs B and F: `param_slots` [1, 2, 4, 5, ...]
    assert_eq!((p[0].3, p[3].3), (26, 30));
    let files = modgen::generate(&lib, &models, "test/patched-pluck").unwrap();
    let g: serde_json::Value = serde_json::from_str(&files[0].1).unwrap();
    let dt2 = &g["machines"][0]["devices"]["dt2"];
    assert_eq!(dt2["type"], 9);
    assert_eq!(dt2["knobs"][1]["param"], "pluck_decay");
}

/// `patchkit gen` loads a module whose object files do not exist yet (the generated headers are inputs of the build): it has no records then.
#[test]
fn generation_needs_no_object_files() {
    let models = Models::from_dir(&root().join("devices")).unwrap();
    let dir = root().join("tests/fixtures/modules/patched-pluck");
    let text = std::fs::read_to_string(dir.join("module.toml")).unwrap();
    let missing = |p: &str| Err(patchkit::Error(format!("{p}: no such file")));
    assert!(patchkit::module::Module::from_parts(&text, &missing).is_err());
    let m = patchkit::module::Module::from_parts_lenient(&text, &missing).unwrap();
    assert!(m.objects.is_empty() && m.manifest.machine.is_empty());
    let mut lib = Library { modules: vec![m] };
    lib.lower(&models).unwrap();
    assert!(modgen::generate(&lib, &models, "test/patched-pluck").unwrap().is_empty());
}

/// The bundle path of the browser: the same lowering from texts (ports, manifests, objects, device files).
#[test]
fn bundle_lowering_matches_the_scan() {
    let mut texts = vec![];
    for f in ["digitakt-ii.toml", "digitone-ii.toml"] {
        texts.push(std::fs::read_to_string(root().join("devices").join(f)).unwrap());
    }
    let models = Models::from_texts(texts.iter().map(|s| s.as_str())).unwrap();
    let Some(scanned) = library() else {
        return;
    };
    let dir = root().join("tests/fixtures/modules/patched-pluck");
    let text = std::fs::read_to_string(dir.join("module.toml")).unwrap();
    // the objects of a content module are in the cache (the scan above built them); the gallery bundle holds them as texts
    let cache = root().join("out/cache/dko/test/patched-pluck");
    let read = |p: &str| {
        std::fs::read_to_string(cache.join(p)).map_err(|e| patchkit::Error(e.to_string()))
    };
    let m = patchkit::module::Module::from_parts(&text, &read).unwrap();
    let mut lib = Library { modules: vec![m] };
    lib.lower(&models).unwrap();
    let a: Vec<_> = lib
        .get("test/patched-pluck")
        .unwrap()
        .manifest
        .entry
        .iter()
        .map(|e| (&e.registry, &e.id, &e.devices, &e.fields))
        .collect();
    let b: Vec<_> = scanned
        .get("test/patched-pluck")
        .unwrap()
        .manifest
        .entry
        .iter()
        .map(|e| (&e.registry, &e.id, &e.devices, &e.fields))
        .collect();
    assert_eq!(a, b);
    assert_eq!(
        a.len(),
        1 + 1 + 4 + 1 + 1 + 4 + 1,
        "dt2: voice, machine, 4 params; dn2: voice, machine, 4 params, page"
    );
}

/// A manifest that still declares `[[fx]]` is refused, so an effect is never dropped without a word.
#[test]
fn a_manifest_fx_section_is_refused() {
    let dir = root().join("tests/fixtures/modules/dyn-fx-gain");
    let mut text = std::fs::read_to_string(dir.join("module.toml")).unwrap();
    text.push_str("\n[[fx]]\nid = \"gain\"\nprocess = \"_dg_process\"\ndesc = \"_dg_desc\"\n");
    let none = |p: &str| Err(patchkit::Error(format!("{p}: no such file")));
    let e = patchkit::module::Module::from_parts_lenient(&text, &none).unwrap_err();
    assert!(e.to_string().contains("DK_FX"), "{e}");
}

/// A manifest that still declares machines, params, pages or voices is refused: they are DK_MACHINE, DK_PARAM and DK_PAGE records now.
#[test]
fn a_manifest_machine_section_is_refused() {
    let dir = root().join("tests/fixtures/modules/patched-pluck");
    let base = std::fs::read_to_string(dir.join("module.toml")).unwrap();
    let none = |p: &str| Err(patchkit::Error(format!("{p}: no such file")));
    for section in ["machine", "param", "page", "voice"] {
        let text = format!("{base}\n[[{section}]]\nid = \"x\"\n");
        let e = patchkit::module::Module::from_parts_lenient(&text, &none).unwrap_err();
        assert!(
            e.to_string().contains("DK_MACHINE") && e.to_string().contains(section),
            "{e}"
        );
    }
}
