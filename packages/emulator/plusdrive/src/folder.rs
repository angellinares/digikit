//! A `plus_drive/<device>/` folder as a [`Tree`]: the folder contract in one place, for the `plusdrive` CLI and the
//! desktop app.
//!
//! - Files and folders map to the path filesystem as they are; a `.wav` becomes a native sample named without the
//!   extension; any other file is copied byte for byte (a file that already is a native sample stays one).
//!   A device without a path filesystem (DN2) takes none of them.
//! - `.mmcfs/{projects,sounds,kits}/NNNN.bin` are raw `MmcFs` slots.
//! - `.mmcfs/boot/SSSSSS.bin` (SSSSSS the first sector in hex) is a raw 16 MiB region outside the pools: the boot
//!   project copies and the working project of the layout. A boot copy given here replaces the generated boot
//!   project at that sector (the exporter writes these files; a copy made for another sample set no longer matches
//!   the card's sample ids, so delete it to have the project generated again).
//! - `digikit/` (top level, recursive) goes to the module store (docs/design/digikit-store.md), not to the path
//!   filesystem.

use std::{
    fs,
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use crate::{
    CardLayout, Content, Pool, SparseImage, StoreFile, Tree, TreeFile, WavLimits, parse_native,
    wav_to_native,
};

const WAV_SOURCE_LIMIT: u64 = 64 * 1024 * 1024;

#[derive(Default)]
pub struct FolderScan {
    pub tree: Tree,
    /// What was left out, one line each.
    pub skipped: Vec<String>,
}

/// The regular files under `root` that a build reads (relative `/` paths, sorted): the same walk as the scan, for
/// cache keys. Symbolic links and `.DS_Store` are left out.
pub fn folder_files(root: &Path) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        for entry in fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if kind.is_symlink() || name == ".DS_Store" {
                continue;
            }
            if kind.is_dir() {
                stack.push((entry.path(), rel));
            } else if kind.is_file() {
                out.push(rel);
            }
        }
    }
    out.sort();
    Ok(out)
}

fn slot_number(name: &str) -> Option<u32> {
    let digits = name.strip_suffix(".bin")?;
    (digits.len() == 4 && digits.bytes().all(|b| b.is_ascii_digit()))
        .then(|| digits.parse().ok())
        .flatten()
}

fn region_sector(name: &str) -> Option<u64> {
    let digits = name.strip_suffix(".bin")?;
    (!digits.is_empty() && digits.len() <= 8 && digits.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| u64::from_str_radix(digits, 16).ok())
        .flatten()
}

/// Walk `root` for the device of `layout`.
///
/// `progress(done, total)` is called after each file of the path filesystem is read (a WAV is converted there).
pub fn scan_folder(
    root: &Path,
    layout: &CardLayout,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<FolderScan, String> {
    let total = folder_files(root)?.len();
    let mut done = 0usize;
    let path_fs = layout.path_fs.is_some();
    let mut out = FolderScan::default();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        let mut entries = fs::read_dir(&dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        entries.sort_by_key(|entry| entry.file_name());
        if !prefix.is_empty() && entries.is_empty() && path_fs {
            out.tree.dirs.push(prefix.clone());
        }
        for entry in entries {
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| format!("{}: name is not UTF-8", entry.path().display()))?;
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if kind.is_symlink() {
                out.skipped.push(format!("{rel} (symbolic link)"));
                continue;
            }
            if prefix.is_empty() && kind.is_dir() {
                if name == ".mmcfs" {
                    scan_mmcfs(&entry.path(), layout, &mut out)?;
                    continue;
                }
                if name == "digikit" {
                    scan_store(&entry.path(), layout, &mut out)?;
                    continue;
                }
            }
            if name == ".DS_Store" {
                continue;
            }
            if !path_fs {
                out.skipped
                    .push(format!("{rel} (this device has no path filesystem)"));
                continue;
            }
            if kind.is_dir() {
                stack.push((entry.path(), rel));
            } else if kind.is_file() {
                let path = entry.path();
                let len = entry.metadata().map_err(|e| e.to_string())?.len();
                let is_wav = Path::new(&name)
                    .extension()
                    .is_some_and(|x| x.eq_ignore_ascii_case("wav"));
                let content = if is_wav {
                    if len > WAV_SOURCE_LIMIT {
                        return Err(format!("{} exceeds WAV source limit", path.display()));
                    }
                    let wav = fs::read(&path).map_err(|e| e.to_string())?;
                    Content::Native(
                        wav_to_native(&wav, WavLimits::default())
                            .map_err(|e| format!("{}: {e:?}", path.display()))?,
                    )
                } else {
                    let bytes = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                    match parse_native(bytes.clone()) {
                        Some(sample) => Content::Native(sample),
                        None => Content::Plain(bytes),
                    }
                };
                out.tree.files.push(TreeFile { path: rel, content });
                done += 1;
                progress(done, total);
            } else {
                out.skipped.push(format!("{rel} (not a regular file)"));
            }
        }
    }
    Ok(out)
}

