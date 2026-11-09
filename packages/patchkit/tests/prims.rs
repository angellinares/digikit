//! The generic primitives (phase 3b.1): sections copied from the image (P1), registries and entries (P2),
//! expressions and typed writes (P3), hooks (P4), device filters (P5) and multi-site writes (P6), with
//! synthetic modules over the real ports and stock images (skipped when the images are absent).

use patchkit::apply::{self, Applied, Options};
use patchkit::dsp::Stream;
use patchkit::firmware::Firmware;
use patchkit::module::{Library, Module};
use patchkit::port::{Anchor, Hook, Registry};
use patchkit::verify::read_segments;
use serde_json::json;
use std::path::PathBuf;

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

fn library(extra: Vec<Module>) -> Library {
    // without the toolchain the content modules are left out; the tests that need one start with `need_toolchain!()`
    let mut lib = Library::scan_repo_or_skip(&root())
        .unwrap_or_else(|| Library::scan_repo_for(&root(), true, Some(&[])).unwrap());
    lib.modules.extend(extra);
    lib
}

fn try_module(
    id: &str,
    header_extra: &str,
    body: &str,
    dko: Option<serde_json::Value>,
) -> patchkit::Result<Module> {
    let text = format!(
        "[module]\nid = \"{id}\"\nversion = \"1.0.0\"\nname = \"{id}\"\ncategory = \"test\"\ntype = \"raw\"\nauthor = \"digikit\"\nstatus = \"experimental\"\nsummary = \"synthetic\"\n{header_extra}\n{body}\n"
    );
    let read = move |_p: &str| -> patchkit::Result<String> { Ok(dko.clone().unwrap().to_string()) };
    Module::from_parts(&text, &read)
}

fn module(id: &str, header_extra: &str, body: &str, dko: Option<serde_json::Value>) -> Module {
    try_module(id, header_extra, body, dko).unwrap()
}

/// A ColdFire object with `rts; rts` in .text and the global symbols `_f` (0) and `_g` (2).
fn cf_obj() -> serde_json::Value {
    json!({
        "format": 1, "arch": "cf", "source_sha256": "0".repeat(64),
        "sections": [{"name": ".text", "kind": "text", "align": 2, "size": 4, "hex": "4e754e75"}],
        "symbols": [
            {"name": "_f", "section": ".text", "value": 0, "global": true},
            {"name": "_g", "section": ".text", "value": 2, "global": true}
        ],
        "relocs": []
    })
}

fn expect_err<T>(r: patchkit::Result<T>, needle: &str) {
    match r {
        Ok(_) => panic!("expected an error containing {needle:?}"),
        Err(e) => assert!(
            e.0.contains(needle),
            "error {:?} does not contain {needle:?}",
            e.0
        ),
    }
}

fn run_on(img: &str, reg: &Registry, lib: &Library, ids: &[&str]) -> patchkit::Result<Applied> {
    let raw = image(img).expect("image");
    let ids: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
    apply::apply_with(&raw, reg, lib, &ids, Options::default())
}

fn run(lib: &Library, ids: &[&str]) -> patchkit::Result<Applied> {
    run_on(DT2, &registry(), lib, ids)
}

/// The stock MAIN OS image and its load address.
fn stock_main(img: &str) -> (Vec<u8>, u64) {
    let fw = Firmware::parse(&image(img).unwrap()).unwrap();
    let s = fw.section(3).unwrap();
    (s.data.clone(), s.dest as u64)
}

/// A data or code anchor over bytes of the stock MAIN OS, hash included.
fn add_anchor(reg: &mut Registry, img: &str, name: &str, kind: &str, addr: u64, len: u64) {
    use sha2::{Digest, Sha256};
    let (main, base) = stock_main(img);
    let b = &main[(addr - base) as usize..(addr - base + len) as usize];
    let sha: String = Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect();
    let fw = Firmware::parse(&image(img).unwrap()).unwrap();
    for p in reg
        .ports
        .iter_mut()
        .filter(|p| p.port.syx_sha256 == fw.syx_sha256)
    {
        p.anchor.push(Anchor {
            name: name.into(),
            cpu: "cf".into(),
            kind: kind.into(),
            addr,
            pm: None,
            len,
            sha256: sha.clone(),
            src: None,
            mark: "D".into(),
            rule: "test".into(),
        });
    }
}

/// A declared hook on the port of `img`.
fn add_hook(reg: &mut Registry, img: &str, name: &str, site: &str, offset: i64, mode: &str) {
    let fw = Firmware::parse(&image(img).unwrap()).unwrap();
    for p in reg
        .ports
        .iter_mut()
        .filter(|p| p.port.syx_sha256 == fw.syx_sha256)
    {
        p.hook.push(Hook {
            name: name.into(),
            site: site.into(),
            offset,
            mode: mode.into(),
        });
    }
}

/// `n` bytes at `addr` of the patched MAIN OS: in the image, or in a segment of the expansion chain.
fn read_cf(out: &Applied, addr: u64, n: usize) -> Vec<u8> {
    let fw = Firmware::parse(&out.syx).unwrap();
    let s = fw.section(3).unwrap();
    let base = s.dest as u64;
    for (dest, payload, _) in read_segments(&s.data, base).unwrap() {
        if addr >= dest && addr + n as u64 <= dest + payload.len() as u64 {
            return payload[(addr - dest) as usize..][..n].to_vec();
        }
    }
    s.data[(addr - base) as usize..][..n].to_vec()
}

fn sym(out: &Applied, module: &str, name: &str) -> u64 {
    *out.plan
        .symbols
        .get(&format!("{module}@1.0.0:{name}"))
        .unwrap_or_else(|| {
            panic!(
                "no symbol {module}:{name} in {:?}",
                out.plan.symbols.keys().collect::<Vec<_>>()
            )
        })
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes(b[..4].try_into().unwrap())
}

// ---- P3: typed writes -----------------------------------------------------------------------------------------

fn write_val(offset: i64, value: &str, width: u32) -> String {
    format!(
        "[[op]]\nop = \"write\"\nanchor = \"cf.main\"\noffset = {offset}\nvalue = \"{value}\"\nwidth = {width}\n"
    )
}

#[test]
fn a_typed_write_evaluates_an_expression_with_image_reads() {
    need_image!(DT2);
    let (main, base) = stock_main(DT2);
    let off = 0x4_0000usize; // somewhere in the image: only the bytes matter
    let at = off - 0x1000;
    let stock = u32::from_be_bytes(main[at..at + 4].try_into().unwrap());
    let body = write_val(
        off as i64,
        &format!("((image.u32(cf.main + {at}) << 1) + 5) & 0xffffffff"),
        4,
    ) + &write_val(off as i64 + 4, "image.u16(cf.main + 2) | 0x8000", 2)
        + &write_val(off as i64 + 6, "-3", 1);
    let lib = library(vec![module("t/w", "", &body, None)]);
    let out = run(&lib, &["t/w"]).unwrap();
    let got = read_cf(&out, base + off as u64, 7);
    assert_eq!(be32(&got), stock.wrapping_shl(1).wrapping_add(5));
    let h = u16::from_be_bytes([main[2], main[3]]) | 0x8000;
    assert_eq!(&got[4..6], &h.to_be_bytes());
    assert_eq!(got[6], 0xfd);
}

