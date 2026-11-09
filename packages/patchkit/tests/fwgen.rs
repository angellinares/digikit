//! The firmware bindings: signatures, struct layout and the rules that refuse bad declarations.

use patchkit::fwgen::{POINTER_RETURNS_ALLOWED, RUST_OUT, reference, render};
use patchkit::port::Registry;
use serde_json::Value;

const RULE: &str = "cpu = \"cf\"\nrule = { kind = \"cf_func\", hash = \"00\", offset = 0, len = 4 }\ndoc = \"A doc.\"\n";

fn fn_entry(name: &str, sig: &str) -> String {
    format!("[[fn]]\nname = \"{name}\"\n{RULE}sig = \"{sig}\"\n")
}

fn out(text: &str, suffix: &str) -> String {
    let files = render(&[("t.toml", text)]).expect("renders");
    files
        .into_iter()
        .find(|(p, _)| p.ends_with(suffix))
        .unwrap_or_else(|| panic!("no output {suffix}"))
        .1
}

#[test]
fn sig_with_pointers_and_const() {
    let t = fn_entry(
        "cf.view.fn.view_controller_push",
        "u32 (void *v, const char *name, u8 flag)",
    );
    let rs = out(&t, "rs/src/lib.rs");
    assert!(rs.contains("#[link_name = \"cf.view.fn.view_controller_push\"]"));
    assert!(rs.contains(
        "pub fn view_controller_push(v: *mut core::ffi::c_void, name: *const u8, flag: u8) -> u32;"
    ));
    let c = out(&t, "include/dk_fw_view.h");
    assert!(c.contains(
        "uint32_t dk_fw_view_view_controller_push(void *v, const char *name, uint8_t flag) __asm__(\"cf.view.fn.view_controller_push\");"
    ));
}

#[test]
fn varargs_and_empty_list() {
    let t = format!(
        "{}{}",
        fn_entry("cf.text.fn.fmt", "void (u32 x, ...)"),
        fn_entry("cf.text.fn.none", "void ()")
    );
    let rs = out(&t, "rs/src/lib.rs");
    assert!(rs.contains("pub fn fmt(x: u32, ...);"));
    assert!(rs.contains("pub fn none();"));
    let c = out(&t, "include/dk_fw_text.h");
    assert!(c.contains("void dk_fw_text_fmt(uint32_t x, ...)"));
    assert!(c.contains("void dk_fw_text_none(void)"));
}

const GAP: &str = "[[struct]]\nname = \"cf.menu.struct.Item\"\nsize = 0x10\nfields = [\n  { name = \"id\", off = 0x4, type = \"u32\" },\n  { name = \"cb\", off = 0xc, type = \"ptr\" },\n]\nmark = \"D\"\ndoc = \"An item.\"\n";

#[test]
fn struct_gap_gets_padding_and_asserts() {
    let rs = out(GAP, "rs/src/lib.rs");
    assert!(rs.contains("pub _pad0: [u8; 4],"));
    assert!(rs.contains("pub _pad1: [u8; 4],"));
    assert!(rs.contains("pub cb: u32,"));
    assert!(rs.contains("size_of::<Item>() == 0x10"));
    assert!(rs.contains("offset_of!(Item, cb) == 0xc"));
    let c = out(GAP, "include/dk_fw_menu.h");
    assert!(c.contains("_Static_assert(offsetof(Item, id) == 0x4"));
    let inc = out(GAP, "inc/dk_fw_menu.inc");
    assert!(inc.contains(".equ Item_size, 0x10"));
    assert!(inc.contains(".equ Item_cb, 0xc"));
}

#[test]
fn overlapping_field_is_an_error() {
    let t = "[[struct]]\nname = \"cf.menu.struct.Bad\"\nsize = 0x10\nfields = [\n  { name = \"a\", off = 0x0, type = \"u32\" },\n  { name = \"b\", off = 0x2, type = \"u32\" },\n]\ndoc = \"Bad.\"\n";
    let e = render(&[("t.toml", t)]).err().expect("error");
    assert!(e.0.contains("cf.menu.struct.Bad"), "{}", e.0);
}

#[test]
fn pointer_return_renders_in_rust_and_c() {
    assert!(POINTER_RETURNS_ALLOWED);
    let t = format!(
        "{}{}",
        fn_entry("cf.mem.fn.new", "void *(u32 n)"),
        fn_entry("cf.mem.fn.bytes", "const u8 *(u32 n)")
    );
    let rs = out(&t, "rs/src/lib.rs");
    assert!(rs.contains("pub fn new(n: u32) -> *mut core::ffi::c_void;"));
    assert!(rs.contains("pub fn bytes(n: u32) -> *const u8;"));
    let c = out(&t, "include/dk_fw_mem.h");
    assert!(c.contains("void * dk_fw_mem_new(uint32_t n) __asm__(\"cf.mem.fn.new\");"));
    assert!(
        c.contains("const uint8_t * dk_fw_mem_bytes(uint32_t n) __asm__(\"cf.mem.fn.bytes\");")
    );
}

