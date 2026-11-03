//! `patchkit build`: the canonical JSON writer, the ELF to object conversion, the link and the ISA check, the freshness hash and the
//! build of a small module. Tests that run the m68k-elf toolchain skip when it is not installed (`pk_toolchains m68k`).

use patchkit::build::cf::{self, BASE};
use patchkit::build::elf::{self, RelocTo};
use patchkit::build::toolchain::Env;
use patchkit::build::{self, BuildOptions, Status, rust, sharc};
use patchkit::dko::Dko;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// `data/tiny.s` assembled with -mcpu=54418.
fn tiny() -> Vec<u8> {
    include_bytes!("data/tiny.o").to_vec()
}

fn toolchain() -> Option<Env> {
    let env = Env::new(&root());
    env.m68k("gcc").is_ok().then_some(env)
}

fn find_objects(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            find_objects(&p, out);
        } else if p.to_string_lossy().ends_with(".dko.json")
            && p.parent().is_some_and(|d| d.ends_with("build"))
        {
            out.push(p);
        }
    }
}

#[test]
fn committed_objects_are_in_the_canonical_form() {
    let mut files = vec![];
    find_objects(&root().join("modules/core"), &mut files);
    assert!(files.len() > 20, "{} objects", files.len());
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(
            build::pretty(&v).unwrap() == text,
            "{} is not canonical",
            f.display()
        );
    }
}

#[test]
fn local_binary_hashes_are_not_part_of_the_comparison() {
    let make = |selas: &str, rev: &str, hex: &str| serde_json::json!({"sections": [{"hex": hex}], "toolchain": {"selache_rev": rev, "selas_sha256": selas, "seldump_sha256": selas}});
    let base = build::comparable(make("s1", "r1", "00"));
    assert_eq!(base, build::comparable(make("s2", "r1", "00")));
    assert_ne!(base, build::comparable(make("s1", "r2", "00")));
    assert_ne!(base, build::comparable(make("s1", "r1", "01")));
}

/// Symbols the ColdFire libgcc defines that the compiler never calls for code a module compiles (trampolines, thread-local storage,
/// stack hardening, internal tables): `is_helper` rightly refuses them.
const LIBGCC_NOT_CALLED: [&str; 14] = [
    "___trampoline",
    "__transfer_from_trampoline",
    "__enable_execute_stack",
    "__clear_cache",
    "__clear_sticky_bit",
    "__emutls_get_address",
    "__emutls_register_common",
    "__gcc_bcmp",
    "__hardcfr_check",
    "__strub_enter",
    "__strub_leave",
    "__strub_update",
    "_fpCCR",
    "$_exception_handler",
];

/// `is_helper` against the symbol list of the libgcc archive that module builds link (`out/tools/m68k-elf/lib/libgcc-cf54418.a`): every
/// routine the archive defines is a helper, except the ones listed above. A new symbol in a newer libgcc fails this test.
#[test]
fn compiler_helper_names_match_libgcc() {
    let Some(env) = toolchain() else { return };
    let Some(lib) = env.libgcc_cf() else {
        eprintln!("SKIP: libgcc-cf54418.a is not built (uv run python -m tools.patchkit.pk_toolchains libgcc-cf)");
        return;
    };
    let out = std::process::Command::new(env.m68k("nm").unwrap())
        .args(["-g", "--defined-only"])
        .arg(&lib)
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    let defined: std::collections::BTreeSet<&str> = text
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            (f.len() == 3).then(|| f[2])
        })
        .collect();
    assert!(defined.contains("__udivdi3"), "{} symbols", defined.len());
    let missed: Vec<_> = defined
        .iter()
        .filter(|n| !rust::is_helper(n) && !LIBGCC_NOT_CALLED.contains(n))
        .collect();
    assert!(
        missed.is_empty(),
        "libgcc defines routines that is_helper refuses: {missed:?}"
    );
    let wrong: Vec<_> = LIBGCC_NOT_CALLED
        .iter()
        .filter(|n| rust::is_helper(n) || !defined.contains(**n))
        .collect();
    assert!(wrong.is_empty(), "stale LIBGCC_NOT_CALLED entries: {wrong:?}");
}

#[test]
fn make_dependency_lists_are_split_like_make_does() {
    let t = "x: a.c \\\n  b.h c\\ d.h\n";
    assert_eq!(cf::parse_make_deps(t), ["a.c", "b.h", "c d.h"]);
    assert!(cf::parse_make_deps("x:\n").is_empty());
}