fn scan_mmcfs(dir: &Path, layout: &CardLayout, out: &mut FolderScan) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = entry.path().is_dir();
        if name == "boot" && is_dir {
            for file in fs::read_dir(entry.path()).map_err(|e| e.to_string())? {
                let file = file.map_err(|e| e.to_string())?;
                let file_name = file.file_name().to_string_lossy().into_owned();
                let rel = format!(".mmcfs/boot/{file_name}");
                let is_file = file.file_type().is_ok_and(|k| k.is_file());
                match region_sector(&file_name).filter(|_| is_file) {
                    Some(sector) if layout.region_at(sector).is_some() => {
                        let bytes = fs::read(file.path()).map_err(|e| format!("{rel}: {e}"))?;
                        out.tree.regions.insert(sector, bytes);
                    }
                    Some(_) => out.skipped.push(format!(
                        "{rel} (no project region at this sector on this device)"
                    )),
                    None => out.skipped.push(format!("{rel} (not SSSSSS.bin)")),
                }
            }
            continue;
        }
        let Some(pool) = Pool::from_dir_name(&name).filter(|_| is_dir) else {
            out.skipped.push(format!(
                ".mmcfs/{name} (not projects, sounds, kits or boot)"
            ));
            continue;
        };
        for slot_file in fs::read_dir(entry.path()).map_err(|e| e.to_string())? {
            let slot_file = slot_file.map_err(|e| e.to_string())?;
            let file_name = slot_file.file_name().to_string_lossy().into_owned();
            let rel = format!(".mmcfs/{name}/{file_name}");
            match (slot_number(&file_name), slot_file.file_type()) {
                (Some(slot), Ok(kind)) if kind.is_file() => {
                    let bytes = fs::read(slot_file.path()).map_err(|e| format!("{rel}: {e}"))?;
                    out.tree.slots.slots.insert((pool, slot), bytes);
                }
                _ => out.skipped.push(format!("{rel} (not NNNN.bin)")),
            }
        }
    }
    Ok(())
}

/// `digikit/` into the store files (paths relative to it).
fn scan_store(dir: &Path, layout: &CardLayout, out: &mut FolderScan) -> Result<(), String> {
    let mut stack = vec![(dir.to_path_buf(), String::new())];
    while let Some((here, prefix)) = stack.pop() {
        for entry in fs::read_dir(&here).map_err(|e| format!("{}: {e}", here.display()))? {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| format!("{}: name is not UTF-8", entry.path().display()))?;
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if name == ".DS_Store" {
                continue;
            }
            if kind.is_symlink() {
                out.skipped.push(format!("digikit/{rel} (symbolic link)"));
            } else if kind.is_dir() {
                stack.push((entry.path(), rel));
            } else if kind.is_file() {
                if layout.store.is_none() {
                    out.skipped
                        .push(format!("digikit/{rel} (this device has no module store)"));
                    continue;
                }
                let bytes = fs::read(entry.path()).map_err(|e| format!("digikit/{rel}: {e}"))?;
                if rel == crate::SELECTION_FILE {
                    // the runtime's selection record, not a store file
                    let text = String::from_utf8(bytes)
                        .map_err(|_| format!("digikit/{rel}: not UTF-8 text"))?;
                    let paths = crate::parse_selection_txt(&text)
                        .map_err(|e| format!("digikit/{rel}: {e}"))?;
                    out.tree.selection = Some(paths);
                    continue;
                }
                out.tree.store.push(StoreFile {
                    dir: false,
                    path: rel,
                    bytes,
                });
            }
        }
        // (an empty folder of `digikit/` is not built into the store: a folder with files is implied by their paths, and a store of nothing but
        // an empty folder would make a card "have a store". Folder entries are made by the runtime's MKDIR and are exported, not built.)
    }
    if let Some(paths) = &out.tree.selection {
        for path in paths {
            if !out.tree.store.iter().any(|f| &f.path == path) {
                out.skipped.push(format!(
                    "digikit/{}: {path} is not in the store (the runtime will report NOT FOUND)",
                    crate::SELECTION_FILE
                ));
            }
        }
    }
    Ok(())
}

