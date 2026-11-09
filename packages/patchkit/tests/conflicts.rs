//! Conflict and safety rules of `apply`, with synthetic modules over the real ports and stock images
//! (skipped when the images are absent).

use patchkit::apply::{self, Options};
use patchkit::firmware::Firmware;
use patchkit::module::{Library, Module};
use patchkit::port::{Hook, Registry};
use serde_json::json;
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn image(name: &str) -> Option<Vec<u8>> {
    std::fs::read(root().join("firmware").join(name)).ok()
}

const DT2: &str = "Digitakt_II_OS1.17.syx";

/// The content modules are built on demand (`Library::scan_repo`): a test that needs one skips without the toolchain.
macro_rules! need_toolchain {
    () => {
        if patchkit::build::toolchain::Env::new(&root())
            .m68k("gcc")
            .is_err()
        {
            eprintln!("SKIP: the m68k-elf toolchain is not installed");
            return;
        }
    };
}

fn registry() -> Registry {
    Registry::from_dir(&root().join("devices/ports")).unwrap()
}

/// The real library (core/expansion, core/fx, ...) plus synthetic modules.
fn library(extra: Vec<Module>) -> Library {
    // without the toolchain the content modules are left out; the tests that need one start with `need_toolchain!()`
    let mut lib = Library::scan_repo_or_skip(&root())
        .unwrap_or_else(|| Library::scan_repo_for(&root(), true, Some(&[])).unwrap());
    lib.modules.extend(extra);
    lib
}

/// A module whose manifest is `body` after a standard [module] header; `dko` is its only object, if any.
fn module(id: &str, header_extra: &str, body: &str, dko: Option<serde_json::Value>) -> Module {
    let text = format!(
        "[module]\nid = \"{id}\"\nversion = \"1.0.0\"\nname = \"{id}\"\ncategory = \"test\"\ntype = \"raw\"\nauthor = \"digikit\"\nstatus = \"experimental\"\nsummary = \"synthetic\"\n{header_extra}\n{body}\n"
    );
    let read = move |_p: &str| -> patchkit::Result<String> { Ok(dko.clone().unwrap().to_string()) };
    Module::from_parts(&text, &read).unwrap()
}

fn write_op(offset: i64, hex: &str) -> String {
    format!(
        "[[op]]\nop = \"write\"\nanchor = \"dsp.master.fn.q31_converter\"\noffset = {offset}\nhex = \"{hex}\"\n"
    )
}

fn expect_err(r: patchkit::Result<apply::Applied>, needle: &str) {
    match r {
        Ok(_) => panic!("expected an error containing {needle:?}"),
        Err(e) => assert!(
            e.0.contains(needle),
            "error {:?} does not contain {needle:?}",
            e.0
        ),
    }
}

fn run(lib: &Library, ids: &[&str]) -> patchkit::Result<apply::Applied> {
    run_with(&registry(), lib, ids)
}

fn run_with(reg: &Registry, lib: &Library, ids: &[&str]) -> patchkit::Result<apply::Applied> {
    let raw = image(DT2).expect("image");
    let ids: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
    apply::apply_with(&raw, reg, lib, &ids, Options::default())
}

macro_rules! need_image {
    () => {
        if image(DT2).is_none() {
            return;
        }
    };
}

#[test]
fn unmet_requires_and_conflicts_are_refused() {
    need_image!();
    let a = module("t/a", "requires = [\"t/b\"]", "", None);
    let b = module("t/b", "", "", None);
    let c = module("t/c", "conflicts = [\"t/b\"]", "", None);
    let lib = library(vec![a, b, c]);
    expect_err(run(&lib, &["t/a"]), "requires t/b");
    expect_err(run(&lib, &["t/c", "t/b"]), "conflicts with");
    expect_err(run(&lib, &["t/missing"]), "unknown module");
    assert!(run(&lib, &["t/a", "t/b"]).is_ok());
}

#[test]
fn order_is_canonical_whatever_the_selection_order() {
    need_image!();
    let a = module("t/a", "", "", None);
    let b = module("t/b", "requires = [\"t/a\"]", "", None);
    let c = module("t/c", "", "", None);
    let lib = library(vec![a, b, c]);
    let x = run(&lib, &["t/c", "t/b", "t/a"]).unwrap();
    let y = run(&lib, &["t/a", "t/c", "t/b"]).unwrap();
    assert_eq!(x.plan.modules, vec!["t/a@1.0.0", "t/b@1.0.0", "t/c@1.0.0"]);
    assert!(x.syx == y.syx, "different selection order, different bytes");
}

#[test]
fn colliding_claims_are_refused() {
    need_image!();
    let claim = "[[claim]]\nresource = \"dsp.thing\"\n";
    let lib = library(vec![
        module("t/a", "", claim, None),
        module("t/b", "", claim, None),
    ]);
    expect_err(run(&lib, &["t/a", "t/b"]), "both claim dsp.thing");
}