#[test]
fn compiler_helper_names() {
    for n in [
        "memcpy",
        "memmove",
        "memset",
        "memcmp",
        "bcmp",
        "__muldi3",
        "__udivdi3",
        "__umoddi3",
        "__ashldi3",
        "__clzsi2",
        "__popcountdi2",
        "__addsf3",
        "__floatsidf",
        "__fixdfsi",
        "__truncdfsf2",
        "__extendsfdf2",
        "__eqdf2",
        "__multi3",
    ] {
        assert!(rust::is_helper(n), "{n}");
    }
    for n in [
        "dkrt_start",
        "memory",
        "__rust_alloc",
        "mem",
        "_RNvC4core3fmt",
        "memcpy2",
    ] {
        assert!(!rust::is_helper(n), "{n}");
    }
}

#[test]
fn source_defines_count_for_ifdef() {
    let a = "#define A\n#ifdef A\nyes\n#else\nno\n#endif";
    assert_eq!(sharc::apply_conditionals(a, &[]).unwrap(), "#define A\nyes");
    let b = "#ifdef X\n#define B\n#endif\n#ifdef B\nb\n#endif\nz";
    assert_eq!(sharc::apply_conditionals(b, &[]).unwrap(), "z");
    let f = "#define F(x) \\\n#ifdef F\nf\n#endif";
    assert_eq!(
        sharc::apply_conditionals(f, &[]).unwrap(),
        "#define F(x) \\\nf"
    );
    let u = "#undef X\n#ifndef X\nn\n#endif";
    assert_eq!(
        sharc::apply_conditionals(u, &["X".into()]).unwrap(),
        "#undef X\nn"
    );
}

