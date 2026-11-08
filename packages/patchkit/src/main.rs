//! `patchkit` command line.

use patchkit::apply;
use patchkit::error::{Error, Result, sha256_hex};
use patchkit::firmware::Firmware;
use patchkit::module::Library;
use patchkit::port::Registry;
use patchkit::verify;
use std::path::{Path, PathBuf};

#[cfg(not(target_arch = "wasm32"))]
mod build_cli;
#[cfg(not(target_arch = "wasm32"))]
mod flash_cli;
#[cfg(not(target_arch = "wasm32"))]
mod storedump;

const USAGE: &str = "usage: patchkit [--root DIR] COMMAND ...

  inspect X.syx                          device, version, port, patch record
  check   --firmware X.syx [--modules a,b]  which modules fit, and why not
  apply   --firmware X.syx [--modules a,b | --set NAME] [-o OUT.syx] [--plan plan.json] [--no-record (oracles only)]
                                         without --modules: the device's patch set (devices/*.toml [patch_sets]), `device` by default
  (any command that reads modules: [--modules-dir DIR], PATCHKIT_MODULES=dir:dir for extra module roots)
  explain --firmware X.syx (--modules a,b | --plan plan.json)   what a patch does, site by site
  verify  OUT.syx [--base X.syx]         re-decode and check an image
  preflight OUT.syx                      before flashing: known stock base, sections 2/4/5/8 identical, checksum, HMAC, limits;
                                         exit 1 on any failure (stock files are looked up in firmware/ by sha256)
  isa-scan X.syx [--list]                stores in the DSP section (7) the SHARC+ ISA does not have, one per line; exit 1 when found
  rebuild X.syx -o OUT.syx               codec round trip (an unchanged image reproduces itself)
  pack    MODULE_ID -o OUT.dkm [--name N] [--runtime-min V] [--device SHORT] [--trace] [--ui-preview] [--manifest PATH]...   the module's ColdFire object as a run-time .dkm;
                                         builds the objects that are stale first (content modules: cache out/cache/dko)
                                         (a `dynamic = true` module: its machine and param records for --device)
                                         (--trace: built with DK_TRACE defined, trace bit set in the header; objects cached apart)
                                         --ui-preview: declarative machine UI only, stub init and no DSP; for isolated zero-DSP emulator previews
                                         --manifest PATH (repeatable, relative to cwd): explicit library instead of discovery; include the module and its requires closure
                                         explicit manifests replace modules/, --modules-dir and PATCHKIT_MODULES
  desc    MODULE_ID                      the effects the module declares (the DK_FX records of its ColdFire object), as JSON
  synth-dkm SPEC.toml --device SHORT -o OUT.dkm   a synthetic .dkm of a chosen size, machines, parameters and DSP voices (capacity tests;
                                         the spec keys are in packages/patchkit/src/dkmsynth.rs)
  dkm-info FILE.dkm                      check a .dkm and list what it holds
  fit     --json -                       the fit rule (dkrt-fit): {caps, needs, empty?} on stdin -> {each, set, line}; exit 1 on a bad request
  needs   [--json] FILE.dkm...           what a load of each file takes (arena, DSP area, rows), inferred from its contents; `-` reads stdin;
                                         --json: one object, file -> the needs or {\"error\": text}; exit 1 when a file has none
  dkm-dsp-image FILE BASE [NAME=VALUE..]  the relocated DSP image of a .dkm at the L2 byte address BASE (hex)
  pack-section IN OUT                    aPLib-pack a raw file as a section stream (for tests)
  store dump IMAGE [--device dt2|dn2] [--overlay FILE] [--base-sector N] [--json]   digikit's own data on a card image or a raw sector dump:
                                         the store header and files, the selection record, the settings record (both copies, the one in use,
                                         every entry decoded); `store dump --record FILE` reads one settings record file (docs/design/digikit-store.md)
  build   [MODULE...] [--check] [--force] [--define NAME]... [--out DIR] [--status] [--paths] [--tree DIR]
                                         module sources -> objects: build/*.dko.json for a core/* module (committed), else the cache
                                         out/cache/dko/<module id>/<src12>/<path> (ColdFire C, asm and Rust, and SHARC+ with Selache as a
                                         library: all in this process). MODULE is a directory or a module id; none: every module.
                                         An object whose stored source_sha256 equals the hash of its sources is not compiled
                                         (--force compiles it). --check writes nothing and fails on a stale object.
                                         --define NAME[=V] selects an #ifdef of the DSP sources (needs --out; the stored objects
                                         stay). --status prints {module: \"unchanged\"|\"needs build\"} as JSON and builds nothing.
                                         --paths prints {module: {object path: stored file}} for the sources now and builds nothing.
                                         --tree DIR: every module under DIR only.
  dko show FILE.dko.json [--relocs]      arch, toolchain record, sections, symbols and relocations of an object
  dko isa-digest FILE.dko.json           the ISA check record the object's bytes need (toolchain.isa_check)
  dko isa-check FILE...                  ColdFire V4e ISA check of .dko.json (cf), ELF objects and ar archives
  sharc text|asm|link|shift-check ...    the SHARC+ build for the Python test rigs: expanded source text, an assembled object as JSON, an
                                         object placed at a short-word address and verified, the shift-immediate rule on hex words
  cf-link SRC... --base HEX [--sym NAME=HEX]... [-I DIR]... [--bin OUT] [--json OUT]
                                         compile, link at BASE against the symbols, ISA-check (unit tests of C routines)
  flash   STOCK.syx (--port NAME | --emulator SOCKET) [--restore] [--set NAME] [--dry-run] [--save OUT.syx] [--yes]
                                         patch the stock file with the device's patch set, check the image and the messages the way the
                                         device's OS UPGRADE receiver does, then send it (USB MIDI) and read INFO after the restart;
                                         --restore sends the stock file itself; --dry-run only checks (docs/PATCHING.md, \"Flashing\")
  list                                   modules under modules/ and ports under devices/ports/
  catalog-check                          validate all modules/ and tests/fixtures/modules/ manifests and requires;
                                         host only: no objects, builds, device models or firmware are read
  gen [--check [--scope all|modules|shared]] [--modules a,b] [--manifest PATH]...
                                         write (or check) generated module, SDK and firmware files
                                         --manifest PATH (repeatable, needs --modules, incompatible with --scope shared): explicit library including requires;
                                         include core/machines for machine galleries or [[gen]] device constants
                                         --scope restricts checks to module outputs or shared SDK/firmware outputs (default all)
  reference [-o OUT.html] [--fragment]   the searchable firmware reference page (default out/reference/fw.html) from
                                         modules/core/fw/reference.json (written by `gen`); --fragment writes the
                                         page body without the document tags, for publishing
";

struct Args {
    root: PathBuf,
    rest: Vec<String>,
}

fn find_root(given: Option<String>) -> PathBuf {
    if let Some(g) = given {
        return PathBuf::from(g);
    }
    let mut d = std::env::current_dir().unwrap_or_default();
    loop {
        if d.join("devices/ports").is_dir() {
            return d;
        }
        if !d.pop() {
            return std::env::current_dir().unwrap_or_default();
        }
    }
}

fn flag(rest: &[String], name: &str) -> Option<String> {
    rest.iter()
        .position(|a| a == name)
        .and_then(|i| rest.get(i + 1).cloned())
}

/// Preview only declarations: dropping arbitrary CF bytes could silently hide callbacks or
/// initialization. Require descriptor-only CF objects rather than recognizing hook names.
fn check_ui_preview(m: &patchkit::module::Module) -> Result<()> {
    let id = &m.manifest.module.id;
    let reject = |reason: &str| Error(format!("{id}: --ui-preview {reason}"));
    if !m.manifest.module.dynamic
        || m.manifest.module.kind != patchkit::module::ModuleKind::Machine
        || m.manifest.machine.is_empty()
    {
        return Err(reject("requires a declarative run-time machine"));
    }
    if !patchkit::desc::module_fx(m)?.is_empty() {
        return Err(reject("does not support effects"));
    }
    if m.manifest.sample_bank.is_some() || m.manifest.sample_tracks.is_some() {
        return Err(reject("does not support sample banks or sample tracks"));
    }
    if m.manifest.param.iter().any(|p| p.action.is_some()) {
        return Err(reject("does not support parameter click actions"));
    }
    if !m.manifest.native_page.is_empty() {
        return Err(reject("does not support native callback pages"));
    }
    for obj in m.objects.iter().filter(|o| o.arch == "cf") {
        if let Some(obj) = patchkit::desc::strip(obj) {
            let sections: Vec<_> = obj
                .sections
                .iter()
                .filter(|s| s.size > 0)
                .map(|s| s.name.as_str())
                .collect();
            return Err(reject(&format!(
                "requires descriptor-only ColdFire objects; remaining sections {} could contain hooks or callbacks",
                sections.join(", ")
            )));
        }
    }
    Ok(())
}

fn registry(root: &Path) -> Result<Registry> {
    Registry::from_dir(&root.join("devices/ports"))
}

/// `modules/` and `tests/fixtures/modules/` of the repository, then the extra roots of `--modules-dir DIR` and the PATCHKIT_MODULES
/// environment variable (a path list), in that order. Generated modules (derived from a firmware) live there. Content modules get their
/// objects from the cache `out/cache/dko`; a stale object is built.
fn library(root: &Path, rest: &[String]) -> Result<Library> {
    library_with(root, rest, false, None)
}

/// `library`; with `lenient`, a module whose object files are missing still loads (for `gen`); with `only`, the objects of these
/// content modules only are loaded or built (`pack` and `apply`: the others load without objects).
fn library_with(
    root: &Path,
    rest: &[String],
    lenient: bool,
    only: Option<&[String]>,
) -> Result<Library> {
    // `--trace` builds the modules with DK_TRACE defined (C: -DDK_TRACE; SHARC+: `#ifdef DK_TRACE`)
    let defines: Vec<String> = if rest.iter().any(|a| a == "--trace") {
        vec!["DK_TRACE".into()]
    } else {
        vec![]
    };
    let mut lib = Library::scan_repo_defined(root, lenient, only, &defines)?;
    let mut extra: Vec<PathBuf> = flag(rest, "--modules-dir")
        .into_iter()
        .map(PathBuf::from)
        .collect();
    if let Some(v) = std::env::var_os("PATCHKIT_MODULES") {
        extra.extend(std::env::split_paths(&v));
    }
    let build = patchkit::module::Build {
        root,
        only,
        defines: &defines,
    };
    for d in extra {
        if d.is_dir() {
            lib.extend_unique(Library::scan_in(Some(build), &d, lenient)?.modules)?;
        }
    }
    lib.lower(&patchkit::typed::Models::from_dir(&root.join("devices"))?)?;
    Ok(lib)
}

/// Explicit manifests replace all discovery roots, including PATCHKIT_MODULES. They must include
/// the selected IDs, their requires closure and any allocation providers used by generation.
fn selected_library(
    root: &Path,
    rest: &[String],
    lenient: bool,
    only: &[String],
    selected: &[String],
) -> Result<Library> {
    let mut paths = vec![];
    for (i, arg) in rest.iter().enumerate() {
        if arg == "--manifest" {
            let path = rest
                .get(i + 1)
                .filter(|p| !p.starts_with('-'))
                .ok_or_else(|| Error("--manifest requires a path".into()))?;
            paths.push(PathBuf::from(path));
        }
    }
    if paths.is_empty() {
        return library_with(root, rest, lenient, Some(only));
    }
    let defines = if rest.iter().any(|a| a == "--trace") {
        vec!["DK_TRACE".to_string()]
    } else {
        vec![]
    };
    let mut lib = Library::from_manifests(
        patchkit::module::Build {
            root,
            only: Some(only),
            defines: &defines,
        },
        &paths,
        lenient,
        selected,
    )?;
    lib.lower(&patchkit::typed::Models::from_dir(&root.join("devices"))?)?;
    if lenient && lib.get("core/machines").is_none() {
        for id in selected {
            let m = lib.get(id).unwrap();
            if !m.manifest.gens.is_empty() || !m.manifest.machine.is_empty() {
                return Err(Error(format!(
                    "{id}: generation requires --manifest for core/machines (machine allocations)"
                )));
            }
        }
    }
    Ok(lib)
}

/// The selection plus its requirements; says what it added.
fn with_requires(lib: &Library, ids: Vec<String>) -> Result<Vec<String>> {
    let (all, added) = lib.with_requires(&ids)?;
    if !added.is_empty() {
        println!("added by requires: {}", added.join(", "));
    }
    Ok(all)
}

fn library_for_apply(
    root: &Path,
    rest: &[String],
    ids: Vec<String>,
) -> Result<(Library, Vec<String>)> {
    // Discover requires before building content: unrelated sources need not be buildable.
    let manifests = library_with(root, rest, false, Some(&[]))?;
    let ids = with_requires(&manifests, ids)?;
    Ok((library_with(root, rest, false, Some(&ids))?, ids))
}

fn preflight_known(root: &Path) -> Result<Vec<patchkit::preflight::Known>> {
    let dir = root.join("devices");
    let rd = std::fs::read_dir(&dir).map_err(|e| Error(format!("{}: {e}", dir.display())))?;
    let mut paths: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.sort();
    let mut texts = vec![];
    for p in paths
        .iter()
        .filter(|p| p.extension().is_some_and(|e| e == "toml"))
    {
        texts.push(std::fs::read_to_string(p).map_err(|e| Error(format!("{}: {e}", p.display())))?);
    }
    patchkit::preflight::known_from_texts(texts.iter().map(|s| s.as_str()))
}

fn read(p: &str) -> Result<Vec<u8>> {
    std::fs::read(p).map_err(|e| Error(format!("{p}: {e}")))
}

fn write(p: &str, b: &[u8]) -> Result<()> {
    std::fs::write(p, b).map_err(|e| Error(format!("{p}: {e}")))
}

fn need(rest: &[String]) -> Result<&String> {
    rest.first().ok_or_else(|| Error(USAGE.into()))
}

fn run(a: Args) -> Result<()> {
    let cmd = a.rest.first().map(|s| s.as_str()).unwrap_or("");
    let rest = if a.rest.is_empty() {
        &a.rest[..]
    } else {
        &a.rest[1..]
    };
    match cmd {
        "catalog-check" => {
            if !rest.is_empty() {
                return Err(Error(
                    "catalog-check takes no arguments (use --root DIR before the command)".into(),
                ));
            }
            let count = Library::catalog_check(&a.root)?;
            println!("catalog ok: {count} module manifests");
        }
        "inspect" => {
            let raw = read(need(rest)?)?;
            let fw = Firmware::parse(&raw)?;
            let reg = registry(&a.root)?;
            println!("device    {}", fw.device.short());
            println!("sha256    {}", fw.syx_sha256);
            match reg.find(&fw.syx_sha256) {
                Some(p) => println!("port      {} (stock image)", p.id()),
                None => println!("port      none for this sha256"),
            }
            let rep = verify::verify(&raw, Some(&reg), None)?;
            match rep.record {
                Some(r) => println!(
                    "record    patched from {} with {}",
                    r.base_sha256,
                    r.modules.join(", ")
                ),
                None => println!("record    none"),
            }
            for (id, d, s) in rep.sections {
                println!("section {id}  {d} bytes decoded, {s} stored");
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        "store" => {
            if rest.first().map(String::as_str) != Some("dump") {
                return Err(Error(USAGE.into()));
            }
            storedump::run(&rest[1..]).map_err(Error)?;
        }
        #[cfg(not(target_arch = "wasm32"))]
        "build" => build_cli::build(&a.root, rest)?,
        #[cfg(not(target_arch = "wasm32"))]
        "dko" => build_cli::dko(rest)?,
        #[cfg(not(target_arch = "wasm32"))]
        "cf-link" => build_cli::cf_link(&a.root, rest)?,
        #[cfg(not(target_arch = "wasm32"))]
        "sharc" => build_cli::sharc(&a.root, rest)?,
        "list" => {
            let reg = registry(&a.root)?;
            for p in &reg.ports {
                println!("port   {}  {}", p.id(), p.port.syx_sha256);
            }
            let lib = library(&a.root, rest)?;
            for m in &lib.modules {
                println!(
                    "module {}  {}  {}",
                    m.tag(),
                    m.manifest.module.status,
                    m.manifest.module.summary
                );
            }
        }
        "check" | "apply" => {
            let fwp = flag(rest, "--firmware").ok_or_else(|| Error(USAGE.into()))?;
            let raw = read(&fwp)?;
            let ids: Vec<String> = match flag(rest, "--modules") {
                Some(m) => m
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect(),
                // no --modules: the named set of the firmware's device (`device` unless --set says another)
                None if cmd == "apply" || flag(rest, "--set").is_some() => {
                    let short = Firmware::parse(&raw)?.device.short();
                    let set = flag(rest, "--set").unwrap_or_else(|| "device".into());
                    let info = patchkit::devdata::by_short(short)
                        .ok_or_else(|| Error(format!("no device file for {short}")))?;
                    info.patch_set(&set)?.to_vec()
                }
                None => vec![],
            };
            let reg = registry(&a.root)?;
            let (lib, ids) = if cmd == "apply" {
                library_for_apply(&a.root, rest, ids)?
            } else {
                let lib = library(&a.root, rest)?;
                let ids = with_requires(&lib, ids)?;
                (lib, ids)
            };
            if cmd == "check" {
                let fw = Firmware::parse(&raw)?;
                let port = reg
                    .find(&fw.syx_sha256)
                    .ok_or_else(|| Error(format!("no port for {}", fw.syx_sha256)))?;
                for m in &lib.modules {
                    match apply::compatible(m, port) {
                        Ok(()) => println!("ok       {}", m.tag()),
                        Err(e) => println!("refused  {e}"),
                    }
                }
                if !ids.is_empty() {
                    apply::apply(&raw, &reg, &lib, &ids)?;
                    println!("the selection applies");
                }
            } else {
                let opts = apply::Options {
                    record: !rest.iter().any(|a| a == "--no-record"),
                };
                let out = apply::apply_with(&raw, &reg, &lib, &ids, opts)?;
                if let Some(o) = flag(rest, "-o") {
                    write(&o, &out.syx)?;
                    println!(
                        "{o}: {} bytes, sha256 {}",
                        out.syx.len(),
                        sha256_hex(&out.syx)
                    );
                }
                if let Some(p) = flag(rest, "--plan") {
                    let j = serde_json::to_string_pretty(&out.plan)?;
                    write(&p, (j + "\n").as_bytes())?;
                }
                let o = out.plan.output.as_ref().unwrap();
                println!(
                    "applied {} to {} {}: {} writes, container {} bytes",
                    out.plan.modules.join(", "),
                    out.plan.base.device,
                    out.plan.base.version,
                    out.plan.writes.len(),
                    o.container_bytes
                );
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        "flash" => {
            let reg = registry(&a.root)?;
            let lib = library(&a.root, rest)?;
            flash_cli::run(&reg, &lib, rest)?;
        }
        "gen" => {
            let check = rest.iter().any(|a| a == "--check");
            let scope = flag(rest, "--scope");
            if rest.iter().any(|a| a == "--scope")
                && (!check || !matches!(scope.as_deref(), Some("all" | "modules" | "shared")))
            {
                return Err(Error(
                    "gen --scope requires --check and all, modules or shared".into(),
                ));
            }
            if scope.as_deref() == Some("shared") && rest.iter().any(|a| a == "--manifest") {
                return Err(Error(
                    "gen --manifest cannot be used with --scope shared".into(),
                ));
            }
            let mut stale = 0;
            if scope.as_deref() != Some("shared") {
                // the generated files are inputs of the build: nothing is built here
                let only: Vec<String> = flag(rest, "--modules")
                    .unwrap_or_default()
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect();
                if rest.iter().any(|a| a == "--manifest") && only.is_empty() {
                    return Err(Error("gen --manifest requires --modules a,b".into()));
                }
                let lib = selected_library(&a.root, rest, true, &[], &only)?;
                let models = patchkit::typed::Models::from_dir(&a.root.join("devices"))?;
                let facts = patchkit::devgen::Facts::from_dir(&a.root.join("devices"))?;
                for m in &lib.modules {
                    if !only.is_empty() && !only.iter().any(|o| o == m.id()) {
                        continue;
                    }
                    let mut files = patchkit::modgen::generate(&lib, &models, m.id())?;
                    files.extend(patchkit::devgen::generate(&lib, &facts, m.id())?);
                    for (rel, text) in files {
                        let path = m.dir.join(&rel);
                        let have = std::fs::read_to_string(&path).ok();
                        if have.as_deref() == Some(text.as_str()) {
                            continue;
                        }
                        if check {
                            println!("stale    {}", path.display());
                            stale += 1;
                        } else {
                            if let Some(d) = path.parent() {
                                std::fs::create_dir_all(d)
                                    .map_err(|e| Error(format!("{}: {e}", d.display())))?;
                            }
                            write(&path.to_string_lossy(), text.as_bytes())?;
                            println!("wrote    {}", path.display());
                        }
                    }
                }
                // A content module declares its machines in its objects (.dk.desc), so its gallery.json needs them: a second library loads
                // (and builds when stale) the objects, after the files above are written, which the build reads.
                let with_gallery: Vec<String> = lib
                    .modules
                    .iter()
                    .filter(|m| patchkit::typed::has_typed(&m.manifest))
                    .map(|m| m.id().to_string())
                    .collect();
                let content: Vec<String> = lib
                    .modules
                    .iter()
                    .filter(|m| !with_gallery.iter().any(|g| g == m.id()))
                    .filter(|m| only.is_empty() || only.iter().any(|o| o == m.id()))
                    .map(|m| m.id().to_string())
                    .collect();
                if !content.is_empty() {
                    let full = selected_library(&a.root, rest, true, &content, &only)?;
                    for (path, text) in patchkit::modgen::generate_many(&full, &models, &content)? {
                        let have = std::fs::read_to_string(&path).ok();
                        if have.as_deref() == Some(text.as_str()) {
                            continue;
                        }
                        if check {
                            println!("stale    {}", path.display());
                            stale += 1;
                        } else {
                            if let Some(d) = path.parent() {
                                std::fs::create_dir_all(d)
                                    .map_err(|e| Error(format!("{}: {e}", d.display())))?;
                            }
                            write(&path.to_string_lossy(), text.as_bytes())?;
                            println!("wrote    {}", path.display());
                        }
                    }
                }
            }
            if scope.as_deref() != Some("modules") {
                for (rel, text) in patchkit::sdkgen::generate(&a.root)? {
                    let path = a.root.join(&rel);
                    let have = std::fs::read_to_string(&path).ok();
                    if have.as_deref() == Some(text.as_str()) {
                        continue;
                    }
                    if check {
                        println!("stale    {}", path.display());
                        stale += 1;
                    } else {
                        if let Some(d) = path.parent() {
                            std::fs::create_dir_all(d)
                                .map_err(|e| Error(format!("{}: {e}", d.display())))?;
                        }
                        write(&path.to_string_lossy(), text.as_bytes())?;
                        println!("wrote    {}", path.display());
                    }
                }
                for (rel, text) in patchkit::fwgen::generate_with_ports(&a.root)? {
                    let path = a.root.join(&rel);
                    let have = std::fs::read_to_string(&path).ok();
                    if have.as_deref() == Some(text.as_str()) {
                        continue;
                    }
                    if check {
                        println!("stale    {}", path.display());
                        stale += 1;
                    } else {
                        if let Some(d) = path.parent() {
                            std::fs::create_dir_all(d)
                                .map_err(|e| Error(format!("{}: {e}", d.display())))?;
                        }
                        write(&path.to_string_lossy(), text.as_bytes())?;
                        println!("wrote    {}", path.display());
                    }
                }
            }
            if stale > 0 {
                return Err(Error(format!(
                    "{stale} generated file(s) are stale: run `patchkit gen`"
                )));
            }
        }
        "reference" => {
            let src = a.root.join(patchkit::fwgen::REFERENCE_OUT);
            let json = std::fs::read_to_string(&src)
                .map_err(|_| Error(format!("{}: missing: run `patchkit gen`", src.display())))?;
            let frag = include_str!("fwref.html").replace("__DATA__", &json.replace("</", "<\\/"));
            let page = if rest.iter().any(|x| x == "--fragment") {
                frag
            } else {
                format!(
                    "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1, viewport-fit=cover\">\n</head>\n<body>\n{frag}\n</body>\n</html>\n"
                )
            };
            let out =
                PathBuf::from(flag(rest, "-o").unwrap_or_else(|| "out/reference/fw.html".into()));
            let out = if out.is_absolute() {
                out
            } else {
                a.root.join(out)
            };
            if let Some(d) = out.parent() {
                std::fs::create_dir_all(d).map_err(|e| Error(format!("{}: {e}", d.display())))?;
            }
            write(&out.to_string_lossy(), page.as_bytes())?;
            println!("{}", out.display());
        }
        "explain" => {
            let fwp = flag(rest, "--firmware").ok_or_else(|| Error(USAGE.into()))?;
            let raw = read(&fwp)?;
            let reg = registry(&a.root)?;
            let lib = library(&a.root, rest)?;
            let plan_path = flag(rest, "--plan");
            let mut ids: Vec<String> = flag(rest, "--modules")
                .unwrap_or_default()
                .split(',')
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect();
            let old_plan = match &plan_path {
                Some(p) => Some(serde_json::from_slice::<serde_json::Value>(&read(p)?)?),
                None => None,
            };
            if ids.is_empty()
                && let Some(p) = &old_plan
            {
                ids = p["modules"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|m| m.as_str())
                    .map(|m| m.split('@').next().unwrap().to_string())
                    .collect();
            }
            let ids = with_requires(&lib, ids)?;
            let out = apply::apply(&raw, &reg, &lib, &ids)?;
            print!("{}", patchkit::explain::explain(&raw, &out)?);
            if let Some(p) = old_plan {
                let now = serde_json::to_value(&out.plan)?;
                println!(
                    "
plan.json {}",
                    if p == now {
                        "matches this patch exactly"
                    } else {
                        "DIFFERS from this patch"
                    }
                );
                if p != now {
                    std::process::exit(1);
                }
            }
        }
        "verify" => {
            let raw = read(need(rest)?)?;
            let reg = registry(&a.root)?;
            let base = match flag(rest, "--base") {
                Some(b) => Some(read(&b)?),
                None => None,
            };
            let rep = verify::verify(&raw, Some(&reg), base.as_deref())?;
            for c in &rep.checks {
                println!("ok  {c}");
            }
            match &rep.record {
                Some(r) => println!(
                    "ok  record: base {} modules {}",
                    r.base_sha256,
                    r.modules.join(", ")
                ),
                None => println!("--  no patch record (stock image)"),
            }
            println!(
                "verified {} ({} bytes container)",
                rep.device, rep.container_bytes
            );
        }
        "preflight" => {
            let path = need(rest)?;
            let raw = read(path)?;
            let reg = registry(&a.root)?;
            let known = preflight_known(&a.root)?;
            let fwdir = a.root.join("firmware");
            let stock = |k: &patchkit::preflight::Known| -> Option<Vec<u8>> {
                let mut files: Vec<PathBuf> = std::fs::read_dir(&fwdir)
                    .ok()?
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.extension().is_some_and(|e| e == "syx"))
                    .collect();
                files.sort();
                // the file name is a hint; the sha256 decides
                files.sort_by_key(|p| {
                    p.file_name()
                        .is_none_or(|n| n.to_string_lossy() != k.filename)
                });
                files.into_iter().find_map(|p| {
                    let b = std::fs::read(p).ok()?;
                    (sha256_hex(&b) == k.sha256).then_some(b)
                })
            };
            let out = patchkit::preflight::preflight(&raw, &reg, &known, &stock);
            for l in &out.lines {
                println!("{}  {}", if l.ok { "ok  " } else { "FAIL" }, l.text);
            }
            if !out.passed() {
                println!("PREFLIGHT FAILED: do not flash {path}");
                std::process::exit(1);
            }
            println!("PREFLIGHT OK: {path}");
        }
        "isa-scan" => {
            let fw = Firmware::parse(&read(need(rest)?)?)?;
            let dsp = fw
                .section(7)
                .ok_or_else(|| Error("the image has no DSP section (7)".into()))?;
            if rest.iter().any(|a| a == "--list") {
                // every scanned instruction (comparison aid)
                for l in patchkit::dspscan::list(&dsp.data)? {
                    println!("{l}");
                }
                return Ok(());
            }
            let found = patchkit::dspscan::scan(&dsp.data)?;
            for f in &found {
                println!("{f}");
            }
            println!("{} instruction(s)", found.len());
            if !found.is_empty() {
                std::process::exit(1);
            }
        }
        "rebuild" => {
            let fw = Firmware::parse(&read(need(rest)?)?)?;
            let o = flag(rest, "-o").ok_or_else(|| Error(USAGE.into()))?;
            write(&o, &fw.build(None)?)?;
        }
        "pack" => {
            let id = need(rest)?;
            let out = flag(rest, "-o").ok_or_else(|| Error(USAGE.into()))?;
            // builds what is stale (the objects of this module only) and packs the objects as they are in memory
            let lib = selected_library(
                &a.root,
                rest,
                false,
                std::slice::from_ref(id),
                std::slice::from_ref(id),
            )?;
            let m = lib
                .get(id)
                .ok_or_else(|| Error(format!("no module {id}")))?;
            let ui_preview = rest.iter().any(|a| a == "--ui-preview");
            if ui_preview {
                check_ui_preview(m)?;
            }
            let cf: Vec<_> = m
                .objects
                .iter()
                .zip(&m.manifest.object)
                .filter(|(o, _)| o.arch == "cf")
                .collect();
            let dynamic = m.manifest.module.dynamic;
            if cf.len() > 1 || (cf.is_empty() && !dynamic) {
                return Err(Error(format!(
                    "{id} has {} ColdFire objects; a .dkm packs exactly one",
                    cf.len()
                )));
            }
            let mut opts = patchkit::dkm::Options {
                name: flag(rest, "--name").unwrap_or_else(|| id.clone()),
                version: patchkit::dkm::parse_version(&m.manifest.module.version)?,
                runtime_min: match flag(rest, "--runtime-min") {
                    Some(v) => v
                        .parse()
                        .map_err(|_| Error("--runtime-min: a number".into()))?,
                    None => patchkit::dkm::runtime_version(),
                },
                trace: rest.iter().any(|a| a == "--trace"),
                stereo: false,
                api_minor: {
                    let mut newest = 0;
                    for name in &m.manifest.module.uses_api {
                        newest = newest.max(patchkit::sdkgen::api_since(name)?);
                    }
                    newest
                },
            };
            // a run-time module: its typed sections, lowered for one device, are the `register` records
            let mut dsp: Vec<u8> = vec![];
            let reg = if dynamic {
                let dev = flag(rest, "--device").ok_or_else(|| {
                    Error(format!(
                        "a run-time module needs --device {}",
                        patchkit::devdata::short_list()
                    ))
                })?;
                // `ports` is the only device restriction of a module
                let ports = &m.manifest.module.ports;
                if !ports.is_empty()
                    && !ports
                        .iter()
                        .any(|p| p.split('-').next() == Some(dev.as_str()))
                {
                    return Err(Error(format!(
                        "{id} is not built for {dev}: its ports are {}",
                        ports.join(", ")
                    )));
                }
                let models = patchkit::typed::Models::from_dir(&a.root.join("devices"))?;
                let res = patchkit::typed::resolve(&m.manifest, &models)?;
                // the DSP object of the device, if any: its voices are the `[[voice]]` entries (type from the machine)
                let sharc: Vec<_> = m
                    .objects
                    .iter()
                    .zip(&m.manifest.object)
                    .filter(|(o, r)| {
                        o.arch == "sharc"
                            && (r.devices.is_empty() || r.devices.iter().any(|d| *d == dev))
                    })
                    .collect();
                if sharc.len() > 1 {
                    return Err(Error(format!(
                        "{id} has {} DSP objects for {dev}; a .dkm packs one",
                        sharc.len()
                    )));
                }
                let resolved = res.iter().find(|r| r.dev == dev);
                let mut voices = vec![];
                for v in resolved.map(|r| r.voices.as_slice()).unwrap_or_default() {
                    let t = patchkit::register::machine_key(id, &v.machine);
                    let ranges = resolved
                        .map(|r| patchkit::typed::voice_ranges(r, &v.machine))
                        .unwrap_or([0; 8]);
                    voices.push(patchkit::dkmdsp::VoiceSpec {
                        type_no: t,
                        render: v.entry.clone(),
                        note_on: v.note_on.clone(),
                        desc: v.desc.clone(),
                        stereo: v.stereo,
                        latched: v.latched,
                        ranges,
                    });
                }
                // machine contract 1.2 only for a stereo machine: a mono one stays loadable by a runtime of 1.1
                opts.stereo = voices.iter().any(|v| v.stereo);
                // the effects of the device (the `DK_FX` records of the ColdFire object): process routine and descriptor in the DSP object;
                // a page's knobs set the first words of the row
                let all_fx = patchkit::desc::module_fx(m)?;
                let dev_fx: Vec<_> = all_fx
                    .iter()
                    .filter(|f| f.devices.is_empty() || f.devices.iter().any(|d| *d == dev))
                    .collect();
                let mut fxs: Vec<patchkit::dkmdsp::FxSpec> = vec![];
                let mut pages = vec![];
                for (i, f) in dev_fx.iter().enumerate() {
                    let (routes, route) = patchkit::register::fx_routes(id, f)?;
                    fxs.push(patchkit::dkmdsp::FxSpec {
                        process: f.process.clone(),
                        desc: f.desc.clone(),
                        defaults: patchkit::register::fx_defaults(id, f)?,
                        routes,
                        route,
                    });
                    if let Some(p) = patchkit::register::effect_page(id, i, f)? {
                        pages.push(p);
                    }
                }
                match sharc.first().filter(|_| !ui_preview) {
                    Some((o, _)) => {
                        dsp = patchkit::dkmdsp::pack_with_samples(
                            o,
                            patchkit::register::device_id(&dev)?,
                            &voices,
                            &fxs,
                            m.manifest.sample_bank.as_ref(),
                            m.manifest.sample_tracks.as_ref(),
                        )?
                    }
                    None if !ui_preview
                        && (!voices.is_empty()
                            || !fxs.is_empty()
                            || m.manifest.sample_bank.is_some()
                            || m.manifest.sample_tracks.is_some()) =>
                    {
                        return Err(Error(format!(
                            "{id}: a voice or an effect needs a DSP object for {dev}"
                        )));
                    }
                    None => {}
                }
                let mut regs = match resolved {
                    Some(r) => Some(patchkit::register::Register::lower(r, id)?),
                    None if !dsp.is_empty() => None,
                    None => return Err(Error(format!("{id}: nothing for device {dev}"))),
                };
                // the click callbacks of the parameters (`DK_PARAM_ACTION`): their offsets in the image of the object that is packed
                if let (Some(r), Some(reg)) = (resolved, regs.as_mut())
                    && (r.params.iter().any(|p| p.action.is_some()) || !r.native_pages.is_empty())
                {
                    let obj = cf
                        .first()
                        .and_then(|c| patchkit::desc::strip(c.0))
                        .ok_or_else(|| {
                            Error(format!("{id}: a click action needs a ColdFire object"))
                        })?;
                    reg.set_actions(r, &patchkit::dkm::image_symbols(&obj)?)?;
                    for page in &r.native_pages {
                        for callback in [&page.draw, &page.event] {
                            if !obj.symbols.iter().any(|symbol| {
                                symbol.global
                                    && &symbol.name == callback
                                    && symbol.section.as_ref().is_some_and(|section| {
                                        obj.sections
                                            .iter()
                                            .any(|s| &s.name == section && s.kind == "text")
                                    })
                            }) {
                                return Err(Error(format!(
                                    "{id}: native page callback {callback} must be global text"
                                )));
                            }
                        }
                    }
                    reg.set_native_pages(r, id, &patchkit::dkm::image_symbols(&obj)?)?;
                }
                if !pages.is_empty() {
                    let r = regs.get_or_insert_with(|| patchkit::register::Register {
                        device: patchkit::register::device_id(&dev).unwrap_or(0),
                        ..Default::default()
                    });
                    r.effects = pages;
                    r.check()?;
                }
                regs.map(|r| r.encode()).unwrap_or_default()
            } else {
                vec![]
            };
            // the descriptor records stay behind: the object of the .dkm has no `.dk.desc` section
            let cf_obj = cf
                .first()
                .filter(|_| !ui_preview)
                .and_then(|c| patchkit::desc::strip(c.0));
            let file = patchkit::dkm::pack_full(cf_obj.as_ref(), &reg, &dsp, &opts)?;
            write(&out, &file)?;
            let d = patchkit::dkm::Dkm::parse(&file)?;
            println!(
                "{out}: {} bytes, {} (image {} + bss {}, {} relocations, {} imports), sha256 {}",
                file.len(),
                d.name,
                d.image.len(),
                d.bss,
                d.relocs.len(),
                d.imports.len(),
                d.sha256
            );
            println!("size     {out}: {}", d.size_summary(file.len()));
            if ui_preview {
                println!(
                    "UI preview only: no module DSP or ColdFire hooks; use only in an isolated zero-DSP emulator run"
                );
            }
        }
        "desc" => {
            let id = need(rest)?;
            // Only this module's compiled declarations are needed. Unrelated content
            // may have stale or unbuildable sources, just as when packing one module.
            let lib = library_with(&a.root, rest, false, Some(std::slice::from_ref(id)))?;
            let m = lib
                .get(id)
                .ok_or_else(|| Error(format!("no module {id}")))?;
            let fx = patchkit::desc::module_fx(m)?;
            let mf = &m.manifest;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "fx": fx,
                    "machines": mf.machine,
                    "params": mf.param,
                    "pages": mf.page,
                    "native_pages": mf.native_page,
                    "voices": mf.voice,
                    "sample_bank": mf.sample_bank,
                    "sample_tracks": mf.sample_tracks,
                }))?
            );
        }
        "dkm-dsp-image" => {
            // the DSP image of a .dkm placed at the L2 byte address BASE, imports as NAME=VALUE: the reference loader
            let d = patchkit::dkm::Dkm::parse(&read(need(rest)?)?)?;
            let base = rest
                .get(1)
                .and_then(|b| u32::from_str_radix(b.trim_start_matches("0x"), 16).ok())
                .ok_or_else(|| Error("dkm-dsp-image FILE BASE(hex) [NAME=VALUE(hex)..]".into()))?;
            let imports: std::collections::BTreeMap<String, u32> = rest[2..]
                .iter()
                .filter_map(|a| a.split_once('='))
                .filter_map(|(k, v)| {
                    Some((
                        k.to_string(),
                        u32::from_str_radix(v.trim_start_matches("0x"), 16).ok()?,
                    ))
                })
                .collect();
            let x = patchkit::dkmdsp::DspSection::parse(&d.dsp)?;
            let img = x.relocated(base, &|n| imports.get(n).copied())?;
            println!("{}", patchkit::error::hex(&img));
        }
        "synth-dkm" => {
            let spec =
                patchkit::dkmsynth::Spec::parse(&String::from_utf8_lossy(&read(need(rest)?)?))?;
            let dev = flag(rest, "--device").ok_or_else(|| Error(USAGE.into()))?;
            let out = flag(rest, "-o").ok_or_else(|| Error(USAGE.into()))?;
            let models = patchkit::typed::Models::from_dir(&a.root.join("devices"))?;
            write(&out, &patchkit::dkmsynth::synth(&spec, &dev, &models)?)?;
        }
        "needs" => {
            let json = rest.iter().any(|a| a == "--json");
            let files: Vec<&String> = rest.iter().filter(|a| *a != "--json").collect();
            if files.is_empty() {
                return Err(Error(USAGE.into()));
            }
            let mut out = serde_json::Map::new();
            let mut failed = false;
            for f in files {
                let raw = if f == "-" {
                    let mut b = vec![];
                    std::io::Read::read_to_end(&mut std::io::stdin(), &mut b)
                        .map_err(|e| Error(format!("stdin: {e}")))?;
                    b
                } else {
                    read(f)?
                };
                let v = patchkit::dkm::needs_json(&raw).unwrap_or_else(|e| {
                    failed = true;
                    serde_json::json!({ "error": e.0 })
                });
                if !json {
                    match patchkit::dkm::needs(&raw) {
                        Ok(n) => println!(
                            "{f}: file_size {}, {}",
                            n.file_size,
                            n.fields()
                                .iter()
                                .map(|(k, v)| format!("{} {v}", k.to_lowercase()))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                        Err(e) => println!("{f}: no needs: {}", e.0),
                    }
                }
                out.insert(f.clone(), v);
            }
            if json {
                println!("{}", serde_json::Value::Object(out));
            }
            if failed {
                std::process::exit(1);
            }
        }
        "fit" => {
            // the fit rule for the device tools: the request on stdin (`-`), the answer as JSON
            if !rest.iter().any(|a| a == "--json") || !rest.iter().any(|a| a == "-") {
                return Err(Error(USAGE.into()));
            }
            let mut req = vec![];
            std::io::Read::read_to_end(&mut std::io::stdin(), &mut req)
                .map_err(|e| Error(format!("stdin: {e}")))?;
            println!("{}", patchkit::fit::fit_json(&req)?);
        }
        "dkm-info" => {
            let raw = read(need(rest)?)?;
            let file_len = raw.len();
            let d = patchkit::dkm::Dkm::parse(&raw)?;
            println!("name      {}", d.name);
            println!(
                "version   {}.{}.{}",
                d.version >> 16,
                d.version >> 8 & 255,
                d.version & 255
            );
            println!("runtime   >= {}", patchkit::dkm::fmt_runtime(d.runtime_min));
            println!(
                "trace     {}",
                if d.trace {
                    "yes (built with DK_TRACE)"
                } else {
                    "no"
                }
            );
            println!("sha256    {}", d.sha256);
            println!("size      {}", d.size_summary(file_len));
            println!(
                "image     {} bytes, bss {}, align {}",
                d.image.len(),
                d.bss,
                d.align
            );
            match patchkit::dkm::needs(&raw) {
                Ok(n) => println!(
                    "needs     {}",
                    n.fields()
                        .iter()
                        .map(|(n, v)| format!("{} {v}", n.to_lowercase()))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                Err(e) => println!("needs     none ({})", e.0),
            }
            println!("relocs    {}", d.relocs.len());
            println!("imports   {}", d.imports.join(", "));
            println!("init      {:?}  fini {:?}", d.init, d.fini);
            println!("slots     {:?}", d.slots);
            println!("register  {} bytes", d.register.len());
            println!("dsp       {} bytes", d.dsp.len());
            if !d.dsp.is_empty() {
                let x = patchkit::dkmdsp::DspSection::parse(&d.dsp)?;
                println!(
                    "  device {} image {} bytes, bss {}, align {}, {} relocations, imports {}",
                    x.device,
                    x.image.len(),
                    x.bss,
                    x.align,
                    x.relocs.len(),
                    x.imports.join(", ")
                );
                for v in &x.voices {
                    println!(
                        "  voice type {} entry +{:#x} note_on +{:#x} state {}x{} clear {} ranges {:x?}",
                        patchkit::register::fmt_type(v.type_no),
                        v.entry,
                        v.note_on,
                        v.voices,
                        v.state_bytes,
                        v.clear_bytes,
                        v.ranges
                    );
                }
                for e in &x.fxs {
                    println!(
                        "  effect process +{:#x} desc +{:#x} state {} params {} defaults {:x?}",
                        e.process,
                        e.desc,
                        e.state_bytes,
                        e.n_params,
                        &e.defaults[..e.n_params as usize]
                    );
                }
            }
            if !d.register.is_empty() {
                let r = patchkit::register::Register::parse(&d.register)?;
                println!(
                    "  device {} ({} params, {} machines, {} pages)",
                    r.device,
                    r.params.len(),
                    r.machines.len(),
                    r.pages.len()
                );
                for p in &r.params {
                    println!(
                        "  param {} {:?} {:?} template {:#x} slot {} range {}..{} default {}",
                        p.index, p.name, p.short, p.template, p.slot, p.min, p.max, p.default
                    );
                }
                for m in &r.machines {
                    println!(
                        "  machine type {} {:?} {:?} clone_of {} level {} own_dsp {} knobs {:x?}",
                        patchkit::register::fmt_type(m.type_no),
                        m.name,
                        m.short,
                        m.clone_of,
                        m.level,
                        m.own_dsp,
                        m.knobs
                    );
                }
                for g in &r.pages {
                    println!(
                        "  page of type {} {:?} {:?} level {} knobs {:x?}",
                        patchkit::register::fmt_type(g.type_no),
                        g.name,
                        g.title,
                        g.level,
                        g.knobs
                    );
                }
            }
        }
        "pack-section" => {
            let data = read(need(rest)?)?;
            let o = rest.get(1).ok_or_else(|| Error(USAGE.into()))?;
            write(o, &patchkit::aplib::pack_section(&data))?;
        }
        _ => {
            eprint!("{USAGE}");
            std::process::exit(2);
        }
    }
    Ok(())
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut given = None;
    if args.first().is_some_and(|a| a == "--root") && args.len() >= 2 {
        given = Some(args[1].clone());
        args.drain(0..2);
    }
    let a = Args {
        root: find_root(given),
        rest: args,
    };
    if let Err(e) = run(a) {
        eprintln!("patchkit: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use patchkit::build::{cache_dir, toolchain::Env};

    fn copy_tree(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let dest = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_tree(&entry.path(), &dest);
            } else {
                std::fs::copy(entry.path(), dest).unwrap();
            }
        }
    }

    #[test]
    fn ui_preview_preserves_machine_registration_without_module_code() {
        let real = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let output = ApplyRoot::new("ui-preview-pack");
        for dev in ["dt2", "dn2"] {
            let pack = |preview: bool| {
                let path = output.0.join(format!("{dev}-{preview}.dkm"));
                let mut rest: Vec<String> = vec![
                    "pack".into(),
                    "test/sine".into(),
                    "--device".into(),
                    dev.into(),
                    "-o".into(),
                    path.display().to_string(),
                    "--manifest".into(),
                    real.join("tests/fixtures/modules/sine/module.toml")
                        .display()
                        .to_string(),
                ];
                if preview {
                    rest.push("--ui-preview".into());
                }
                run(Args {
                    root: real.clone(),
                    rest,
                })
                .unwrap();
                patchkit::dkm::Dkm::parse(&std::fs::read(path).unwrap()).unwrap()
            };
            let full = pack(false);
            let preview = pack(true);
            assert!(!full.dsp.is_empty());
            assert_eq!(preview.register, full.register);
            let reg = patchkit::register::Register::parse(&preview.register).unwrap();
            assert_eq!(
                (reg.machines.len(), reg.params.len(), reg.pages.len()),
                (1, 1, usize::from(dev == "dn2"))
            );
            assert!(preview.dsp.is_empty());
            assert_eq!(preview.image, [0x70, 0x00, 0x4e, 0x75]);
            assert_eq!(preview.bss, 0);
            assert_eq!(preview.init, Some(0));
            assert!(preview.fini.is_none() && preview.menu.is_none());
            assert!(
                preview.slots.is_empty() && preview.relocs.is_empty() && preview.imports.is_empty()
            );
        }
    }

    #[test]
    fn ui_preview_refuses_declarations_and_cf_content_it_cannot_preserve() {
        let real = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let id = "test/sine".to_string();
        let lib = selected_library(
            &real,
            &[],
            false,
            std::slice::from_ref(&id),
            std::slice::from_ref(&id),
        )
        .unwrap();
        let machine = lib.get(&id).unwrap();
        check_ui_preview(machine).unwrap();
        let rejected = |m: &patchkit::module::Module, message: &str| {
            let err = check_ui_preview(m).unwrap_err().0;
            assert!(err.contains(message), "{err}");
        };
        let mut m = machine.clone();
        m.manifest.module.kind = patchkit::module::ModuleKind::Fx;
        rejected(&m, "requires a declarative run-time machine");
        let mut m = machine.clone();
        m.manifest.module.dynamic = false;
        rejected(&m, "requires a declarative run-time machine");
        let mut m = machine.clone();
        m.manifest.sample_bank = Some(patchkit::typed::SampleBankDef {
            local_id: 0,
            bank: "bank".into(),
            descriptor: "desc".into(),
            capacity_frames: 32,
        });
        rejected(&m, "sample banks or sample tracks");
        let mut m = machine.clone();
        m.manifest.sample_tracks = Some(patchkit::typed::SampleTracksDef {
            local_id: 0,
            records: "records".into(),
            control: "control".into(),
        });
        rejected(&m, "sample banks or sample tracks");
        let mut m = machine.clone();
        m.manifest.param[0].action = Some("click".into());
        rejected(&m, "parameter click actions");
        // Even anonymous/static code and state must not be stripped silently.
        for (name, kind, hex) in [
            (".text", "text", "4e75"),
            (".data", "data", "0000"),
            (".bss", "bss", ""),
        ] {
            let mut m = machine.clone();
            m.objects
                .iter_mut()
                .find(|o| o.arch == "cf")
                .unwrap()
                .sections
                .push(patchkit::dko::DkoSection {
                    name: name.into(),
                    kind: kind.into(),
                    size: 2,
                    align: 2,
                    hex: hex.into(),
                });
            rejected(&m, "requires descriptor-only ColdFire objects");
        }
    }

    #[test]
    fn scoped_generation_checks_detect_stale_outputs_without_writing() {
        let fixture = ApplyRoot::new("generation");
        // Replace the build fixture's SDK link with a private copy: stale checks never touch the repository.
        std::fs::remove_file(fixture.0.join("sdk")).unwrap();
        let real = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for dir in ["sdk", "devices", "modules/core/adapters/machines"] {
            copy_tree(&real.join(dir), &fixture.0.join(dir));
        }
        std::fs::create_dir_all(fixture.0.join("modules/core/runtime")).unwrap();
        std::fs::copy(
            real.join(patchkit::abigen::SOURCE),
            fixture.0.join(patchkit::abigen::SOURCE),
        )
        .unwrap();
        let module_dir = fixture.0.join("modules/fixture");
        std::fs::create_dir_all(&module_dir).unwrap();
        std::fs::write(
            module_dir.join("module.toml"),
            r#"
[module]
id = "test/generation"
version = "0.1.0"
name = "Generation fixture"
category = "test"
type = "raw"
author = "test"
status = "experimental"
summary = "Scoped generation check"
[[gen]]
kind = "gas"
path = "build/gen/device-{dev}.inc"
"#,
        )
        .unwrap();
        let args = |scope: &str| Args {
            root: fixture.0.clone(),
            rest: [
                "gen",
                "--check",
                "--scope",
                scope,
                "--modules",
                "test/generation",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
        };
        let lib = library_with(&fixture.0, &[], true, Some(&[])).unwrap();
        let facts = patchkit::devgen::Facts::from_dir(&fixture.0.join("devices")).unwrap();
        let module_files = patchkit::devgen::generate(&lib, &facts, "test/generation").unwrap();
        assert!(!module_files.is_empty());
        let mut files = patchkit::sdkgen::generate(&fixture.0).unwrap();
        files.extend(patchkit::fwgen::generate_with_ports(&fixture.0).unwrap());
        files.extend(
            module_files
                .into_iter()
                .map(|(p, t)| (format!("modules/fixture/{p}"), t)),
        );
        for (rel, text) in &files {
            let path = fixture.0.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        let snapshot = || {
            files
                .iter()
                .map(|(rel, _)| {
                    let path = fixture.0.join(rel);
                    (
                        std::fs::read(&path).unwrap(),
                        std::fs::metadata(path).unwrap().modified().unwrap(),
                    )
                })
                .collect::<Vec<_>>()
        };
        let baseline = snapshot();
        let explicit_args = || {
            let mut a = args("modules");
            for path in [
                module_dir.join("module.toml"),
                fixture.0.join("modules/core/adapters/machines/module.toml"),
            ] {
                a.rest
                    .extend(["--manifest".into(), path.display().to_string()]);
            }
            a
        };
        let mut missing_alloc = args("modules");
        missing_alloc.rest.extend([
            "--manifest".into(),
            module_dir.join("module.toml").display().to_string(),
        ]);
        assert!(run(missing_alloc).unwrap_err().0.contains("core/machines"));
        run(explicit_args()).unwrap();
        assert_eq!(baseline, snapshot());
        let unrelated = fixture.0.join("modules/unrelated/module.toml");
        std::fs::create_dir_all(unrelated.parent().unwrap()).unwrap();
        std::fs::write(&unrelated, "malformed = [").unwrap();
        run(explicit_args()).unwrap();
        assert!(run(args("modules")).is_err());
        std::fs::remove_file(unrelated).unwrap();
        for scope in ["shared", "modules", "all"] {
            run(args(scope)).unwrap();
        }
        let default_args = || {
            let mut a = args("all");
            a.rest.drain(2..4);
            a
        };
        run(default_args()).unwrap();
        assert_eq!(baseline, snapshot());
        for (rel, scope, other) in [
            (patchkit::sdkgen::RUST_OUT, "shared", "modules"),
            (patchkit::fwgen::RUST_OUT, "shared", "modules"),
            (
                "modules/fixture/build/gen/device-dt2.inc",
                "modules",
                "shared",
            ),
        ] {
            let path = fixture.0.join(rel);
            let original = std::fs::read(&path).unwrap();
            std::fs::write(&path, "stale fixture\n").unwrap();
            let stale = snapshot();
            assert!(
                run(args(scope))
                    .unwrap_err()
                    .0
                    .contains("generated file(s) are stale")
            );
            run(args(other)).unwrap();
            assert!(run(args("all")).is_err());
            assert!(run(default_args()).is_err());
            assert_eq!(stale, snapshot(), "check rewrote {rel}");
            std::fs::write(path, original).unwrap();
        }
        for rest in [
            vec!["gen", "--scope", "shared"],
            vec!["gen", "--check", "--scope", "bogus"],
            vec!["gen", "--check", "--scope"],
        ] {
            assert!(
                run(Args {
                    root: fixture.0.clone(),
                    rest: rest.into_iter().map(String::from).collect()
                })
                .unwrap_err()
                .0
                .contains("gen --scope requires")
            );
        }
    }

    struct ApplyRoot(PathBuf);

    impl ApplyRoot {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("pk-apply-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("out")).unwrap();
            std::fs::create_dir_all(dir.join("devices")).unwrap();
            let real = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
            std::os::unix::fs::symlink(Env::new(&real).tools, dir.join("out/tools")).unwrap();
            std::os::unix::fs::symlink(real.join("sdk"), dir.join("sdk")).unwrap();
            Self(dir)
        }

        fn module(&self, base: &str, id: &str, requires: &[&str], source: bool) {
            let dir = self.0.join(base).join(id);
            std::fs::create_dir_all(dir.join("src")).unwrap();
            std::fs::write(
                dir.join("module.toml"),
                format!(
                    "[module]\nid = {id:?}\nversion = \"0.1.0\"\nname = \"fixture\"\ncategory = \"test\"\ntype = \"raw\"\nauthor = \"test\"\nstatus = \"experimental\"\nsummary = \"apply selection fixture\"\nrequires = {requires:?}\n\n[[object]]\npath = \"build/tiny.dko.json\"\nplace = \"cf.expansion\"\nsources = [\"src/t.c\"]\n"
                ),
            )
            .unwrap();
            if source {
                std::fs::write(dir.join("src/t.c"), "int twice(int x) { return x + x; }\n")
                    .unwrap();
            }
        }
    }

    impl Drop for ApplyRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn desc_does_not_build_unrelated_content() {
        let root = ApplyRoot::new("desc-selection");
        root.module("modules", "test/selected", &[], true);
        root.module("tests/fixtures/modules", "test/unrelated", &[], false);
        let describe = |id: &str| {
            run(Args {
                root: root.0.clone(),
                rest: vec!["desc".into(), id.into()],
            })
        };
        describe("test/selected").unwrap();
        // The same missing source still fails when explicitly requested.
        assert!(describe("test/unrelated").is_err());
    }

    #[test]
    fn catalog_check_validates_all_headers_and_requires_without_objects() {
        let root = ApplyRoot::new("catalog");
        root.module("modules", "test/selected", &["test/dependency"], false);
        root.module("tests/fixtures/modules", "test/dependency", &[], false);
        // Device models and declared objects/sources are deliberately unusable: catalog validation never reads them.
        std::fs::write(root.0.join("devices/broken.toml"), "malformed = [").unwrap();
        let check = || Library::catalog_check(&root.0);
        assert_eq!(check().unwrap(), 2);
        run(Args {
            root: root.0.clone(),
            rest: vec!["catalog-check".into()],
        })
        .unwrap();
        assert!(!cache_dir(&root.0).exists());
        let path = root
            .0
            .join("tests/fixtures/modules/test/dependency/module.toml");
        let original = std::fs::read_to_string(&path).unwrap();
        for (text, error) in [
            ("malformed = [".to_string(), "module.toml"),
            (
                original.replace("sources = [\"src/t.c\"]", "sources = false"),
                "invalid type",
            ),
            (
                original.replace("version = \"0.1.0\"\n", ""),
                "missing field `version`",
            ),
            (
                original.replace("test/dependency", "test/selected"),
                "duplicate module id",
            ),
            (
                original.replace("requires = []", "requires = [\"test/missing\"]"),
                "requires test/missing",
            ),
            (
                original.replace("requires = []", "requires = [\"test/selected\"]"),
                "requires cycle",
            ),
            (
                original.replace("category = \"test\"", "category = \"core\""),
                "a core module needs",
            ),
            (format!("{original}\n[[op]]\nop = \"unknown\"\n"), "unknown"),
        ] {
            std::fs::write(&path, text).unwrap();
            assert!(check().unwrap_err().0.contains(error), "expected {error}");
        }
        std::fs::write(path, original).unwrap();
        assert_eq!(check().unwrap(), 2);
        assert!(!cache_dir(&root.0).exists());
    }

    #[test]
    fn explicit_pack_matches_legacy_and_ignores_unrelated_bad_manifest() {
        let root = ApplyRoot::new("explicit-pack");
        root.module("modules", "test/selected", &[], true);
        std::fs::write(
            root.0.join("modules/test/selected/src/t.c"),
            "int dkm_init(const void *api) { return api ? 0 : 1; }\n",
        )
        .unwrap();
        let pack = |out: &str, explicit: bool| {
            let mut rest: Vec<String> = ["pack", "test/selected", "-o", out]
                .into_iter()
                .map(String::from)
                .collect();
            if explicit {
                rest.extend([
                    "--manifest".into(),
                    root.0
                        .join("modules/test/selected/module.toml")
                        .display()
                        .to_string(),
                ]);
            }
            run(Args {
                root: root.0.clone(),
                rest,
            })
        };
        let legacy = root.0.join("legacy.dkm").display().to_string();
        let explicit = root.0.join("explicit.dkm").display().to_string();
        pack(&legacy, false).unwrap();
        root.module("modules", "test/unrelated", &[], false);
        std::fs::write(
            root.0.join("modules/test/unrelated/module.toml"),
            "malformed = [",
        )
        .unwrap();
        assert!(
            run(Args {
                root: root.0.clone(),
                rest: vec!["catalog-check".into()]
            })
            .is_err()
        );
        pack(&explicit, true).unwrap();
        assert_eq!(
            std::fs::read(&legacy).unwrap(),
            std::fs::read(&explicit).unwrap()
        );
        // Selected source/object failures must still fail rather than using a partial module.
        std::fs::remove_file(root.0.join("modules/test/selected/src/t.c")).unwrap();
        assert!(pack(&explicit, true).is_err());
    }

    #[test]
    fn explicit_manifests_validate_selection_and_requirements_before_objects() {
        let root = ApplyRoot::new("explicit-validation");
        root.module("modules", "test/selected", &["test/dependency"], false);
        root.module("modules", "test/dependency", &[], false);
        root.module("modules", "test/unrelated", &[], false);
        let selected = vec!["test/selected".into()];
        let path = |id: &str| root.0.join("modules").join(id).join("module.toml");
        let rest = |ids: &[&str]| {
            ids.iter()
                .flat_map(|id| ["--manifest".to_string(), path(id).display().to_string()])
                .collect::<Vec<_>>()
        };
        let load = |r: &[String], ids: &[String]| selected_library(&root.0, r, false, &[], ids);
        let both = rest(&["test/selected", "test/dependency"]);
        assert!(
            load(&both, &["test/unknown".into()])
                .unwrap_err()
                .0
                .contains("unknown module")
        );
        assert!(
            load(&rest(&["test/selected"]), &selected)
                .unwrap_err()
                .0
                .contains("requires test/dependency")
        );
        std::fs::write(path("test/unrelated"), "malformed = [").unwrap();
        let lib = load(&both, &selected).unwrap();
        assert_eq!(lib.modules.len(), 2);
        root.module("modules", "test/selected", &["test/dependency"], true);
        let object_error =
            selected_library(&root.0, &both, false, &selected, &selected).unwrap_err();
        assert!(object_error.0.contains("src/t.c"));
        assert!(
            load(
                &rest(&["test/selected", "test/dependency", "test/dependency"]),
                &selected
            )
            .unwrap_err()
            .0
            .contains("duplicate module id")
        );
        root.module("modules", "test/dependency", &["test/selected"], false);
        assert!(
            load(&both, &selected)
                .unwrap_err()
                .0
                .contains("requires cycle")
        );
        std::fs::write(path("test/dependency"), "malformed = [").unwrap();
        assert!(load(&both, &selected).is_err());
        std::fs::write(path("test/selected"), "malformed = [").unwrap();
        assert!(load(&rest(&["test/selected"]), &selected).is_err());
        assert!(!cache_dir(&root.0).join("test/dependency").exists());
    }

    #[test]
    fn apply_builds_selected_content_and_requires_but_not_unrelated_sources() {
        let root = ApplyRoot::new("selection");
        assert!(
            Env::new(&root.0).m68k("gcc").is_ok(),
            "this build regression needs the public m68k-elf toolchain"
        );
        root.module("modules", "test/selected", &["test/dependency"], true);
        root.module("extra", "test/dependency", &[], true);
        root.module("modules", "test/unrelated", &[], false);
        let rest = vec![
            "--modules-dir".into(),
            root.0.join("extra").display().to_string(),
        ];
        let (lib, ids) = library_for_apply(&root.0, &rest, vec!["test/selected".into()]).unwrap();
        assert_eq!(ids.len(), 2);
        for id in ["test/selected", "test/dependency"] {
            assert!(ids.iter().any(|selected| selected == id));
            assert_eq!(lib.get(id).unwrap().objects.len(), 1);
            // `<id>/<src12>/build/tiny.dko.json`
            let keys = std::fs::read_dir(cache_dir(&root.0).join(id)).unwrap();
            assert!(
                keys.filter_map(|k| k.ok())
                    .any(|k| k.path().join("build/tiny.dko.json").is_file())
            );
        }
        assert!(lib.get("test/unrelated").unwrap().objects.is_empty());
        assert!(!cache_dir(&root.0).join("test/unrelated").exists());
    }

    #[test]
    fn apply_refuses_missing_requires_before_building_content() {
        let root = ApplyRoot::new("missing");
        root.module("modules", "test/selected", &["test/missing"], false);
        assert!(library_for_apply(&root.0, &[], vec!["test/selected".into()]).is_err());
        assert!(!cache_dir(&root.0).exists());
    }

    #[test]
    fn apply_refuses_requires_cycles_before_building_content() {
        let root = ApplyRoot::new("cycle");
        root.module("modules", "test/selected", &["test/dependency"], false);
        root.module("modules", "test/dependency", &["test/selected"], false);
        assert!(library_for_apply(&root.0, &[], vec!["test/selected".into()]).is_err());
        assert!(!cache_dir(&root.0).exists());
    }
}
