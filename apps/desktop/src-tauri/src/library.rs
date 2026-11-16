//! The module library: the `.dkm` files of the run-time modules, one folder per device (`out/modules/<device>/<id>.dkm`).
//! `lib_build` builds the stale objects and packs every run-time module for a device in this process (patchkit); `lib_scan` and
//! `lib_read` let the page read the folder (the one thing the page cannot do itself: file access). A packaged app would bundle a
//! folder and point LIBRARY_DIR there.
use std::path::{Path, PathBuf};

use serde::Serialize;

/// The library folder, relative to the repository root (one subfolder per device: `dt2`, `dn2`).
const LIBRARY_DIR: &str = "out/modules";

#[derive(Serialize, Debug, PartialEq)]
pub struct LibFile {
    /// Relative to the device's folder, `/` separated (`<author/name>.dkm`).
    pub path: String,
    pub size: u64,
}

fn device_dir(root: &Path, device: &str) -> Result<PathBuf, String> {
    if device.is_empty()
        || !device
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    {
        return Err(format!("not a device id: {device:?}"));
    }
    Ok(root.join(LIBRARY_DIR).join(device))
}

fn walk(dir: &Path, base: &Path, out: &mut Vec<LibFile>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let meta = entry.metadata()?;
        if meta.is_dir() {
            walk(&path, base, out)?;
        } else if path.extension().is_some_and(|e| e == "dkm" || e == "dks") {
            let rel = path.strip_prefix(base).unwrap_or(&path);
            let rel = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            out.push(LibFile {
                path: rel,
                size: meta.len(),
            });
        }
    }
    Ok(())
}

/// The `.dkm` module files and the `.dks` service files (unbound) under the device's folder, by path. A missing folder is an empty library.
pub fn scan(root: &Path, device: &str) -> Result<Vec<LibFile>, String> {
    let dir = device_dir(root, device)?;
    let mut out = vec![];
    match walk(&dir, &dir, &mut out) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// The bytes of one library file; a path that leaves the device's folder is refused.
pub fn read(root: &Path, device: &str, rel: &str) -> Result<Vec<u8>, String> {
    let dir = device_dir(root, device)?;
    if rel
        .split('/')
        .any(|p| p.is_empty() || p == "." || p == "..")
        || !(rel.ends_with(".dkm") || rel.ends_with(".dks"))
    {
        return Err(format!("not a library file: {rel:?}"));
    }
    let path = dir.join(rel);
    std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))
}

/// The address map file of a library path: `<author/name>.dkm` has `<author/name>.addrmap.json`.
fn map_rel(dkm: &str) -> String {
    format!("{}.addrmap.json", dkm.strip_suffix(".dkm").unwrap_or(dkm))
}

/// One module's address map as `lib_build` wrote it.
#[derive(Serialize, Debug, PartialEq)]
pub struct LibMap {
    /// The module's library path without `.dkm` (`author/name`).
    pub id: String,
    /// The map file's JSON text.
    pub json: String,
}

/// Where the ship maps of prepared firmware images are kept, one folder per device: `out/addrmap/<device>/<image id>.ship.json`.
const SHIP_DIR: &str = "out/addrmap";

/// The ship map of a firmware image and the addresses of its load-table symbols (`firmware_prepare` writes it; docs/ADDRMAP.md "Workbench").
#[derive(Serialize, serde::Deserialize, Debug, PartialEq, Clone)]
pub struct ShipRecord {
    /// The id the image reports in INFO after the flash, 16 hex digits.
    pub image_id: String,
    /// The ship map as JSON text.
    pub ship: String,
    /// Symbol name to byte address.
    pub symbols: std::collections::BTreeMap<String, u64>,
}

fn ship_file(root: &Path, device: &str, image_id: &str) -> Result<PathBuf, String> {
    if image_id.is_empty() || !image_id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("not an image id: {image_id:?}"));
    }
    if device.is_empty()
        || !device
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    {
        return Err(format!("not a device id: {device:?}"));
    }
    Ok(root
        .join(SHIP_DIR)
        .join(device)
        .join(format!("{image_id}.ship.json")))
}

