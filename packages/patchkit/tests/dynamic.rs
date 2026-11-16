//! Run-time modules: `dynamic = true` modules, the `.dkm` register records, the reserved rows of `core/dynreg`
//! (docs/design/digikit-registry.md). The image tests are skipped when the stock images are absent.

use patchkit::apply;
use patchkit::module::{AllocTable, Library};
use patchkit::port::Registry;
use patchkit::typed::Models;
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn library() -> Option<Library> {
    let mut lib = Library::scan_repo_or_skip(&root())?;
    lib.lower(&Models::from_dir(&root().join("devices")).unwrap())
        .unwrap();
    Some(lib)
}

#[test]
fn a_dynamic_module_is_not_lowered_and_not_patched() {
    let Some(lib) = library() else {
        return;
    };
    let m = lib.get("test/sine").unwrap();
    assert!(m.manifest.module.dynamic);
    assert!(
        m.manifest.entry.is_empty(),
        "no registry rows are made for a run-time module"
    );
    let reg = Registry::from_dir(&root().join("devices/ports")).unwrap();
    for file in ["Digitakt_II_OS1.17.syx"] {
        let Ok(raw) = std::fs::read(root().join("firmware").join(file)) else {
            continue;
        };
        let e = apply::apply(&raw, &reg, &lib, &["test/sine".to_string()])
            .err()
            .expect("refused");
        assert!(e.0.contains("run-time module"), "{e}");
    }
}

#[test]
fn the_run_time_type_range_is_checked() {
    let ok =
        "[dt2]\nlast = 23\ndynamic_first = 16\n[dt2.alloc]\n\"a:x\" = 7\n\"core/dynreg:d\" = 16\n";
    assert!(AllocTable::parse(ok).is_ok());
    let static_in_range = ok.replace("\"a:x\" = 7", "\"a:x\" = 18");
    assert!(
        AllocTable::parse(&static_in_range)
            .unwrap_err()
            .0
            .contains("run-time range")
    );
    // run-time machines have no number in the table any more: `[dt2.dynamic]` is refused
    let old = format!("{ok}[dt2.dynamic]\n\"m:y\" = 17\n");
    assert!(AllocTable::parse(&old).is_err());
}

/// The framework reserves 8 machine types (16 .. 23) and 16 parameter codes, with the runtime's table of addresses; the
/// runtime alone still patches (its link to the table is optional).
#[test]
fn dynreg_reserves_rows_and_the_runtime_links_optionally() {
    let Some(lib) = library() else {
        return;
    };
    let reg = Registry::from_dir(&root().join("devices/ports")).unwrap();
    let mut ran = 0;
    for file in ["Digitakt_II_OS1.17.syx"] {
        let Ok(raw) = std::fs::read(root().join("firmware").join(file)) else {
            continue;
        };
        let want = |ids: &[&str]| -> Vec<String> {
            lib.with_requires(&ids.iter().map(|s| s.to_string()).collect::<Vec<_>>())
                .unwrap()
                .0
        };
        let out = apply::apply(&raw, &reg, &lib, &want(&["core/runtime", "core/dynreg"])).unwrap();
        let plan = serde_json::to_value(&out.plan).unwrap();
        let sym = |n: &str| -> u64 {
            plan["symbols"]
                .as_object()
                .unwrap()
                .iter()
                .find(|(k, _)| k.split(':').nth(1) == Some(n))
                .unwrap_or_else(|| panic!("no symbol {n}"))
                .1
                .as_u64()
                .unwrap()
        };
        assert_eq!(sym("machines.dyn16.index"), 16);
        assert_eq!(sym("machines.dyn23.index"), 23);
        assert_eq!(sym("machines.count"), 17, "types 7 .. 23");
        assert_eq!(sym("params.dp15.index"), sym("params.dp0.index") + 15);
        assert!(sym("dynreg_info") > 0x4000_0000);
        // the runtime alone: the optional link resolves to 0, nothing else changes
        let alone = apply::apply(&raw, &reg, &lib, &want(&["core/runtime"])).unwrap();
        let alone_plan = serde_json::to_value(&alone.plan).unwrap();
        assert!(
            alone_plan["symbols"]
                .as_object()
                .unwrap()
                .keys()
                // (the runtime has functions of its own with `dynreg` in the name, `dkc_dynreg` of the service imports: look for the module's table)
                .all(|k| k.split(':').nth(1) != Some("dynreg_info"))
        );
        ran += 1;
    }
    eprintln!("dynreg: {ran} images");
}
