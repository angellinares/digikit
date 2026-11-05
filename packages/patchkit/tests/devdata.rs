//! Device data: the constants `patchkit gen` writes from the device files agree with them (the files are committed, and a
//! stale one fails here as it fails `patchkit gen --check`), a fact that devices disagree on is refused for the shared runtime,
//! and the device list of the code is the device files' list.

use patchkit::devdata;
use patchkit::devgen::{self, Facts};
use patchkit::modgen;
use patchkit::module::Library;
use patchkit::port::Registry;
use patchkit::typed::Models;
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn every_generated_file_is_current() {
    let Some(mut lib) = Library::scan_repo_or_skip(&root()) else {
        return;
    };
    let models = Models::from_dir(&root().join("devices")).unwrap();
    lib.lower(&models).unwrap();
    let facts = Facts::from_dir(&root().join("devices")).unwrap();
    let mut n = 0;
    for m in &lib.modules {
        let mut files = modgen::generate(&lib, &models, m.id()).unwrap();
        files.extend(devgen::generate(&lib, &facts, m.id()).unwrap());
        for (rel, text) in files {
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
    assert!(n > 20, "only {n} generated files");
}

#[test]
fn the_runtime_constants_are_the_device_files_numbers() {
    let Some(mut lib) = Library::scan_repo_or_skip(&root()) else {
        return;
    };
    lib.lower(&Models::from_dir(&root().join("devices")).unwrap())
        .unwrap();
    let facts = Facts::from_dir(&root().join("devices")).unwrap();
    let files = devgen::generate(&lib, &facts, "core/runtime").unwrap();
    let get = |p: &str| &files.iter().find(|f| f.0 == p).unwrap().1;
    let rs = get("sdk/src/device.rs");
    // the numbers of devices/*.toml [board] that both devices share (the store is per device: below)
    assert!(
        !rs.contains("STORE_FIRST"),
        "the store area differs per device"
    );
    for want in [
        "DYN_TYPE_FIRST: u32 = 0x10;",
        "DYN_TYPE_LAST: u32 = 0x17;",
        "SAFE_KEY_CODE: u32 = 0x13;",
        "DSP_BLOCK_CYCLES: u32 = 0xa2c2b;",
        "KEEP_BASE: u32 = 0x47fe0000;",
        "KEEP_BYTES: u32 = 0x1000;",
    ] {
        assert!(rs.contains(want), "{want}");
    }
    // the effect slots differ per device (the Digitakt II reserves 3 run-time effect rows, core/dynfx): not in the shared file
    assert!(
        !rs.contains("LIMIT_FX_SLOTS"),
        "the effect slots differ per device"
    );
    let dn2 = get("build/gen/device-dn2.inc");
    assert!(dn2.contains(".equ DEVICE_ID, 0x2 "), "{dn2}");
    assert!(dn2.contains(".equ LIMIT_FX_SLOTS, 0x3 "), "{dn2}");
    assert!(
        get("build/gen/device-dt2.inc").contains(".equ LIMIT_FX_SLOTS, 0x3 "),
        "dt2"
    );
    // the store area of each device ([card.store], docs/design/digikit-store.md): DN2 from 0x800000, DT2 after the +Drive, both to the end of the card
    for (inc, first, sectors) in [
        (dn2, "0x800000", "0x261a000"),
        (get("build/gen/device-dt2.inc"), "0x2df1000", "0x29000"),
    ] {
        assert!(
            inc.contains(&format!(".equ STORE_FIRST, {first} ")),
            "{inc}"
        );
        assert!(
            inc.contains(&format!(".equ STORE_SECTORS, {sectors} ")),
            "{inc}"
        );
        assert!(
            inc.contains(".equ STORE_HEADER_B, ")
                && inc.contains(".equ STORE_SEL_A, 0x2e19ffe ")
                && inc.contains(".equ STORE_SEL_B, 0x2e19fff "),
            "{inc}"
        );
    }
    assert!(dn2.contains(".equ DL_AREA_BYTES, 0x50000 "), "{dn2}");
    // the PEEK ranges are per device (the runtime's region differs), up to four, unused ones 0, 0
    assert!(dn2.contains(".equ PEEK_N, 0x2 "), "{dn2}");
    assert!(dn2.contains(".equ PEEK_LO_1, 0x46712000 "), "{dn2}");
    assert!(dn2.contains(".equ PEEK_HI_3, 0x0 "), "{dn2}");
    let dt2 = get("build/gen/device-dt2.inc");
    assert!(
        dt2.contains(".equ PEEK_LO_1, 0x47e8b000 ") && dt2.contains(".equ PEEK_HI_1, 0x47fe1000 "),
        "{dt2}"
    );
}

#[test]
fn peek_and_keep_ranges_are_checked() {
    let dt2 = std::fs::read_to_string(root().join("devices/digitakt-ii.toml")).unwrap();
    let dn2 = std::fs::read_to_string(root().join("devices/digitone-ii.toml")).unwrap();
    for (what, bad) in [
        (
            "an empty range",
            dt2.replace("[0x40000000, 0x47e30000]", "[0x40000000, 0x40000000]"),
        ),
        (
            "an unaligned range",
            dt2.replace("[0x40000000, 0x47e30000]", "[0x40000001, 0x47e30000]"),
        ),
        (
            "five ranges",
            dt2.replace("peek = [", "peek = [[4, 8], [8, 12], [12, 16], [16, 20], "),
        ),
        (
            "a keep area that is not a page multiple",
            dt2.replace("keep_bytes = 0x1000", "keep_bytes = 0x800"),
        ),
    ] {
        assert_ne!(bad, dt2, "{what}: the test text did not change");
        assert!(
            Facts::from_texts([bad.as_str(), dn2.as_str()]).is_err(),
            "{what} is accepted"
        );
    }
}

#[test]
fn a_fact_that_devices_disagree_on_is_refused_for_the_shared_runtime() {
    let Some(mut lib) = Library::scan_repo_or_skip(&root()) else {
        return;
    };
    lib.lower(&Models::from_dir(&root().join("devices")).unwrap())
        .unwrap();
    let dt2 = std::fs::read_to_string(root().join("devices/digitakt-ii.toml")).unwrap();
    let dn2 = std::fs::read_to_string(root().join("devices/digitone-ii.toml")).unwrap();
    // move the Digitone's safe key (a fact the shared runtime image has once)
    let moved = dn2.replace("safe_key = 19", "safe_key = 20");
    assert_ne!(moved, dn2);
    let facts = Facts::from_texts([dt2.as_str(), moved.as_str()]).unwrap();
    let e = devgen::generate(&lib, &facts, "core/runtime")
        .unwrap_err()
        .0;
    assert!(
        e.contains("SAFE_KEY_CODE") && e.contains("one image for all devices"),
        "{e}"
    );
    // per-device files (the device objects) are still fine for numbers that differ
    let ok = Facts::from_texts([dt2.as_str(), dn2.as_str()]).unwrap();
    assert!(devgen::generate(&lib, &ok, "core/dsp-link").is_ok());
}

#[test]
fn the_code_knows_the_devices_of_the_device_files() {
    // every port names a device the code knows, and the limits a port records are the device's `[patch]` limits
    let reg = Registry::from_dir(&root().join("devices/ports")).unwrap();
    assert!(!reg.ports.is_empty());
    for p in &reg.ports {
        let d = devdata::by_short(&p.port.device).unwrap_or_else(|| panic!("{}", p.id()));
        assert_eq!(p.limits.container_max, d.container_max, "{}", p.id());
        assert_eq!(p.limits.dsp_stored_max, d.dsp_stored_max, "{}", p.id());
        assert_eq!(p.limits.dsp_depacked_max, d.dsp_depacked_max, "{}", p.id());
        assert_eq!(p.limits.main_staged_max, d.main_staged_max, "{}", p.id());
    }
    // ids are unique per kind
    let all = devdata::all();
    for (i, a) in all.iter().enumerate() {
        for b in &all[i + 1..] {
            assert_ne!((a.syx_id, a.digikit_id), (b.syx_id, b.digikit_id));
            assert_ne!(a.digikit_id, b.digikit_id);
        }
    }
}

fn device_texts() -> (String, String) {
    (
        std::fs::read_to_string(root().join("devices/digitakt-ii.toml")).unwrap(),
        std::fs::read_to_string(root().join("devices/digitone-ii.toml")).unwrap(),
    )
}

#[test]
fn a_limit_above_the_contract_maximum_is_refused() {
    let (dt2, dn2) = device_texts();
    // the committed files are within the maxima
    Facts::from_texts([dt2.as_str(), dn2.as_str()]).unwrap();
    for (what, from, to) in [
        ("modules", "modules = 8", "modules = 9"),
        ("fx_slots", "fx_slots = 3", "fx_slots = 5"),
        ("send_slots", "send_slots = 2", "send_slots = 3"),
        ("param_rows", "param_rows = 16", "param_rows = 17"),
        (
            "dsp_area_bytes",
            "dsp_area_bytes = 524288",
            "dsp_area_bytes = 524292",
        ),
    ] {
        let bad = dt2.replace(from, to);
        assert_ne!(bad, dt2, "{what}: the test text did not change");
        let e = Facts::from_texts([bad.as_str(), dn2.as_str()])
            .unwrap_err()
            .0;
        assert!(
            e.contains(what) && e.contains("contract maximum"),
            "{what}: {e}"
        );
    }
    let bad = dn2.replace("page_rows = 8", "page_rows = 9");
    assert!(Facts::from_texts([dt2.as_str(), bad.as_str()]).is_err());
    // a value below the maximum is a device choice and is accepted
    let less = dt2.replace("fx_slots = 3", "fx_slots = 2");
    Facts::from_texts([less.as_str(), dn2.as_str()]).unwrap();
}

#[test]
fn machine_rows_are_the_run_time_machine_types_of_types_toml() {
    let Some(mut lib) = Library::scan_repo_or_skip(&root()) else {
        return;
    };
    lib.lower(&Models::from_dir(&root().join("devices")).unwrap())
        .unwrap();
    let (dt2, dn2) = device_texts();
    let bad = dt2.replace("machine_rows = 8", "machine_rows = 7");
    assert_ne!(bad, dt2);
    let facts = Facts::from_texts([bad.as_str(), dn2.as_str()]).unwrap();
    let e = devgen::generate(&lib, &facts, "core/runtime")
        .unwrap_err()
        .0;
    assert!(
        e.contains("machine_rows") && e.contains("types.toml"),
        "{e}"
    );
}

#[test]
fn the_free_effect_rows_are_the_devices_fx_slots() {
    // core/dynfx reserves one free row of the effect chain for each of the device's effect slots (docs/design/digikit-dkm.md); both devices have it
    let Some(lib) = Library::scan_repo_or_skip(&root()) else {
        return;
    };
    let dynfx = lib.get("core/dynfx").unwrap();
    let rows = dynfx
        .manifest
        .entry
        .iter()
        .filter(|e| e.registry == "fx")
        .count() as u64;
    for file in ["digitakt-ii", "digitone-ii"] {
        let t: toml::Value = toml::from_str(
            &std::fs::read_to_string(root().join(format!("devices/{file}.toml"))).unwrap(),
        )
        .unwrap();
        assert_eq!(
            rows,
            t["limits"]["fx_slots"].as_integer().unwrap() as u64,
            "{file}"
        );
    }
    assert!(
        dynfx
            .manifest
            .module
            .ports
            .iter()
            .all(|p| p.starts_with("dt2-") || p.starts_with("dn2-"))
    );
}

#[test]
fn the_effect_page_rows_are_eight_parameter_rows_for_each_fx_slot() {
    // core/fxpages reserves 7 knobs and MIX for each effect slot; the device file's [fx] has a value index for each of them (docs/design/digikit-registry.md "Effect pages")
    let Some(lib) = Library::scan_repo_or_skip(&root()) else {
        return;
    };
    let m = lib.get("core/fxpages").unwrap();
    let rows = m
        .manifest
        .entry
        .iter()
        .filter(|e| e.registry == "params" && e.id.starts_with("fp"))
        .count() as u64;
    // and one more row for each send cell of the send page (`[limits] send_slots`, finding 64)
    let send_rows = m
        .manifest
        .entry
        .iter()
        .filter(|e| e.registry == "params" && e.id.starts_with("fs"))
        .count() as u64;
    assert!(
        m.manifest
            .module
            .ports
            .iter()
            .all(|p| p.starts_with("dt2-") || p.starts_with("dn2-"))
    );
    // (device file, the number of stock pages: the firmware's page accessor has that many rows, the effect pages follow them, the array's halfwords: 71 / 101)
    for (file, first, array_hw) in [("digitakt-ii", 33, 71), ("digitone-ii", 37, 101)] {
        let t: toml::Value = toml::from_str(
            &std::fs::read_to_string(root().join(format!("devices/{file}.toml"))).unwrap(),
        )
        .unwrap();
        let slots = t["limits"]["fx_slots"].as_integer().unwrap() as u64;
        assert_eq!(rows, 8 * slots, "{file}");
        let idx = t["fx"]["value_indices"].as_array().unwrap();
        assert!(
            idx.len() as u64 >= rows,
            "{file}: a value index for each parameter row"
        );
        assert_eq!(t["fx"]["page_first"].as_integer(), Some(first), "{file}");
        // the send page (finding 64): a device that has one lists the Sound indices of its cells and a Sound layout that ends before the Fx array; its cells are parameter rows of core/fxpages
        if let Some(track_page) = t["fx"].get("track_page").and_then(|v| v.as_integer()) {
            let send = t["limits"]["send_slots"].as_integer().unwrap() as u64;
            let n = t["fx"]["send_indices"].as_array().unwrap().len() as u64;
            // the rows are shared by both devices (as many as the device with the most send cells); a device uses n of them
            assert!(
                track_page > 0 && send <= n && n <= send_rows,
                "{file}: {send} {n} {send_rows}"
            );
            let (off, stride) = (
                t["fx"]["sound_off"].as_integer().unwrap(),
                t["fx"]["sound_stride"].as_integer().unwrap(),
            );
            assert_eq!(
                off + 16 * stride,
                t["fx"]["array_off"].as_integer().unwrap(),
                "{file}"
            );
        }
        // the values and the mark are indices of the array that no stock parameter uses; the u32 behind the array
        assert_eq!(
            t["fx"]["tail_off"].as_integer(),
            Some(2 * array_hw),
            "{file}"
        );
        let mark = t["fx"]["mark_index"].as_integer().unwrap();
        assert!(mark < array_hw, "{file}");
        assert!(
            idx.iter().all(|i| i.as_integer().unwrap() < array_hw),
            "{file}"
        );
        // persistence: the kit's spare storage halfwords hold the values and one ID for each row (finding 61), inside the 0xa0-byte record, after its 46 used halfwords and before its u32 at 0x98
        let off = t["fx"]["store_off"].as_integer().unwrap() as usize;
        let hw = t["fx"]["store_halfwords"].as_integer().unwrap() as usize;
        assert!(idx.len() + idx.len() / 8 <= hw, "{file}");
        assert!(off >= 4 + 2 * 46 && off + 2 * hw <= 0x98, "{file}");
    }
}

#[test]
fn the_reserved_parameter_and_page_rows_are_the_profile_limits() {
    let Some(mut lib) = Library::scan_repo_or_skip(&root()) else {
        return;
    };
    lib.lower(&Models::from_dir(&root().join("devices")).unwrap())
        .unwrap();
    let dynreg = lib.get("core/dynreg").unwrap();
    let count = |registry: &str, dev: &str| {
        dynreg
            .manifest
            .entry
            .iter()
            .filter(|e| {
                e.registry == registry
                    && (e.devices.is_empty() || e.devices.iter().any(|d| d == dev))
            })
            .count() as u64
    };
    let dyndsp = lib.get("core/dyndsp").unwrap();
    let voices = |dev: &str| {
        dyndsp
            .manifest
            .entry
            .iter()
            .filter(|e| {
                e.registry == "voices"
                    && (e.devices.is_empty() || e.devices.iter().any(|d| d == dev))
            })
            .count() as u64
    };
    for (file, dev) in [("digitakt-ii", "dt2"), ("digitone-ii", "dn2")] {
        let t: toml::Value = toml::from_str(
            &std::fs::read_to_string(root().join(format!("devices/{file}.toml"))).unwrap(),
        )
        .unwrap();
        let l = &t["limits"];
        assert_eq!(
            voices(dev),
            l["machine_rows"].as_integer().unwrap() as u64,
            "{dev} voice rows of core/dyndsp"
        );
        assert_eq!(
            count("params", dev),
            l["param_rows"].as_integer().unwrap() as u64,
            "{dev} parameter rows of core/dynreg"
        );
        assert_eq!(
            count("pages", dev),
            l["page_rows"].as_integer().unwrap() as u64,
            "{dev} page rows of core/dynreg"
        );
        assert_eq!(
            count("machines", dev),
            l["machine_rows"].as_integer().unwrap() as u64,
            "{dev} machine rows of core/dynreg"
        );
    }
}

#[test]
fn the_link_width_is_a_shared_c_header_from_the_contract_maximum() {
    let Some(mut lib) = Library::scan_repo_or_skip(&root()) else {
        return;
    };
    lib.lower(&Models::from_dir(&root().join("devices")).unwrap())
        .unwrap();
    let facts = Facts::from_dir(&root().join("devices")).unwrap();
    let files = devgen::generate(&lib, &facts, "core/dsp-link").unwrap();
    let h = &files
        .iter()
        .find(|f| f.0 == "build/gen/dl-limits.h")
        .expect("a c-shared file")
        .1;
    // 2^17 words of 4 bytes: the offset field of a chunk; not a device's area (DN2 has 0x50000)
    assert!(h.contains("#define DL_AREA_MAX_BYTES 0x80000u"), "{h}");
    assert!(h.contains("#define DL_AREA_MAX_WORDS 0x20000u"), "{h}");
}

/// The modules of the rt021 flash images (`out/patch/rt021-dt2-1.17.plan.json`, `modules`), by id.
const RT021_MODULES: [&str; 10] = [
    "core/digikit-menu",
    "core/dsp-link",
    "core/dsp-voice",
    "core/dyndsp",
    "core/dynreg",
    "core/expansion",
    "core/health",
    "core/machines",
    "core/params",
    "core/runtime",
];

#[test]
fn the_patch_sets_are_the_rt021_flash_modules() {
    let Some(lib) = Library::scan_repo_or_skip(&root()) else {
        return;
    };
    for dev in devdata::all() {
        // both sets are the flash modules and core/input
        for (set, extra) in [
            ("device", vec!["core/input"]),
            ("emulator", vec!["core/input"]),
        ] {
            let roots = dev.patch_set(set).unwrap();
            let (mut all, _) = lib.with_requires(roots).unwrap();
            all.sort();
            let mut want: Vec<&str> = RT021_MODULES.to_vec();
            want.extend(extra);
            // run-time effects (core/dynfx and the chain it reserves rows in): both devices have them in both sets (the Digitone II's emulator set since the engine bars only the
            // changed blocks: the hook costs x1.01 DSP CPU there, finding 55)
            want.extend(["core/dynfx", "core/fx", "core/fxpages"]);
            // the DIGIKIT menu is written on <dk/ui.h>: core/digikit-menu requires core/ui
            want.push("core/ui");
            // the sample pool: both Digitakt II sets have it; the Digitone II flashes without it until the SPI wire-size decision
            if dev.short == "dt2" || set == "emulator" {
                want.push("core/sample-pool");
            }
            want.sort();
            assert_eq!(all, want, "{} set {set}", dev.short);
        }
        assert!(dev.patch_set("nope").is_err());
    }
}

#[test]
fn the_input_facts_are_the_device_files_panel() {
    let Some(mut lib) = Library::scan_repo_or_skip(&root()) else {
        return;
    };
    lib.lower(&Models::from_dir(&root().join("devices")).unwrap())
        .unwrap();
    let facts = Facts::from_dir(&root().join("devices")).unwrap();
    let files = devgen::generate(&lib, &facts, "core/input").unwrap();
    let get = |p: &str| &files.iter().find(|f| f.0 == p).unwrap().1;
    // DT2 lists 49 = [6, 1] and 50 = [6, 0]; DN2 adds 51 to 54 = [6, 4 to 7]
    for (dev, wants) in [
        (
            "dt2",
            vec![
                "PANEL_EXC_N, 0x2 ",
                "PANEL_EXC_CODE_0, 0x31 ",
                "PANEL_EXC_BIT_0, 0x1 ",
                "PANEL_EXC_CODE_1, 0x32 ",
                "PANEL_EXC_BIT_1, 0x0 ",
            ],
        ),
        (
            "dn2",
            vec![
                "PANEL_EXC_N, 0x6 ",
                "PANEL_EXC_CODE_5, 0x36 ",
                "PANEL_EXC_BIT_5, 0x7 ",
            ],
        ),
    ] {
        let text = get(&format!("build/gen/input-{dev}.inc"));
        for want in [
            "INPUT_BACKEND, 0x1 ",
            "PANEL_LINEAR, 0x6 ",
            "PANEL_ENCODERS, 0x9 ",
            "PANEL_BUTTON_CHANNELS, 0x7 ",
        ]
        .into_iter()
        .chain(wants)
        {
            assert!(
                text.contains(&format!(".equ {want}")),
                "{dev}: {want}\n{text}"
            );
        }
    }
}

#[test]
fn an_unknown_backend_or_a_bad_panel_exception_is_refused() {
    let (dt2, dn2) = device_texts();
    for (what, from, to) in [
        (
            "backend",
            "backend = \"panel-uart\"",
            "backend = \"carrier-pigeon\"",
        ),
        ("exceptions", "49 = [6, 1]", "49 = [3, 1]"),
        ("exceptions", "49 = [6, 1]", "49 = [6, 9]"),
        // a channel at or above button_channels is outside the decoder's tables
        ("exceptions", "49 = [6, 1]", "49 = [7, 1]"),
        (
            "button_channels",
            "button_channels = 7",
            "button_channels = 5",
        ),
    ] {
        let bad = dt2.replace(from, to);
        assert_ne!(bad, dt2, "{what}: the test text did not change");
        let e = Facts::from_texts([bad.as_str(), dn2.as_str()])
            .unwrap_err()
            .0;
        assert!(e.contains(what), "{what}: {e}");
    }
}

/// `patchkit gen` loads a content module's objects for its gallery.json: a run-time module declares its machines in its objects, so a
/// library without them generates nothing and `gen --check` could not see a stale file (test/dyn-params is a fixture with a gallery).
#[test]
fn a_content_modules_gallery_needs_its_objects() {
    let id = vec!["test/dyn-params".to_string()];
    let models = Models::from_dir(&root().join("devices")).unwrap();
    // the fixture root read without a build: its objects are files of the module folder, which has none (the object cache is not read)
    let light = Library::scan_in(None, &root().join("tests/fixtures/modules"), true).unwrap();
    assert!(
        modgen::generate_many(&light, &models, &id)
            .unwrap()
            .is_empty(),
        "without objects the module has no typed sections"
    );
    let Some(full) = Library::scan_repo_for_or_skip(&root(), Some(&id)) else {
        return;
    };
    let files = modgen::generate_many(&full, &models, &id).unwrap();
    assert_eq!(files.len(), 1);
    let (path, text) = &files[0];
    assert!(path.ends_with("build/gen/gallery.json"));
    assert_eq!(
        &std::fs::read_to_string(path).unwrap(),
        text,
        "{} is stale",
        path.display()
    );
}

#[test]
fn the_sample_pool_facts_are_checked() {
    let (dt2, dn2) = device_texts();
    // the committed files: DT2 borrows stock slots, DN2 owns a DDR range of the DSP
    Facts::from_texts([dt2.as_str(), dn2.as_str()]).unwrap();
    let on_dt2 = |from: &str, to: &str| (dt2.replace(from, to), dn2.clone());
    let on_dn2 = |from: &str, to: &str| (dt2.clone(), dn2.replace(from, to));
    for (what, (a, b)) in [
        (
            "a slot floor that is a multiple of 0x80 (the loader refuses those slots)",
            on_dt2("slot_floor = 0x3f0", "slot_floor = 0x380"),
        ),
        (
            "a slot floor above the last stock slot",
            on_dt2("slot_floor = 0x3f0", "slot_floor = 0x400"),
        ),
        (
            "a range with a base and no size",
            on_dn2("ddr_bytes  = 0x0ff80000", "ddr_bytes  = 0"),
        ),
        (
            "a range below the DSP's DDR",
            on_dn2("ddr_base   = 0x80531000", "ddr_base   = 0x40531000"),
        ),
        (
            "an unaligned range",
            on_dn2("ddr_base   = 0x80531000", "ddr_base   = 0x80531800"),
        ),
        (
            "a range past 4 GiB",
            on_dn2("ddr_bytes  = 0x0ff80000", "ddr_bytes  = 0x90000000"),
        ),
        (
            "R rings inside the pool range",
            on_dn2("ring_r_base = 0x904b1000", "ring_r_base = 0x90000000"),
        ),
        (
            "R rings below the stock writes",
            on_dn2("ring_r_base = 0x904b1000", "ring_r_base = 0x80530000"),
        ),
        (
            "a stock floor and an own range together",
            on_dn2("slot_floor = 0", "slot_floor = 0x3f0"),
        ),
    ] {
        assert!(a != dt2 || b != dn2, "{what}: the test text did not change");
        let e = Facts::from_texts([a.as_str(), b.as_str()]).unwrap_err().0;
        assert!(e.contains("sample_pool"), "{what}: {e}");
    }
}