/// Keep the ship map of a prepared image for the workbench (written when its bytes changed).
pub fn store_ship(root: &Path, device: &str, record: &ShipRecord) -> Result<(), String> {
    let path = ship_file(root, device, &record.image_id)?;
    let text = serde_json::to_string(record).map_err(|e| e.to_string())?;
    if std::fs::read(&path).ok().as_deref() == Some(text.as_bytes()) {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
}

/// What `lib_maps` answers: the ship maps kept for the device's images and the address maps of its built modules.
#[derive(Serialize, Debug, PartialEq)]
pub struct LibMaps {
    pub ships: Vec<ShipRecord>,
    pub modules: Vec<LibMap>,
}

/// The kept ship maps of a device, by image id; a file that does not parse is left out.
fn read_ships(root: &Path, device: &str) -> Result<Vec<ShipRecord>, String> {
    let dir = ship_file(root, device, "0")?
        .parent()
        .map(Path::to_path_buf)
        .ok_or("no folder")?;
    let mut out: Vec<ShipRecord> = vec![];
    match std::fs::read_dir(&dir) {
        Ok(entries) => {
            for e in entries.flatten() {
                if e.path().to_string_lossy().ends_with(".ship.json")
                    && let Ok(text) = std::fs::read_to_string(e.path())
                    && let Ok(r) = serde_json::from_str::<ShipRecord>(&text)
                {
                    out.push(r);
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    }
    out.sort_by(|a, b| a.image_id.cmp(&b.image_id));
    Ok(out)
}

/// The address maps of the device: the ship maps of its prepared images, and the maps of the modules that have a `.dkm` in the library. A module
/// without a map file (built before maps were written) is left out; a missing folder is an empty list.
pub fn read_maps(root: &Path, device: &str) -> Result<LibMaps, String> {
    let mut modules = vec![];
    for file in scan(root, device)? {
        if !file.path.ends_with(".dkm") {
            continue;
        }
        let rel = map_rel(&file.path);
        let path = device_dir(root, device)?.join(&rel);
        match std::fs::read_to_string(&path) {
            Ok(json) => modules.push(LibMap {
                id: file.path.trim_end_matches(".dkm").to_string(),
                json,
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
    }
    Ok(LibMaps {
        ships: read_ships(root, device)?,
        modules,
    })
}

fn root() -> Result<PathBuf, String> {
    digi_control::runtime_patch::find_root(None)
}

/// A module that did not build or pack for the device.
#[derive(Serialize, Debug)]
pub struct LibFailure {
    pub id: String,
    pub error: String,
}

/// What `lib_build` did for one device.
#[derive(Serialize, Debug, Default)]
pub struct LibBuildReport {
    /// Modules packed for the device.
    pub packed: usize,
    /// Ids whose `.dkm` was new or changed (written).
    pub changed: Vec<String>,
    /// Library paths (`<author/name>.dkm`) deleted because their module no longer packs for the device.
    pub removed: Vec<String>,
    pub failed: Vec<LibFailure>,
    pub ms: u64,
}

/// One build at a time: the Transfer page, the memory panel and the default modules may ask together.
static BUILD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Write one packed module into the device folder when its bytes changed, and count it; a failure goes to `report.failed`.
fn record(
    dir: &Path,
    id: &str,
    packed: Result<patchkit::pack::Packed, String>,
    report: &mut LibBuildReport,
    kept: &mut std::collections::BTreeSet<String>,
) {
    let packed = match packed {
        Ok(p) => p,
        Err(error) => {
            report.failed.push(LibFailure {
                id: id.to_string(),
                error,
            });
            return;
        }
    };
    let rel = format!("{id}.dkm");
    let path = dir.join(&rel);
    if std::fs::read(&path).ok().as_deref() != Some(packed.bytes.as_slice()) {
        let written = match path.parent() {
            Some(parent) => std::fs::create_dir_all(parent),
            None => Ok(()),
        }
        .and_then(|()| std::fs::write(&path, &packed.bytes));
        if let Err(e) = written {
            report.failed.push(LibFailure {
                id: id.to_string(),
                error: format!("{}: {e}", path.display()),
            });
            return;
        }
        report.changed.push(id.to_string());
    }
    // the module's address map (docs/ADDRMAP.md) goes beside the .dkm; the workbench names addresses with it (`lib_maps`)
    let map_path = dir.join(map_rel(&rel));
    match packed.addrmap() {
        Ok(map) => {
            let text = map.to_json();
            if std::fs::read(&map_path).ok().as_deref() != Some(text.as_bytes())
                && let Err(e) = std::fs::write(&map_path, text)
            {
                report.failed.push(LibFailure {
                    id: format!("{id} (address map)"),
                    error: format!("{}: {e}", map_path.display()),
                });
            }
        }
        Err(e) => report.failed.push(LibFailure {
            id: format!("{id} (address map)"),
            error: e.to_string(),
        }),
    }
    report.packed += 1;
    kept.insert(rel);
}

/// Where the built `.dks` services (unbound) of a device are, relative to the repository root: `dist/modules/<device>/<stem>.dks`.
const SERVICES_DIST_DIR: &str = "dist/modules";

/// Copy the unbound `.dks` files of `dist/modules/<device>/` into the device's library folder when their bytes changed (as for a `.dkm`).
/// A missing source folder is no services. A failure goes to `report.failed`.
fn copy_services(root: &Path, device: &str, dir: &Path, report: &mut LibBuildReport) {
    let source = root.join(SERVICES_DIST_DIR).join(device);
    let entries = match std::fs::read_dir(&source) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            report.failed.push(LibFailure {
                id: "services".to_string(),
                error: format!("{}: {e}", source.display()),
            });
            return;
        }
    };
    for entry in entries.flatten() {
        let from = entry.path();
        if !from.is_file() || from.extension().is_none_or(|e| e != "dks") {
            continue;
        }
        let Some(name) = from.file_name() else {
            continue;
        };
        let to = dir.join(name);
        let result = std::fs::read(&from).and_then(|bytes| {
            if std::fs::read(&to).ok().as_deref() == Some(bytes.as_slice()) {
                return Ok(());
            }
            std::fs::create_dir_all(dir)?;
            std::fs::write(&to, &bytes)
        });
        if let Err(e) = result {
            report.failed.push(LibFailure {
                id: name.to_string_lossy().into_owned(),
                error: format!("{}: {e}", from.display()),
            });
        }
    }
}

/// Build and pack every run-time module that builds for `device` into its library folder. A `.dkm` is written only when its bytes
/// changed; a `.dkm` whose module did not pack this time is deleted. `patchkit::pack::pack_all` packs the candidates: one library for
/// all of them, or one per module when that load fails, so one failure does not stop the others. A patch-only module is skipped.
fn build(root: &Path, device: &str) -> Result<LibBuildReport, String> {
    let _one = BUILD_LOCK.lock().map_err(|e| e.to_string())?;
    let started = std::time::Instant::now();
    let dir = device_dir(root, device)?;
    let source = patchkit::module::LibrarySource::default();
    // the manifests only: no content module objects are loaded or built here
    let headers = patchkit::module::library_with(root, &source, false, Some(&[][..]))
        .map_err(|e| e.to_string())?;
    let ids: Vec<String> = headers
        .modules
        .iter()
        .map(|m| &m.manifest.module)
        .filter(|m| m.is_runtime() && m.builds_for(device))
        .map(|m| m.id.clone())
        .collect();
    let mut report = LibBuildReport::default();
    let mut kept = std::collections::BTreeSet::new();
    for (id, packed) in patchkit::pack::pack_all(root, &ids, device) {
        record(&dir, &id, packed, &mut report, &mut kept);
    }
    copy_services(root, device, &dir, &mut report);
    for file in scan(root, device)? {
        // the `.dks` services are not packed here: only a `.dkm` whose module did not pack is removed
        if file.path.ends_with(".dkm") && !kept.contains(&file.path) {
            let path = dir.join(&file.path);
            let _ = std::fs::remove_file(dir.join(map_rel(&file.path)));
            match std::fs::remove_file(&path) {
                Ok(()) => report.removed.push(file.path),
                Err(e) => report.failed.push(LibFailure {
                    id: file.path,
                    error: format!("{}: {e}", path.display()),
                }),
            }
        }
    }
    report.ms = started.elapsed().as_millis() as u64;
    Ok(report)
}

/// Build and pack the run-time modules of a device (`dt2`, `dn2`) into the library.
#[tauri::command]
pub async fn lib_build(device: String) -> Result<LibBuildReport, String> {
    tauri::async_runtime::spawn_blocking(move || build(&root()?, &device))
        .await
        .map_err(|e| e.to_string())?
}

/// The built modules of a device (`dt2`, `dn2`): path and size of each.
#[tauri::command]
pub async fn lib_scan(device: String) -> Result<Vec<LibFile>, String> {
    tauri::async_runtime::spawn_blocking(move || scan(&root()?, &device))
        .await
        .map_err(|e| e.to_string())?
}

/// The address maps of a device: its prepared images' ship maps (`firmware_prepare`) and its built modules' maps (`lib_build` writes each beside its `.dkm`).
#[tauri::command]
pub async fn lib_maps(device: String) -> Result<LibMaps, String> {
    tauri::async_runtime::spawn_blocking(move || read_maps(&root()?, &device))
        .await
        .map_err(|e| e.to_string())?
}

/// One library file as raw bytes.
#[tauri::command]
pub async fn lib_read(device: String, path: String) -> Result<tauri::ipc::Response, String> {
    tauri::async_runtime::spawn_blocking(move || {
        read(&root()?, &device, &path).map(tauri::ipc::Response::new)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The converter of the Samples tab (`apps/web/public/dksample.wasm`, built by `mise run wasm:patchkit`),
/// as raw bytes. The page cannot import it: the file is a build product, not in the repository.
#[tauri::command]
pub async fn sample_wasm() -> Result<tauri::ipc::Response, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let path = root()?.join("apps/web/public/dksample.wasm");
        std::fs::read(&path)
            .map(tauri::ipc::Response::new)
            .map_err(|e| format!("{}: {e} (build it: mise run wasm:patchkit)", path.display()))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The needs reader of the Transfer page: `apps/web/public/patchkit.wasm` (`pk_needs`, the Rust code that infers what a `.dkm` takes of the device;
/// built by `mise run wasm:patchkit`), as raw bytes.
#[tauri::command]
pub async fn patchkit_wasm() -> Result<tauri::ipc::Response, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let path = root()?.join("apps/web/public/patchkit.wasm");
        std::fs::read(&path)
            .map(tauri::ipc::Response::new)
            .map_err(|e| format!("{}: {e} (build it: mise run wasm:patchkit)", path.display()))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The bind bundle of the site build, `apps/web/public/modules/bind.json` (`{ports, fw}`, written by `apps/web/scripts/gen-modules.mjs`): what `pk_bind` reads
/// beside the user's stock image to bind a service file. It holds no firmware.
#[tauri::command]
pub async fn bind_bundle() -> Result<tauri::ipc::Response, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let path = root()?.join("apps/web/public/modules/bind.json");
        std::fs::read(&path)
            .map(tauri::ipc::Response::new)
            .map_err(|e| {
                format!(
                    "{}: {e} (build it: node apps/web/scripts/gen-modules.mjs)",
                    path.display()
                )
            })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The profile and address-lookup code of the Log pane: `apps/web/public/dkprof.wasm` (built by `mise run wasm:dkprof`), as raw bytes.
#[tauri::command]
pub async fn dkprof_wasm() -> Result<tauri::ipc::Response, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let path = root()?.join("apps/web/public/dkprof.wasm");
        std::fs::read(&path)
            .map(tauri::ipc::Response::new)
            .map_err(|e| format!("{}: {e} (build it: mise run wasm:dkprof)", path.display()))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// What the device profile says about content (`[storage]`): the device's name and the backend of its samples.
#[derive(Serialize, Debug, PartialEq)]
pub struct DeviceStorage {
    pub name: String,
    pub samples: String,
}

/// The `[storage]` of a device (`dt2`, `dn2`) from `devices/*.toml`.
pub fn storage_of(device: &str) -> Result<DeviceStorage, String> {
    let registry = device_profile::Registry::load().map_err(|e| e.to_string())?;
    let profile = registry
        .devices()
        .iter()
        .find(|d| d.short == device)
        .ok_or_else(|| format!("no device profile for {device:?}"))?;
    let storage = profile
        .storage
        .as_ref()
        .ok_or_else(|| format!("the {} profile has no [storage]", profile.name))?;
    Ok(DeviceStorage {
        name: profile.name.clone(),
        samples: storage.samples.clone(),
    })
}

/// Where the device keeps samples: read from its profile, so the page names no device.
#[tauri::command]
pub async fn device_storage(device: String) -> Result<DeviceStorage, String> {
    tauri::async_runtime::spawn_blocking(move || storage_of(&device))
        .await
        .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("digi-library-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join(LIBRARY_DIR).join("dt2");
        std::fs::create_dir_all(dir.join("acme")).unwrap();
        std::fs::create_dir_all(dir.join("test")).unwrap();
        std::fs::write(dir.join("acme/synth.dkm"), b"SYNTH").unwrap();
        std::fs::write(dir.join("test/sine.dkm"), b"SINE!!").unwrap();
        std::fs::write(dir.join("notes.txt"), b"not a module").unwrap();
        root
    }

    #[test]
    fn the_scan_lists_dkm_and_dks_files_by_path_and_a_missing_folder_is_empty() {
        let root = library("scan");
        let dir = root.join(LIBRARY_DIR).join("dt2");
        std::fs::write(dir.join("fx.dks"), b"FXSV").unwrap();
        assert_eq!(
            scan(&root, "dt2").unwrap(),
            vec![
                LibFile {
                    path: "acme/synth.dkm".into(),
                    size: 5
                },
                LibFile {
                    path: "fx.dks".into(),
                    size: 4
                },
                LibFile {
                    path: "test/sine.dkm".into(),
                    size: 6
                }
            ]
        );
        assert_eq!(scan(&root, "dn2").unwrap(), vec![]);
        assert!(scan(&root, "../x").is_err());
        assert_eq!(read(&root, "dt2", "acme/synth.dkm").unwrap(), b"SYNTH");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_maps_are_the_files_beside_the_built_modules_and_the_kept_ship_maps() {
        let root = library("maps");
        let dir = root.join(LIBRARY_DIR).join("dt2");
        std::fs::write(
            dir.join("acme/synth.addrmap.json"),
            b"{\"kind\":\"module\"}",
        )
        .unwrap();
        let ship = ShipRecord {
            image_id: "0123456789abcdef".into(),
            ship: "{\"kind\":\"ship\"}".into(),
            symbols: [("DKRT_STATE".to_string(), 0x4010_0000)].into(),
        };
        store_ship(&root, "dt2", &ship).unwrap();
        store_ship(&root, "dt2", &ship).unwrap(); // unchanged: not written again
        // sine has no map file (built before maps were written): left out
        let maps = read_maps(&root, "dt2").unwrap();
        assert_eq!(
            maps.modules,
            vec![LibMap {
                id: "acme/synth".into(),
                json: "{\"kind\":\"module\"}".into()
            }]
        );
        assert_eq!(maps.ships, vec![ship.clone()]);
        // another device has neither
        let none = read_maps(&root, "dn2").unwrap();
        assert!(none.modules.is_empty() && none.ships.is_empty());
        assert!(read_maps(&root, "../x").is_err());
        assert!(
            store_ship(
                &root,
                "dt2",
                &ShipRecord {
                    image_id: "../x".into(),
                    ..ship
                }
            )
            .is_err()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_services_of_dist_are_copied_into_the_library_when_changed() {
        let root = library("services");
        let dir = root.join(LIBRARY_DIR).join("dt2");
        let dist = root.join(SERVICES_DIST_DIR).join("dt2");
        std::fs::create_dir_all(&dist).unwrap();
        std::fs::write(dist.join("fx.dks"), b"FXSV").unwrap();
        std::fs::write(dist.join("notes.txt"), b"not a service").unwrap();
        let mut report = LibBuildReport::default();
        copy_services(&root, "dt2", &dir, &mut report);
        assert_eq!(std::fs::read(dir.join("fx.dks")).unwrap(), b"FXSV");
        // only `.dks` files are copied: the library's own notes.txt is untouched
        assert_eq!(std::fs::read(dir.join("notes.txt")).unwrap(), b"not a module");
        // a changed file is written again, the .dkm files stay as they are
        std::fs::write(dist.join("fx.dks"), b"FXS2").unwrap();
        copy_services(&root, "dt2", &dir, &mut report);
        assert_eq!(std::fs::read(dir.join("fx.dks")).unwrap(), b"FXS2");
        assert_eq!(std::fs::read(dir.join("acme/synth.dkm")).unwrap(), b"SYNTH");
        // no source folder for another device: nothing copied, nothing failed
        copy_services(&root, "dn2", &root.join(LIBRARY_DIR).join("dn2"), &mut report);
        assert!(report.failed.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_build_of_an_unknown_device_is_refused() {
        let root = library("build");
        assert!(build(&root, "DT2!").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_storage_comes_from_the_profile() {
        let dt2 = storage_of("dt2").unwrap();
        assert_eq!(
            (dt2.name.as_str(), dt2.samples.as_str()),
            ("Digitakt II", "stock-rpc")
        );
        let dn2 = storage_of("dn2").unwrap();
        assert_eq!(
            (dn2.name.as_str(), dn2.samples.as_str()),
            ("Digitone II", "digikit")
        );
        assert!(storage_of("nothing").is_err());
    }

    #[test]
    fn a_read_stays_inside_the_device_folder() {
        let root = library("read");
        for bad in [
            "../dn2/x.dkm",
            "digikit/../../x.dkm",
            "/etc/passwd",
            "notes.txt",
            "",
            "a//b.dkm",
        ] {
            assert!(read(&root, "dt2", bad).is_err(), "{bad}");
        }
        assert!(read(&root, "dt2", "nothing.dkm").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
