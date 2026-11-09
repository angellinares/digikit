//! `firmware.load {runtime: true}`: the stock image, patched in this process with a named module set of the device profile
//! (`devices/*.toml` `[patch_sets]`, default `emulator`), so the emulator boots with the digikit runtime.
//!
//! The patched image is cached in `<root>/out/runtime-cache/<stock sha256>-<set>-<key>.syx`. The key hashes everything that
//! decides the output: the modules of the set (manifest and object bytes), the device and port files, and the patchkit sources
//! ([`patchkit::SRC_ID`]). An input that already has a patch record is used as it is.
//!
//! The module library and the device files are read from a repository root (a dev build: the checkout). A shipped app needs
//! a bundle instead; until then a missing root is an error that says so.

use std::path::{Path, PathBuf};

use patchkit::{apply, devdata, firmware::Firmware, module::Library, port::Registry, verify};
use sha2::{Digest, Sha256};

/// What `firmware.load` reports about the runtime patch.
#[derive(Debug)]
pub struct Patched {
    /// The image to load.
    pub syx: Vec<u8>,
    /// The stock image's SHA-256 (the base of the patched one).
    pub base_sha256: String,
    pub set: String,
    /// The modules applied, requirements included, in apply order.
    pub modules: Vec<String>,
    pub cache_hit: bool,
    pub cache_file: PathBuf,
}

/// The input as it goes on: patched here, or left alone because it carries a patch record.
#[derive(Debug)]
pub enum Outcome {
    Patched(Patched),
    AlreadyPatched,
}

/// The short device name (`dt2`, `dn2`) of a .syx, from the device byte of its framing message; no decoding. `None` for anything
/// that is not a framed .syx of a known device.
pub fn device_short(syx: &[u8]) -> Option<&'static str> {
    let first = patchkit::syx::messages(syx).ok()?.into_iter().next()?;
    let id = *first.get(1..first.len().checked_sub(1)?)?.get(3)?;
    patchkit::syx::Device::from_id(id)
        .ok()
        .map(patchkit::syx::Device::short)
}

/// The repository root: `configured`, else `$DIGI_REPO_ROOT`, else the nearest directory from the working directory up that
/// holds `devices/ports`.
pub fn find_root(configured: Option<&Path>) -> Result<PathBuf, String> {
    let ok = |d: &Path| d.join("devices/ports").is_dir() && d.join("modules").is_dir();
    if let Some(d) = configured {
        return if ok(d) {
            Ok(d.to_path_buf())
        } else {
            Err(no_root(&d.display().to_string()))
        };
    }
    if let Some(d) = std::env::var_os("DIGI_REPO_ROOT").map(PathBuf::from) {
        return if ok(&d) {
            Ok(d)
        } else {
            Err(no_root(&format!("DIGI_REPO_ROOT={}", d.display())))
        };
    }
    let mut d = std::env::current_dir().unwrap_or_default();
    loop {
        if ok(&d) {
            return Ok(d);
        }
        if !d.pop() {
            return Err(no_root("the working directory and its parents"));
        }
    }
}

fn no_root(looked: &str) -> String {
    format!(
        "runtime: no module library: {looked} has no modules/ and devices/ports/. A build outside the repository cannot \
         patch the runtime in yet; start from the checkout or set DIGI_REPO_ROOT"
    )
}

/// The cache key: a hash over everything that decides the patched image (see the module comment).
pub fn cache_key(
    root: &Path,
    lib: &Library,
    modules: &[String],
    stock_sha256: &str,
    set: &str,
) -> Result<String, String> {
    let mut h = Sha256::new();
    let mut part = |label: &str, bytes: &[u8]| {
        h.update((label.len() as u64).to_le_bytes());
        h.update(label.as_bytes());
        h.update((bytes.len() as u64).to_le_bytes());
        h.update(bytes);
    };
    part("patchkit", patchkit::SRC_ID.as_bytes());
    part("stock", stock_sha256.as_bytes());
    part("set", set.as_bytes());
    for id in modules {
        let m = lib
            .get(id)
            .ok_or_else(|| format!("runtime: unknown module {id}"))?;
        part(id, m.digest.as_bytes());
    }
    // The device files and the ports lower the modules' typed sections and place them.
    for dir in [root.join("devices"), root.join("devices/ports")] {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map_err(|e| format!("runtime: {}: {e}", dir.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "toml"))
            .collect();
        files.sort();
        for f in files {
            let text = std::fs::read(&f).map_err(|e| format!("runtime: {}: {e}", f.display()))?;
            part(&f.to_string_lossy(), &text);
        }
    }
    Ok(format!("{:x}", h.finalize())[..16].to_string())
}