/// Give the new, empty `file` the NTFS sparse flag. Without it NTFS gives a file all of its clusters at `set_len`,
/// and a 3.7 GB card takes 3.7 GB of disk. Linux and macOS make a hole at `set_len` with no flag. A file system
/// without sparse files (FAT32, exFAT) refuses the call, and the file is then written in full as before.
#[cfg(windows)]
fn mark_sparse(file: &fs::File) {
    use std::os::windows::io::AsRawHandle;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn DeviceIoControl(
            device: *mut std::ffi::c_void,
            code: u32,
            in_buf: *mut std::ffi::c_void,
            in_len: u32,
            out_buf: *mut std::ffi::c_void,
            out_len: u32,
            returned: *mut u32,
            overlapped: *mut std::ffi::c_void,
        ) -> i32;
    }
    const FSCTL_SET_SPARSE: u32 = 0x0009_00C4;
    let mut returned = 0u32;
    // SAFETY: the handle is open for the life of `file`, and the call takes no buffers.
    unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            FSCTL_SET_SPARSE,
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        );
    }
}

#[cfg(not(windows))]
fn mark_sparse(_file: &fs::File) {}

/// Write `image` to a new file `path` (sparse: only the written ranges are written); the number of bytes written.
/// Goes through `<path>.part` and a rename, so a crash never leaves a half image under the final name. `path` must
/// not exist.
pub fn write_image_file(image: &SparseImage, path: &Path) -> Result<u64, String> {
    if path.exists() {
        return Err(format!("{} already exists", path.display()));
    }
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let mut partial: PathBuf = path.as_os_str().to_owned().into();
    partial.as_mut_os_string().push(".part");
    let _ = fs::remove_file(&partial);
    let written = (|| -> Result<u64, String> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&partial)
            .map_err(|e| format!("{}: {e}", partial.display()))?;
        mark_sparse(&file);
        file.set_len(image.logical_len())
            .map_err(|e| e.to_string())?;
        let mut bytes = 0u64;
        for (&at, data) in image.ranges() {
            file.seek(SeekFrom::Start(at))
                .and_then(|_| file.write_all(data))
                .map_err(|e| e.to_string())?;
            bytes += data.len() as u64;
        }
        file.flush().map_err(|e| e.to_string())?;
        Ok(bytes)
    })();
    match written {
        Ok(bytes) => {
            fs::rename(&partial, path).map_err(|e| format!("{}: {e}", path.display()))?;
            Ok(bytes)
        }
        Err(error) => {
            let _ = fs::remove_file(&partial);
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{card_layout, dt2_layout};

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("plusdrive-folder-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(windows)]
    #[test]
    fn image_file_is_sparse_on_ntfs() {
        use std::os::windows::fs::MetadataExt;
        let dir = scratch("sparse");
        let path = dir.join("card.img");
        let mut image = SparseImage::new(1 << 30);
        image.write_at(1 << 21, &[0xa5; 512]).unwrap();
        assert_eq!(write_image_file(&image, &path).unwrap(), 512);
        let meta = fs::metadata(&path).unwrap();
        assert_eq!(meta.len(), 1 << 30);
        assert_ne!(
            meta.file_attributes() & 0x200,
            0,
            "FILE_ATTRIBUTE_SPARSE_FILE"
        );
        let mut file = fs::File::open(&path).unwrap();
        let mut sector = [0u8; 512];
        for (at, want) in [(1u64 << 21, 0xa5u8), (1 << 29, 0)] {
            file.seek(SeekFrom::Start(at)).unwrap();
            std::io::Read::read_exact(&mut file, &mut sector).unwrap();
            assert!(sector.iter().all(|&b| b == want), "sector at {at:#x}");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn selection_txt_is_the_record_and_not_a_store_file() {
        let dir = scratch("selection");
        for (path, bytes) in [
            ("digikit/modules/a.dkm", &b"module"[..]),
            (
                "digikit/selection.txt",
                b"# sel\nmodules/a.dkm\nmodules/gone.dkm\n",
            ),
        ] {
            let p = dir.join(path);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, bytes).unwrap();
        }
        let scan = scan_folder(&dir, &dt2_layout(), &mut |_, _| {}).unwrap();
        assert_eq!(
            scan.tree.selection.as_deref(),
            Some(&["modules/a.dkm".to_string(), "modules/gone.dkm".to_string()][..])
        );
        let store: Vec<_> = scan.tree.store.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(store, ["modules/a.dkm"]);
        assert_eq!(scan.skipped.len(), 1, "{:?}", scan.skipped);
        assert!(
            scan.skipped[0].contains("modules/gone.dkm"),
            "{:?}",
            scan.skipped
        );
        // a bad file stops the build with the line
        fs::write(
            dir.join("digikit/selection.txt"),
            format!("{}\n", "x".repeat(60)),
        )
        .unwrap();
        let err = scan_folder(&dir, &dt2_layout(), &mut |_, _| {})
            .err()
            .unwrap();
        assert!(
            err.contains("selection.txt") && err.contains("line 1"),
            "{err}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn maps_the_folder_contract() {
        let dir = scratch("scan");
        for (path, bytes) in [
            ("notes.txt", &b"hi"[..]),
            ("a/b/c.bin", b"deep"),
            (".mmcfs/projects/0003.bin", b"proj"),
            (".mmcfs/boot/040000.bin", b"boot"),
            (".mmcfs/boot/999999.bin", b"nope"),
            ("digikit/m/x.dkm", b"module"),
            ("digikit/top.dkm", b"top"),
            (".DS_Store", b"x"),
        ] {
            let p = dir.join(path);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, bytes).unwrap();
        }
        let scan = scan_folder(&dir, &dt2_layout(), &mut |_, _| {}).unwrap();
        let mut paths: Vec<_> = scan.tree.files.iter().map(|f| f.path.as_str()).collect();
        paths.sort();
        assert_eq!(paths, ["a/b/c.bin", "notes.txt"]);
        assert_eq!(scan.tree.slots.slots[&(Pool::Projects, 3)], b"proj");
        assert_eq!(
            scan.tree.regions.keys().copied().collect::<Vec<_>>(),
            [0x40000]
        );
        let mut store: Vec<_> = scan.tree.store.iter().map(|f| f.path.as_str()).collect();
        store.sort();
        assert_eq!(store, ["m/x.dkm", "top.dkm"]);
        assert_eq!(scan.skipped.len(), 1, "{:?}", scan.skipped);
        // DN2: no path filesystem; mmcfs, boot and digikit still apply.
        let dn2 = scan_folder(&dir, &card_layout("dn2").unwrap(), &mut |_, _| {}).unwrap();
        assert!(dn2.tree.files.is_empty() && dn2.tree.dirs.is_empty());
        assert_eq!(dn2.tree.store.len(), 2);
        assert_eq!(dn2.tree.slots.slots.len(), 1);
        let files = folder_files(&dir).unwrap();
        assert!(files.contains(&"digikit/m/x.dkm".to_string()));
        assert!(!files.contains(&".DS_Store".to_string()));
        let _ = fs::remove_dir_all(&dir);
    }
}
