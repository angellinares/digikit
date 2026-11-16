//! `patchkit service-check`: the layout, the one exported entry and the declarations against the manifest of a service crate.
//! The good case is the real template (`sdk/templates/service`), the bad ones are that template broken one way each.

use patchkit::svccheck::check;
use std::fs;
use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// A fresh folder for one test.
fn scratch(test: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("pk-svc-check-{}-{test}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap().flatten() {
        let p = e.path();
        let target = to.join(e.file_name());
        if p.is_dir() {
            copy_tree(&p, &target);
        } else {
            let text = fs::read_to_string(&p)
                .unwrap()
                .replace("__NAME__", "sweep")
                .replace("__CAMEL__", "Sweep")
                .replace("__KEBAB__", "sweep");
            fs::write(target, text).unwrap();
        }
    }
}

/// The template as `<scratch>/sweep`, and an `sdk.toml` with the service's row and `kinds` (key and length) as its record kinds.
fn service(test: &str, kinds: &[&str]) -> (PathBuf, PathBuf) {
    let root = scratch(test);
    let dir = root.join("sweep");
    copy_tree(&repo().join("sdk/templates/service"), &dir);
    let mut sdk = String::from(
        "[[service]]\nname = \"sweep\"\nrust = \"dkrt_svc_sweep::records::Sweep\"\n\n[records.kinds]\n",
    );
    for k in kinds {
        sdk.push_str(&format!(
            "{k} = {{ id = 9, len = 16, service = \"sweep\" }}\n"
        ));
    }
    sdk.push_str("OTHER = { id = 2, len = 16, service = \"elsewhere\" }\n");
    let sdk_path = root.join("sdk.toml");
    fs::write(&sdk_path, sdk).unwrap();
    (dir, sdk_path)
}

fn problems(dir: &Path, sdk: &Path) -> Vec<String> {
    check(dir, sdk).unwrap()
}

fn append(path: &Path, text: &str) {
    let mut s = fs::read_to_string(path).unwrap();
    s.push_str(text);
    fs::write(path, s).unwrap();
}

fn mentions(found: &[String], what: &str) -> bool {
    found.iter().any(|p| p.contains(what))
}

#[test]
fn the_template_passes() {
    let (dir, sdk) = service("good", &[]);
    assert_eq!(problems(&dir, &sdk), Vec::<String>::new());
}

#[test]
fn a_missing_file_or_folder_of_the_layout_fails() {
    for (gone, what) in [
        ("src/slots.rs", "src/slots.rs is missing"),
        ("src/records.rs", "src/records.rs is missing"),
        ("Cargo.toml", "Cargo.toml is missing"),
    ] {
        let (dir, sdk) = service("layout-file", &[]);
        fs::remove_file(dir.join(gone)).unwrap();
        assert!(
            mentions(&problems(&dir, &sdk), what),
            "{gone}: {:?}",
            problems(&dir, &sdk)
        );
    }
    let (dir, sdk) = service("layout-tests", &[]);
    fs::remove_dir_all(dir.join("tests")).unwrap();
    assert!(mentions(&problems(&dir, &sdk), "tests/ has no .rs file"));
}

#[test]
fn the_layout_ties_the_crate_name_the_modules_and_the_dsp_folder() {
    let (dir, sdk) = service("layout-name", &[]);
    let cargo = fs::read_to_string(dir.join("Cargo.toml"))
        .unwrap()
        .replace("dkrt-svc-sweep", "dkrt-svc-other");
    fs::write(dir.join("Cargo.toml"), cargo).unwrap();
    assert!(mentions(
        &problems(&dir, &sdk),
        "the layout wants `dkrt-svc-sweep`"
    ));

    let (dir, sdk) = service("layout-mod", &[]);
    fs::write(dir.join("src/lib.rs"), "#![no_std]\npub mod records;\n").unwrap();
    assert!(mentions(
        &problems(&dir, &sdk),
        "does not declare `pub mod slots;`"
    ));

    // a service with DSP handlers has the dsp/ folder, and the folder has a mod.rs
    let (dir, sdk) = service("layout-dsp", &[]);
    append(
        &dir.join("src/records.rs"),
        "impl Sweep {\n    unsafe fn dsp_admit() {}\n}\n",
    );
    assert!(mentions(&problems(&dir, &sdk), "has no src/dsp/ folder"));
    fs::create_dir(dir.join("src/dsp")).unwrap();
    let found = problems(&dir, &sdk);
    assert!(mentions(&found, "src/dsp/ has no mod.rs"), "{found:?}");
    assert!(
        mentions(&found, "does not declare `pub mod dsp;`"),
        "{found:?}"
    );
    fs::write(dir.join("src/dsp/mod.rs"), "").unwrap();
    append(&dir.join("src/lib.rs"), "pub mod dsp;\n");
    assert_eq!(problems(&dir, &sdk), Vec::<String>::new());
}

