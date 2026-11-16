//! `patchkit bind`: the service fixture is packed in memory (no toolchain, no firmware) and its `HOOKS` entry is checked; binding is checked against
//! the real stock images (skipped when the images are absent): the site, the stock bytes and the return address a service is bound with are the
//! ones `apply` finds for the same hook. The bound files stay in memory (a bound file holds firmware bytes). The pure parts of binding (trampoline
//! encoding, the limits, the bss shift) are unit tests in `src/bind.rs`.

use dkneeds::keys::{Keys, name_hash};
use patchkit::apply::{self, Options};
use patchkit::dkm::{Dkm, ROLE_SERVICE};
use patchkit::dko::{Dko, DkoReloc, DkoSection, DkoSymbol, FORMAT};
use patchkit::error::hex;
use patchkit::firmware::Firmware;
use patchkit::module::{Library, Module};
use patchkit::pack::{PackOptions, pack_module};
use patchkit::port::Registry;
use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn image(name: &str) -> Option<Vec<u8>> {
    std::fs::read(root().join("firmware").join(name)).ok()
}

const DT2: &str = "Digitakt_II_OS1.17.syx";
const DN2: &str = "Digitone_II_OS1.12.syx";

macro_rules! need_image {
    ($f:expr) => {
        if image($f).is_none() {
            return;
        }
    };
}

const HOOK: &str = "cf.menu.hook.settings_list.tail";

const SERVICE: &str = "[module]\nid = \"test/svc-test\"\nversion = \"0.8.2\"\nname = \"S\"\ncategory = \"test\"\nrole = \"service\"\nservice = \"svc-test\"\ntype = \"raw\"\nauthor = \"digikit\"\nstatus = \"experimental\"\nsummary = \"s\"\n[[hook]]\nname = \"cf.menu.hook.settings_list.tail\"\nentry = \"hooktest_settings_tail\"\n[[object]]\npath = \"svc.dko\"\nplace = \"cf.expansion\"\n";

fn sec(name: &str, kind: &str, align: u32, bytes: &[u8]) -> DkoSection {
    DkoSection {
        name: name.into(),
        kind: kind.into(),
        align,
        size: bytes.len() as u32,
        hex: hex(bytes),
    }
}

fn sym(name: &str, section: &str, value: u64, global: bool) -> DkoSymbol {
    DkoSymbol {
        name: name.into(),
        section: Some(section.into()),
        value,
        global,
    }
}

fn rel(offset: u64, symbol: &str) -> DkoReloc {
    DkoReloc {
        section: ".data".into(),
        offset,
        rtype: "cf.32".into(),
        symbol: symbol.into(),
        addend: 0,
    }
}

/// A ColdFire object with `dk_module` (init and tick set), the global handler `hooktest_settings_tail` and the static `local_tail`.
fn service_object() -> Dko {
    let text = [
        0x4e, 0x75, 0x4e, 0x75, 0x4e, 0x75, 0x4e, 0x75, 0x4e, 0x75, 0x4e, 0x75,
    ];
    let mut o = Dko {
        format: FORMAT,
        arch: "cf".into(),
        source_sha256: "0".repeat(64),
        toolchain: Default::default(),
        sections: vec![
            sec(".text", "text", 2, &text),
            sec(".data", "data", 4, &[0; 24]),
        ],
        symbols: vec![
            sym("init_fn", ".text", 0, true),
            sym("tick_fn", ".text", 2, true),
            sym("hooktest_settings_tail", ".text", 6, true),
            sym("local_tail", ".text", 8, false),
            sym("dk_module", ".data", 8, true),
        ],
        relocs: vec![rel(8, "init_fn"), rel(16, "tick_fn")],
    };
    o.stamp_isa();
    o
}

fn library(manifest: &str) -> Library {
    Library {
        modules: vec![Module {
            manifest: toml::from_str(manifest).unwrap(),
            objects: vec![service_object()],
            alloc: Default::default(),
            digest: String::new(),
            dir: Default::default(),
        }],
    }
}

fn pack_with(manifest: &str, device: &str) -> patchkit::Result<Vec<u8>> {
    let o = PackOptions {
        device: Some(device.into()),
        ..Default::default()
    };
    pack_module(&library(manifest), "test/svc-test", &o).map(|p| p.bytes)
}

/// The packed service file for `device` ("dt2" or "dn2").
fn pack_service(device: &str) -> Vec<u8> {
    pack_with(SERVICE, device).unwrap()
}

fn registry() -> Registry {
    Registry::from_dir(&root().join("devices/ports")).unwrap()
}