#[test]
fn site_bind_static_makes_a_static() {
    let site = |extra: &str| {
        format!("[[site]]\nname = \"cf.menu.site.settings_list.system_icon_ref\"\n{RULE}{extra}")
    };
    let rs = out(&site("bind = \"static\"\n"), "rs/src/lib.rs");
    assert!(rs.contains("#[link_name = \"cf.menu.site.settings_list.system_icon_ref\"]"));
    assert!(rs.contains("pub static SETTINGS_LIST_SYSTEM_ICON_REF: u8;"));
    let plain = render(&[("t.toml", site("").as_str())]).expect("renders");
    assert!(!plain[0].1.contains("SETTINGS_LIST_SYSTEM_ICON_REF"));
    assert!(render(&[("t.toml", site("bind = \"extern\"\n").as_str())]).is_err());
}

#[test]
fn entry_without_doc_is_an_error() {
    let t = "[[fn]]\nname = \"cf.view.fn.push\"\ncpu = \"cf\"\nrule = { kind = \"cf_func\", hash = \"00\", offset = 0, len = 4 }\nsig = \"void ()\"\n";
    let e = render(&[("t.toml", t)]).err().expect("error");
    assert!(e.0.contains("cf.view.fn.push: no doc"), "{}", e.0);
}

#[test]
fn unknown_key_is_an_error() {
    let t = format!(
        "{}colour = \"red\"\n",
        fn_entry("cf.view.fn.view_controller_push", "void ()")
    );
    assert!(render(&[("t.toml", t.as_str())]).is_err());
}

/// A site `cf.view.site.loop` and a hook `cf.view.hook.loop` on `site`; `regs` is a TOML inline table.
fn hook_text(site: &str, mode: &str, regs: &str, extra: &str) -> String {
    let doc = if extra.contains("doc =") {
        ""
    } else {
        "doc = \"A hook.\"\n"
    };
    format!(
        "[[site]]\nname = \"cf.view.site.loop\"\n{RULE}[[hook]]\nname = \"cf.view.hook.loop\"\nsite = \"{site}\"\ncpu = \"cf\"\nmode = \"{mode}\"\nregs = {regs}\n{doc}{extra}"
    )
}

fn hook_error(site: &str, mode: &str, regs: &str, extra: &str) -> String {
    let t = hook_text(site, mode, regs, extra);
    let e = render(&[("t.toml", t.as_str())]).err().expect("error");
    assert!(e.0.contains("cf.view.hook.loop"), "{}", e.0);
    e.0
}

#[test]
fn before_hook_with_a_register_arg() {
    let t = hook_text(
        "cf.view.site.loop",
        "before",
        "{ in = [\"a2\"], clobbers = [\"d0-d1\", \"a0-a1\", \"ccr\"] }",
        "handler = { sig = \"void (u8 *menu)\", args = [\"a2\"] }\ndoc = \"Top of the loop.\"\n",
    );
    let inc = out(&t, "inc/dk_fw_view.inc");
    assert!(inc.contains(
        "/* cf.view.hook.loop: Top of the loop. */\n.macro dk_fw_view_loop_hook handler, next\n    move.l  %a2,-(%sp)\n    jsr     \\handler\n    addq.l  #4,%sp\n    jmp     \\next\n.endm\n"
    ), "{inc}");
    let c = out(&t, "include/dk_fw_view.h");
    assert!(
        c.contains("/* Top of the loop. */\ntypedef void dk_fw_view_loop_hook(uint8_t *menu);"),
        "{c}"
    );
    let rs = out(&t, "rs/src/lib.rs");
    assert!(
        rs.contains(
            "/// Top of the loop.\n    pub type LoopHook = unsafe extern \"C\" fn(menu: *mut u8);"
        ),
        "{rs}"
    );
}

