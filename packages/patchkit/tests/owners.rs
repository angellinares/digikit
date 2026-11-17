//! `patchkit owners`: each owner's hash changes with its own items only; an unmapped path is an error.

use patchkit::dko::{Dko, DkoReloc, DkoSection, DkoSymbol};
use patchkit::error::hex;
use patchkit::objdiff::diff_owned;
use patchkit::owners::{Owners, hashes};
use std::collections::BTreeMap;
use std::path::PathBuf;

const OWNERS: &str = r#"
shared = ["core", "dkrt::state"]
"S:fx" = ["dkrt_svc_fx"]
"S:machines" = ["dkrt::services::machines"]
"C:storage" = ["dkrt::store"]
"#;

/// The v0 mangled name of the function `path` (crate first), as rustc writes it.
fn mangle(path: &[&str]) -> String {
    let mut s = String::from("_RNv");
    s.push_str(&"Nt".repeat(path.len() - 2));
    s.push_str("CsdWzhuXZnywa_");
    for p in path {
        s.push_str(&format!("{}{p}", p.len()));
    }
    s
}

/// `path::{closure#0}`
fn closure(path: &[&str]) -> String {
    format!("_RNC{}0", &mangle(path)[2..])
}

/// `path::<usize>`
fn generic(path: &[&str]) -> String {
    format!("_RI{}mE", &mangle(path)[2..])
}

fn text(name: &str, code: &[u8]) -> DkoSection {
    DkoSection {
        name: format!(".text.{name}"),
        kind: "text".into(),
        align: 2,
        size: code.len() as u32,
        hex: hex(code),
    }
}

fn data(name: &str, bytes: &[u8]) -> DkoSection {
    DkoSection {
        name: name.into(),
        kind: "data".into(),
        align: 1,
        size: bytes.len() as u32,
        hex: hex(bytes),
    }
}

/// A relocation in `section` at `offset` to section `target` (the full section name).
fn reloc(section: &str, offset: u64, target: &str) -> DkoReloc {
    DkoReloc {
        section: section.into(),
        offset,
        rtype: "cf.32".into(),
        symbol: format!(".sec.{target}"),
        addend: 0,
    }
}

fn object(sections: Vec<DkoSection>, relocs: Vec<DkoReloc>) -> Dko {
    let symbols = sections
        .iter()
        .map(|s| DkoSymbol {
            name: format!(".sec.{}", s.name),
            section: Some(s.name.clone()),
            value: 0,
            global: false,
        })
        .collect();
    Dko {
        format: 1,
        arch: "cf".into(),
        source_sha256: String::new(),
        toolchain: BTreeMap::new(),
        sections,
        symbols,
        relocs,
    }
}

fn owners() -> Owners {
    Owners::parse(OWNERS).unwrap()
}

fn hash_of(o: &Dko) -> BTreeMap<String, String> {
    hashes(o, &owners())
        .unwrap()
        .into_iter()
        .map(|(tag, (_, h))| (tag, h))
        .collect()
}

/// The tags whose hash differs.
fn changed(a: &Dko, b: &Dko) -> Vec<String> {
    let (x, y) = (hash_of(a), hash_of(b));
    x.iter()
        .filter(|(k, v)| y.get(*k) != Some(v))
        .map(|(k, _)| k.clone())
        .collect()
}

const FX: [&str; 3] = ["dkrt_svc_fx", "send", "apply"];
const STORE: [&str; 3] = ["dkrt", "store", "put"];
const PEEK: [&str; 3] = ["dkrt", "state", "peek"];
const NOTE: [&str; 4] = ["dkrt", "services", "machines", "note"];

/// One function per owner; `fx` and `store` are the bytes of the fx and storage functions.
fn base(fx: &[u8], store: &[u8]) -> Dko {
    object(
        vec![
            text(&mangle(&FX), fx),
            text(&mangle(&STORE), store),
            text(&mangle(&PEEK), &[0x4e, 0x75]),
            text(&mangle(&NOTE), &[0x4e, 0x71, 0x4e, 0x75]),
        ],
        vec![],
    )
}

