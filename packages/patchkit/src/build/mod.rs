//! `patchkit build`: module sources -> `.dko.json` objects.
//!
//! For every `[[object]]` of a module's `module.toml` that lists `sources`, the sources are compiled by the pinned toolchain (ColdFire
//! C and assembly: m68k-elf gcc/as; ColdFire Rust: a `Cargo.toml` as the only source; SHARC+: Selache as a library), turned into
//! the canonical object JSON and written to the object's `path`. Every instruction is re-decoded by our own decoders, so a toolchain
//! encoding bug cannot reach an image (ColdFire: [`cf::check_isa`]; SHARC+: [`sharc_obj`]); both run in this process.
//!
//! Where an object is written: a `core/*` module (`category = "core"`) keeps its objects in the module (`build/x.dko.json`, committed: the
//! workbench and the browser patch without a toolchain). Every other module (content modules, test fixtures, imported modules) is
//! build output: its objects go to the cache `out/cache/dko/<module id>/<object path>`. [`object_file`] names the file.
//!
//! An object whose `source_sha256` equals the hash of its sources now is not compiled ("unchanged"). `--check` rebuilds and
//! compares with the stored object, ignoring the `*_sha256` keys of the toolchain record (they name the local binaries).
//!
//! Library entry points for the command line and the desktop app: [`build_module`], [`module_status`], [`link_sources`], and
//! [`load_object`], which the module library uses to build a stale object on demand.

pub mod cf;
pub mod elf;
pub mod rust;
pub mod sharc;
pub mod sharc_legal;
pub mod sharc_obj;
pub mod show;
pub mod toolchain;

use crate::dko::Dko;
use crate::error::*;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use toolchain::Env;

#[derive(Clone, Debug, Default)]
pub struct BuildOptions {
    /// The repository root: tools, caches and the shared include directories.
    pub root: PathBuf,
    /// Build nothing; fail when a committed object differs from a fresh build.
    pub check: bool,
    /// Rebuild objects whose source hash is unchanged.
    pub force: bool,
    /// Build options for DSP sources (`#ifdef NAME`). They need `out`.
    pub defines: Vec<String>,
    /// Write the objects under this directory (at their `path`) instead of into the module.
    pub out: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Compiled and written.
    Built,
    /// The committed object carries the hash of the sources: not compiled.
    Unchanged,
    /// `--check`: the committed object equals a fresh build.
    Current,
}

#[derive(Clone, Debug)]
pub struct ObjectReport {
    /// Where the object is (written to, or checked).
    pub path: PathBuf,
    pub status: Status,
    /// ColdFire and SHARC+ instructions re-decoded (a built object).
    pub instructions: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct BuildReport {
    pub objects: Vec<ObjectReport>,
}

impl BuildReport {
    /// One line per object.
    pub fn lines(&self) -> Vec<String> {
        self.objects
            .iter()
            .map(|o| match o.status {
                Status::Unchanged => format!("{}: unchanged", o.path.display()),
                Status::Current => format!("{}: current", o.path.display()),
                Status::Built => format!(
                    "{}: {} instructions re-decoded OK",
                    o.path.display(),
                    o.instructions.unwrap_or(0)
                ),
            })
            .collect()
    }
}

/// The parts of a `module.toml` the build reads.
#[derive(Deserialize)]
pub struct ModuleToml {
    module: ModuleHead,
    #[serde(default)]
    object: Vec<ObjectToml>,
}

#[derive(Deserialize)]
struct ModuleHead {
    id: String,
    category: String,
}

#[derive(Deserialize)]
struct ObjectToml {
    path: String,
    #[serde(default)]
    sources: Vec<String>,
}

impl ModuleToml {
    pub fn parse(text: &str) -> Result<ModuleToml> {
        toml::from_str(text).map_err(|e| err!("module.toml: {e}"))
    }

    pub fn id(&self) -> &str {
        &self.module.id
    }

    /// A content module: its objects are build output (the cache), not committed.
    pub fn is_content(&self) -> bool {
        self.module.category != "core"
    }