#[test]
fn a_typed_write_to_a_value_anchor_lands_in_its_instruction() {
    need_image!(DT2);
    // `dsp.machine.value.selector.table_ref` is a SHARC literal: addr = the number the instruction loads, src = the instruction
    let body = "[[op]]\nop = \"write\"\nanchor = \"dsp.machine.value.selector.table_ref\"\noffset = 2\nvalue = \"0x1234\"\nwidth = 2\n\
                [[op]]\nop = \"write\"\nanchor = \"dsp.machine.value.selector.table_ref\"\noffset = 4\nvalue = \"0x5678\"\nwidth = 2\n";
    let lib = library(vec![module("t/v", "", body, None)]);
    let out = run(&lib, &["t/v"]).unwrap();
    let base = Firmware::parse(&image(DT2).unwrap()).unwrap();
    let reg = registry();
    let a = reg
        .ports
        .iter()
        .find(|p| p.port.syx_sha256 == base.syx_sha256)
        .unwrap()
        .anchor("dsp.machine.value.selector.table_ref")
        .unwrap()
        .clone();
    let src = a.src.unwrap() as u32;
    let sb = Stream::parse(base.section(7).unwrap().data.clone()).unwrap();
    let so = Stream::parse(
        Firmware::parse(&out.syx)
            .unwrap()
            .section(7)
            .unwrap()
            .data
            .clone(),
    )
    .unwrap();
    let (old, new) = (sb.read(src, 6).unwrap(), so.read(src, 6).unwrap());
    assert_eq!(&new[..2], &old[..2], "the opcode parcel is untouched");
    assert_eq!(&new[2..4], &0x1234u16.to_le_bytes());
    assert_eq!(&new[4..6], &0x5678u16.to_le_bytes());
    assert_ne!(&old[2..6], &new[2..6]);
}

#[test]
fn typed_writes_check_width_range_and_symbols() {
    need_image!(DT2);
    // a width of 3, a missing value/hex, both at once: refused when the manifest loads
    expect_err(
        try_module("t/a", "", &write_val(0x100, "1", 3), None),
        "width 1, 2 or 4",
    );
    expect_err(
        try_module(
            "t/a",
            "",
            "[[op]]\nop = \"write\"\nanchor = \"cf.main\"\nwidth = 1\n",
            None,
        ),
        "hex or value",
    );
    expect_err(
        try_module(
            "t/a",
            "",
            "[[op]]\nop = \"write\"\nanchor = \"cf.main\"\nhex = \"00\"\nvalue = \"1\"\nwidth = 1\n",
            None,
        ),
        "hex or value",
    );
    // overflow: 0x1ff in a byte, -129 in a byte, 2^32 in a word
    for (v, w) in [
        ("0x1ff", 1),
        ("-129", 1),
        ("0x100000000", 4),
        ("0x10000", 2),
    ] {
        let lib = library(vec![module("t/a", "", &write_val(0x100, v, w), None)]);
        expect_err(run(&lib, &["t/a"]), "does not fit");
    }
    // unknown symbol, a division by zero, an image read outside its anchor
    let lib = library(vec![module(
        "t/a",
        "",
        &write_val(0x100, "nowhere + 1", 4),
        None,
    )]);
    expect_err(run(&lib, &["t/a"]), "unknown symbol nowhere");
    let lib = library(vec![module("t/a", "", &write_val(0x100, "1 / 0", 4), None)]);
    expect_err(run(&lib, &["t/a"]), "division by zero");
    let lib = library(vec![module(
        "t/a",
        "",
        &write_val(
            0x100,
            "image.u32(dsp.master.site.q31_converter.call + 4)",
            4,
        ),
        None,
    )]);
    expect_err(run(&lib, &["t/a"]), "lies outside the anchor");
    // a write outside its anchor still needs expect_sha256
    let lib = library(vec![module(
        "t/a",
        "",
        &write_val(0x100, "1", 1).replace("cf.main", "dsp.master.site.q31_converter.call"),
        None,
    )]);
    expect_err(run(&lib, &["t/a"]), "no expect_sha256");
}

// ---- P6: multi-site writes ------------------------------------------------------------------------------------

#[test]
fn a_write_over_numbered_anchors_visits_each_with_this() {
    need_image!(DT2);
    let (main, base) = stock_main(DT2);
    let mut reg = registry();
    let addrs = [0x4004_0000u64, 0x4004_0010, 0x4004_0020];
    for (i, a) in addrs.iter().enumerate() {
        add_anchor(
            &mut reg,
            DT2,
            &format!("t.site.{}", [2, 0, 1][i]),
            "data",
            *a,
            4,
        );
    }
    add_anchor(&mut reg, DT2, "t.site.x", "data", 0x4004_0030, 4); // not numeric: not matched
    let body = "[[op]]\nop = \"write\"\nanchors = \"t.site.*\"\nvalue = \"image.u32(this) + this - t.site.0\"\nwidth = 4\n";
    let lib = library(vec![module("t/m", "", body, None)]);
    let out = run_on(DT2, &reg, &lib, &["t/m"]).unwrap();
    let t0 = addrs[1];
    for a in [addrs[0], addrs[1], addrs[2]] {
        let o = (a - base) as usize;
        let stock = u32::from_be_bytes(main[o..o + 4].try_into().unwrap());
        assert_eq!(
            be32(&read_cf(&out, a, 4)),
            stock.wrapping_add((a as i64 - t0 as i64) as u32),
            "{a:#x}"
        );
    }
    let o = (0x4004_0030 - base) as usize;
    assert_eq!(
        read_cf(&out, 0x4004_0030, 4),
        main[o..o + 4],
        "t.site.x untouched"
    );
    // no anchor matches: the module is not compatible
    let lib = library(vec![module(
        "t/m",
        "",
        &body.replace("t.site.*", "t.nothing.*"),
        None,
    )]);
    expect_err(run_on(DT2, &reg, &lib, &["t/m"]), "anchors t.nothing.*");
}

// ---- P2: registries and entries --------------------------------------------------------------------------------

const FW: &str = r#"
[[object]]
path = "x.json"
place = "cf.expansion"

[[registry]]
name = "toys"
first = "image.u8(cf.main + 8) + 100"
max = 3
region = "cf.expansion"
[[registry.field]]
name = "kind"
kind = "u8"
default = 1
[[registry.field]]
name = "w"
kind = "u16"
default = 0x1234
[[registry.field]]
name = "label"
kind = "cstr"
max_len = 6
[[registry.field]]
name = "fn"
kind = "sym"
[[registry.field]]
name = "bound"
kind = "u32"
default = "image.u16(cf.main + 2) * 2"
"#;

fn entry(id: &str, extra: &str) -> String {
    format!("[[entry]]\nregistry = \"toys\"\nid = \"{id}\"\n{extra}\n")
}