#[test]
fn the_one_exported_entry_is_dk_module_and_a_second_export_fails() {
    let (dir, sdk) = service("export-ok", &[]);
    append(
        &dir.join("src/records.rs"),
        "#[no_mangle]\npub unsafe extern \"C\" fn dk_module() {}\n",
    );
    assert_eq!(problems(&dir, &sdk), Vec::<String>::new());

    // a second row symbol next to the entry
    append(
        &dir.join("src/slots.rs"),
        "#[no_mangle]\npub static dk_service_row: u32 = 0;\n",
    );
    let found = problems(&dir, &sdk);
    assert!(mentions(&found, "exports `dk_service_row`"), "{found:?}");

    // the entry twice
    let (dir, sdk) = service("export-twice", &[]);
    append(
        &dir.join("src/records.rs"),
        "#[no_mangle]\npub unsafe extern \"C\" fn dk_module() {}\n",
    );
    append(
        &dir.join("src/slots.rs"),
        "#[unsafe(no_mangle)]\npub unsafe extern \"C\" fn dk_module() {}\n",
    );
    assert!(mentions(
        &problems(&dir, &sdk),
        "`dk_module` is defined 2 times"
    ));

    // the state a device test reads by name is not an entry
    let (dir, sdk) = service("export-state", &[]);
    append(
        &dir.join("src/slots.rs"),
        "#[no_mangle]\npub static mut DKRT_SWEEP: u32 = 0;\n",
    );
    assert_eq!(problems(&dir, &sdk), Vec::<String>::new());

    // an export under another name by attribute, and an export of a test build only
    let (dir, sdk) = service("export-name", &[]);
    append(
        &dir.join("src/slots.rs"),
        "#[export_name = \"svc_row\"]\npub static ROW: u32 = 0;\n",
    );
    assert!(mentions(&problems(&dir, &sdk), "exports `svc_row`"));
    let (dir, sdk) = service("export-test", &[]);
    append(
        &dir.join("src/slots.rs"),
        "#[cfg(test)]\nmod stubs {\n    #[no_mangle]\n    pub extern \"C\" fn a_test_stub() {}\n}\n",
    );
    assert_eq!(problems(&dir, &sdk), Vec::<String>::new());
}

#[test]
fn a_declaration_that_differs_from_the_manifest_fails() {
    // declared but not in the manifest
    let (dir, sdk) = service("decl-extra", &[]);
    let rec = fs::read_to_string(dir.join("src/records.rs")).unwrap().replace("unsafe fn declare(_d: &mut Declare) {}", "unsafe fn declare(d: &mut Declare) {\n        d.kind(KIND_SWEEP_STEP, LEN_SWEEP_STEP);\n    }");
    fs::write(dir.join("src/records.rs"), rec).unwrap();
    assert!(mentions(
        &problems(&dir, &sdk),
        "declares the record kind SWEEP_STEP, which the manifest does not give"
    ));

    // in the manifest but not declared
    let (dir, sdk) = service("decl-missing", &["SWEEP_STEP"]);
    assert!(mentions(
        &problems(&dir, &sdk),
        "gives the record kind SWEEP_STEP to `sweep`, but records.rs does not declare it"
    ));

    // declared and in the manifest: equal; another service's kind is not this one's
    let (dir, sdk) = service("decl-equal", &["SWEEP_STEP"]);
    let rec = fs::read_to_string(dir.join("src/records.rs")).unwrap().replace("unsafe fn declare(_d: &mut Declare) {}", "unsafe fn declare(d: &mut Declare) {\n        d.kind(dkrt_sdk::records::KIND_SWEEP_STEP, dkrt_sdk::records::LEN_SWEEP_STEP);\n    }");
    fs::write(dir.join("src/records.rs"), rec).unwrap();
    assert_eq!(problems(&dir, &sdk), Vec::<String>::new());

    // a length of another kind, and a declaration that has no manifest entry at all
    let (dir, sdk) = service("decl-pair", &["SWEEP_STEP"]);
    let rec = fs::read_to_string(dir.join("src/records.rs")).unwrap().replace("unsafe fn declare(_d: &mut Declare) {}", "unsafe fn declare(d: &mut Declare) {\n        d.kind(KIND_SWEEP_STEP, LEN_OTHER);\n        d.slot(1, 0);\n    }");
    fs::write(dir.join("src/records.rs"), rec).unwrap();
    let found = problems(&dir, &sdk);
    assert!(
        mentions(&found, "declares `kind(KIND_SWEEP_STEP, LEN_OTHER)`"),
        "{found:?}"
    );
    assert!(mentions(&found, "declares `slot`"), "{found:?}");
}

#[test]
fn the_service_needs_its_row_in_the_manifest_with_a_type_of_its_crate() {
    let (dir, sdk) = service("row-missing", &[]);
    fs::write(&sdk, "[records.kinds]\n").unwrap();
    assert!(mentions(
        &problems(&dir, &sdk),
        "has no [[service]] row named `sweep`"
    ));
    fs::write(
        &sdk,
        "[[service]]\nname = \"sweep\"\nrust = \"crate::services::sweep::Sweep\"\n",
    )
    .unwrap();
    assert!(mentions(
        &problems(&dir, &sdk),
        "the type is not in the crate `dkrt_svc_sweep`"
    ));
}

/// The four service crates of the runtime follow the layout today.
#[test]
fn the_service_crates_of_the_repository_pass() {
    let sdk = repo().join("sdk/sdk.toml");
    for name in ["fx", "machines", "samples", "lfo"] {
        let dir = repo().join("modules/core/services").join(name);
        assert_eq!(problems(&dir, &sdk), Vec::<String>::new(), "{name}");
    }
}
