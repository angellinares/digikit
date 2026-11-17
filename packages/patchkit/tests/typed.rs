//! Generated module files and the manifest checks over the real device models (skipped when the repository has no toolchain).

use patchkit::modgen;
use patchkit::module::Library;
use patchkit::typed::Models;
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn library() -> Option<Library> {
    Library::scan_repo_or_skip(&root())
}

/// The committed `build/gen/` files equal fresh ones (`patchkit gen` rewrites them).
#[test]
fn generated_files_are_current() {
    let Some(lib) = library() else {
        return;
    };
    let models = Models::from_dir(&root().join("devices")).unwrap();
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
        }
    }
}

/// `patchkit gen` loads a module whose object files do not exist yet (the generated headers are inputs of the build): it has no objects then.
#[test]
fn generation_needs_no_object_files() {
    let models = Models::from_dir(&root().join("devices")).unwrap();
    let dir = root().join("tests/fixtures/modules/dyn-dn2");
    let text = std::fs::read_to_string(dir.join("module.toml")).unwrap();
    let missing = |p: &str| Err(patchkit::Error(format!("{p}: no such file")));
    assert!(patchkit::module::Module::from_parts(&text, &missing).is_err());
    let m = patchkit::module::Module::from_parts_lenient(&text, &missing).unwrap();
    assert!(m.objects.is_empty() && m.manifest.machine.is_empty());
    let mut lib = Library { modules: vec![m] };
    lib.lower(&models).unwrap();
    assert!(
        modgen::generate(&lib, &models, "test/dyn-dn2")
            .unwrap()
            .is_empty()
    );
}

/// An `[[fx]]` table takes an id only; the old fields (`process`, `desc`) are refused, so an effect is never dropped without a word.
#[test]
fn a_manifest_fx_section_is_refused() {
    let dir = root().join("tests/fixtures/modules/dyn-fx-gain");
    let mut text = std::fs::read_to_string(dir.join("module.toml")).unwrap();
    text.push_str("\n[[fx]]\nid = \"gain\"\nprocess = \"_dg_process\"\ndesc = \"_dg_desc\"\n");
    let none = |p: &str| Err(patchkit::Error(format!("{p}: no such file")));
    let e = patchkit::module::Module::from_parts_lenient(&text, &none).unwrap_err();
    assert!(e.to_string().contains("unknown field"), "{e}");
}

/// A manifest that still declares params, pages or voices is refused: a module declares them in `init` (`[[machine]]` takes an id only).
#[test]
fn a_manifest_machine_section_is_refused() {
    let dir = root().join("tests/fixtures/modules/dyn-dn2");
    let base = std::fs::read_to_string(dir.join("module.toml")).unwrap();
    let none = |p: &str| Err(patchkit::Error(format!("{p}: no such file")));
    for section in ["param", "page", "voice"] {
        let text = format!("{base}\n[[{section}]]\nid = \"x\"\n");
        let e = patchkit::module::Module::from_parts_lenient(&text, &none).unwrap_err();
        assert!(
            e.to_string().contains("dk_module_ctx_add_machine") && e.to_string().contains(section),
            "{e}"
        );
    }
}
