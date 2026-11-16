//! Service files: a module with `role = "service"` packs to a `.dks` with header role 1 and a version 2 header table with `PROVIDES` and `EXPORTS`; any
//! other module is a `.dkm` with role 0 and a version 1 table. The packing tests skip when the m68k-elf toolchain is not installed.

use dkneeds::keys::{Keys, name_hash};
use patchkit::build::toolchain::Env;
use patchkit::dkm::Dkm;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let p = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &p);
        } else {
            std::fs::copy(e.path(), p).unwrap();
        }
    }
}

/// A repository root of its own for a refusal that happens when the manifest loads: the repository's `modules`, `devices` and `sdk`, and the fixture
/// `test/svc-test` with the text of its `module.toml` changed by `edit`. No toolchain is needed.
fn manifest_root(dir: &Path, edit: &dyn Fn(&str) -> String) {
    use std::os::unix::fs::symlink;
    std::fs::create_dir_all(dir).unwrap();
    for n in ["modules", "devices", "sdk"] {
        symlink(root().join(n), dir.join(n)).unwrap();
    }
    let to = dir.join("tests/fixtures/modules/svc-test");
    copy_dir(&root().join("tests/fixtures/modules/svc-test"), &to);
    let text = std::fs::read_to_string(to.join("module.toml")).unwrap();
    std::fs::write(to.join("module.toml"), edit(&text)).unwrap();
}

/// `patchkit --root ROOT pack ID --device dt2 -o OUT`.
fn pack(root: &Path, id: &str, out: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_patchkit"))
        .arg("--root")
        .arg(root)
        .args(["pack", id, "--device", "dt2", "-o"])
        .arg(out)
        .output()
        .unwrap()
}

fn packed(root: &Path, id: &str, out: &Path) -> Vec<u8> {
    let r = pack(root, id, out);
    assert!(
        r.status.success(),
        "pack {id} failed:\n{}",
        String::from_utf8_lossy(&r.stderr)
    );
    std::fs::read(out).unwrap()
}

fn refused(root: &Path, id: &str, out: &Path) -> String {
    let r = pack(root, id, out);
    assert!(!r.status.success(), "pack {id} must be refused");
    String::from_utf8_lossy(&r.stderr).into_owned()
}