fn cache_file(root: &Path, stock_sha256: &str, set: &str, key: &str) -> PathBuf {
    root.join("out/runtime-cache")
        .join(format!("{stock_sha256}-{set}-{key}.syx"))
}

/// Patch `stock` with the device's `set` (or take the cached result). The cache is written whole or not at all.
pub fn prepare(root: &Path, stock: &[u8], set: &str) -> Result<Outcome, String> {
    let e = |x: patchkit::Error| x.to_string();
    if verify::patch_record(stock).map_err(e)?.is_some() {
        return Ok(Outcome::AlreadyPatched);
    }
    let fw = Firmware::parse(stock).map_err(e)?;
    let device = devdata::by_short(fw.device.short())
        .ok_or_else(|| format!("runtime: no device file for {}", fw.device.short()))?;
    let roots = device.patch_set(set).map_err(e)?;
    // the patch set is `core/*` modules: their objects are committed, and no content module is built (no toolchain needed)
    let lib = Library::scan_repo_for(root, false, Some(roots)).map_err(e)?;
    let (modules, _) = lib.with_requires(roots).map_err(e)?;
    let key = cache_key(root, &lib, &modules, &fw.syx_sha256, set)?;
    let file = cache_file(root, &fw.syx_sha256, set, &key);
    let base_sha256 = fw.syx_sha256.clone();
    let patched = |syx: Vec<u8>, modules: Vec<String>, cache_hit: bool| {
        Ok(Outcome::Patched(Patched {
            syx,
            base_sha256: base_sha256.clone(),
            set: set.to_string(),
            modules,
            cache_hit,
            cache_file: file.clone(),
        }))
    };
    if let Ok(syx) = std::fs::read(&file)
        && verify::patch_record(&syx)
            .ok()
            .flatten()
            .is_some_and(|r| r.base_sha256 == base_sha256)
    {
        return patched(syx, modules, true);
    }
    let reg = Registry::from_dir(&root.join("devices/ports")).map_err(e)?;
    let out = apply::apply(stock, &reg, &lib, &modules).map_err(e)?;
    write_whole(&file, &out.syx)?;
    patched(out.syx, out.plan.modules, false)
}

/// The stock image with this SHA-256 in `<root>/firmware/`, for a session that loaded an already patched image (it knows the base's
/// digest from the patch record and not the base).
pub fn find_stock(root: &Path, sha256: &str) -> Option<Vec<u8>> {
    std::fs::read_dir(root.join("firmware"))
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "syx"))
        .filter_map(|p| std::fs::read(p).ok())
        .find(|data| format!("{:x}", Sha256::digest(data)) == sha256)
}

/// What [`reapply`] made.
pub struct Reapplied {
    pub applied: apply::Applied,
    /// The ids of the library's modules that require one of `needs` (the UI layer's users, which `dev.reload` swaps by default).
    pub requirers: Vec<String>,
}

/// The image `ids` (a patch record's modules, in apply order) make from `stock` with the module objects as they are on disk now,
/// and the plan that places them: what `dev.reload` compares with the running image. Nothing is cached or written.
pub fn reapply(
    root: &Path,
    stock: &[u8],
    ids: &[String],
    needs: &[&str],
) -> Result<Reapplied, String> {
    let e = |x: patchkit::Error| x.to_string();
    // objects are loaded for the modules of the record only; a content module of it is built when stale
    let lib = Library::scan_repo_for(root, false, Some(ids)).map_err(e)?;
    let reg = Registry::from_dir(&root.join("devices/ports")).map_err(e)?;
    let requirers = lib
        .modules
        .iter()
        .filter(|m| {
            m.manifest
                .module
                .requires
                .iter()
                .any(|r| needs.contains(&r.as_str()))
        })
        .map(|m| m.id().to_string())
        .collect();
    Ok(Reapplied {
        applied: apply::apply(stock, &reg, &lib, ids).map_err(e)?,
        requirers,
    })
}

/// One module the build wrote objects for.
#[derive(Debug)]
pub struct BuiltModule {
    pub id: String,
    /// The object files written.
    pub objects: Vec<PathBuf>,
    pub ms: f64,
}

/// What [`build_modules`] did.
#[derive(Debug, Default)]
pub struct BuildSummary {
    pub built: Vec<BuiltModule>,
    /// Modules whose objects were current (or that have no sources).
    pub skipped: Vec<String>,
}

/// Build the stale objects of the modules `ids` (a patch record's modules) in this process: `patchkit::build::build_module` for each,
/// which writes a `core/*` module's objects into the module and a content module's into `out/cache/dko`. Compiler output of a
/// failed build is in the error. SHARC+ objects are built in this process too (Selache as a library).
pub fn build_modules(root: &Path, ids: &[String]) -> Result<BuildSummary, String> {
    let dirs = patchkit::build::module_dirs(root, ids).map_err(|e| e.to_string())?;
    build_dirs(root, &dirs)
}

