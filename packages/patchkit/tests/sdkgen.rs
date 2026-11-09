//! The module contract: the files `patchkit gen` writes from `sdk/sdk.toml` are current (a stale one fails here as it fails
//! `patchkit gen --check`).

use patchkit::sdkgen;
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn the_fx_chain_cap_is_the_registrys() {
    // the chain cap of the registry in core/fx is the contract's DK_FX_MAX
    let toml = std::fs::read_to_string(root().join(sdkgen::SOURCE)).unwrap();
    let f = sdkgen::fx(&toml).unwrap();
    let max = f
        .consts
        .iter()
        .find(|c| c.name == "DK_FX_MAX")
        .unwrap()
        .value;
    let man = std::fs::read_to_string(root().join("modules/core/adapters/fx/module.toml"))
        .unwrap_or_default();
    if !man.is_empty() {
        assert!(
            man.contains(&format!("max = {max}")),
            "modules/core/adapters/fx/module.toml: the fx registry has max = {max}"
        );
    }
}

/// `text` with its `[api]` fields replaced by `fields` (the lines of the `fields = [...]` array).
fn with_api(text: &str, fields: &str) -> String {
    let a = text.find("[api]\n").unwrap();
    let b = a + text[a..].find("\n[api.types]\n").unwrap() + 1;
    format!(
        "{}[api]\nfields = [\n{fields}]\n\n{}",
        &text[..a],
        &text[b..]
    )
}

const TWO_FIELDS: &str = "    { name = \"version\", type = \"u32\", doc = \"v\" },\n    { name = \"hook\", type = \"void (u32 a, service_cmd handler)\", doc = \"h\" },\n";

#[test]
fn the_api_table_is_generated_for_c_and_rust() {
    let text = std::fs::read_to_string(root().join(sdkgen::SOURCE)).unwrap();
    let files = sdkgen::render(&with_api(&text, TWO_FIELDS)).unwrap();
    let get = |p: &str| &files.iter().find(|f| f.0 == p).unwrap().1;
    let c = get(sdkgen::C_API_OUT);
    assert!(c.contains("_Static_assert(sizeof(dk_api) == 8,"));
    assert!(c.contains("_Static_assert(offsetof(dk_api, hook) == 4,"));
}

#[test]
fn a_bad_type_or_view_is_refused() {
    let text = std::fs::read_to_string(root().join(sdkgen::SOURCE)).unwrap();
    let bad = with_api(&text, &TWO_FIELDS.replace("u32 a", "u64 a"));
    assert!(sdkgen::render(&bad).unwrap_err().0.contains("unknown type"));
    // the names of a typed C view are the contract's name list, in order
    let broken = text.replace("\"i32 pitch // ", "\"i32 pitchx // ");
    assert_ne!(broken, text);
    assert!(sdkgen::render(&broken).unwrap_err().0.contains("name list"));
}

#[test]
fn a_bad_descriptor_layout_is_refused() {
    let text = std::fs::read_to_string(root().join(sdkgen::SOURCE)).unwrap();
    // text that needs padding
    let broken = text.replace("char short_name[8]", "char short_name[6]");
    assert_ne!(broken, text);
    assert!(
        sdkgen::render(&broken)
            .unwrap_err()
            .0
            .contains("multiple of 4")
    );
    // a type that is not a base type or an earlier record
    let broken = text.replace("float min //", "double min //");
    assert_ne!(broken, text);
    assert!(
        sdkgen::render(&broken)
            .unwrap_err()
            .0
            .contains("unknown type")
    );
    // a length that is not a const of [desc]
    let broken = text.replace("knob[DK_PAGE_KNOBS]", "knob[DK_NOPE]");
    assert_ne!(broken, text);
    assert!(sdkgen::render(&broken).unwrap_err().0.contains("DK_NOPE"));
}

#[test]
fn the_record_format_constants_come_from_the_contract_maxima() {
    // The effect chain cap `fx_slots` of `[limits.max]` is the contract's DK_FX_MAX (`[fx]`).
    let max = sdkgen::limits_max();
    let fx = sdkgen::fx(&std::fs::read_to_string(root().join(sdkgen::SOURCE)).unwrap()).unwrap();
    let dk_fx_max = fx
        .consts
        .iter()
        .find(|c| c.name == "DK_FX_MAX")
        .unwrap()
        .value;
    assert_eq!(dk_fx_max as u64, max.fx_slots);
    // the packer's array length is a copy of `DK_PAGE_KNOBS`
    assert_eq!(patchkit::register::KNOBS, sdkgen::page_knobs());
}

#[test]
fn bad_protocol_ranges_commands_and_capacity_kinds_are_refused() {
    let text = std::fs::read_to_string(root().join(sdkgen::SOURCE)).unwrap();
    // a command range a module may register that overlaps a command or leaves the 6-bit numbers is refused
    for bad in [
        "remote = [0x38, 0x40]",
        "remote = [0x10, 0x18]",
        "remote = [0x3f, 0x38]",
        "remote = [0, 0x08]",
        "remote = [0x38, 0x3e]",
    ] {
        let broken = text.replace("remote = [0x38, 0x3d]", bad);
        assert_ne!(broken, text);
        assert!(sdkgen::render(&broken).is_err(), "{bad}");
    }
    // a command of a service module outside every range is refused
    let broken = text.replace("REMOTE_KEY = 0x38 ", "REMOTE_KEY = 0x10 ");
    assert_ne!(broken, text);
    assert!(sdkgen::render(&broken).is_err());
    // two kinds with one number, or kind 0, are refused
    let dup = text.replace("ARENA = 3 ", "ARENA = 2 ");
    assert_ne!(dup, text);
    assert!(
        sdkgen::render(&dup)
            .unwrap_err()
            .0
            .contains("capacity kind")
    );
}

#[test]
fn imports_of_other_modules_get_stubs_from_absent() {
    let text = std::fs::read_to_string(root().join("sdk/sdk.toml")).unwrap();
    let files = sdkgen::render(&text).unwrap();
    let rs = &files.iter().find(|f| f.0 == sdkgen::RUST_OUT).unwrap().1;
    // the ui calls are linked by name and have the API's failure value as a stub; the table lists both addresses
    assert!(rs.contains("pub fn dk_ui_open(menu: *mut u8) -> i32;"));
    assert!(rs.contains("pub unsafe extern \"C\" fn dk_ui_open(_a0: *mut u8) -> i32 { -1 as _ }"));
    assert!(rs.contains(
        "pub unsafe extern \"C\" fn dk_ui_menu_new(_a0: *const u8) -> *mut u8 { 0 as _ }"
    ));
    assert!(rs.contains("pub unsafe extern \"C\" fn dk_ui_close(_a0: *mut u8) { }"));
    assert!(rs.contains(
        "(\"dk_ui_has\", core::hint::black_box(dk_ui_has as usize), absent::dk_ui_has as usize),"
    ));
    // core/input has no stub: the import is not found without the module
    assert!(rs.contains("(\"input_key\", core::hint::black_box(input_key as usize), 0),"));
    // the runtime's own table is the imports without `requires`
    assert!(rs.contains("pub const BUILTIN_NAMES"));
    assert!(!rs.contains("\"remote_enabled\""));
    // an absent value that does not fit the return type, or without requires, is refused
    let broken = text.replacen("absent = \"-1\"", "absent = \"void\"", 1);
    assert_ne!(broken, text);
    assert!(sdkgen::render(&broken).is_err());
    let broken = text.replacen("requires = \"core/ui\"\nabsent", "absent", 1);
    assert_ne!(broken, text);
    assert!(sdkgen::render(&broken).is_err());
}