fn toys_lib() -> Library {
    library(vec![
        module("t/fw", "", FW, Some(cf_obj())),
        module(
            "t/a",
            "requires = [\"t/fw\"]",
            &(entry("alpha", "label = \"ALPHA\"\nfn = \"_f\"\nkind = 7")
                + &entry(
                    "beta",
                    "label = \"B\"\nfn = \"toys.alpha.index + 1000\"\nw = 65535",
                )),
            None,
        ),
        module(
            "t/b",
            "requires = [\"t/fw\"]",
            &entry("gamma", "label = \"\"\nfn = \"_g\"\nbound = 0xdeadbeef"),
            None,
        ),
    ])
}

#[test]
fn registry_rows_symbols_indices_and_strings() {
    need_image!(DT2);
    let (main, _) = stock_main(DT2);
    let first = main[8] as u64 + 100;
    let out = run(&toys_lib(), &["t/b", "t/a", "t/fw"]).unwrap();
    let rows = sym(&out, "t/fw", "toys.rows");
    assert_eq!(sym(&out, "t/fw", "toys.first"), first);
    assert_eq!(sym(&out, "t/fw", "toys.count"), 3);
    // u8 @0, u16 @2, cstr ptr @4, sym @8, u32 @12 -> 16 byte rows
    assert_eq!(sym(&out, "t/fw", "toys.row_size"), 16);
    // canonical order: t/a before t/b, entries in manifest order
    assert_eq!(sym(&out, "t/a", "toys.alpha.index"), first);
    assert_eq!(sym(&out, "t/a", "toys.beta.index"), first + 1);
    assert_eq!(sym(&out, "t/b", "toys.gamma.index"), first + 2);
    let t = read_cf(&out, rows, 48 + 9);
    let f = sym(&out, "t/fw", "_f");
    let g = sym(&out, "t/fw", "_g");
    // alpha
    assert_eq!(t[0], 7);
    assert_eq!(&t[2..4], &0x1234u16.to_be_bytes());
    assert_eq!(be32(&t[8..]) as u64, f);
    let mut bound = ((u16::from_be_bytes([main[2], main[3]]) as u32) * 2).to_be_bytes();
    assert_eq!(&t[12..16], &bound);
    // beta: default kind, w = 65535, a symbol expression
    assert_eq!(t[16], 1);
    assert_eq!(&t[18..20], &[0xff, 0xff]);
    assert_eq!(be32(&t[24..]) as u64, first + 1000);
    // gamma
    assert_eq!(be32(&t[40..]) as u64, g);
    bound = 0xdeadbeefu32.to_be_bytes();
    assert_eq!(&t[44..48], &bound);
    // the strings follow the rows, NUL terminated, pointed to by the label field
    let p0 = be32(&t[4..]) as u64;
    let p1 = be32(&t[20..]) as u64;
    let p2 = be32(&t[36..]) as u64;
    assert_eq!(p0, rows + 48);
    assert_eq!(read_cf(&out, p0, 6), b"ALPHA\0");
    assert_eq!(read_cf(&out, p1, 2), b"B\0");
    assert_eq!(read_cf(&out, p2, 1), b"\0");
    // plan.json carries the registry
    let j = serde_json::to_value(&out.plan).unwrap();
    let r = &j["registries"][0];
    assert_eq!(r["name"], "toys");
    assert_eq!(r["count"], 3);
    assert_eq!(r["entries"][2]["id"], "gamma");
    assert_eq!(r["entries"][2]["index"], first + 2);
    assert_eq!(r["fields"][2], json!(["label", "cstr", 4]));
    // the owner-qualified symbols are in the plan as well
    assert_eq!(sym(&out, "t/a", "toys.alpha.index"), first);
}

#[test]
fn a_terminator_registry_has_a_zero_row_behind_the_entries() {
    need_image!(DT2);
    let fw = FW.replace("max = 3\n", "max = 3\nterminator = true\n");
    let lib = library(vec![
        module("t/fw", "", &fw, Some(cf_obj())),
        module(
            "t/a",
            "requires = [\"t/fw\"]",
            &entry("alpha", "label = \"AB\"\nfn = \"_f\"\nkind = 7"),
            None,
        ),
    ]);
    let out = run(&lib, &["t/a", "t/fw"]).unwrap();
    let rows = sym(&out, "t/fw", "toys.rows");
    assert_eq!(sym(&out, "t/fw", "toys.count"), 1);
    let t = read_cf(&out, rows, 32 + 3);
    assert_eq!(t[0], 7);
    assert!(
        t[16..32].iter().all(|b| *b == 0),
        "the terminator row is zero"
    );
    // the strings follow the terminator row
    assert_eq!(be32(&t[4..]) as u64, rows + 32);
    assert_eq!(&t[32..35], b"AB\0");
}

#[test]
fn the_selection_order_does_not_change_the_bytes() {
    need_image!(DT2);
    let lib = toys_lib();
    let x = run(&lib, &["t/b", "t/a", "t/fw"]).unwrap();
    let y = run(&lib, &["t/fw", "t/a", "t/b"]).unwrap();
    assert!(x.syx == y.syx);
    assert_eq!(x.plan.modules, y.plan.modules);
    // without t/a the remaining entry takes the first index
    let z = run(&lib, &["t/b", "t/fw"]).unwrap();
    let (main, _) = stock_main(DT2);
    assert_eq!(sym(&z, "t/b", "toys.gamma.index"), main[8] as u64 + 100);
}

#[test]
fn entries_are_validated() {
    need_image!(DT2);
    let one = |extra: &str| -> patchkit::Result<Applied> {
        let lib = library(vec![
            module("t/fw", "", FW, Some(cf_obj())),
            module("t/a", "requires = [\"t/fw\"]", &entry("alpha", extra), None),
        ]);
        run(&lib, &["t/a", "t/fw"])
    };
    assert!(one("label = \"ok\"\nfn = \"_f\"").is_ok());
    expect_err(one("fn = \"_f\""), "has no field label");
    expect_err(one("label = \"x\""), "has no field fn");
    expect_err(
        one("label = \"x\"\nfn = \"_f\"\nnope = 1"),
        "has no field nope",
    );
    expect_err(
        one("label = \"toolongstring\"\nfn = \"_f\""),
        "max_len is 6",
    );
    expect_err(
        one("label = \"x\"\nfn = \"_f\"\nkind = 256"),
        "does not fit",
    );
    expect_err(
        one("label = \"x\"\nfn = \"_f\"\nkind = -129"),
        "does not fit",
    );
    expect_err(
        one("label = \"x\"\nfn = \"_missing\""),
        "unknown symbol _missing",
    );
    expect_err(one("label = 5\nfn = \"_f\""), "expected a string");
    expect_err(one("label = \"x\"\nfn = true"), "expected an integer");
    // duplicate id across modules: the automatic claim `toys.alpha`
    let lib = library(vec![
        module("t/fw", "", FW, Some(cf_obj())),
        module(
            "t/a",
            "requires = [\"t/fw\"]",
            &entry("alpha", "label = \"x\"\nfn = 1"),
            None,
        ),
        module(
            "t/b",
            "requires = [\"t/fw\"]",
            &entry("alpha", "label = \"y\"\nfn = 2"),
            None,
        ),
    ]);
    expect_err(run(&lib, &["t/a", "t/b", "t/fw"]), "both claim toys.alpha");
    // max is 3
    let many: String = (0..4)
        .map(|i| entry(&format!("e{i}"), "label = \"x\"\nfn = 1"))
        .collect();
    let lib = library(vec![
        module("t/fw", "", FW, Some(cf_obj())),
        module("t/a", "requires = [\"t/fw\"]", &many, None),
    ]);
    expect_err(run(&lib, &["t/a", "t/fw"]), "registry toys is full");
    // an entry whose framework is not selected
    expect_err(run(&lib_with_orphan(), &["t/o"]), "no registry toys");
}