#[test]
fn retarget_hook_with_stack_args_saves_registers() {
    let t = hook_text(
        "cf.view.site.loop",
        "retarget",
        "{ in = [], clobbers = [\"ccr\"] }",
        "handler = { sig = \"i32 (i32 a, i32 b, i32 c, i32 d)\", args = [\"sp+4\", \"sp+8\", \"sp+12\", \"sp+16\"] }\n",
    );
    let inc = out(&t, "inc/dk_fw_view.inc");
    let push = "    move.l  32(%sp),-(%sp)\n".repeat(4);
    let want = format!(
        ".macro dk_fw_view_loop_hook handler, next\n    lea     -16(%sp),%sp\n    movem.l %d0-%d1/%a0-%a1,(%sp)\n{push}    jsr     \\handler\n    lea     16(%sp),%sp\n    movem.l (%sp),%d0-%d1/%a0-%a1\n    lea     16(%sp),%sp\n    jmp     \\next\n.endm\n"
    );
    assert!(inc.contains(&want), "{inc}");
    let rs = out(&t, "rs/src/lib.rs");
    assert!(
        rs.contains(
            "pub type LoopHook = unsafe extern \"C\" fn(a: i32, b: i32, c: i32, d: i32) -> i32;"
        ),
        "{rs}"
    );
}

#[test]
fn bad_hooks_are_errors() {
    let regs = "{ in = [\"a2\"], clobbers = [\"ccr\"] }";
    let handler =
        |args: &str| format!("handler = {{ sig = \"void (u8 *menu)\", args = {args} }}\n");
    assert!(hook_error("cf.view.site.loop", "after", regs, "").contains("mode"));
    assert!(
        hook_error(
            "cf.view.site.loop",
            "before",
            "{ in = [], clobbers = [\"d9\"] }",
            ""
        )
        .contains("unknown register")
    );
    assert!(
        hook_error("cf.view.site.loop", "replace", regs, &handler("[\"a2\"]")).contains("handler")
    );
    assert!(hook_error("cf.view.site.loop", "before", regs, &handler("[]")).contains("args"));
    assert!(
        hook_error("cf.view.site.loop", "before", regs, &handler("[\"sp+2\"]")).contains("sp+N")
    );
    assert!(
        hook_error("cf.view.site.loop", "before", regs, &handler("[\"a3\"]"))
            .contains("not in regs.in")
    );
    assert!(
        hook_error(
            "cf.view.site.loop",
            "before",
            "{ in = [], clobbers = [], modes = { simd = false } }",
            ""
        )
        .contains("modes")
    );
    assert!(hook_error("cf.view.site.missing", "before", regs, "").contains("site"));
}

#[test]
fn sharc_entries_make_no_output() {
    let t = "[[fn]]\nname = \"dsp.fx.fn.send\"\ncpu = \"sharc\"\nrule = { kind = \"sharc_func\" }\nsig = \"void ()\"\ndoc = \"Sends.\"\n";
    let files = render(&[("devices/fw/dsp/fx.toml", t)]).expect("renders");
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].0, RUST_OUT);
    assert!(!files[0].1.contains("pub mod"));
}

const SUBSYSTEMS: &str = "[cf]\nview = \"views\"\n[dsp]\nfx = \"effects\"\n";

fn port(device: &str, version: &str, sha: char, anchors: &[(&str, u64)]) -> String {
    let mut t = format!(
        "[port]\ndevice = \"{device}\"\nversion = \"{version}\"\nsyx_sha256 = \"{}\"\nmain_sha256 = \"\"\ndsp_sha256 = \"\"\nresolved_by = \"t\"\n\n[limits]\ncontainer_max = 1\ndsp_stored_max = 1\ndsp_depacked_max = 1\nmain_staged_max = 1\n",
        sha.to_string().repeat(64)
    );
    for (name, addr) in anchors {
        t += &format!(
            "\n[[anchor]]\nname = \"{name}\"\ncpu = \"cf\"\naddr = {addr}\nlen = 4\nsha256 = \"00\"\nmark = \"D\"\nrule = \"t\"\n"
        );
    }
    t
}

fn reference_of(decls: &str, ports: &[String]) -> Value {
    let reg = Registry::from_texts(ports.iter().map(|s| s.as_str())).expect("ports");
    let text = reference(&[("devices/fw/view.toml", decls)], SUBSYSTEMS, &reg).expect("reference");
    assert!(text.ends_with('\n'));
    serde_json::from_str(&text).expect("json")
}

fn entry<'a>(v: &'a Value, name: &str) -> &'a Value {
    v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == name)
        .expect("entry")
}

#[test]
fn reference_fn_resolves_on_two_ports_with_bindings() {
    let t = fn_entry("cf.view.fn.push", "void ()");
    let ports = [
        port("dt2", "1.17", 'a', &[("cf.view.fn.push", 0x40115b90)]),
        port("dn2", "1.12", 'b', &[("cf.view.fn.push", 0x40200000)]),
    ];
    let v = reference_of(&t, &ports);
    let e = entry(&v, "cf.view.fn.push");
    assert_eq!(e["at"]["dt2-1.17"][0]["addr"], "0x40115b90");
    assert_eq!(e["at"]["dn2-1.12"][0]["addr"], "0x40200000");
    assert_eq!(e["bindings"]["rust"], "dkfw::view::push");
    assert_eq!(e["bindings"]["c"], "dk_fw_view_push");
    assert_eq!(e["rule"]["dt2"]["kind"], "cf_func");
    assert_eq!(e["rule"]["dn2"]["len"], 4);
    assert_eq!(v["subsystems"][0]["name"], "view");
    assert_eq!(v["subsystems"][0]["meaning"], "views");
    assert_eq!(v["subsystems"][0]["count"], 1);
}