#[test]
fn a_service_with_a_hook_packs_its_hooks_entry() {
    let dks = pack_service("dt2");
    let dkm = Dkm::parse(&dks).unwrap();
    assert_eq!(dkm.role, ROLE_SERVICE);
    let k = Keys::parse(&dkm.register).unwrap();
    let hooks: Vec<_> = k.hooks().map(|p| (p.a, p.b)).collect();
    assert_eq!(hooks.len(), 1);
    assert_eq!(hooks[0].0, name_hash(HOOK));
    assert_eq!(
        hooks[0].1,
        patchkit::dkm::image_symbols(&service_object()).unwrap()["hooktest_settings_tail"]
    );
    assert_eq!(k.bind_count(), 0, "a packed service is not bound");
}

#[test]
fn the_packer_writes_the_device_mask_of_a_hook_in_the_top_byte() {
    let m = SERVICE.replace(
        "entry = \"hooktest_settings_tail\"\n",
        "entry = \"hooktest_settings_tail\"\ndevices = [\"dn2\"]\n",
    );
    let dks = pack_with(&m, "dt2").unwrap();
    let dkm = Dkm::parse(&dks).unwrap();
    let k = Keys::parse(&dkm.register).unwrap();
    let h = k.hooks().next().unwrap();
    let mask = patchkit::dkm::hook_device_mask(&["dn2".to_string()]).unwrap();
    assert_ne!(mask, 0);
    assert_eq!(h.b >> 24, mask);
    assert_eq!(
        h.b & patchkit::dkm::HOOK_ENTRY_MASK,
        patchkit::dkm::image_symbols(&service_object()).unwrap()["hooktest_settings_tail"]
    );
}

#[test]
fn a_missing_static_or_repeated_handler_and_a_hook_without_the_role_are_refused() {
    for bad in ["no_such_fn", "local_tail"] {
        let m = SERVICE.replace("hooktest_settings_tail", bad);
        let e = pack_with(&m, "dt2").unwrap_err().to_string();
        assert!(e.contains(bad), "{e}");
    }
    let twice = SERVICE.replace(
        "[[object]]",
        "[[hook]]\nname = \"cf.menu.hook.settings_list.tail\"\nentry = \"init_fn\"\n[[object]]",
    );
    let e = pack_with(&twice, "dt2").unwrap_err().to_string();
    assert!(e.contains("declared twice"), "{e}");
    let plain = SERVICE
        .replace("role = \"service\"\n", "")
        .replace("service = \"svc-test\"\n", "");
    let e = pack_with(&plain, "dt2").unwrap_err().to_string();
    assert!(
        e.contains("belong to a module with `role = \"service\"`"),
        "{e}"
    );
}

/// A ColdFire object with `rts; rts` and the global symbols `_f` (0) and `_g` (2), for the apply side.
fn apply_object() -> serde_json::Value {
    serde_json::json!({
        "format": 1, "arch": "cf", "source_sha256": "0".repeat(64),
        "sections": [{"name": ".text", "kind": "text", "align": 2, "size": 4, "hex": "4e754e75"}],
        "symbols": [
            {"name": "_f", "section": ".text", "value": 0, "global": true},
            {"name": "_g", "section": ".text", "value": 2, "global": true}
        ],
        "relocs": []
    })
}

fn apply_module() -> Module {
    let text = format!(
        "[module]\nid = \"t/h\"\nversion = \"1.0.0\"\nname = \"t/h\"\ncategory = \"test\"\ntype = \"raw\"\nauthor = \"digikit\"\nstatus = \"experimental\"\nsummary = \"synthetic\"\n\n[[object]]\npath = \"x.json\"\nplace = \"cf.expansion\"\n[[op]]\nop = \"hook\"\nhook = \"{HOOK}\"\nto = \"_f\"\n"
    );
    let read = |_p: &str| -> patchkit::Result<String> { Ok(apply_object().to_string()) };
    Module::from_parts(&text, &read).unwrap()
}