fn build_dirs(root: &Path, dirs: &[(String, PathBuf)]) -> Result<BuildSummary, String> {
    use patchkit::build::{BuildOptions, Status};
    let opts = BuildOptions {
        root: root.to_path_buf(),
        ..Default::default()
    };
    let mut sum = BuildSummary::default();
    for (id, dir) in dirs {
        let started = std::time::Instant::now();
        let report = patchkit::build::build_module(dir, &opts)
            .map_err(|e| format!("build of {id} failed:\n{e}"))?;
        let objects: Vec<PathBuf> = report
            .objects
            .iter()
            .filter(|o| o.status == Status::Built)
            .map(|o| o.path.clone())
            .collect();
        if objects.is_empty() {
            sum.skipped.push(id.clone());
        } else {
            sum.built.push(BuiltModule {
                id: id.clone(),
                objects,
                ms: started.elapsed().as_secs_f64() * 1e3,
            });
        }
    }
    Ok(sum)
}

fn write_whole(file: &Path, data: &[u8]) -> Result<(), String> {
    let dir = file.parent().ok_or("runtime: bad cache path")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("runtime: {}: {e}", dir.display()))?;
    let tmp = file.with_extension(format!("part{}", std::process::id()));
    std::fs::write(&tmp, data).map_err(|e| format!("runtime: {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, file).map_err(|e| format!("runtime: {}: {e}", file.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
    }

    fn lib_and_modules() -> (Library, Vec<String>) {
        let roots = devdata::by_short("dt2")
            .unwrap()
            .patch_set("emulator")
            .unwrap();
        let lib = Library::scan_repo_for(&root(), false, Some(roots)).unwrap();
        let (modules, _) = lib.with_requires(roots).unwrap();
        (lib, modules)
    }

    #[test]
    fn the_key_follows_the_stock_image_the_set_and_the_modules() {
        let (mut lib, modules) = lib_and_modules();
        let key = |lib: &Library, stock: &str, set: &str| {
            cache_key(&root(), lib, &modules, stock, set).unwrap()
        };
        let k = key(&lib, "aa", "emulator");
        assert_eq!(k, key(&lib, "aa", "emulator"));
        assert_ne!(k, key(&lib, "bb", "emulator"), "stock image");
        assert_ne!(k, key(&lib, "aa", "device"), "set name");
        // One changed byte of a module (its digest covers manifest and objects) is another key.
        let m = lib
            .modules
            .iter_mut()
            .find(|m| m.id() == "core/health")
            .unwrap();
        m.digest.push('x');
        assert_ne!(k, key(&lib, "aa", "emulator"), "module bytes");
    }

    #[test]
    fn the_device_comes_from_the_framing_message() {
        assert_eq!(device_short(b"not a syx"), None);
        assert_eq!(device_short(&[]), None);
        for (file, short) in [
            ("Digitakt_II_OS1.17.syx", "dt2"),
            ("Digitone_II_OS1.12.syx", "dn2"),
        ] {
            match stock(file) {
                Some(syx) => assert_eq!(device_short(&syx), Some(short), "{file}"),
                None => eprintln!("skipped: firmware/{file} is absent"),
            }
        }
    }

    #[test]
    fn a_reload_build_writes_stale_objects_once_and_reports_c_errors() {
        if patchkit::build::toolchain::Env::new(&root())
            .m68k("gcc")
            .is_err()
        {
            eprintln!("SKIP: the m68k-elf toolchain is not installed");
            return;
        }
        let id = format!("test/reloadbuild-{}", std::process::id());
        let dir = std::env::temp_dir().join(format!("digikit-reloadbuild-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("module.toml"),
            format!("[module]\nid = \"{id}\"\ncategory = \"core\"\n\n[[object]]\npath = \"build/t.dko.json\"\nsources = [\"src/t.c\"]\n"),
        )
        .unwrap();
        std::fs::write(
            dir.join("src/t.c"),
            "extern int ext(int);\nint twice(int x) { return ext(x) + ext(x); }\n",
        )
        .unwrap();
        let dirs = [(id.clone(), dir.clone())];
        let first = build_dirs(&root(), &dirs).unwrap();
        assert_eq!(first.built.len(), 1);
        assert_eq!(first.built[0].objects, [dir.join("build/t.dko.json")]);
        // current: skipped, not built again
        let second = build_dirs(&root(), &dirs).unwrap();
        assert!(second.built.is_empty() && second.skipped == [id.clone()]);
        // a compile error names the module and carries the compiler's text; the old object stays
        let good = std::fs::read_to_string(dir.join("build/t.dko.json")).unwrap();
        std::fs::write(dir.join("src/t.c"), "int twice(int x) { return x +; }\n").unwrap();
        let err = build_dirs(&root(), &dirs).unwrap_err();
        assert!(
            err.starts_with(&format!("build of {id} failed:")) && err.contains("t.c"),
            "{err}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("build/t.dko.json")).unwrap(),
            good
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stale_sharc_object_is_built_with_the_rest_and_a_bad_instruction_is_an_error() {
        let id = format!("test/reloadsharc-{}", std::process::id());
        let dir = std::env::temp_dir().join(format!("digikit-reloadsharc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/dsp")).unwrap();
        std::fs::write(
            dir.join("module.toml"),
            format!("[module]\nid = \"{id}\"\ncategory = \"core\"\n\n[[object]]\npath = \"build/d.dko.json\"\nsources = [\"src/dsp/d.s\"]\n"),
        )
        .unwrap();
        let src = dir.join("src/dsp/d.s");
        std::fs::write(
            &src,
            ".SECTION/PM seg_pmco;\n.GLOBAL _f;\n_f:\n    R0 = 7;\n    RTS;\n",
        )
        .unwrap();
        let dirs = [(id.clone(), dir.clone())];
        let first = build_dirs(&root(), &dirs).unwrap();
        assert_eq!(first.built.len(), 1);
        assert_eq!(first.built[0].objects, [dir.join("build/d.dko.json")]);
        let text = std::fs::read_to_string(dir.join("build/d.dko.json")).unwrap();
        assert!(
            text.contains("\"arch\": \"sharc\"") && text.contains("selache_rev"),
            "{text}"
        );
        // current: skipped, not built again
        let second = build_dirs(&root(), &dirs).unwrap();
        assert!(second.built.is_empty() && second.skipped == [id.clone()]);
        // an instruction the ISA excludes is refused with the check's text; the old object stays
        std::fs::write(
            &src,
            ".SECTION/PM seg_pmco;\n.GLOBAL _f;\n_f:\n    DM(15, I4) = I0;\n    RTS;\n",
        )
        .unwrap();
        let err = build_dirs(&root(), &dirs).unwrap_err();
        assert!(
            err.starts_with(&format!("build of {id} failed:"))
                && err.contains("the ISA does not allow"),
            "{err}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("build/d.dko.json")).unwrap(),
            text
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_root_says_what_to_do() {
        let err = find_root(Some(Path::new("/nonexistent-digikit-root"))).unwrap_err();
        assert!(
            err.contains("DIGI_REPO_ROOT") && err.contains("no module library"),
            "{err}"
        );
    }

    /// A stock image of the repository's firmware directory, or `None` (the test is skipped without firmware).
    fn stock(name: &str) -> Option<Vec<u8>> {
        std::fs::read(root().join("firmware").join(name)).ok()
    }

    #[test]
    #[cfg(unix)]
    fn stock_is_patched_once_then_served_from_the_cache() {
        let Some(stock) = stock("Digitakt_II_OS1.17.syx") else {
            eprintln!("skipped: firmware/Digitakt_II_OS1.17.syx is absent");
            return;
        };
        // A scratch root with the repository's modules and devices linked in, so the cache lands in a temp directory.
        let dir =
            std::env::temp_dir().join(format!("digikit-runtime-patch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for d in ["modules", "devices", "tests"] {
            std::os::unix::fs::symlink(root().join(d), dir.join(d)).unwrap();
        }
        let first = match prepare(&dir, &stock, "emulator").unwrap() {
            Outcome::Patched(p) => p,
            Outcome::AlreadyPatched => panic!("stock has no patch record"),
        };
        assert!(!first.cache_hit);
        assert_eq!(first.modules.len(), 16); // the Digitakt II emulator set: 12 and core/dynfx, core/fx, core/fxpages, core/sample-pool
        assert!(first.cache_file.is_file());
        let record = verify::patch_record(&first.syx).unwrap().unwrap();
        assert_eq!(record.base_sha256, first.base_sha256);
        let again = match prepare(&dir, &stock, "emulator").unwrap() {
            Outcome::Patched(p) => p,
            Outcome::AlreadyPatched => panic!("stock has no patch record"),
        };
        assert!(again.cache_hit);
        assert_eq!(again.syx, first.syx);
        // The patched image goes on as it is.
        assert!(matches!(
            prepare(&dir, &first.syx, "emulator").unwrap(),
            Outcome::AlreadyPatched
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