#[test]
fn every_owner_has_a_hash_and_an_edit_changes_only_its_owner() {
    let a = base(&[0x70, 1, 0x4e, 0x75], &[0x70, 2, 0x4e, 0x75]);
    let h = hash_of(&a);
    let tags: Vec<&str> = h.keys().map(|s| s.as_str()).collect();
    assert_eq!(tags, ["C:storage", "S:fx", "S:machines", "shared"]);
    assert_eq!(changed(&a, &a), Vec::<String>::new());
    let fx = base(&[0x70, 9, 0x4e, 0x75], &[0x70, 2, 0x4e, 0x75]);
    assert_eq!(changed(&a, &fx), ["S:fx"]);
    let store = base(&[0x70, 1, 0x4e, 0x75], &[0x70, 9, 0x4e, 0x75]);
    assert_eq!(changed(&a, &store), ["C:storage"]);
}

#[test]
fn a_move_inside_the_object_or_inside_an_owner_changes_nothing() {
    let a = base(&[0x70, 1, 0x4e, 0x75], &[0x70, 2, 0x4e, 0x75]);
    // section order reversed, and the storage function in another storage module
    let mut b = base(&[0x70, 1, 0x4e, 0x75], &[0x70, 2, 0x4e, 0x75]);
    b.sections.reverse();
    b.sections[2].name = format!(".text.{}", mangle(&["dkrt", "store", "inner", "put"]));
    b.symbols.clear();
    let b = object(b.sections, vec![]);
    assert_eq!(changed(&a, &b), Vec::<String>::new());
    // a move to another owner changes both
    let mut c = base(&[0x70, 1, 0x4e, 0x75], &[0x70, 2, 0x4e, 0x75]);
    c.sections[1].name = format!(".text.{}", mangle(&["dkrt_svc_fx", "put"]));
    let c = object(c.sections, vec![]);
    assert_eq!(changed(&a, &c), ["C:storage", "S:fx"]);
}

#[test]
fn a_callee_changes_its_own_owner_and_an_inlined_callee_changes_its_caller() {
    // the fx function calls the shared helper (jsr to its section)
    let call = |peek: &[u8]| {
        let sections = vec![
            text(&mangle(&FX), &[0x4e, 0xb9, 0, 0, 0, 0, 0x4e, 0x75]),
            text(&mangle(&PEEK), peek),
        ];
        let target = format!(".text.{}", mangle(&PEEK));
        object(
            sections,
            vec![reloc(&format!(".text.{}", mangle(&FX)), 2, &target)],
        )
    };
    let a = call(&[0x70, 1, 0x4e, 0x75]);
    let b = call(&[0x70, 2, 0x4e, 0x75]);
    assert_eq!(changed(&a, &b), ["shared"]);
    // inlined: the helper's change appears in the bytes of the fx function
    let inlined = |peek: u8| {
        object(
            vec![
                text(&mangle(&FX), &[0x70, peek, 0x4e, 0x75]),
                text(&mangle(&PEEK), &[0x70, peek, 0x4e, 0x75]),
            ],
            vec![],
        )
    };
    assert_eq!(changed(&inlined(1), &inlined(2)), ["S:fx", "shared"]);
    let only_inlined =
        |peek: u8| object(vec![text(&mangle(&FX), &[0x70, peek, 0x4e, 0x75])], vec![]);
    assert_eq!(changed(&only_inlined(1), &only_inlined(2)), ["S:fx"]);
}

#[test]
fn a_generic_instance_and_a_closure_follow_their_path() {
    let obj = |generic_byte: u8, closure_byte: u8, core_byte: u8| {
        object(
            vec![
                text(&generic(&["dkrt", "store", "get"]), &[0x70, generic_byte]),
                text(&closure(&STORE), &[0x70, closure_byte]),
                // `core::ptr::drop_in_place::<usize>`: the standard library is shared
                text(
                    &generic(&["core", "ptr", "drop_in_place"]),
                    &[0x70, core_byte],
                ),
            ],
            vec![],
        )
    };
    let a = obj(1, 1, 1);
    assert_eq!(changed(&a, &obj(2, 1, 1)), ["C:storage"]);
    assert_eq!(changed(&a, &obj(1, 2, 1)), ["C:storage"]);
    assert_eq!(changed(&a, &obj(1, 1, 2)), ["shared"]);
}

/// A reader of a string constant: `lea.l <string>,a0; rts`.
fn reader(name: &str, target: &str) -> (DkoSection, DkoReloc) {
    let t = text(name, &[0x41, 0xf9, 0, 0, 0, 0, 0x4e, 0x75]);
    let r = reloc(&format!(".text.{name}"), 2, target);
    (t, r)
}