fn lib_with_orphan() -> Library {
    library(vec![module(
        "t/o",
        "",
        &entry("x", "label = \"x\"\nfn = 1"),
        None,
    )])
}

#[test]
fn registry_declarations_are_validated() {
    let bad = |fw: &str, needle: &str| {
        expect_err(try_module("t/fw", "", fw, None), needle);
    };
    bad(
        "[[registry]]\nname = \"Toys\"\nfirst = 0\nmax = 1\nregion = \"cf.expansion\"\n",
        "not [a-z]",
    );
    bad(
        "[[registry]]\nname = \"toys\"\nfirst = 0\nmax = 0\nregion = \"cf.expansion\"\n",
        "max is 0",
    );
    bad(
        "[[registry]]\nname = \"toys\"\nfirst = \"1 +\"\nmax = 1\nregion = \"cf.expansion\"\n",
        "expected a number",
    );
    let f = "[[registry]]\nname = \"toys\"\nfirst = 0\nmax = 1\nregion = \"cf.expansion\"\n[[registry.field]]\n";
    bad(
        &format!("{f}name = \"a\"\nkind = \"u64\"\n"),
        "kind \"u64\"",
    );
    bad(&format!("{f}name = \"id\"\nkind = \"u8\"\n"), "reserved");
    bad(
        &format!("{f}name = \"a\"\nkind = \"u8\"\nmax_len = 4\n"),
        "max_len is for cstr",
    );
    bad(
        &format!("{f}name = \"a\"\nkind = \"u8\"\ndefault = 1.5\n"),
        "expected an integer",
    );
    // a registry declared twice
    let lib = library(vec![
        module("t/f1", "", FW, Some(cf_obj())),
        module(
            "t/f2",
            "",
            &FW.replace(
                "[[object]]\npath = \"x.json\"\nplace = \"cf.expansion\"\n",
                "",
            ),
            None,
        ),
    ]);
    expect_err(
        run(&lib, &["t/f1", "t/f2"]),
        "registry toys is declared twice",
    );
}

// ---- P1: sections ----------------------------------------------------------------------------------------------

const SEC_FW: &str = r#"
[[registry]]
name = "toys"
first = 5
max = 4
region = "cf.expansion"
[[registry.field]]
name = "kind"
kind = "u8"
default = 1
[[registry.field]]
name = "w"
kind = "u16"
default = 2

[[section]]
name = "tbl"
region = "cf.expansion"
from = { anchor = "t.tbl", offset = 4, len = "image.u8(t.tbl) & 0x1f" }
append = { registry = "toys", row = ["$kind", "u16:$w", "u8:$index", "toys.first", "tbl"] }
"#;

#[test]
fn a_copied_section_gets_rows_appended_and_a_symbol() {
    need_image!(DT2);
    let (main, base) = stock_main(DT2);
    let mut reg = registry();
    let at = 0x4000_1000u64;
    add_anchor(&mut reg, DT2, "t.tbl", "data", at, 64);
    let lib = library(vec![
        module("t/fw", "", SEC_FW, None),
        module(
            "t/a",
            "requires = [\"t/fw\"]",
            &(entry("one", "kind = 3") + &entry("two", "w = 0x0102")),
            None,
        ),
    ]);
    let out = run_on(DT2, &reg, &lib, &["t/a", "t/fw"]).unwrap();
    let o = (at - base) as usize;
    let len = (main[o] & 0x1f) as usize;
    let sec = sym(&out, "t/fw", "tbl");
    assert_eq!(sym(&out, "t/fw", "tbl.size"), (len + 2 * 15) as u64);
    let b = read_cf(&out, sec, len + 30);
    assert_eq!(
        &b[..len],
        &main[o + 4..o + 4 + len],
        "copied from the image"
    );
    // rows: u32 kind, u16 w, u8 index, u32 first, u32 symbol = 15 bytes
    let r0 = &b[len..len + 15];
    assert_eq!(be32(r0), 3);
    assert_eq!(&r0[4..6], &2u16.to_be_bytes());
    assert_eq!(r0[6], 5);
    assert_eq!(be32(&r0[7..]), 5);
    assert_eq!(be32(&r0[11..]) as u64, sec);
    let r1 = &b[len + 15..];
    assert_eq!(be32(r1), 1);
    assert_eq!(&r1[4..6], &0x0102u16.to_be_bytes());
    assert_eq!(r1[6], 6);
    let j = serde_json::to_value(&out.plan).unwrap();
    assert_eq!(j["sections"][0]["name"], "tbl");
    assert_eq!(j["sections"][0]["copied"], len);
    assert_eq!(j["sections"][0]["appended_rows"], 2);
    // no firmware is stored in the module: only the anchor name and numbers
    let fw_text = SEC_FW.to_string();
    assert!(!fw_text.contains(&hex_of(&main[o..o + 8])));
}

