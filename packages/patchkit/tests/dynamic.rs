//! Run-time modules: `dynamic = true` modules, the `.dkm` register records, the reserved rows of `core/dynreg`
//! (docs/design/digikit-registry.md). The image tests are skipped when the stock images are absent.

use patchkit::apply;
use patchkit::dkm;
use patchkit::module::{AllocTable, Library};
use patchkit::port::Registry;
use patchkit::register::{KNOB_PARAM, Register, machine_hash, machine_key};
use patchkit::typed::{Models, resolve};
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
fn the_register_records_are_the_lowered_typed_sections() {
    let Some(lib) = library() else {
        return;
    };
    let m = lib.get("test/sine").unwrap();
    let models = Models::from_dir(&root().join("devices")).unwrap();
    let res = resolve(&m.manifest, &models).unwrap();
    // one DSP body for both devices
    let dev = |d: &str| res.iter().find(|r| r.dev == d).unwrap();
    assert_eq!(res.len(), 2);
    let reg = Register::lower(dev("dt2"), "test/sine").unwrap();
    assert_eq!(reg.device, 1);
    assert_eq!(reg.machines.len(), 1);
    let mach = &reg.machines[0];
    assert_eq!(
        (mach.type_no, mach.clone_of, mach.position, mach.level),
        (machine_key("test/sine", "sine"), 0, 255, 10)
    );
    // knobs on the Digitakt: SAMPLE's tune and level rows, the module's parameter (index 0) at the first free position
    assert_eq!(
        mach.knobs,
        [0xca, KNOB_PARAM, 0, 0, 0, 0, 0, 0xd1]
    );
    let p = &reg.params[0];
    assert_eq!((p.template, p.slot, p.min, p.max), (0xd1, 1, 0, 0x7f00));
    // the Digitone: a page holds the parameter, the machine row none
    let reg2 = Register::lower(dev("dn2"), "test/sine").unwrap();
    assert_eq!((reg2.device, reg2.machines[0].own_dsp), (2, 1));
    assert_eq!(reg2.machines[0].knobs, [0; 8]);
    assert_eq!(reg2.pages.len(), 1);
    // word 0 sits on knob B (`param_slots[0] = 1`), so knob A stays free and the slot is the base 25 plus 1
    assert_eq!(reg2.pages[0].knobs[..2], [0, KNOB_PARAM]);
    assert_eq!((reg2.params[0].template, reg2.params[0].slot), (0xd5, 26));
    // the file: a stub image, the records, and the same bytes every time
    let opts = dkm::Options {
        name: "test/sine".into(),
        version: 0x000100,
        runtime_min: 1,
        trace: false,
        stereo: false,
        api_minor: 0,
    };
    let a = dkm::pack_with(None, &reg.encode(), &opts).unwrap();
    assert_eq!(a, dkm::pack_with(None, &reg.encode(), &opts).unwrap());
    let d = dkm::Dkm::parse(&a).unwrap();
    assert_eq!(d.init, Some(0));
    assert_eq!(Register::parse(&d.register).unwrap(), reg);
    // the key is the 24-bit FNV-1a hash of "<module id>:<machine id>" above a zero type byte (D7)
    assert_eq!(mach.type_no & 0xff, 0);
    assert_eq!(machine_hash("a", "b"), 0xbd8540);
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
                .all(|k| !k.contains("dynreg"))
        );
        ran += 1;
    }
    eprintln!("dynreg: {ran} images");
}

/// A machine key keeps the low byte 0: the runtime assigns the type number at load.
#[test]
fn a_machine_key_has_no_type_byte() {
    let Some(lib) = library() else {
        return;
    };
    let models = Models::from_dir(&root().join("devices")).unwrap();
    let m = lib.get("test/sine").unwrap();
    let res = resolve(&m.manifest, &models).unwrap();
    let r = res.iter().find(|r| r.dev == "dt2").unwrap();
    assert_eq!(
        Register::lower(r, "test/sine").unwrap().machines[0].type_no,
        machine_hash("test/sine", "sine") << 8
    );
}