#[test]
fn a_string_change_is_seen_by_the_owner_of_its_user_only() {
    let obj = |byte: u8| {
        let (t, r) = reader(&mangle(&FX), ".rodata.global.1A");
        object(
            vec![
                t,
                data(".rodata.global.1A", &[b'h', b'i', byte]),
                text(&mangle(&STORE), &[0x4e, 0x75]),
            ],
            vec![r],
        )
    };
    assert_eq!(changed(&obj(b'a'), &obj(b'b')), ["S:fx"]);
}

#[test]
fn an_anonymous_table_belongs_to_its_users_or_is_shared() {
    // a table with a pointer to a string; one user, or two users of two owners
    let obj = |byte: u8, second_user: bool| {
        let table = ".rodata.global.2T";
        let mut sections = vec![
            text(&mangle(&STORE), &[0x41, 0xf9, 0, 0, 0, 0, 0x4e, 0x75]),
            data(table, &[0, 0, 0, 0]),
            data(".rodata.global.1S", &[b'x', byte]),
        ];
        let mut relocs = vec![
            reloc(&format!(".text.{}", mangle(&STORE)), 2, table),
            reloc(table, 0, ".rodata.global.1S"),
        ];
        if second_user {
            sections.push(text(&mangle(&FX), &[0x41, 0xf9, 0, 0, 0, 0, 0x4e, 0x75]));
            relocs.push(reloc(&format!(".text.{}", mangle(&FX)), 2, table));
        }
        object(sections, relocs)
    };
    assert_eq!(changed(&obj(1, false), &obj(2, false)), ["C:storage"]);
    // the table has two owners: shared, besides its users (their relocation targets name the string)
    let shared = changed(&obj(1, true), &obj(2, true));
    assert!(shared.contains(&"shared".to_string()), "{shared:?}");
}

#[test]
fn an_unmapped_path_is_an_error_that_names_it() {
    let o = object(
        vec![
            text(&mangle(&["dkrt", "newmod", "f"]), &[0x4e, 0x75]),
            text(&mangle(&STORE), &[0x4e, 0x75]),
        ],
        vec![],
    );
    let e = hashes(&o, &owners()).err().unwrap().to_string();
    assert!(e.contains("dkrt::newmod"), "{e}");
    assert!(!e.contains("dkrt::store"), "{e}");
}

#[test]
fn owners_toml_rejects_a_bad_tag_and_a_prefix_under_two_tags() {
    assert!(Owners::parse("\"S:fx\" = [\"a\"]\n\"C:state\" = [\"a\"]").is_err());
    assert!(Owners::parse("\"X:fx\" = [\"a\"]").is_err());
    assert!(Owners::parse("\"A:core/fx\" = [\"a\"]").is_err());
}

#[test]
fn the_report_of_objdiff_names_the_owner() {
    let a = base(&[0x70, 1, 0x4e, 0x75], &[0x70, 2, 0x4e, 0x75]);
    let b = base(&[0x70, 9, 0x4e, 0x75], &[0x70, 2, 0x4e, 0x75]);
    let r = diff_owned(&a, &b, false, Some(&owners())).unwrap();
    assert_eq!(r.differences.len(), 1, "{}", r.text());
    assert_eq!(r.differences[0].owner, "S:fx");
    assert!(r.text().contains("apply  [S:fx]"), "{}", r.text());
    let plain = diff_owned(&a, &b, false, None).unwrap();
    assert_eq!(plain.differences[0].owner, "");
}

fn runtime_object() -> Dko {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../modules/core/runtime/build/runtime.dko.json");
    Dko::parse(&std::fs::read_to_string(p).unwrap()).unwrap()
}

#[test]
fn every_item_of_the_runtime_object_has_an_owner() {
    let p =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../modules/core/runtime/owners.toml");
    let owners = Owners::read(&p).unwrap();
    let o = runtime_object();
    let h = hashes(&o, &owners).unwrap();
    for tag in owners.tags() {
        assert!(h.contains_key(tag), "{tag}");
    }
    let items: usize = h.values().map(|(n, _)| n).sum();
    assert!(items > 500, "{items}");
    assert!(h["shared"].0 > 0 && h["S:samples"].0 > 0 && h["C:storage"].0 > 0);
    // deterministic
    assert_eq!(h, hashes(&o, &owners).unwrap());
}
