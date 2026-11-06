//! A digikit-patched image boots with its stock base's profile, found from its patch record
//! (skipped when the stock images in `firmware/` are absent).

use elektron_native_boot::Emulator;
use patchkit::{apply, module::Library, port::Registry};
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn patched(file: &str, modules: &[&str]) -> Option<Vec<u8>> {
    let raw = std::fs::read(root().join("firmware").join(file)).ok()?;
    let registry = Registry::from_dir(&root().join("devices/ports")).unwrap();
    let ids: Vec<String> = modules.iter().map(|m| m.to_string()).collect();
    // builds the content modules the image needs, no others (skipped without the toolchain)
    let library = Library::scan_repo_for_or_skip(&root(), Some(&ids))?;
    let (ids, _) = library.with_requires(&ids).unwrap();
    Some(apply::apply(&raw, &registry, &library, &ids).unwrap().syx)
}

#[test]
fn a_patched_image_with_a_changed_main_boots_from_its_record() {
    // test/patched-pluck adds ColdFire code, so neither the file nor the MAIN digest is a known one.
    let Some(syx) = patched(
        "Digitakt_II_OS1.17.syx",
        &["core/expansion", "test/patched-pluck"],
    ) else {
        return;
    };
    let record = patchkit::verify::patch_record(&syx).unwrap().unwrap();
    assert_eq!(record.base_sha256.len(), 64);
    Emulator::new(&syx, None).expect("the patched image boots");
}

#[test]
fn a_patched_dsp_only_image_boots() {
    let Some(syx) = patched("Digitone_II_OS1.12.syx", &["core/fx"]) else {
        return;
    };
    Emulator::new(&syx, None).expect("the patched image boots");
}

#[test]
fn an_unknown_image_names_the_supported_firmware() {
    let Some(stock) = std::fs::read(root().join("firmware/Digitakt_II_OS1.15C.syx")).ok() else {
        return;
    };
    let error = Emulator::new(&stock, None)
        .err()
        .expect("1.15C does not boot");
    assert!(error.contains("Digitakt II 1.16 or 1.17"), "{error}");
}
