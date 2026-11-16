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

#[test]
fn a_bad_type_or_view_is_refused() {
    let text = std::fs::read_to_string(root().join(sdkgen::SOURCE)).unwrap();
    // an import signature with a type the contract does not know
    let bad = text.replacen(
        "i32 (dk_module_ctx *ctx, dk_settings **out)",
        "i32 (dk_module_ctx *ctx, u64 *out)",
        1,
    );
    assert_ne!(bad, text);
    assert!(sdkgen::render(&bad).unwrap_err().0.contains("unknown type"));
    // the names of a typed C view are the contract's name list, in order
    let broken = text.replace("\"i32 pitch // ", "\"i32 pitchx // ");
    assert_ne!(broken, text);
    assert!(sdkgen::render(&broken).unwrap_err().0.contains("name list"));
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
    let dup = text.replace("ARENA = { id = 3,", "ARENA = { id = 2,");
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
    // the ui calls are linked by name and have a failure value as a stub; the table lists both addresses
    assert!(rs.contains("pub fn dk_ui_toast(ui: *mut u8, text: *const u8) -> i32;"));
    assert!(rs.contains(
        "pub unsafe extern \"C\" fn dk_ui_toast(_a0: *mut u8, _a1: *const u8) -> i32 { -1 as _ }"
    ));
    assert!(rs.contains(
        "pub unsafe extern \"C\" fn dk_ui_menu_new(_a0: *mut u8, _a1: *const u8) -> *mut u8 { 0 as _ }"
    ));
    assert!(rs.contains("pub unsafe extern \"C\" fn dk_ui_menu_close(_a0: *mut u8) { }"));
    assert!(rs.contains(
        "(\"dk_ui_menu_new\", core::hint::black_box(dk_ui_menu_new as usize), absent::dk_ui_menu_new as usize, \"core/ui\"),"
    ));
    // core/input has a stub too; the row names the module the boot report asks for
    assert!(rs.contains(
        "(\"dk_input_key\", core::hint::black_box(dk_input_key as usize), absent::dk_input_key as usize, \"core/input\"),"
    ));
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

#[test]
fn service_imports_have_a_service_a_context_a_since_and_a_missing_stub() {
    let text = std::fs::read_to_string(root().join("sdk/sdk.toml")).unwrap();
    let files = sdkgen::render(&text).unwrap();
    let rs = &files.iter().find(|f| f.0 == sdkgen::RUST_OUT).unwrap().1;
    let h = &files
        .iter()
        .find(|f| f.0 == sdkgen::C_IMPORTS_OUT)
        .unwrap()
        .1;
    // the prototype, the context and the minor come from the toml; a handle is an opaque type
    assert!(h.contains("typedef struct dk_settings dk_settings;"));
    assert!(h.contains("int32_t dk_settings_open(dk_module_ctx *ctx, dk_settings **out);"));
    assert!(h.contains("typedef int32_t dk_io_step_fn(void *arg);"));
    assert!(h.contains("typedef struct dk_page_frame dk_page_frame;"));
    assert!(h.contains("typedef void dk_page_draw_fn(void *arg, const dk_page_frame *frame);"));
    assert!(h.contains("Context: init. Since runtime 0.7.0."));
    assert!(h.contains("Context: io-step. Since runtime 0.7.0."));
    assert!(h.contains(
        "Needs core/usb-bulk: without it the import binds to a stub that returns DK_E_MISSING."
    ));
    // a module that is not in the image gives DK_E_MISSING (E_MISSING of [load.status]) from `_open`; the accessor's stub records its provider as the module's need before it returns DK_E_MISSING; a void call does nothing
    assert!(rs.contains(
        "pub unsafe extern \"C\" fn dk_usb_bulk_open(_a0: *mut u8, _a1: *mut *mut u8) -> i32 { crate::loader::stub_missing(\"core/usb-bulk\"); -73 as _ }"
    ));
    assert!(
        rs.contains("pub unsafe extern \"C\" fn dk_ui_menu_open(_a0: *mut u8) -> i32 { -73 as _ }")
    );
    assert!(rs.contains("pub unsafe extern \"C\" fn dk_ui_menu_close(_a0: *mut u8) { }"));
    // the Api table is gone: no generated header of it, no `Api` struct
    assert!(!files.iter().any(|f| f.0.ends_with("gen/api.h")));
    assert!(!rs.contains("pub struct Api"));
    // only a compiler helper may have no service: an import without one is refused
    let broken = text.replacen(
        "name = \"dk_alloc\"\nservice = \"runtime\"\nsince = \"0.7.0\"\ncontext = \"any\"\n",
        "name = \"dk_alloc\"\n",
        1,
    );
    assert_ne!(broken, text);
    assert!(sdkgen::render(&broken).is_err());
    // the packer's table
    let infos = sdkgen::import_infos();
    assert_eq!(infos["dk_usb_bulk_open"].service, "usb_bulk");
    assert_eq!(infos["dk_files_write"].context, "io-step");
    assert!(!infos.contains_key("memcpy"));
    // service, since and context come together, with names the toml lists, and since is a runtime version that exists (not above the runtime's)
    for (from, to) in [
        (
            "service = \"usb_bulk\"\nsince = \"0.7.0\"\ncontext = \"init\"",
            "service = \"usb_bulk\"\nsince = \"0.7.0\"",
        ),
        ("service = \"usb_bulk\"", "service = \"modem\""),
        ("context = \"io-step\"", "context = \"anywhere\""),
        (
            "since = \"0.7.0\"\ncontext = \"isr\"",
            "since = \"0.9.0\"\ncontext = \"isr\"",
        ),
        (
            "since = \"0.7.0\"\ncontext = \"isr\"",
            "since = \"7\"\ncontext = \"isr\"",
        ),
        (
            "requires = \"core/ui\"\nabsent = \"missing\"",
            "requires = \"core/ui\"\nabsent = \"nope\"",
        ),
    ] {
        let broken = text.replacen(from, to, 1);
        assert_ne!(broken, text, "{from}");
        assert!(sdkgen::render(&broken).is_err(), "{from}");
    }
    // a void call has no `missing` stub
    let broken = text.replacen(
        "doc = \"Close a menu.\"\nrequires = \"core/ui\"\nabsent = \"void\"",
        "doc = \"Close a menu.\"\nrequires = \"core/ui\"\nabsent = \"missing\"",
        1,
    );
    assert_ne!(broken, text);
    assert!(
        sdkgen::render(&broken)
            .unwrap_err()
            .0
            .contains("does not fit the return type")
    );
}

#[test]
fn the_span_owners_are_the_runtimes_phases_and_poll_slots() {
    // `[spans]` of the contract names what the debug module's span table and the host show: it must match the runtime's own constants
    let toml = std::fs::read_to_string(root().join(sdkgen::SOURCE)).unwrap();
    let line = |key: &str| {
        toml.lines()
            .find(|l| l.starts_with(key))
            .unwrap_or_else(|| panic!("sdk.toml has no {key}"))
            .split('#')
            .next()
            .unwrap()
            .split('=')
            .nth(1)
            .unwrap()
            .trim()
            .to_string()
    };
    let phases = line("phases = [");
    let n_phases = phases.matches('"').count() / 2;
    let abi =
        std::fs::read_to_string(root().join("modules/core/runtime/abi/src/service.rs")).unwrap();
    let block = abi.split("pub mod tick_phase {").nth(1).unwrap();
    let block = block.split('}').next().unwrap();
    assert_eq!(block.matches("pub const ").count(), n_phases);
    let state =
        std::fs::read_to_string(root().join("modules/core/runtime/src/rs/src/state.rs")).unwrap();
    assert!(state.contains(&format!("pub const POLLS: usize = {};", line("polls = "))));
}

#[test]
fn the_service_abi_hash_follows_every_byte_and_the_file_name_and_is_never_zero() {
    let a = sdkgen::svc_abi_hash(&[("x.rs", b"fn a() {}"), ("y.rs", b"")]);
    assert_eq!(
        a,
        sdkgen::svc_abi_hash(&[("x.rs", b"fn a() {}"), ("y.rs", b"")])
    );
    // one byte, the name, the order and the split between two files each change it
    assert_ne!(
        a,
        sdkgen::svc_abi_hash(&[("x.rs", b"fn b() {}"), ("y.rs", b"")])
    );
    assert_ne!(
        a,
        sdkgen::svc_abi_hash(&[("z.rs", b"fn a() {}"), ("y.rs", b"")])
    );
    assert_ne!(
        a,
        sdkgen::svc_abi_hash(&[("y.rs", b""), ("x.rs", b"fn a() {}")])
    );
    assert_ne!(
        sdkgen::svc_abi_hash(&[("x", b"ab"), ("y", b"")]),
        sdkgen::svc_abi_hash(&[("x", b"a"), ("y", b"b")])
    );
    assert_ne!(sdkgen::svc_abi_hash(&[]), 0);
}