#[test]
fn reference_sites_lists_every_numbered_anchor() {
    let t = format!("[[sites]]\nname = \"cf.view.sites.loops\"\n{RULE}");
    let ports = [port(
        "dt2",
        "1.17",
        'a',
        &[
            ("cf.view.sites.loops.0", 0x1000),
            ("cf.view.sites.loops.1", 0x2000),
        ],
    )];
    let v = reference_of(&t, &ports);
    let at = entry(&v, "cf.view.sites.loops")["at"]["dt2-1.17"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(at.len(), 2);
    assert_eq!(at[1]["addr"], "0x2000");
    assert!(entry(&v, "cf.view.sites.loops").get("bindings").is_none());
}

#[test]
fn reference_hook_carries_site_mode_regs_and_bindings() {
    let t = hook_text(
        "cf.view.site.loop",
        "before",
        "{ in = [\"a2\"], clobbers = [\"ccr\"] }",
        "handler = { sig = \"void (u8 *menu)\", args = [\"a2\"] }\n",
    );
    let v = reference_of(&t, &[port("dt2", "1.17", 'a', &[])]);
    let e = entry(&v, "cf.view.hook.loop");
    assert_eq!(e["site"], "cf.view.site.loop");
    assert_eq!(e["mode"], "before");
    assert_eq!(e["regs"]["in"][0], "a2");
    assert_eq!(e["handler"]["args"][0], "a2");
    assert!(e.get("offset").is_none());
    assert_eq!(e["bindings"]["rust"], "dkfw::view::LoopHook");
    assert_eq!(e["bindings"]["c"], "dk_fw_view_loop_hook");
    assert_eq!(e["bindings"]["gas"], "dk_fw_view_loop_hook");
}

#[test]
fn reference_per_device_rule_gives_both_device_rules() {
    let t = "[[fn]]\nname = \"cf.view.fn.push\"\ncpu = \"cf\"\nsig = \"void ()\"\ndoc = \"Pushes.\"\n[fn.rule.dt2]\nkind = \"cf_func\"\nhash = \"00\"\noffset = 2\nlen = 4\n[fn.rule.dn2]\nkind = \"cf_func\"\nhash = \"00\"\noffset = 6\nlen = 4\nmark = \"O\"\n";
    let v = reference_of(t, &[port("dt2", "1.17", 'a', &[])]);
    let r = &entry(&v, "cf.view.fn.push")["rule"];
    assert_eq!(r["dt2"]["offset"], 2);
    assert_eq!(r["dn2"]["offset"], 6);
    assert_eq!(r["dn2"]["mark"], "O");
    assert!(r["dt2"].get("mark").is_none());
}

#[test]
fn reference_hook_address_is_site_plus_offset_on_listing_ports() {
    let t = hook_text(
        "cf.view.site.loop",
        "before",
        "{ in = [], clobbers = [] }",
        "offset = 4\n",
    );
    let with = format!(
        "{}\n[[hook]]\nname = \"cf.view.hook.loop\"\nsite = \"cf.view.site.loop\"\noffset = 4\nmode = \"before\"\n",
        port("dt2", "1.17", 'a', &[("cf.view.site.loop", 0x1000)])
    );
    let without = port("dn2", "1.12", 'b', &[("cf.view.site.loop", 0x2000)]);
    let v = reference_of(&t, &[with, without]);
    let at = &entry(&v, "cf.view.hook.loop")["at"];
    assert_eq!(at["dt2-1.17"][0]["addr"], "0x1004");
    assert!(at["dt2-1.17"][0].get("len").is_none());
    assert!(at.get("dn2-1.12").is_none());
}

#[test]
fn reference_leaves_out_a_port_without_the_anchor() {
    let t = fn_entry("cf.view.fn.push", "void ()");
    let ports = [
        port("dt2", "1.17", 'a', &[("cf.view.fn.push", 0x1000)]),
        port("dn2", "1.12", 'b', &[]),
    ];
    let v = reference_of(&t, &ports);
    let at = &entry(&v, "cf.view.fn.push")["at"];
    assert!(at.get("dt2-1.17").is_some());
    assert!(at.get("dn2-1.12").is_none());
}