#[test]
fn ifdef_alternatives_and_includes_of_dsp_sources() {
    let src = "a\n#ifdef X\nb\n#else\nc\n#endif\n#ifndef X\nd\n#endif\ne";
    assert_eq!(sharc::apply_conditionals(src, &[]).unwrap(), "a\nc\nd\ne");
    assert_eq!(
        sharc::apply_conditionals(src, &["X".into()]).unwrap(),
        "a\nb\ne"
    );
    let nested = "#ifdef X\n#ifdef Y\ny\n#else\nnoy\n#endif\n#endif\nz";
    assert_eq!(
        sharc::apply_conditionals(nested, &["X".into()]).unwrap(),
        "noy\nz"
    );
    assert_eq!(
        sharc::apply_conditionals(nested, &["Y".into()]).unwrap(),
        "z"
    );
    assert!(sharc::apply_conditionals("#ifdef X\na", &[]).is_err());
    assert!(sharc::apply_conditionals("#endif", &[]).is_err());
    assert!(sharc::apply_conditionals("#ifdef\nx\n#endif", &[]).is_err());

    let d = std::env::temp_dir().join(format!("pk-build-test-inc-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("a.inc"), "#define A 1\n").unwrap();
    let out =
        sharc::expand_includes("x\n#include \"a.inc\"\ny", std::slice::from_ref(&d), 0).unwrap();
    assert_eq!(out, "x\n#define A 1\n\ny");
    assert!(
        sharc::expand_includes("#include \"missing.inc\"", std::slice::from_ref(&d), 0).is_err()
    );
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn the_elf_becomes_an_object_prototype() {
    let p = elf::proto_from_elf(&tiny()).unwrap();
    let names: Vec<(&str, &str)> = p
        .sections
        .iter()
        .map(|s| (s.name.as_str(), s.kind))
        .collect();
    assert_eq!(
        names,
        [
            (".text", "text"),
            (".bss", "bss"),
            (".rodata.tiny", "rodata")
        ]
    );
    let sym = |n: &str| {
        p.symbols
            .iter()
            .find(|s| s.name == n)
            .unwrap_or_else(|| panic!("{n}"))
    };
    assert_eq!(sym("tiny_call").section.as_deref(), Some(".text"));
    assert!(sym("tiny_call").global);
    assert_eq!(sym("tiny_word").section.as_deref(), Some(".rodata.tiny"));
    assert!(!sym("tiny_scratch").global);
    assert_eq!(sym("ext_fn").section, None);
    // relocations in the order of the ELF: .rela.text (as the assembler lists them), then .rela.rodata.tiny
    let r: Vec<(&str, u64, &str)> = p
        .relocs
        .iter()
        .map(|r| (r.section.as_str(), r.offset, r.rtype))
        .collect();
    assert_eq!(
        r,
        [
            (".text", 8, "PC32"),
            (".text", 14, "32"),
            (".text", 2, "32"),
            (".rodata.tiny", 4, "32")
        ]
    );
    assert!(matches!(&p.relocs[1].to, RelocTo::Section(s) if s == ".rodata.tiny"));
    assert!(matches!(&p.relocs[0].to, RelocTo::Symbol(s) if s == "ext_near"));
}

#[test]
fn a_big_endian_m68k_object_only() {
    assert!(elf::proto_from_elf(b"not an elf").is_err());
    let mut le = tiny();
    le[5] = 1; // little endian
    assert!(
        elf::proto_from_elf(&le)
            .unwrap_err()
            .0
            .contains("big-endian m68k")
    );
}

fn symmap(names: &[&str]) -> HashMap<String, u32> {
    names
        .iter()
        .enumerate()
        .map(|(i, n)| (n.to_string(), BASE + 0x100 + 8 * i as u32))
        .collect()
}

#[test]
fn link_places_sections_and_applies_relocations() {
    let p = elf::proto_from_elf(&tiny()).unwrap();
    let l = cf::link(&p, BASE, &symmap(&["ext_fn", "ext_near"])).unwrap();
    let w = |a: u32| u32::from_be_bytes(l.image[(a - BASE) as usize..][..4].try_into().unwrap());
    assert_eq!(l.placement[".text"], BASE);
    let word = l.symbols["tiny_word"];
    assert_eq!(w(word), 0x12345678);
    assert_eq!(w(word + 4), l.symbols["tiny_call"]); // the data word holds an address
    // jsr abs.l: the operand is the extern's address; bsr.l: displacement from the extension word
    assert_eq!(w(BASE + 2), BASE + 0x100);
    assert_eq!(
        w(BASE + 8) as i32,
        (BASE + 0x108) as i32 - (BASE + 8) as i32
    );
    assert_eq!(w(BASE + 14), word); // move.l #tiny_word: a section-relative absolute
    assert_eq!(l.relocs_applied["PC32"], 1);
    assert_eq!(l.relocs_applied["32"], 3);
    assert_eq!(l.bss_size, 16);
    // a different base moves only the relocated words
    let m = cf::link(&p, BASE + 0x1000, &symmap(&["ext_fn", "ext_near"])).unwrap();
    assert_eq!(m.image.len(), l.image.len());
    assert_ne!(m.image, l.image);
    // every instruction is ISA_A/B/C
    let hist = cf::check_isa(&l).unwrap();
    assert!(hist.values().sum::<u32>() >= 4, "{hist:?}");
}

#[test]
fn link_refuses_what_it_cannot_resolve() {
    let p = elf::proto_from_elf(&tiny()).unwrap();
    let e = cf::link(&p, BASE, &HashMap::new()).err().unwrap();
    assert!(e.0.contains("unresolved extern symbol"), "{e}");
    // a PC32 reaches everything, but not an absolute 16-bit field: build the case from a PC16 relocation
    let mut q = p.clone();
    q.relocs[0].rtype = "PC16";
    let mut far = symmap(&["ext_fn", "ext_near"]);
    far.insert("ext_near".into(), 0x4000_0000);
    let e = cf::link(&q, BASE, &far).err().unwrap();
    assert!(e.0.contains("out of range"), "{e}");
}

#[test]
fn the_object_carries_its_isa_record() {
    let p = elf::proto_from_elf(&tiny()).unwrap();
    let o = cf::dko_from_proto(&p, "ab".repeat(32), Default::default()).unwrap();
    assert_eq!(o.arch, "cf");
    assert_eq!(o.toolchain["isa_check"], o.isa_digest().unwrap());
    assert!(
        o.toolchain["instructions_redecoded"]
            .parse::<u32>()
            .unwrap()
            >= 4
    );
    // section-relative relocations get a local symbol `.sec.<section>`; symbols are sorted by name
    assert!(
        o.symbols
            .iter()
            .any(|s| s.name == ".sec..rodata.tiny" && !s.global)
    );
    assert!(o.symbols.windows(2).all(|w| w[0].name <= w[1].name));
    assert!(
        o.relocs
            .iter()
            .any(|r| r.rtype == "cf.pc32" && r.symbol == "ext_near")
    );
    o.require_isa_checked().unwrap();
    // the text goes through the canonical writer and parses back
    let text = build::to_text(&o).unwrap();
    assert_eq!(Dko::parse(&text).unwrap(), o);
    assert!(text.starts_with("{\n \"arch\": \"cf\",\n \"format\": 1,\n"));
}

/// The check of unlinked code (`patchkit dko isa-check`): a 68020 form in a text blob is refused with its place; the ColdFire libgcc
/// archive (members read by `elf::ar_members`) passes.
#[test]
fn isa_check_of_blobs_and_archives() {
    // divs.l d1,d2:d0 (64-bit dividend), bfextu: 68020 forms with no ColdFire encoding
    let bad = vec![(".text".to_string(), vec![0x4c, 0x41, 0x0c, 0x02, 0xe9, 0xc0, 0x10, 0x00, 0x4e, 0x75])];
    let e = cf::check_text(&bad, "x: ").unwrap_err().0;
    assert!(e.contains("x: .text+0x0:") && e.contains("outside ISA_A/B/C+EMAC"), "{e}");
    let ok = vec![(".text".to_string(), vec![0x4e, 0x75])];
    assert_eq!(cf::check_text(&ok, "").unwrap().values().sum::<u32>(), 1);
    let Some(env) = toolchain() else { return };
    let Some(lib) = env.libgcc_cf() else { return };
    let members = elf::ar_members(&std::fs::read(lib).unwrap()).unwrap();
    assert!(members.len() > 50, "{} members", members.len());
    for (name, body) in members {
        let secs = elf::text_sections(&body).unwrap();
        cf::check_text(&secs, &format!("{name}: ")).unwrap();
    }
}

#[test]
fn a_68020_form_is_refused() {
    let Some(env) = toolchain() else { return };
    let d = build_dir("isa");
    let s = d.join("bad.s");
    std::fs::write(&s, ".text\n.globl f\nf:\n  bfextu (%a0){4:8},%d0\n  rts\n").unwrap();
    let o = d.join("bad.o");
    let st = std::process::Command::new(env.m68k("as").unwrap())
        .args(["-m68020", "-o"])
        .arg(&o)
        .arg(&s)
        .status()
        .unwrap();
    assert!(st.success());
    let p = elf::proto_from_elf(&std::fs::read(&o).unwrap()).unwrap();
    let e = cf::dko_from_proto(&p, "00".repeat(32), Default::default())
        .err()
        .unwrap();
    assert!(e.0.contains("outside ISA_A/B/C+EMAC"), "{e}");
    assert!(e.0.contains("illegal encoding"), "{e}");
}

fn build_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("pk-build-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn a_header_change_changes_the_source_hash() {
    let Some(env) = toolchain() else { return };
    let d = build_dir("hash");
    std::fs::write(d.join("a.h"), "#define N 1\n").unwrap();
    std::fs::write(
        d.join("a.c"),
        "#include \"a.h\"\nint f(void) { return N; }\n",
    )
    .unwrap();
    std::fs::write(d.join("b.inc"), ".equ N, 1\n").unwrap();
    std::fs::write(d.join("b.s"), ".include \"b.inc\"\n.text\nnop\n").unwrap();
    let inc = [d.clone()];
    let hash = |f: &str| cf::source_digest(&env, &[d.join(f)], &inc).unwrap();
    let (a, b) = (hash("a.c"), hash("b.s"));
    assert_eq!(a, hash("a.c"));
    std::fs::write(d.join("a.h"), "#define N 2\n").unwrap();
    std::fs::write(d.join("b.inc"), ".equ N, 2\n").unwrap();
    assert_ne!(a, hash("a.c"));
    assert_ne!(b, hash("b.s"));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_module_builds_once_and_then_is_unchanged() {
    let Some(_) = toolchain() else { return };
    let d = build_dir("module");
    std::fs::create_dir_all(d.join("src")).unwrap();
    std::fs::write(
        d.join("module.toml"),
        "[module]\nid = \"test/tiny\"\ncategory = \"core\"\n\n[[object]]\npath = \"build/tiny.dko.json\"\nsources = [\"src/t.c\"]\n",
    )
    .unwrap();
    std::fs::write(
        d.join("src/t.c"),
        "extern int ext(int);\nint twice(int x) { return ext(x) + ext(x); }\n",
    )
    .unwrap();
    let opts = BuildOptions {
        root: root(),
        ..Default::default()
    };
    let r = build::build_module(&d, &opts).unwrap();
    assert_eq!(r.objects[0].status, Status::Built);
    assert!(r.objects[0].instructions.unwrap() > 3);
    let text = std::fs::read_to_string(d.join("build/tiny.dko.json")).unwrap();
    let o = Dko::parse(&text).unwrap();
    o.require_isa_checked().unwrap();
    assert!(
        o.symbols
            .iter()
            .any(|s| s.name == "ext" && s.section.is_none())
    );
    // nothing changed: not compiled again; --force compiles it; the result is the same bytes
    let again = build::build_module(&d, &opts).unwrap();
    assert_eq!(again.objects[0].status, Status::Unchanged);
    assert_eq!(
        build::module_status(&root(), &d).unwrap(),
        [("build/tiny.dko.json".to_string(), true)]
    );
    let forced = build::build_module(
        &d,
        &BuildOptions {
            force: true,
            ..opts.clone()
        },
    )
    .unwrap();
    assert_eq!(forced.objects[0].status, Status::Built);
    assert_eq!(
        std::fs::read_to_string(d.join("build/tiny.dko.json")).unwrap(),
        text
    );
    // --check: current, then stale after a source change
    let check = BuildOptions {
        check: true,
        ..opts.clone()
    };
    assert_eq!(
        build::build_module(&d, &check).unwrap().objects[0].status,
        Status::Current
    );
    std::fs::write(
        d.join("src/t.c"),
        "extern int ext(int);\nint twice(int x) { return ext(x) * 2; }\n",
    )
    .unwrap();
    assert!(!build::module_status(&root(), &d).unwrap()[0].1);
    let e = build::build_module(&d, &check).err().unwrap();
    assert!(e.0.contains("is stale"), "{e}");
    // a build into another directory leaves the module alone
    let out = build_dir("module-out");
    let o2 = BuildOptions {
        out: Some(out.clone()),
        ..opts
    };
    let r = build::build_module(&d, &o2).unwrap();
    assert_eq!(r.objects[0].status, Status::Built);
    assert!(out.join("build/tiny.dko.json").is_file());
    assert_eq!(
        std::fs::read_to_string(d.join("build/tiny.dko.json")).unwrap(),
        text
    );
    let _ = std::fs::remove_dir_all(&d);
    let _ = std::fs::remove_dir_all(&out);
}

/// A content module (any category but `core`) keeps its objects in the cache `out/cache/dko/<id>/<path>`, not in the module; the
/// library builds a stale or missing one on demand and leaves out the objects of modules it was not asked for.
#[test]
fn a_content_module_builds_into_the_cache_on_demand() {
    use patchkit::module::{Build, Library};
    let Some(_) = toolchain() else { return };
    let id = format!("test/cache-{}", std::process::id());
    let d = build_dir("content");
    let moddir = d.join("m");
    std::fs::create_dir_all(moddir.join("src")).unwrap();
    std::fs::write(
        moddir.join("module.toml"),
        format!(
            "[module]\nid = \"{id}\"\nversion = \"0.1.0\"\nname = \"x\"\ncategory = \"test\"\ntype = \"raw\"\nauthor = \"x\"\nstatus = \"experimental\"\nsummary = \"x\"\n\n[[object]]\npath = \"build/tiny.dko.json\"\nplace = \"cf.expansion\"\nsources = [\"src/t.c\"]\n"
        ),
    )
    .unwrap();
    std::fs::write(moddir.join("src/t.c"), "int twice(int x) { return x + x; }\n").unwrap();
    let cached = build::cache_dir(&root()).join(&id).join("build/tiny.dko.json");
    let _ = std::fs::remove_dir_all(build::cache_dir(&root()).join(&id));
    let scan = |only: Option<&[String]>| {
        Library::scan_in(Some(Build { root: &root(), only, defines: &[] }), &d, false).unwrap()
    };

    // asked for nothing: no object, nothing built
    assert!(scan(Some(&[])).get(&id).unwrap().objects.is_empty());
    assert!(!cached.exists());
    // a missing object is built into the cache, never into the module
    assert_eq!(scan(None).get(&id).unwrap().objects.len(), 1);
    assert!(cached.is_file() && !moddir.join("build").exists());
    let text = std::fs::read_to_string(&cached).unwrap();
    let stamp = std::fs::metadata(&cached).unwrap().modified().unwrap();
    // a current object is read, not built again
    assert_eq!(scan(Some(&[id.clone()])).get(&id).unwrap().objects.len(), 1);
    assert_eq!(std::fs::metadata(&cached).unwrap().modified().unwrap(), stamp);
    // a changed source makes it stale; the next scan builds it again
    std::fs::write(moddir.join("src/t.c"), "int twice(int x) { return 2 * x + 1; }\n").unwrap();
    assert_eq!(scan(None).get(&id).unwrap().objects.len(), 1);
    assert_ne!(std::fs::read_to_string(&cached).unwrap(), text);
    // `patchkit build` finds it current
    let r = build::build_module(&moddir, &BuildOptions { root: root(), ..Default::default() }).unwrap();
    assert_eq!(r.objects[0].status, Status::Unchanged);
    // both canonical: in a lane `out` is a symlink to the main tree's
    assert_eq!(std::fs::canonicalize(&r.objects[0].path).unwrap(), std::fs::canonicalize(&cached).unwrap());
    let _ = std::fs::remove_dir_all(build::cache_dir(&root()).join(&id));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn defines_need_an_output_directory() {
    let opts = BuildOptions {
        root: root(),
        defines: vec!["X".into()],
        ..Default::default()
    };
    let e = build::build_module(Path::new("/nonexistent"), &opts)
        .err()
        .unwrap();
    assert!(e.0.contains("--define needs --out"), "{e}");
}

#[test]
fn sharc_sources_given_as_texts_make_the_object_of_the_files() {
    // the browser has no directories: the includes are a map by the name the source uses
    let includes = std::collections::BTreeMap::from([(
        "k.inc".to_string(),
        "#define N 8\n.PKBSS buf, N;\n".to_string(),
    )]);
    let src = ".SECTION/PM seg_pmco;\n#include \"k.inc\"\n.GLOBAL _f;\n_f:\n    R0 = DM(buf);\n#ifdef EXTRA\n    R1 = 1;\n#endif\n    RTS;\n"
        .to_string();
    let build = |defines: &[String]| {
        let t = sharc::source_text_from(std::slice::from_ref(&src), &includes, defines).unwrap();
        (sharc::source_hash(&t), build::to_text(&sharc::sharc_object(&t).unwrap()).unwrap())
    };
    let (h0, plain) = build(&[]);
    let (h1, extra) = build(&["EXTRA".into()]);
    assert_ne!(h0, h1);
    assert!(plain.contains("\"arch\": \"sharc\"") && plain.contains("pkbss_buf"));
    assert_ne!(plain, extra);
    // the same text from files: the same bytes
    let d = std::env::temp_dir().join(format!("pk-build-test-sharc-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("k.inc"), &includes["k.inc"]).unwrap();
    std::fs::write(d.join("m.s"), &src).unwrap();
    let t = sharc::source_text_in(vec![d.clone()], &[d.join("m.s")], &[]).unwrap();
    assert_eq!(build::to_text(&sharc::sharc_object(&t).unwrap()).unwrap(), plain);
    std::fs::remove_dir_all(&d).unwrap();
    // a missing include and a refused instruction are errors, not panics
    assert!(sharc::source_text_from(&["#include \"no.inc\"".into()], &includes, &[]).is_err());
    let e = sharc::sharc_object(".SECTION/PM seg_pmco;\nDM(15, I4) = I0;\n").unwrap_err();
    assert!(e.0.contains("the ISA does not allow"), "{e}");
}

/// Exercise endian conversions compiled by the actual ColdFire Rust backend.
/// The weak compiler_builtins bswap used to call itself, overflowing the guest
/// stack; host-endian tests could not detect that target-only recursion.
#[test]
fn coldfire_rust_endian_conversions_return_without_recursive_helpers() {
    use coldfire::{Bus, BusError, Cpu};
    let Some(env) = toolchain() else { return };
    if !env.codegen_gcc_ready() {
        return;
    }
    let dir = patchkit::build::toolchain::TempDir::new("pk-rust-endian-test-").unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    let manifest = dir.path().join("Cargo.toml");
    std::fs::write(
        &manifest,
        r#"
[package]
name = "cf-endian-proof"
version = "0.1.0"
edition = "2021"
[lib]
crate-type = ["staticlib"]
[workspace]
[profile.release]
panic = "abort"
opt-level = "s"
"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("src/lib.rs"),
        r#"
#![no_std]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! { loop {} }
#[no_mangle]
pub unsafe extern "C" fn endian_read(p: *const u8) -> u32 {
    u32::from_le_bytes([*p, *p.add(1), *p.add(2), *p.add(3)])
}
#[no_mangle]
pub unsafe extern "C" fn endian_write(value: u32, p: *mut u32) {
    core::ptr::write_unaligned(p, value.to_le());
}
"#,
    )
    .unwrap();
    let (proto, hash, info) = rust::object_proto(&env, &manifest).unwrap();
    let helper = proto
        .symbols
        .iter()
        .find(|s| s.name == "__bswapsi2")
        .unwrap();
    assert!(
        !helper.global,
        "compiler helpers remain private to each object"
    );
    assert!(
        proto
            .relocs
            .iter()
            .all(|r| Some(&r.section) != helper.section.as_ref()),
        "the target helper must have no calls, including self recursion"
    );
    let object = cf::dko_from_proto(&proto, hash, info).unwrap();
    let linked = patchkit::link::link(&object, 0x1000, &Default::default()).unwrap();
    assert!(linked.image.len() < 0x6000);

    struct Ram(Vec<u8>);
    impl Bus for Ram {
        fn read8(&mut self, addr: u32) -> Result<u8, BusError> {
            self.0
                .get(addr as usize)
                .copied()
                .ok_or(BusError { addr, write: false })
        }
        fn read16(&mut self, addr: u32) -> Result<u16, BusError> {
            Ok(u16::from_be_bytes([
                self.read8(addr)?,
                self.read8(addr + 1)?,
            ]))
        }
        fn read32(&mut self, addr: u32) -> Result<u32, BusError> {
            Ok(u32::from_be_bytes([
                self.read8(addr)?,
                self.read8(addr + 1)?,
                self.read8(addr + 2)?,
                self.read8(addr + 3)?,
            ]))
        }
        fn write8(&mut self, addr: u32, value: u8) -> Result<(), BusError> {
            *self
                .0
                .get_mut(addr as usize)
                .ok_or(BusError { addr, write: true })? = value;
            Ok(())
        }
        fn write16(&mut self, addr: u32, value: u16) -> Result<(), BusError> {
            for (i, byte) in value.to_be_bytes().iter().enumerate() {
                self.write8(addr + i as u32, *byte)?;
            }
            Ok(())
        }
        fn write32(&mut self, addr: u32, value: u32) -> Result<(), BusError> {
            for (i, byte) in value.to_be_bytes().iter().enumerate() {
                self.write8(addr + i as u32, *byte)?;
            }
            Ok(())
        }
    }
    for value in [0u32, 0x01234567, 0x00200000, 0x80000001, u32::MAX] {
        for entry in ["endian_read", "endian_write"] {
            let mut ram = Ram(vec![0; 0x10000]);
            ram.0[0x1000..0x1000 + linked.image.len()].copy_from_slice(&linked.image);
            ram.0[0x8000..0x8004].copy_from_slice(&value.to_le_bytes());
            let mut cpu = Cpu::new();
            cpu.pc = linked.symbols[entry] as u32;
            cpu.sr = 0x2700;
            cpu.a[7] = 0xf000;
            for i in 2..8 {
                cpu.d[i] = 0x12340000 + i as u32;
            }
            for i in 2..7 {
                cpu.a[i] = 0x56780000 + i as u32;
            }
            let before_d = cpu.d;
            let before_a = cpu.a;
            ram.write32(0xf000, 0x9000).unwrap();
            ram.write32(
                0xf004,
                if entry == "endian_read" {
                    0x8000
                } else {
                    value
                },
            )
            .unwrap();
            ram.write32(0xf008, 0x8100).unwrap();
            for _ in 0..100 {
                if cpu.pc == 0x9000 {
                    break;
                }
                cpu.step(&mut ram).unwrap();
                assert_eq!(cpu.last_exception, None);
            }
            assert_eq!(cpu.pc, 0x9000, "{entry} must return, value {value:#x}");
            assert_eq!(cpu.a[7], 0xf004, "cdecl stack restored");
            assert_eq!(&cpu.d[2..], &before_d[2..]);
            assert_eq!(&cpu.a[2..7], &before_a[2..7]);
            if entry == "endian_read" {
                assert_eq!(cpu.d[0], value);
            } else {
                assert_eq!(&ram.0[0x8100..0x8104], &value.to_le_bytes());
            }
        }
    }
}