fn check(img: &str, device: &str, site: u64, ret: u64) {
    let raw = image(img).unwrap();
    let reg = registry();
    let texts = patchkit::fwgen::fw_texts(&root()).unwrap();
    let decls: Vec<&str> = texts.iter().map(String::as_str).collect();
    let dks = pack_service(device);
    let (file, hooks) = patchkit::bind::bind(&dks, &raw, &reg, &decls).unwrap();
    assert_eq!(hooks.len(), 1);
    let b = &hooks[0];
    assert_eq!(b.hook, HOOK);

    // the bound file: one BIND item with the site hash, address, trampoline offset and stock bytes
    let dkm = Dkm::parse(&file).unwrap();
    assert_eq!(dkm.role, ROLE_SERVICE);
    let k = Keys::parse(&dkm.register).unwrap();
    assert_eq!(k.bind_count(), 1);
    let item = k.bind(0).unwrap();
    assert_eq!(item.site, name_hash(HOOK));
    assert_eq!(item.addr, b.site_addr);
    assert_eq!(item.len, b.site_len);
    assert_eq!(item.tramp, b.tramp_off);
    assert_eq!(item.stock, &b.stock[..]);
    assert!((item.tramp as usize) < dkm.image.len());
    assert!(
        k.hooks()
            .eq(Keys::parse(&Dkm::parse(&dks).unwrap().register)
                .unwrap()
                .hooks())
    );
    // binding twice is refused
    let e = patchkit::bind::bind(&file, &raw, &reg, &decls).unwrap_err();
    assert!(e.0.contains("already bound"), "{e}");

    let mut lib = Library::scan_repo_for(&root(), true, Some(&[])).unwrap();
    lib.modules.push(apply_module());
    let out =
        apply::apply_with(&raw, &reg, &lib, &["t/h".to_string()], Options::default()).unwrap();
    let j = serde_json::to_value(&out.plan).unwrap();
    let h = &j["hooks"][0];
    assert_eq!(h["hook"], HOOK);

    // the same site, length, return address and displaced instructions
    assert_eq!(b.site_addr as u64, h["addr"].as_u64().unwrap());
    assert_eq!(b.site_len as u64, h["site_len"].as_u64().unwrap());
    assert_eq!(b.ret as u64, h["ret"].as_u64().unwrap());
    let displaced: Vec<String> = h["displaced_insns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect();
    assert_eq!(b.displaced, displaced);
    assert_eq!(b.stock.len(), b.site_len as usize);
    let fw = Firmware::parse(&raw).unwrap();
    let main = fw.section(3).unwrap();
    let o = (b.site_addr as u64 - main.dest as u64) as usize;
    assert_eq!(&main.data[o..o + b.stock.len()], &b.stock[..]);

    // the values of the static gate
    assert_eq!(b.site_addr as u64, site);
    assert_eq!(b.ret as u64, ret);
    assert_eq!(hex(&b.stock), "205248780002");
}

#[test]
fn bind_and_apply_agree_on_the_dt2_site() {
    need_image!(DT2);
    check(DT2, "dt2", 0x400a_5eac, 0x400a_5eb2);
}

#[test]
fn bind_and_apply_agree_on_the_dn2_site() {
    need_image!(DN2);
    check(DN2, "dn2", 0x4009_ff88, 0x4009_ff8e);
}

// ---- raw hooks with a tail exit ------------------------------------------------------------------------------------

/// A raw hook (declared in `devices/fw/` with no handler, mode `replace`) on the DT2 image whose displaced instructions can be copied: its name, the declarations
/// with `tail = true` added to it and the resolved site.
fn raw_hook(raw: &[u8], reg: &Registry) -> (String, Vec<String>, patchkit::apply::HookSite) {
    let texts = patchkit::fwgen::fw_texts(&root()).unwrap();
    let f = Firmware::parse(raw).unwrap();
    let port = reg.find(&f.syx_sha256).unwrap();
    for h in port.hook.iter().filter(|h| h.name.starts_with("cf.")) {
        let Ok(site) = patchkit::apply::resolve_hook_site(raw, reg, &h.name) else {
            continue;
        };
        if site.mode != "replace" || site.old.len() > dkneeds::keys::BIND_STOCK_MAX {
            continue;
        }
        let a = patchkit::prim::cf_tail(&site.cf, 0, site.ret);
        let b = patchkit::prim::cf_tail(&site.cf, 0x1000, site.ret);
        if a.is_err() || a.ok() != b.ok() {
            continue;
        }
        let key = format!("name = \"{}\"\n", h.name);
        let Some(text) = texts
            .iter()
            .find(|t| t.contains(&key) && t.contains("[[hook]]"))
        else {
            continue;
        };
        let mut decls = texts.clone();
        let i = decls.iter().position(|t| t == text).unwrap();
        decls[i] = text.replacen(&key, &format!("{key}tail = true\n"), 1);
        return (h.name.clone(), decls, site);
    }
    panic!("no raw hook with a copyable site on the image");
}

/// The service fixture taking the raw hook `hook`, whose object refers to the tail of the hook (`dk_tail_<hash>`) in a data word (after the four of `dk_module`).
fn tail_service(hook: &str, device: &str) -> Vec<u8> {
    let manifest = SERVICE.replace(HOOK, hook);
    let mut obj = service_object();
    obj.sections[1] = sec(".data", "data", 4, &[0; 28]);
    obj.relocs.push(rel(24, &patchkit::bind::tail_import(hook)));
    obj.stamp_isa();
    let lib = Library {
        modules: vec![Module {
            manifest: toml::from_str(&manifest).unwrap(),
            objects: vec![obj],
            alloc: Default::default(),
            digest: String::new(),
            dir: Default::default(),
        }],
    };
    let o = PackOptions {
        device: Some(device.into()),
        ..Default::default()
    };
    pack_module(&lib, "test/svc-test", &o).unwrap().bytes
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn a_raw_hook_with_a_tail_binds_to_the_stock_bytes_of_the_image() {
    need_image!(DT2);
    let raw = image(DT2).unwrap();
    let reg = registry();
    let (hook, decls, site) = raw_hook(&raw, &reg);
    let decls: Vec<&str> = decls.iter().map(String::as_str).collect();
    let dks = tail_service(&hook, "dt2");
    let (file, hooks) = patchkit::bind::bind(&dks, &raw, &reg, &decls).unwrap();
    let b = &hooks[0];
    let tail_off = b.tail_off.expect("the raw hook has a tail") as usize;
    let dkm = Dkm::parse(&file).unwrap();
    // the BIND item carries the site's stock bytes; the trampoline is the entry itself
    let k = Keys::parse(&dkm.register).unwrap();
    let item = k.bind(0).unwrap();
    assert_eq!(item.stock, &site.old[..]);
    assert_eq!(
        item.tramp,
        patchkit::dkm::image_symbols(&service_object()).unwrap()["hooktest_settings_tail"]
    );
    // the tail is the displaced instructions of the user's image, then a jump to the first instruction after the site
    let want = patchkit::prim::cf_tail(&site.cf, 0, site.ret).unwrap();
    assert_eq!(&dkm.image[tail_off..tail_off + want.len()], &want[..]);
    // the reference is a base-relative relocation to the tail and the import is gone
    assert!(
        dkm.imports
            .iter()
            .all(|n| n != &patchkit::bind::tail_import(&hook))
    );
    assert!(dkm.relocs.iter().any(|(off, k)| {
        *off != 0
            && k.is_none()
            && u32::from_be_bytes(
                dkm.image[*off as usize..*off as usize + 4]
                    .try_into()
                    .unwrap(),
            ) as usize
                == tail_off
    }));
}

#[test]
fn an_unbound_dks_holds_no_firmware_bytes() {
    need_image!(DT2);
    let raw = image(DT2).unwrap();
    let reg = registry();
    let (hook, decls, site) = raw_hook(&raw, &reg);
    let decls: Vec<&str> = decls.iter().map(String::as_str).collect();
    let dks = tail_service(&hook, "dt2");
    let tail = patchkit::prim::cf_tail(&site.cf, 0, site.ret).unwrap();
    // unbound: the tail is a named import, and neither the site's stock bytes nor the tail code are anywhere in the file
    let dkm = Dkm::parse(&dks).unwrap();
    assert!(dkm.imports.contains(&patchkit::bind::tail_import(&hook)));
    assert!(
        !contains(&dks, &site.old),
        "stock bytes of the site in the unbound file"
    );
    assert!(!contains(&dks, &tail), "the tail code in the unbound file");
    // bound: both are there
    let (file, _) = patchkit::bind::bind(&dks, &raw, &reg, &decls).unwrap();
    assert!(contains(&file, &site.old) && contains(&file, &tail));
}

/// `patchkit --root ROOT bind IN --firmware FW -o OUT`: the refusal comes before the firmware and the ports are read.
fn bind_cli(root: &Path, dks: &Path, out: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_patchkit"))
        .arg("--root")
        .arg(root)
        .arg("bind")
        .arg(dks)
        .arg("--firmware")
        .arg(root.join("no-such.syx"))
        .arg("-o")
        .arg(out)
        .output()
        .unwrap()
}

#[test]
fn the_command_refuses_an_output_in_the_source_tree_outside_dist_and_out() {
    let d = std::env::temp_dir().join(format!("pk-bind-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let dks = d.join("in.dks");
    std::fs::write(&dks, pack_service("dt2")).unwrap();
    for out in ["svc.dks", "modules/svc.dks"] {
        let r = bind_cli(&d, &dks, &d.join(out));
        assert!(!r.status.success(), "{out} must be refused");
        let e = String::from_utf8_lossy(&r.stderr);
        assert!(e.contains("must never be committed"), "{out}: {e}");
        assert!(!d.join(out).exists());
    }
    // under dist/ or out/ the check passes: the command goes on and stops at the missing ports
    for out in ["dist/svc.dks", "out/x/svc.dks"] {
        let r = bind_cli(&d, &dks, &d.join(out));
        assert!(!r.status.success());
        let e = String::from_utf8_lossy(&r.stderr);
        assert!(!e.contains("must never be committed"), "{out}: {e}");
    }
    let _ = std::fs::remove_dir_all(&d);
}
