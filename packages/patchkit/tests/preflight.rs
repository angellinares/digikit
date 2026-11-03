//! `patchkit preflight` against the stock images in `firmware/` (skipped when absent).

use patchkit::apply;
use patchkit::firmware::Firmware;
use patchkit::module::Library;
use patchkit::port::Registry;
use patchkit::preflight::{Known, Outcome, known_from_texts, preflight};
use patchkit::syx;
use patchkit::typed::Models;
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

struct Rig {
    stock: Vec<u8>,
    patched: Vec<u8>,
    reg: Registry,
    known: Vec<Known>,
}

fn rig() -> Option<Rig> {
    let stock = std::fs::read(root().join("firmware/Digitakt_II_OS1.17.syx")).ok()?;
    let reg = Registry::from_dir(&root().join("devices/ports")).unwrap();
    // only `core/*` modules: their objects are committed, no content module is built
    let mut lib = Library::scan_repo_for(&root(), false, Some(&[])).unwrap();
    lib.lower(&Models::from_dir(&root().join("devices")).unwrap())
        .unwrap();
    let ids = lib
        .with_requires(&["core/runtime".to_string(), "core/health".to_string()])
        .unwrap()
        .0;
    let patched = apply::apply(&stock, &reg, &lib, &ids).unwrap().syx;
    let text = std::fs::read_to_string(root().join("devices/digitakt-ii.toml")).unwrap();
    let known = known_from_texts([text.as_str()]).unwrap();
    Some(Rig {
        stock,
        patched,
        reg,
        known,
    })
}

fn run(r: &Rig, image: &[u8], have_stock: bool) -> Outcome {
    let stock = r.stock.clone();
    preflight(
        image,
        &r.reg,
        &r.known,
        &move |_| have_stock.then(|| stock.clone()),
        None,
    )
}

fn failed(o: &Outcome) -> String {
    o.lines
        .iter()
        .filter(|l| !l.ok)
        .map(|l| l.text.clone())
        .collect::<Vec<_>>()
        .join(" | ")
}

#[test]
fn a_patched_image_passes_and_says_which_sections_changed() {
    let Some(r) = rig() else { return };
    let o = run(&r, &r.patched, true);
    assert!(o.passed(), "{}", failed(&o));
    assert!(
        o.lines
            .iter()
            .any(|l| l.text.contains("changed sections: 3 (MAIN OS only"))
    );
    assert!(
        o.lines
            .iter()
            .any(|l| l.text.contains("section 2 (bootstrap) identical"))
    );
}

#[test]
fn a_stock_image_is_refused_as_not_patched() {
    let Some(r) = rig() else { return };
    let o = run(&r, &r.stock, true);
    assert!(!o.passed());
    assert!(failed(&o).contains("not patched"), "{}", failed(&o));
}

#[test]
fn a_tampered_bootstrap_is_refused() {
    let Some(r) = rig() else { return };
    let mut fw = Firmware::parse(&r.patched).unwrap();
    let s = fw.sections.iter_mut().find(|s| s.id == 2).unwrap();
    s.data[100] ^= 1;
    s.stored = syx::store_for_section(2, &s.data).unwrap();
    let image = fw.build(None).unwrap();
    let o = run(&r, &image, true);
    assert!(!o.passed());
    assert!(
        failed(&o).contains("section 2 (bootstrap) DIFFERS"),
        "{}",
        failed(&o)
    );
}

#[test]
fn a_missing_stock_file_or_an_unknown_base_is_refused() {
    let Some(r) = rig() else { return };
    let o = run(&r, &r.patched, false);
    assert!(failed(&o).contains("not in firmware/"), "{}", failed(&o));
    let o = preflight(&r.patched, &r.reg, &[], &|_| None, None);
    assert!(
        failed(&o).contains("not a known stock firmware"),
        "{}",
        failed(&o)
    );
}