fn hex_of(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[test]
fn a_section_copy_past_its_anchor_needs_the_hash() {
    need_image!(DT2);
    let mut reg = registry();
    add_anchor(&mut reg, DT2, "t.tbl", "data", 0x4000_1000, 16);
    let body = |extra: &str| {
        format!(
            "[[section]]\nname = \"c\"\nregion = \"cf.expansion\"\nfrom = {{ anchor = \"t.tbl\", offset = 8, len = 16{extra} }}\n"
        )
    };
    let lib = library(vec![module("t/s", "", &body(""), None)]);
    expect_err(run_on(DT2, &reg, &lib, &["t/s"]), "has no expect_sha256");
    let lib = library(vec![module(
        "t/s",
        "",
        &body(&format!(", expect_sha256 = \"{}\"", "00".repeat(32))),
        None,
    )]);
    expect_err(
        run_on(DT2, &reg, &lib, &["t/s"]),
        "do not hash to expect_sha256",
    );
    // default length: the rest of the anchor
    let lib = library(vec![module(
        "t/s",
        "",
        "[[section]]\nname = \"c\"\nregion = \"cf.expansion\"\nfrom = { anchor = \"t.tbl\", offset = 4 }\n",
        None,
    )]);
    let out = run_on(DT2, &reg, &lib, &["t/s"]).unwrap();
    assert_eq!(sym(&out, "t/s", "c.size"), 12);
    // a section for a region the port lacks
    let lib = library(vec![module(
        "t/s",
        "",
        "[[section]]\nname = \"c\"\nregion = \"cf.nowhere\"\nfrom = { anchor = \"t.tbl\" }\n",
        None,
    )]);
    expect_err(run_on(DT2, &reg, &lib, &["t/s"]), "region cf.nowhere");
}

#[test]
fn a_sharc_section_is_copied_into_the_dsp_stream_little_endian() {
    need_image!(DT2);
    let body = r#"
[[registry]]
name = "toys"
first = 3
max = 2
region = "dsp.l2"
[[registry.field]]
name = "v"
kind = "u16"
[[section]]
name = "dsptbl"
region = "dsp.l2"
from = { anchor = "dsp.master.fn.q31_converter" }
append = { registry = "toys", row = ["u16:$v", "$index"] }
"#;
    let lib = library(vec![
        module("t/fw", "", body, None),
        module(
            "t/a",
            "requires = [\"t/fw\"]",
            &entry("x", "v = 0x1234"),
            None,
        ),
    ]);
    let out = run(&lib, &["t/a", "t/fw"]).unwrap();
    let fw = Firmware::parse(&out.syx).unwrap();
    let st = Stream::parse(fw.section(7).unwrap().data.clone()).unwrap();
    let base = Firmware::parse(&image(DT2).unwrap()).unwrap();
    let sb = Stream::parse(base.section(7).unwrap().data.clone()).unwrap();
    let conv = registry()
        .ports
        .iter()
        .find(|p| p.port.syx_sha256 == base.syx_sha256)
        .unwrap()
        .anchor("dsp.master.fn.q31_converter")
        .unwrap()
        .clone();
    let sec = sym(&out, "t/fw", "dsptbl") as u32;
    let b = st.read(sec, 6 + 6).unwrap();
    assert_eq!(&b[..6], &sb.read(conv.addr as u32, 6).unwrap()[..]);
    assert_eq!(&b[6..8], &0x1234u16.to_le_bytes());
    assert_eq!(&b[8..12], &3u32.to_le_bytes());
    // the registry table is data in the DSP too: its address symbol is a byte address
    let rows = sym(&out, "t/fw", "toys.rows") as u32;
    assert_eq!(&st.read(rows, 2).unwrap()[..], &0x1234u16.to_le_bytes());
}

// ---- P4: hooks -------------------------------------------------------------------------------------------------

fn hook_body(hook: &str, extra: &str) -> String {
    format!(
        "[[object]]\npath = \"x.json\"\nplace = \"cf.expansion\"\n[[op]]\nop = \"hook\"\nhook = \"{hook}\"\nto = \"_f\"\n{extra}\n"
    )
}

/// A code anchor at a function of DT2 1.17 whose first instructions are: `jsr (d16,pc)` (4) ; `clr.l abs.l` (6).
const JSR_PC_FN: u64 = 0x4000_4dac;
/// ... `bra.b` (2) ; `link.w` (4)
const BRA_FN: u64 = 0x400e_ea78;
/// ... `link.w a6,#-0x54` (4) ; `movem.l` (4)
const PLAIN_FN: u64 = 0x4005_fb0c;

fn hook_reg() -> Registry {
    let mut reg = registry();
    add_anchor(&mut reg, DT2, "t.jsrpc", "code", JSR_PC_FN, 16);
    add_anchor(&mut reg, DT2, "t.bra", "code", BRA_FN, 16);
    add_anchor(&mut reg, DT2, "t.plain", "code", PLAIN_FN, 16);
    add_hook(&mut reg, DT2, "t.jsrpc.hook", "t.jsrpc", 0, "before");
    add_hook(&mut reg, DT2, "t.bra.hook", "t.bra", 0, "before");
    add_hook(&mut reg, DT2, "t.plain.hook", "t.plain", 0, "before");
    add_hook(&mut reg, DT2, "t.plain.drop", "t.plain", 0, "replace");
    add_hook(&mut reg, DT2, "t.plain.at2", "t.plain", 2, "before");
    add_hook(&mut reg, DT2, "t.plain.at4", "t.plain", 4, "before");
    reg
}

#[test]
fn a_coldfire_hook_jumps_and_the_tail_runs_the_displaced_instructions() {
    need_image!(DT2);
    let (main, base) = stock_main(DT2);
    let lib = library(vec![module(
        "t/h",
        "",
        &hook_body("t.plain.hook", ""),
        Some(cf_obj()),
    )]);
    let out = run_on(DT2, &hook_reg(), &lib, &["t/h"]).unwrap();
    let f = sym(&out, "t/h", "_f");
    let tail = sym(&out, "t/h", "hook.t.plain.tail");
    let ret = sym(&out, "t/h", "hook.t.plain.return");
    // link.w (4) + movem.l (4) cover the six bytes: site 8 bytes, jmp.l + nop
    assert_eq!(ret, PLAIN_FN + 8);
    let site = read_cf(&out, PLAIN_FN, 8);
    assert_eq!(&site[..2], &[0x4e, 0xf9]);
    assert_eq!(be32(&site[2..]) as u64, f);
    assert_eq!(&site[6..], &[0x4e, 0x71]);
    let o = (PLAIN_FN - base) as usize;
    let t = read_cf(&out, tail, 14);
    assert_eq!(
        &t[..8],
        &main[o..o + 8],
        "displaced instructions are copied"
    );
    assert_eq!(&t[8..10], &[0x4e, 0xf9]);
    assert_eq!(be32(&t[10..]) as u64, ret);
    // plan.json
    let j = serde_json::to_value(&out.plan).unwrap();
    let h = &j["hooks"][0];
    assert_eq!(h["anchor"], "t.plain");
    assert_eq!(h["hook"], "t.plain.hook");
    assert_eq!(h["displaced"], "run");
    assert_eq!(h["site_len"], 8);
    assert_eq!(h["tail"], tail);
    assert_eq!(h["ret"], ret);
    // the patch is a normal in-place write, hashed against the old bytes
    let w = out
        .plan
        .writes
        .iter()
        .find(|w| w.what == "hook t.plain+0")
        .unwrap();
    assert_eq!((w.kind.as_str(), w.addr, w.len), ("patch", PLAIN_FN, 8));
}

#[test]
fn a_replace_hook_leaves_no_tail() {
    need_image!(DT2);
    let lib = library(vec![module(
        "t/h",
        "",
        &hook_body("t.plain.drop", ""),
        Some(cf_obj()),
    )]);
    let out = run_on(DT2, &hook_reg(), &lib, &["t/h"]).unwrap();
    assert!(out.plan.symbols.keys().all(|k| !k.contains(".tail")));
    assert!(
        out.plan
            .symbols
            .keys()
            .any(|k| k.ends_with("hook.t.plain.return"))
    );
}

#[test]
fn a_hook_op_takes_everything_but_the_target_from_its_declaration() {
    need_image!(DT2);
    expect_err(
        try_module(
            "t/h",
            "",
            &hook_body("t.plain.hook", "anchor = \"t.plain\""),
            Some(cf_obj()),
        ),
        "anchor comes from the declaration",
    );
    expect_err(
        try_module(
            "t/h",
            "",
            &hook_body("t.plain.hook", "displaced = \"drop\""),
            Some(cf_obj()),
        ),
        "displaced comes from the declaration",
    );
    expect_err(
        try_module(
            "t/h",
            "",
            "[[op]]\nop = \"retarget\"\nanchor = \"t.plain\"\nto = \"_f\"\n",
            None,
        ),
        "op retarget is gone",
    );
    // a hook the port does not declare
    let lib = library(vec![module(
        "t/h",
        "",
        &hook_body("t.nope", ""),
        Some(cf_obj()),
    )]);
    expect_err(run_on(DT2, &hook_reg(), &lib, &["t/h"]), "no hook t.nope");
}

#[test]
fn a_hook_on_a_non_boundary_or_with_a_bad_target_is_refused() {
    need_image!(DT2);
    // offset 2 is inside the 4-byte `link.w`
    let lib = library(vec![module(
        "t/h",
        "",
        &hook_body("t.plain.at2", ""),
        Some(cf_obj()),
    )]);
    expect_err(
        run_on(DT2, &hook_reg(), &lib, &["t/h"]),
        "not on an instruction boundary",
    );
    // offset 4 is a boundary (the movem.l): the site reaches 4 + 4 + ... bytes, still inside the 16 byte anchor
    let lib = library(vec![module(
        "t/h",
        "",
        &hook_body("t.plain.at4", ""),
        Some(cf_obj()),
    )]);
    assert!(run_on(DT2, &hook_reg(), &lib, &["t/h"]).is_ok());
    // unknown target symbol
    let lib = library(vec![module(
        "t/h",
        "",
        &hook_body("t.plain.hook", "").replace("to = \"_f\"", "to = \"_nope\""),
        Some(cf_obj()),
    )]);
    expect_err(
        run_on(DT2, &hook_reg(), &lib, &["t/h"]),
        "unknown symbol _nope",
    );
    // a site that reaches past the anchor needs the hash: a 6-byte anchor, 8-byte site
    let mut reg = registry();
    add_anchor(&mut reg, DT2, "t.short", "code", PLAIN_FN, 6);
    add_hook(&mut reg, DT2, "t.short.hook", "t.short", 0, "before");
    let lib = library(vec![module(
        "t/h",
        "",
        &hook_body("t.short.hook", ""),
        Some(cf_obj()),
    )]);
    expect_err(run_on(DT2, &reg, &lib, &["t/h"]), "has no expect_sha256");
    // a data anchor is not a code site
    let mut reg = registry();
    add_anchor(&mut reg, DT2, "t.data", "data", PLAIN_FN, 16);
    add_hook(&mut reg, DT2, "t.data.hook", "t.data", 0, "before");
    let lib = library(vec![module(
        "t/h",
        "",
        &hook_body("t.data.hook", ""),
        Some(cf_obj()),
    )]);
    expect_err(run_on(DT2, &reg, &lib, &["t/h"]), "needs a code anchor");
    // two hooks at the same place overlap
    let lib = library(vec![
        module("t/h", "", &hook_body("t.plain.hook", ""), Some(cf_obj())),
        module(
            "t/i",
            "",
            &hook_body("t.plain.hook", "").replace("_f", "_h"),
            Some(
                cf_obj()
                    .to_string()
                    .replace("_f", "_h")
                    .replace("_g", "_i")
                    .parse()
                    .unwrap(),
            ),
        ),
    ]);
    expect_err(
        run_on(DT2, &hook_reg(), &lib, &["t/h", "t/i"]),
        "defined twice",
    );
    // hooks at overlapping sites of one anchor
    let lib = library(vec![
        module("t/h", "", &hook_body("t.plain.hook", ""), Some(cf_obj())),
        module(
            "t/i",
            "",
            &hook_body("t.plain.at4", "").replace("_f", "_h"),
            Some(
                cf_obj()
                    .to_string()
                    .replace("_f", "_h")
                    .replace("_g", "_i")
                    .parse()
                    .unwrap(),
            ),
        ),
    ]);
    expect_err(
        run_on(DT2, &hook_reg(), &lib, &["t/h", "t/i"]),
        "overlapping writes",
    );
}

#[test]
fn displaced_pc_relative_instructions_are_re_encoded() {
    need_image!(DT2);
    let (main, base) = stock_main(DT2);
    // jsr (d16,pc) -> jsr abs.l with the same target; the clr.l abs.l is copied
    let lib = library(vec![module(
        "t/h",
        "",
        &hook_body("t.jsrpc.hook", ""),
        Some(cf_obj()),
    )]);
    let out = run_on(DT2, &hook_reg(), &lib, &["t/h"]).unwrap();
    let tail = sym(&out, "t/h", "hook.t.jsrpc.tail");
    let o = (JSR_PC_FN - base) as usize;
    assert_eq!(&main[o..o + 2], &[0x4e, 0xba]);
    let d16 = i16::from_be_bytes([main[o + 2], main[o + 3]]) as i64;
    let target = (JSR_PC_FN as i64 + 2 + d16) as u32;
    let t = read_cf(&out, tail, 6 + 6 + 6);
    assert_eq!(&t[..2], &[0x4e, 0xb9]);
    assert_eq!(be32(&t[2..]), target);
    assert_eq!(&t[6..12], &main[o + 4..o + 10], "clr.l abs.l unchanged");
    assert_eq!(&t[12..14], &[0x4e, 0xf9]);
    // bra.b -> bra.l with the same target
    let lib = library(vec![module(
        "t/h",
        "",
        &hook_body("t.bra.hook", ""),
        Some(cf_obj()),
    )]);
    let out = run_on(DT2, &hook_reg(), &lib, &["t/h"]).unwrap();
    let tail = sym(&out, "t/h", "hook.t.bra.tail");
    let o = (BRA_FN - base) as usize;
    let tgt = BRA_FN as i64 + 2 + main[o + 1] as i8 as i64;
    let t = read_cf(&out, tail, 6);
    assert_eq!(&t[..2], &[0x60, 0xff]);
    assert_eq!(
        i32::from_be_bytes(t[2..6].try_into().unwrap()) as i64,
        tgt - (tail as i64 + 2)
    );
}

#[test]
fn a_sharc_hook_replaces_the_instruction_by_a_jump_and_the_tail_executes_it() {
    need_image!(DT2);
    let dsp_obj = json!({
        "format": 1, "arch": "sharc", "source_sha256": "0".repeat(64),
        "sections": [{"name": "seg_pmco", "kind": "text", "align": 8, "size": 6, "hex": "000000000000"}],
        "symbols": [{"name": "_f", "section": "seg_pmco", "value": 0, "global": true}],
        "relocs": []
    });
    let body = "[[object]]\npath = \"x.json\"\nplace = \"dsp.l2\"\n[[op]]\nop = \"hook\"\nhook = \"t.q31.hook\"\nto = \"_f\"\n";
    let lib = library(vec![module("t/h", "", body, Some(dsp_obj))]);
    let mut reg = registry();
    add_hook(
        &mut reg,
        DT2,
        "t.q31.hook",
        "dsp.master.site.q31_converter.call",
        0,
        "before",
    );
    let out = run_on(DT2, &reg, &lib, &["t/h"]).unwrap();
    let base = Firmware::parse(&image(DT2).unwrap()).unwrap();
    let a = reg
        .ports
        .iter()
        .find(|p| p.port.syx_sha256 == base.syx_sha256)
        .unwrap()
        .anchor("dsp.master.site.q31_converter.call")
        .unwrap()
        .clone();
    let sb = Stream::parse(base.section(7).unwrap().data.clone()).unwrap();
    let so = Stream::parse(
        Firmware::parse(&out.syx)
            .unwrap()
            .section(7)
            .unwrap()
            .data
            .clone(),
    )
    .unwrap();
    let f = sym(&out, "t/h", "_f");
    let site = so.read(a.addr as u32, 6).unwrap();
    assert_eq!(&site[..2], &0x063eu16.to_le_bytes());
    assert_eq!(site[2] as u64, (f >> 16) & 0xff);
    assert_eq!(&site[4..6], &((f & 0xffff) as u16).to_le_bytes());
    let tail_pc = sym(&out, "t/h", "hook.dsp.master.site.q31_converter.call.tail");
    let ret = sym(
        &out,
        "t/h",
        "hook.dsp.master.site.q31_converter.call.return",
    );
    assert_eq!(ret, a.pm.unwrap() + 3);
    let tail_byte = 0x2000_0000 + (tail_pc as u32 - 0xB8_0000) * 2;
    let t = so.read(tail_byte, 12).unwrap();
    assert_eq!(
        &t[..6],
        &sb.read(a.addr as u32, 6).unwrap()[..],
        "the displaced instruction"
    );
    assert_eq!(&t[6..8], &0x063eu16.to_le_bytes());
    assert_eq!(t[8] as u64, (ret >> 16) & 0xff);
    assert_eq!(&t[10..12], &((ret & 0xffff) as u16).to_le_bytes());
}

// ---- P5: device filters ----------------------------------------------------------------------------------------

#[test]
fn device_filters_apply_to_every_table() {
    need_image!(DT2);
    need_image!(DN2);
    let fw = r#"
[[registry]]
name = "toys"
first = 10
max = 4
region = "cf.expansion"
devices = ["dt2"]
[[registry.field]]
name = "v"
kind = "u8"
[[registry]]
name = "toys"
first = 20
max = 4
region = "cf.expansion"
devices = ["dn2"]
[[registry.field]]
name = "v"
kind = "u8"
[[section]]
name = "sec_dn2"
region = "cf.expansion"
devices = ["dn2"]
from = { anchor = "cf.main", len = 16 }
[[section]]
name = "sec_dt2"
region = "cf.expansion"
devices = ["dt2"]
from = { anchor = "cf.main", len = 8 }
[[op]]
op = "write"
anchor = "cf.main"
offset = 0x100
value = 1
width = 1
devices = ["dn2"]
[[claim]]
resource = "only.dt2"
devices = ["dt2"]
"#;
    let ents = "[[entry]]\nregistry = \"toys\"\nid = \"a\"\nv = 1\ndevices = [\"dt2\"]\n[[entry]]\nregistry = \"toys\"\nid = \"b\"\nv = 2\ndevices = [\"dn2\"]\n[[entry]]\nregistry = \"toys\"\nid = \"c\"\nv = 3\n";
    let lib = library(vec![
        module("t/fw", "", fw, None),
        module("t/e", "requires = [\"t/fw\"]", ents, None),
    ]);
    let reg = registry();
    let a = run_on(DT2, &reg, &lib, &["t/e", "t/fw"]).unwrap();
    assert_eq!(sym(&a, "t/fw", "toys.first"), 10);
    assert_eq!(sym(&a, "t/fw", "toys.count"), 2);
    assert_eq!(sym(&a, "t/e", "toys.a.index"), 10);
    assert_eq!(sym(&a, "t/e", "toys.c.index"), 11);
    assert!(a.plan.symbols.keys().any(|k| k.ends_with(":sec_dt2")));
    assert!(a.plan.symbols.keys().all(|k| !k.ends_with(":sec_dn2")));
    assert!(a.plan.writes.iter().all(|w| w.what != "write cf.main+256"));
    let b = run_on(DN2, &reg, &lib, &["t/e", "t/fw"]).unwrap();
    assert_eq!(sym(&b, "t/fw", "toys.first"), 20);
    assert_eq!(sym(&b, "t/e", "toys.b.index"), 20);
    assert_eq!(sym(&b, "t/e", "toys.c.index"), 21);
    assert!(b.plan.symbols.keys().any(|k| k.ends_with(":sec_dn2")));
    assert!(b.plan.writes.iter().any(|w| w.what == "write cf.main+256"));
    // an unknown device name is refused when the manifest loads
    expect_err(
        try_module(
            "t/x",
            "",
            "[[entry]]\nregistry = \"toys\"\nid = \"a\"\ndevices = [\"dt3\"]\n",
            None,
        ),
        "is not one of dt2|dn2",
    );
    // a claim for the other device does not collide
    let dup = "[[claim]]\nresource = \"only.dt2\"\ndevices = [\"dn2\"]\n";
    let lib = library(vec![
        module("t/fw", "", fw, None),
        module("t/z", "", dup, None),
    ]);
    assert!(run_on(DT2, &reg, &lib, &["t/fw", "t/z"]).is_ok());
    assert!(run_on(DN2, &reg, &lib, &["t/fw", "t/z"]).is_ok());
}

// ---- explain ---------------------------------------------------------------------------------------------------

#[test]
fn explain_lists_registries_sections_and_hooks() {
    need_image!(DT2);
    let mut reg = hook_reg();
    add_anchor(&mut reg, DT2, "t.tbl", "data", 0x4000_1000, 64);
    let lib = library(vec![
        module("t/fw", "", SEC_FW, None),
        module(
            "t/a",
            "requires = [\"t/fw\"]",
            &entry("one", "kind = 3"),
            None,
        ),
        module("t/h", "", &hook_body("t.jsrpc.hook", ""), Some(cf_obj())),
    ]);
    let raw = image(DT2).unwrap();
    let ids: Vec<String> = ["t/a", "t/fw", "t/h"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let out = apply::apply_with(&raw, &reg, &lib, &ids, Options::default()).unwrap();
    let text = patchkit::explain::explain(&raw, &out).unwrap();
    for needle in [
        "registry toys",
        "1 of at most 4 entries",
        "one  [t/a@1.0.0]",
        "copied sections:",
        "tbl  [t/fw@1.0.0]",
        "copied from t.tbl",
        "1 appended row(s)",
        "hooks:",
        "[t/h@1.0.0] t.jsrpc.hook = t.jsrpc +0 (cf)",
        "displaced run",
        "jsr",
        "hook tail hook.t.jsrpc",
        "section tbl",
    ] {
        assert!(text.contains(needle), "explain lacks {needle:?}:\n{text}");
    }
    // the hook site before and after
    assert!(
        text.contains("before") && text.contains("after") && text.contains("jmp"),
        "{text}"
    );
}

#[test]
fn old_plans_do_not_grow_new_keys() {
    need_image!(DT2);
    need_toolchain!();
    let out = run(&library(vec![]), &["test/cf-demo"]).unwrap();
    let j = serde_json::to_value(&out.plan).unwrap();
    for k in ["registries", "sections", "hooks"] {
        assert!(j.get(k).is_none(), "{k}");
    }
}

// ---- permanent entry numbers (registry index_table) -----------------------------------------------------------------

fn nfw(table: &str) -> Module {
    try_nfw(table).unwrap()
}

fn try_nfw(table: &str) -> patchkit::Result<Module> {
    let text = format!(
        "[module]\nid = \"n/fw\"\nversion = \"1.0.0\"\nname = \"n\"\ncategory = \"test\"\ntype = \"raw\"\nauthor = \"digikit\"\nstatus = \"experimental\"\nsummary = \"s\"\n{}",
        r#"
[[registry]]
name = "nums"
first = "image.u8(cf.main + 8) + 100"
max = 6
region = "cf.expansion"
index_table = "nums.toml"
[registry.gap]
tag = 0xff
[[registry.field]]
name = "tag"
kind = "u8"
[[registry.field]]
name = "label"
kind = "cstr"
default = ""
"#
    );
    let table = table.to_string();
    let read = move |_p: &str| -> patchkit::Result<String> { Ok(table.clone()) };
    Module::from_parts(&text, &read)
}

fn nentry(id: &str, tag: u32) -> String {
    format!("[[entry]]\nregistry = \"nums\"\nid = \"{id}\"\ntag = {tag}\n")
}

#[test]
fn entries_take_their_permanent_numbers_and_the_gaps_are_filled() {
    need_image!(DT2);
    let (main, _) = stock_main(DT2);
    let first = main[8] as i64 + 100;
    let table = format!(
        "[dt2]\nlast = {}\n[dt2.alloc]\n\"n/a:one\" = {}\n\"n/a:two\" = {}\n\"n/b:three\" = {}\n",
        first + 4,
        first + 3,
        first + 1,
        first + 4
    );
    let lib = library(vec![
        nfw(&table),
        module(
            "n/a",
            "requires = [\"n/fw\"]",
            &(nentry("one", 1) + &nentry("two", 2)),
            None,
        ),
        module("n/b", "requires = [\"n/fw\"]", &nentry("three", 3), None),
    ]);
    // all three: numbers are the table's, not the selection order's
    let out = run(&lib, &["n/b", "n/a", "n/fw"]).unwrap();
    let rows = sym(&out, "n/fw", "nums.rows");
    assert_eq!(sym(&out, "n/a", "nums.one.index") as i64, first + 3);
    assert_eq!(sym(&out, "n/a", "nums.two.index") as i64, first + 1);
    assert_eq!(sym(&out, "n/b", "nums.three.index") as i64, first + 4);
    assert_eq!(
        sym(&out, "n/fw", "nums.count"),
        5,
        "rows: first ..= highest"
    );
    assert_eq!(sym(&out, "n/fw", "nums.entries"), 3);
    let row_size = sym(&out, "n/fw", "nums.row_size") as usize;
    let t = read_cf(&out, rows, 5 * row_size);
    let tags: Vec<u8> = (0..5).map(|k| t[k * row_size]).collect();
    assert_eq!(tags, [0xff, 2, 0xff, 1, 3], "gap rows take [registry.gap]");
    let j = serde_json::to_value(&out.plan).unwrap();
    assert_eq!(j["registries"][0]["gaps"], json!([first, first + 2]));
    // a subset keeps the numbers; the table ends at the highest selected
    let out = run(&lib, &["n/fw", "n/a"]).unwrap();
    assert_eq!(sym(&out, "n/a", "nums.two.index") as i64, first + 1);
    assert_eq!(sym(&out, "n/a", "nums.one.index") as i64, first + 3);
    assert_eq!(sym(&out, "n/fw", "nums.count"), 4);
    // an entry without a number, a number below first, above last, and a table with a duplicate number
    let lib = library(vec![
        nfw(&format!(
            "[dt2]\nlast = {}\n[dt2.alloc]\n\"n/a:one\" = {}\n",
            first + 4,
            first
        )),
        module(
            "n/a",
            "requires = [\"n/fw\"]",
            &(nentry("one", 1) + &nentry("two", 2)),
            None,
        ),
    ]);
    expect_err(
        run(&lib, &["n/fw", "n/a"]),
        "n/a:two has no number in the allocation table",
    );
    let lib = library(vec![
        nfw(&format!(
            "[dt2]\nlast = {}\n[dt2.alloc]\n\"n/a:one\" = {}\n",
            first + 4,
            first - 1
        )),
        module("n/a", "requires = [\"n/fw\"]", &nentry("one", 1), None),
    ]);
    expect_err(run(&lib, &["n/fw", "n/a"]), "stock one");
    expect_err(
        try_nfw("[dt2]\nlast = 102\n[dt2.alloc]\n\"n/a:one\" = 103\n"),
        "outside 0..=",
    );
    expect_err(
        try_nfw("[dt2]\nlast = 20\n[dt2.alloc]\n\"n/a:one\" = 9\n\"n/a:two\" = 9\n"),
        "both have number 9",
    );
    // no section for the device
    let lib = library(vec![
        nfw(&format!("[dn2]\nlast = {}\n", first + 4)),
        module("n/a", "requires = [\"n/fw\"]", &nentry("one", 1), None),
    ]);
    expect_err(run(&lib, &["n/fw", "n/a"]), "no [dt2] section");
}

#[test]
fn requires_are_added_and_cycles_and_missing_modules_refused() {
    let lib = library(vec![
        module("c/a", "requires = [\"c/b\"]", "", None),
        module("c/b", "requires = [\"c/c\"]", "", None),
        module("c/c", "", "", None),
        module("c/x", "requires = [\"c/y\"]", "", None),
        module("c/y", "requires = [\"c/x\"]", "", None),
        module("c/m", "requires = [\"c/none\"]", "", None),
    ]);
    let ids = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let (all, added) = lib.with_requires(&ids(&["c/a"])).unwrap();
    assert_eq!(all, ids(&["c/a", "c/c", "c/b"]));
    assert_eq!(added, ids(&["c/c", "c/b"]));
    let (all, added) = lib.with_requires(&ids(&["c/c", "c/a"])).unwrap();
    assert_eq!((all.len(), added), (3, ids(&["c/b"])));
    expect_err(lib.with_requires(&ids(&["c/x"])), "requires cycle");
    expect_err(
        lib.with_requires(&ids(&["c/m"])),
        "c/m requires c/none, which is not in the module library",
    );
    expect_err(lib.with_requires(&ids(&["nope"])), "unknown module nope");
}

#[test]
fn a_registry_limit_caps_the_entries_at_the_device_files_number() {
    need_image!(DT2);
    // the registry takes 8, the device file's [limits] fx_slots (3 on the Digitakt II, 4 on the Digitone II) caps it
    let capped = FW
        .replace("max = 3", "max = 8\nlimit = \"fx_slots\"")
        .to_string();
    let set = |n: usize| {
        let many: String = (0..n)
            .map(|i| entry(&format!("e{i}"), "label = \"x\"\nfn = 1"))
            .collect();
        library(vec![
            module("t/fw", "", &capped, Some(cf_obj())),
            module("t/a", "requires = [\"t/fw\"]", &many, None),
        ])
    };
    assert!(run(&set(3), &["t/a", "t/fw"]).is_ok());
    expect_err(run(&set(4), &["t/a", "t/fw"]), "[limits] fx_slots is 3");
    // an unknown limit name is refused when the module loads
    let bad = FW.replace("max = 3", "max = 3\nlimit = \"nope\"");
    expect_err(
        try_module("t/fw", "", &bad, Some(cf_obj())),
        "is not a device limit",
    );
}