fn toolchain() -> bool {
    if Env::new(&root()).m68k("gcc").is_err() {
        eprintln!(
            "SKIP: the m68k-elf toolchain is not installed (uv run python -m tools.patchkit.pk_toolchains m68k)"
        );
        return false;
    }
    true
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("pk-svc-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn a_service_packs_to_a_dks_with_provides_and_exports_and_a_user_has_a_hard_need() {
    if !toolchain() {
        return;
    }
    let here = root();
    let d = scratch("pack");

    let svc = packed(&here, "test/svc-test", &d.join("svc-test.dks"));
    let dkm = Dkm::parse(&svc).unwrap();
    assert_eq!(dkm.role, 1);
    assert_eq!(&svc[0x38..0x3c], &1u32.to_be_bytes());
    assert_eq!(&dkm.register[4..6], &2u16.to_be_bytes(), "table version 2");
    let k = Keys::parse(&dkm.register).unwrap();
    let provides: Vec<_> = k.provides().collect();
    assert_eq!(provides, vec![(name_hash("svc-test"), dkm.version)]);
    let exports: Vec<_> = k.exports().collect();
    let mut names: Vec<u32> = exports.iter().map(|e| e.a).collect();
    names.sort();
    let mut want = vec![name_hash("dk_svctest_ping"), name_hash("dk_svctest_hits")];
    want.sort();
    assert_eq!(names, want);
    assert!(exports.iter().all(|e| (e.b as usize) < dkm.image.len()));
    let hooks: Vec<_> = k.hooks().collect();
    assert_eq!(hooks.len(), 1, "the hook of the manifest");
    assert_eq!(hooks[0].a, name_hash("cf.menu.hook.settings_list.tail"));
    assert!((hooks[0].b as usize) < dkm.image.len());

    let user = packed(&here, "test/svc-test-user", &d.join("svc-test-user.dkm"));
    let dkm = Dkm::parse(&user).unwrap();
    assert_eq!(dkm.role, 0);
    assert_eq!(&user[0x38..0x3c], &[0u8; 4]);
    assert_eq!(&dkm.register[4..6], &1u16.to_be_bytes(), "table version 1");
    let k = Keys::parse(&dkm.register).unwrap();
    let needs: Vec<_> = k.needs().collect();
    // the service of the test imports, and the runtime's own service (dk_log of `init`)
    let need = needs
        .iter()
        .find(|n| n.service == name_hash("svc-test"))
        .expect("a need of svc-test");
    assert!(need.hard);
    assert!(
        needs
            .iter()
            .all(|n| n.service == name_hash("svc-test") || n.service == name_hash("runtime"))
    );
    assert_eq!(k.provides().count(), 0);

    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_output_name_must_match_the_role() {
    if !toolchain() {
        return;
    }
    let here = root();
    let d = scratch("ext");
    let e = refused(&here, "test/svc-test", &d.join("svc-test.dkm"));
    assert!(e.contains("must end in .dks"), "{e}");
    let e = refused(&here, "test/svc-test-user", &d.join("user.dks"));
    assert!(e.contains("must not end in .dks"), "{e}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_service_with_an_unknown_service_or_a_foreign_export_is_refused() {
    let d = scratch("bad");
    let cases: [(&str, &dyn Fn(&str) -> String, &str); 7] = [
        (
            "unknown service",
            &|t| t.replace("service = \"svc-test\"", "service = \"no-such\""),
            "not a [[service]]",
        ),
        (
            "no service",
            &|t| t.replace("service = \"svc-test\"\n", ""),
            "needs `service",
        ),
        (
            "export that is no import",
            &|t| t.replace("name = \"dk_svctest_ping\"", "name = \"dk_not_an_import\""),
            "is not an [[import]] of the service",
        ),
        (
            "export of another service",
            &|t| t.replace("name = \"dk_svctest_ping\"", "name = \"dk_settings_open\""),
            "is not an [[import]] of the service",
        ),
        (
            "id that is not the service name",
            &|t| t.replace("id = \"test/svc-test\"", "id = \"test/other-name\""),
            "must equal the provided service",
        ),
        (
            "id leaf over 50 bytes",
            &|t| {
                t.replace(
                    "id = \"test/svc-test\"",
                    &format!("id = \"test/{}\"", "x".repeat(51)),
                )
            },
            "at most 50",
        ),
        (
            "service and export without the role",
            &|t| t.replace("role = \"service\"\n", ""),
            "belong to a module with `role = \"service\"`",
        ),
    ];
    for (i, (what, edit, want)) in cases.iter().enumerate() {
        let r = d.join(format!("root{i}"));
        manifest_root(&r, *edit);
        let e = refused(&r, "test/svc-test", &d.join("x.dks"));
        assert!(e.contains(want), "{what}: {e}");
    }
    let _ = std::fs::remove_dir_all(&d);
}

/// The big-endian bytes of `word` occur in a section of the committed object `path` (a `.dko.json`).
fn object_holds_word(path: &Path, word: u32) -> bool {
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let want: String = word
        .to_be_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    v["sections"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["hex"].as_str().is_some_and(|h| h.contains(&want)))
}

#[test]
fn lfo_packs_to_a_dks_that_owns_lfo_shape_and_its_users_need_it() {
    if !toolchain() {
        return;
    }
    let here = root();
    let d = scratch("lfo");
    let svc = packed(&here, "core/services/lfo", &d.join("lfo.dks"));
    let dkm = Dkm::parse(&svc).unwrap();
    assert_eq!(dkm.role, 1);
    assert_eq!(&dkm.register[4..6], &2u16.to_be_bytes(), "table version 2");
    let k = Keys::parse(&dkm.register).unwrap();
    assert_eq!(
        k.provides().collect::<Vec<_>>(),
        vec![(name_hash("lfo"), dkm.version)]
    );
    // the kind and its handler (the glue declares init as the handler of every kind)
    assert!(k.kinds().eq([dkneeds::keys::KindDecl {
        kind: dkneeds::KIND_LFO_SHAPE,
        len: dkneeds::LEN_LFO_SHAPE,
        entry: dkm.init.unwrap(),
    }]));
    // the ABI word: set, and the value the runtime object carries (`dkrt_abi::SVC_ABI` of the ColdFire build, which the INFO reply reports)
    let abi = u32::from_be_bytes(svc[dkneeds::H_SVC_ABI as usize..][..4].try_into().unwrap());
    assert_ne!(abi, 0);
    assert!(
        object_holds_word(
            &here.join("modules/core/runtime/build/runtime.dko.json"),
            abi
        ),
        "H_SVC_ABI {abi:#x} is not the runtime's SVC_ABI: rebuild the objects (`patchkit build --tree modules/core`)"
    );

    // a module with an lfo_shape key needs the service; any other module of the tree needs no more than before
    let user = packed(&here, "test/lfo-shape", &d.join("lfo-shape.dkm"));
    let user = Dkm::parse(&user).unwrap();
    let k = Keys::parse(&user.register).unwrap();
    let lfo: Vec<_> = k
        .needs()
        .filter(|n| n.service == name_hash("lfo"))
        .collect();
    assert_eq!(lfo.len(), 1);
    assert!(lfo[0].hard);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn fx_packs_to_a_dks_that_owns_effect_and_takes_nine_hooks_by_entry() {
    if !toolchain() {
        return;
    }
    let here = root();
    let d = scratch("fx");
    let svc = packed(&here, "core/services/fx", &d.join("fx.dks"));
    let dkm = Dkm::parse(&svc).unwrap();
    assert_eq!(dkm.role, 1);
    let k = Keys::parse(&dkm.register).unwrap();
    assert_eq!(
        k.provides().collect::<Vec<_>>(),
        vec![(name_hash("fx"), dkm.version)]
    );
    assert!(k.kinds().eq([dkneeds::keys::KindDecl {
        kind: dkneeds::KIND_EFFECT,
        len: dkneeds::LEN_EFFECT,
        entry: dkm.init.unwrap(),
    }]));
    // the nine hooks of the effect pages, each by the offset of its entry in the image (a service with hooks is bound to the firmware before it is loaded)
    let hooks: Vec<_> = k.hooks().collect();
    assert_eq!(hooks.len(), 9);
    for name in [
        "cf.page.hook.page_desc",
        "cf.page.hook.tab_label_test",
        "cf.page.hook.filter_graph_test",
        "cf.page.hook.fx_view_list.load",
        "cf.page.hook.fx_array.load",
        "cf.page.hook.fx_array.save",
        "cf.page.hook.fx_array.init_exit",
        "cf.page.hook.fx_key_list.load",
        "cf.page.hook.sound_load_exit",
    ] {
        let h = hooks
            .iter()
            .find(|p| p.a == name_hash(name))
            .unwrap_or_else(|| panic!("no HOOKS entry for {name}"));
        assert!(
            (h.b as usize) < dkm.image.len(),
            "{name}: the entry is in the image"
        );
    }
    let abi = u32::from_be_bytes(svc[dkneeds::H_SVC_ABI as usize..][..4].try_into().unwrap());
    assert!(
        object_holds_word(
            &here.join("modules/core/runtime/build/runtime.dko.json"),
            abi
        ),
        "H_SVC_ABI {abi:#x} is not the runtime's SVC_ABI: rebuild the objects (`patchkit build --tree modules/core`)"
    );
    // an effect module needs the service: its effect record is a kind of the fx service, which has no row in the runtime any more
    let user = packed(&here, "test/dyn-fx-gain", &d.join("dyn-fx-gain.dkm"));
    let user = Dkm::parse(&user).unwrap();
    let k = Keys::parse(&user.register).unwrap();
    let needs: Vec<_> = k.needs().filter(|n| n.service == name_hash("fx")).collect();
    assert_eq!(needs.len(), 1);
    assert!(needs[0].hard);
    let _ = std::fs::remove_dir_all(&d);
}