    /// Is the object at `path` of a content module built from sources (and so stored in the cache)?
    pub fn builds(&self, path: &str) -> bool {
        self.is_content()
            && self
                .object
                .iter()
                .any(|o| o.path == path && !o.sources.is_empty())
    }
}

fn manifest(moddir: &Path) -> Result<ModuleToml> {
    let p = moddir.join("module.toml");
    ModuleToml::parse(&toolchain::read_text(&p)?).map_err(|e| err!("{}: {e}", p.display()))
}

/// The object cache of a repository: `out/cache/dko`.
pub fn cache_dir(root: &Path) -> PathBuf {
    root.join("out/cache/dko")
}

/// Where the object `o` of a module is stored: in the module for `core/*`, else in the cache.
fn object_file(env: &Env, moddir: &Path, m: &ModuleToml, o: &ObjectToml) -> PathBuf {
    if m.is_content() {
        // a build with defines has its own directory, so objects of different define sets never share a file
        let mut dir = m.module.id.clone();
        for d in &env.defines {
            dir.push('+');
            dir.push_str(d);
        }
        cache_dir(&env.root).join(dir).join(&o.path)
    } else {
        moddir.join(&o.path)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Sharc,
    Rust,
    Cf,
}

fn kind_of(sources: &[String]) -> Kind {
    let paths: Vec<&Path> = sources.iter().map(Path::new).collect();
    if paths
        .iter()
        .all(|p| p.extension().is_some_and(|e| e == "s"))
        && paths
            .iter()
            .any(|p| p.components().any(|c| c.as_os_str() == "dsp"))
    {
        Kind::Sharc
    } else if paths
        .iter()
        .any(|p| p.file_name().is_some_and(|n| n == "Cargo.toml"))
    {
        Kind::Rust
    } else {
        Kind::Cf
    }
}

/// The include directories of a ColdFire C or assembly object of the module.
pub fn cf_include_dirs(root: &Path, moddir: &Path) -> Vec<PathBuf> {
    let mut v = vec![moddir.join("build/gen"), moddir.join("src")];
    v.extend(
        toolchain::global_include_dirs(true)
            .iter()
            .map(|d| root.join(d)),
    );
    v
}

/// The `source_sha256` a build of the object would write, computed the same way as the builders do and without compiling.
fn source_hash(env: &Env, moddir: &Path, sources: &[String]) -> Result<String> {
    let paths: Vec<PathBuf> = sources.iter().map(|s| moddir.join(s)).collect();
    match kind_of(sources) {
        Kind::Sharc => {
            let text = sharc::source_text(&env.root, moddir, &paths, &env.defines)?;
            Ok(sharc::source_hash(&text))
        }
        Kind::Rust => rust::source_sha256(&paths[0]),
        Kind::Cf => cf::source_digest(env, &paths, &cf_include_dirs(&env.root, moddir)),
    }
}

fn committed_hash(path: &Path) -> Option<String> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    v.get("source_sha256")?.as_str().map(String::from)
}

fn object_unchanged(env: &Env, moddir: &Path, m: &ModuleToml, o: &ObjectToml) -> bool {
    let out = object_file(env, moddir, m, o);
    match source_hash(env, moddir, &o.sources) {
        Ok(want) => committed_hash(&out).is_some_and(|h| h == want),
        Err(_) => false,
    }
}

/// Is the stored object of every object with sources current with its sources? One entry per such object: (its path, unchanged).
pub fn module_status(root: &Path, moddir: &Path) -> Result<Vec<(String, bool)>> {
    let env = Env::new(root);
    let m = manifest(moddir)?;
    Ok(m.object
        .iter()
        .filter(|o| !o.sources.is_empty())
        .map(|o| (o.path.clone(), object_unchanged(&env, moddir, &m, o)))
        .collect())
}

/// Canonical JSON: keys sorted (the map of `serde_json::Value` is ordered), one space of indent per level, a newline at the end.
pub fn pretty(v: &Value) -> Result<String> {
    use serde::Serialize;
    let mut buf = Vec::new();
    let fmt = serde_json::ser::PrettyFormatter::with_indent(b" ");
    v.serialize(&mut serde_json::Serializer::with_formatter(&mut buf, fmt))?;
    buf.push(b'\n');
    String::from_utf8(buf).map_err(|e| err!("{e}"))
}

/// An object as the canonical JSON text.
pub fn to_text(o: &Dko) -> Result<String> {
    pretty(&serde_json::to_value(o)?)
}

/// The committed object with the local binary hashes of the toolchain record left out.
pub fn comparable(mut v: Value) -> Value {
    if let Some(Value::Object(t)) = v.get_mut("toolchain") {
        t.retain(|k, _| !k.ends_with("_sha256"));
    }
    v
}

fn build_object(env: &Env, moddir: &Path, o: &ObjectToml, defines: &[String]) -> Result<Dko> {
    let paths: Vec<PathBuf> = o.sources.iter().map(|s| moddir.join(s)).collect();
    match kind_of(&o.sources) {
        Kind::Sharc => {
            let text = sharc::source_text(&env.root, moddir, &paths, defines)?;
            sharc::sharc_object(&text)
        }
        k => {
            if !defines.is_empty() {
                return Err(err!("--define applies to DSP sources only"));
            }
            if k == Kind::Rust {
                // A Rust crate: the manifest is the only source entry
                if paths.len() != 1 {
                    return Err(err!(
                        "a Rust object lists its Cargo.toml as the only source"
                    ));
                }
                let (proto, sha, toolchain) = rust::object_proto(env, &paths[0])?;
                cf::dko_from_proto(&proto, sha, toolchain)
            } else {
                cf::cf_object(env, &paths, &cf_include_dirs(&env.root, moddir))
            }
        }
    }
}

/// Write an object file in one step (a rename), so a reader that runs at the same time never sees half a file.
fn write_object(path: &Path, text: &str) -> Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| err!("{}: {e}", d.display()))?;
    }
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!("tmp-{}-{n}", std::process::id()));
    std::fs::write(&tmp, text).map_err(|e| err!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| err!("{}: {e}", path.display()))
}