#[test]
fn overlapping_writes_are_refused() {
    need_image!();
    // both rewrite the converter's first instruction word (6 bytes at the anchor); expect_sha256 is not
    // needed inside the anchor
    let lib = library(vec![
        module("t/a", "", &write_op(0, "180400bd0000"), None),
        module("t/b", "", &write_op(2, "00bd0000"), None),
    ]);
    expect_err(run(&lib, &["t/a", "t/b"]), "overlapping writes");
}

#[test]
fn a_write_outside_its_anchor_needs_a_pinned_old_value() {
    need_image!();
    let lib = library(vec![module("t/a", "", &write_op(0x100, "0000"), None)]);
    expect_err(run(&lib, &["t/a"]), "no expect_sha256");
    let wrong = format!(
        "{}expect_sha256 = \"{}\"\n",
        write_op(0x100, "0000"),
        "00".repeat(32)
    );
    let lib = library(vec![module("t/a", "", &wrong, None)]);
    expect_err(run(&lib, &["t/a"]), "old bytes hash");
}

#[test]
fn a_region_over_capacity_is_refused() {
    need_image!();
    let dko = json!({
        "format": 1, "arch": "cf", "source_sha256": "0".repeat(64),
        "sections": [{"name": ".bss", "kind": "bss", "align": 4, "size": 3_000_000, "hex": ""}],
        "symbols": [], "relocs": []
    });
    let m = module(
        "t/big",
        "",
        "[[object]]\npath = \"x.json\"\nplace = \"cf.expansion\"\n",
        Some(dko),
    );
    let lib = library(vec![m]);
    expect_err(run(&lib, &["t/big"]), "do not fit in cf.expansion");
}

#[test]
fn an_anchor_hash_mismatch_is_refused() {
    need_image!();
    need_toolchain!();
    let mut reg = registry();
    for p in reg
        .ports
        .iter_mut()
        .filter(|p| p.port.version == "1.17" && p.port.device == "dt2")
    {
        for a in p.anchor.iter_mut().filter(|a| a.name == "dsp.fx.fn.entry") {
            a.sha256 = "ab".repeat(32);
        }
    }
    let lib = library(vec![]);
    expect_err(run_with(&reg, &lib, &["core/fx"]), "bytes hash");
}

#[test]
fn a_module_for_another_firmware_is_refused() {
    need_image!();
    let m = module("t/pinned", "ports = [\"dn2-1.12\"]", "", None);
    let lib = library(vec![m]);
    expect_err(run(&lib, &["t/pinned"]), "not compatible");
}

#[test]
fn the_container_cap_is_enforced() {
    need_image!();
    need_toolchain!();
    let mut reg = registry();
    for p in reg
        .ports
        .iter_mut()
        .filter(|p| p.port.version == "1.17" && p.port.device == "dt2")
    {
        p.limits.container_max = 1_000_000;
    }
    let lib = library(vec![]);
    expect_err(run_with(&reg, &lib, &["core/fx"]), "over the");
}

#[test]
fn sections_2_4_and_8_can_never_change() {
    need_image!();
    let mut fw = Firmware::parse(&image(DT2).unwrap()).unwrap();
    for id in [2u32, 4, 8] {
        assert!(fw.replace(id, vec![0; 16]).is_err(), "section {id}");
    }
    assert!(fw.replace(7, fw.section(7).unwrap().data.clone()).is_ok());
}

#[test]
fn a_patched_image_has_no_port() {
    need_image!();
    need_toolchain!();
    let out = run(&library(vec![]), &["core/fx"]).unwrap();
    let lib = library(vec![]);
    let again = apply::apply(&out.syx, &registry(), &lib, &["core/fx".to_string()]);
    expect_err(again, "no port for firmware");
}

#[test]
fn retarget_of_something_that_is_not_a_call_is_refused() {
    need_image!();
    let m = module(
        "t/r",
        "",
        "[[op]]\nop = \"hook\"\nhook = \"t.retarget.hook\"\nto = \"_x\"\n[[object]]\npath = \"x.json\"\nplace = \"dsp.l2\"\n",
        Some(json!({
            "format": 1, "arch": "sharc", "source_sha256": "0".repeat(64),
            "sections": [{"name": "seg_pmco", "kind": "text", "align": 8, "size": 6, "hex": "000000000000"}],
            "symbols": [{"name": "_x", "section": "seg_pmco", "value": 0, "global": true}],
            "relocs": []
        })),
    );
    let lib = library(vec![m]);
    let mut reg = registry();
    let sha = Firmware::parse(&image(DT2).unwrap()).unwrap().syx_sha256;
    for p in reg.ports.iter_mut().filter(|p| p.port.syx_sha256 == sha) {
        p.hook.push(Hook {
            name: "t.retarget.hook".into(),
            site: "dsp.master.fn.q31_converter".into(),
            offset: 0,
            mode: "retarget".into(),
        });
    }
    expect_err(run_with(&reg, &lib, &["t/r"]), "not a direct CALL");
}