/// Read an existing object without hashing sources or starting a build. Generation
/// uses cached declarations to bootstrap headers that the next build may need.
pub fn read_cached_object(env: &Env, moddir: &Path, m: &ModuleToml, rel: &str) -> Result<String> {
    let o = m
        .object
        .iter()
        .find(|o| o.path == rel)
        .ok_or_else(|| err!("{rel}: not an object of the module"))?;
    toolchain::read_text(&object_file(env, moddir, m, o))
}

/// The text of the object `rel` of the content module in `moddir`, for the module library: the stored object when its `source_sha256`
/// is the hash of the sources now, else a fresh build, which is also stored. The error names the missing toolchain when there is one.
pub fn load_object(env: &Env, moddir: &Path, m: &ModuleToml, rel: &str) -> Result<String> {
    // threads of one process (the tests) that need the same object build it once
    static BUILDING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one_at_a_time = BUILDING.lock().unwrap_or_else(|e| e.into_inner());
    let o = m
        .object
        .iter()
        .find(|o| o.path == rel)
        .ok_or_else(|| err!("{rel}: not an object of the module"))?;
    let file = object_file(env, moddir, m, o);
    let want = source_hash(env, moddir, &o.sources)
        .map_err(|e| err!("{}: {rel}: cannot hash the sources: {e}", moddir.display()))?;
    if committed_hash(&file).is_some_and(|h| h == want)
        && let Ok(t) = std::fs::read_to_string(&file)
    {
        return Ok(t);
    }
    // the define set of the env: SHARC+ sources take it as `#ifdef` names, C sources as `-D` (the compiler step reads `env.defines`)
    let defines = if kind_of(&o.sources) == Kind::Sharc {
        env.defines.clone()
    } else {
        vec![]
    };
    let obj = build_object(env, moddir, o, &defines)
        .map_err(|e| err!("{}: {rel}: build failed: {e}", moddir.display()))?;
    let text = to_text(&obj)?;
    write_object(&file, &text)?;
    Ok(text)
}

/// Build every object of a module that lists sources. With `defines` (build options for the DSP sources) the stored objects are not
/// touched: the objects go under `out` at their `path`.
pub fn build_module(moddir: &Path, opts: &BuildOptions) -> Result<BuildReport> {
    if !opts.defines.is_empty() && (opts.check || opts.out.is_none()) {
        return Err(err!(
            "--define needs --out and cannot be combined with --check"
        ));
    }
    let env = Env::new(&opts.root);
    let m = manifest(moddir)?;
    let mut report = BuildReport::default();
    for o in m.object.iter().filter(|o| !o.sources.is_empty()) {
        let stored = object_file(&env, moddir, &m, o);
        if !opts.check
            && opts.defines.is_empty()
            && !opts.force
            && object_unchanged(&env, moddir, &m, o)
        {
            report.objects.push(ObjectReport {
                path: stored,
                status: Status::Unchanged,
                instructions: None,
            });
            continue;
        }
        let obj = build_object(&env, moddir, o, &opts.defines)?;
        let out = match &opts.out {
            Some(d) => d.join(&o.path),
            None => stored,
        };
        let n = obj
            .toolchain
            .get("instructions_redecoded")
            .and_then(|s| s.parse().ok());
        if opts.check {
            let current = match std::fs::read_to_string(&out)
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            {
                Some(v) => comparable(v) == comparable(serde_json::to_value(&obj)?),
                None => false,
            };
            if !current {
                return Err(err!(
                    "{} is stale: mise run module:build -- {}",
                    out.display(),
                    moddir.display()
                ));
            }
            report.objects.push(ObjectReport {
                path: out,
                status: Status::Current,
                instructions: n,
            });
        } else {
            write_object(&out, &to_text(&obj)?)?;
            report.objects.push(ObjectReport {
                path: out,
                status: Status::Built,
                instructions: n,
            });
        }
    }
    Ok(report)
}

/// The directories of the modules `ids` name, in the order of `ids`; an id no module root has is an error.
pub fn module_dirs(root: &Path, ids: &[String]) -> Result<Vec<(String, PathBuf)>> {
    let mut found: HashMap<String, PathBuf> = HashMap::new();
    for d in all_modules(root, None) {
        if let Ok(t) = std::fs::read_to_string(d.join("module.toml"))
            && let Ok(m) = ModuleToml::parse(&t)
            && ids.iter().any(|i| i == m.id())
        {
            found.insert(m.id().to_string(), d);
        }
    }
    ids.iter()
        .map(|i| {
            found
                .remove(i)
                .map(|d| (i.clone(), d))
                .ok_or_else(|| err!("unknown module {i:?}"))
        })
        .collect()
}

/// The module roots of a repository (`modules/`, `tests/fixtures/modules/`) and the extra roots of PATCHKIT_MODULES.
pub fn module_roots(root: &Path) -> Vec<PathBuf> {
    let mut v = vec![root.join("modules"), root.join("tests/fixtures/modules")];
    if let Some(p) = std::env::var_os("PATCHKIT_MODULES") {
        v.extend(std::env::split_paths(&p).filter(|p| !p.as_os_str().is_empty()));
    }
    v
}

fn find_manifests(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            find_manifests(&p, out);
        } else if p.file_name().is_some_and(|n| n == "module.toml") {
            out.push(p.parent().unwrap().to_path_buf());
        }
    }
}

/// Every module directory under the given roots (default: [`module_roots`]), sorted.
pub fn all_modules(root: &Path, only: Option<&Path>) -> Vec<PathBuf> {
    let roots = match only {
        Some(r) => vec![r.to_path_buf()],
        None => module_roots(root),
    };
    let mut v = vec![];
    for r in &roots {
        find_manifests(r, &mut v);
    }
    v.sort();
    v.dedup();
    v
}

/// A module directory from a path (relative to the working directory or the repository) or from a module id.
pub fn resolve_module(root: &Path, arg: &str) -> Result<PathBuf> {
    for base in [PathBuf::new(), root.to_path_buf()] {
        let p = base.join(arg);
        if p.join("module.toml").is_file() {
            return Ok(if p.is_absolute() {
                p
            } else {
                std::env::current_dir().unwrap_or_default().join(p)
            });
        }
    }
    #[derive(Deserialize)]
    struct Id {
        module: IdModule,
    }
    #[derive(Deserialize)]
    struct IdModule {
        id: String,
    }
    for d in all_modules(root, None) {
        if let Ok(t) = std::fs::read_to_string(d.join("module.toml"))
            && let Ok(Id { module }) = toml::from_str::<Id>(&t)
            && module.id == arg
        {
            return Ok(d);
        }
    }
    Err(err!("unknown module {arg:?}"))
}

/// C and assembly sources compiled, merged, linked at `base` against `symmap` and ISA-checked: for unit tests of C routines against
/// firmware addresses (tools/patchkit/pk_cfunit.py). Returns the placed image.
pub fn link_sources(
    root: &Path,
    sources: &[PathBuf],
    base: u32,
    symmap: &HashMap<String, u32>,
    include_dirs: &[PathBuf],
) -> Result<(cf::Linked, std::collections::BTreeMap<String, u32>)> {
    let env = Env::new(root);
    let proto = cf::proto_of_sources(&env, sources, include_dirs)?;
    let linked = cf::link(&proto, base, symmap)?;
    let hist = cf::check_isa(&linked)?;
    Ok((linked, hist))
}
