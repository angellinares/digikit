//! The module store as a folder: a mirror of the card's store files in `<folder>/digikit/`, kept up to date while the emulator runs
//! (docs/design/digikit-store.md, "The folder mirror"; docs/plans/card-folder.md).
//!
//! One way only, card to folder. A store file that is new or different on the card is written to `digikit/<path>`; a file the card
//! no longer has is removed from the folder (a card built from the folder at the next start then does not bring it back). The mirror
//! removes only what it has seen on the card in this session (`seen`: the store as the session started, then every file it read), so
//! a session that starts on an empty card (the DN2 has no card file at boot) never removes the files an earlier session left. A
//! replaced or removed file is moved to the backup first. The selection record is not a store file and is not mirrored (the full
//! export writes `digikit/selection.txt`). Nothing is read from the folder into the card.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use device_profile::StoreLayout;
use emmc_card::RandomAccessRead;

/// What one mirror pass did, as store paths (relative to `digikit/`).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MirrorReport {
    pub added: Vec<String>,
    pub replaced: Vec<String>,
    pub removed: Vec<String>,
    pub unchanged: usize,
    /// Store files that could not be mirrored (a path the folder cannot hold, a folder in the way, a failed write).
    pub issues: Vec<String>,
}

/// A file of the folder's `digikit/` tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FolderFile {
    /// Relative to `digikit/`, `/` separated.
    pub path: String,
    pub bytes: u64,
    /// Hex.
    pub hash: String,
}

/// A card with no image file and no writes (the DN2 before anything is written): every read is zero.
pub struct EmptyImage;

impl RandomAccessRead for EmptyImage {
    fn len(&self) -> u64 {
        0
    }
    fn read_at(&self, _offset: u64, out: &mut [u8]) -> usize {
        out.fill(0);
        0
    }
}

/// The files the store of `view` holds, by path. `Ok(empty)` for a card with no store. A store that does not read (a header or table
/// that fails its check, a file whose hash differs: the guest is in the middle of an upload) is an `Err`: try again later.
pub fn store_files(
    view: &dyn RandomAccessRead,
    range: &StoreLayout,
) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let mut files = BTreeMap::new();
    let store = crate::read_store(
        view,
        range.first_sector,
        range.store_limit(),
        range.header_b(),
    )
    .map_err(|e| e.to_string())?;
    for entry in store.iter().flat_map(|s| &s.entries) {
        if !crate::valid_store_path(&entry.path)
            || entry.path.contains('\\')
            || entry.path == crate::SELECTION_FILE
        {
            continue;
        }
        if entry.is_dir {
            continue; // a folder with nothing in it: no file (**[O]** the mirror does not make empty folders yet)
        }
        let bytes =
            crate::read_store_file(view, range.first_sector, entry).map_err(|e| e.to_string())?;
        files.insert(entry.path.clone(), bytes);
    }
    Ok(files)
}

fn move_to_backup(file: &Path, backup: &Path, rel: &str) -> Result<(), String> {
    let to = backup.join("digikit").join(rel);
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    if fs::rename(file, &to).is_err() {
        fs::copy(file, &to).map_err(|e| format!("{}: {e}", file.display()))?;
        fs::remove_file(file).map_err(|e| format!("{}: {e}", file.display()))?;
    }
    Ok(())
}

fn write_atomic(to: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let mut part = to.as_os_str().to_owned();
    part.push(".part");
    let part = PathBuf::from(part);
    fs::write(&part, bytes).map_err(|e| format!("{}: {e}", part.display()))?;
    fs::rename(&part, to).map_err(|e| format!("{}: {e}", to.display()))
}

/// Bring `<folder>/digikit/` up to date with the store of `view`. `seen` is the set of store paths this session has seen on the card
/// (start it with the store of the base image, `store_files` of the card as it was built); it is updated.
pub fn mirror_store(
    view: &dyn RandomAccessRead,
    range: &StoreLayout,
    folder: &Path,
    backup: &Path,
    seen: &mut BTreeSet<String>,
) -> Result<MirrorReport, String> {
    let now = store_files(view, range)?;
    let root = folder.join("digikit");
    let mut report = MirrorReport::default();
    for (path, bytes) in &now {
        let to = root.join(path);
        match fs::read(&to) {
            Ok(have) if &have == bytes => report.unchanged += 1,
            Ok(_) => {
                let step =
                    move_to_backup(&to, backup, path).and_then(|()| write_atomic(&to, bytes));
                match step {
                    Ok(()) => report.replaced.push(path.clone()),
                    Err(e) => report.issues.push(e),
                }
            }
            Err(_) if to.is_dir() => report.issues.push(format!(
                "digikit/{path}: a folder is in the way of the file"
            )),
            Err(_) => match write_atomic(&to, bytes) {
                Ok(()) => report.added.push(path.clone()),
                Err(e) => report.issues.push(e),
            },
        }
    }
    let gone: Vec<String> = seen
        .iter()
        .filter(|p| !now.contains_key(*p))
        .cloned()
        .collect();
    for path in gone {
        let file = root.join(&path);
        if file.is_file() {
            match move_to_backup(&file, backup, &path) {
                Ok(()) => report.removed.push(path.clone()),
                Err(e) => {
                    report.issues.push(e);
                    continue;
                }
            }
            // an emptied folder goes too, up to digikit/
            let mut dir = file.parent().map(Path::to_path_buf);
            while let Some(d) = dir.filter(|d| d.starts_with(&root) && *d != root) {
                if fs::remove_dir(&d).is_err() {
                    break;
                }
                dir = d.parent().map(Path::to_path_buf);
            }
        }
        seen.remove(&path);
    }
    seen.extend(now.keys().cloned());
    Ok(report)
}

/// The files under `<folder>/digikit/` (what the mirror holds), by path; the selection file, hidden files and `.part` files left out.
pub fn folder_files(folder: &Path) -> Vec<FolderFile> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<FolderFile>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || name.ends_with(".part") {
                continue;
            }
            if path.is_dir() {
                walk(&path, root, out);
            } else if let Ok(bytes) = fs::read(&path) {
                let rel = path.strip_prefix(root).unwrap_or(&path);
                let rel = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                if rel == crate::SELECTION_FILE {
                    continue;
                }
                let hash = dksample::file_hash(&bytes)
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect();
                out.push(FolderFile {
                    path: rel,
                    bytes: bytes.len() as u64,
                    hash,
                });
            }
        }
    }
    let root = folder.join("digikit");
    let mut out = vec![];
    walk(&root, &root, &mut out);
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SparseImage, StoreFile, encode_store};

    fn range() -> StoreLayout {
        crate::card_layout("dn2").unwrap().store.unwrap()
    }

    fn card(files: &[(&str, &[u8])]) -> SparseImage {
        let r = range();
        let mut image = SparseImage::new((r.end_sector + 1) * 512);
        let store = encode_store(
            files
                .iter()
                .map(|(p, b)| StoreFile {
                    dir: false,
                    path: (*p).into(),
                    bytes: b.to_vec(),
                })
                .collect(),
            r.store_limit(),
        )
        .unwrap();
        image.write_at(r.first_sector * 512, &store).unwrap();
        image
    }

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("plusdrive-mirror-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn files_written_to_the_store_appear_in_the_folder_and_a_deleted_one_is_removed() {
        let root = temp("add-remove");
        let (folder, backup) = (root.join("plus_drive/dn2"), root.join("backup"));
        let mut seen = BTreeSet::new();
        // an upload: two files
        let a = card(&[
            ("modules/pluck.dkm", b"PLUCK"),
            ("modules/sine.dkm", b"SINE!"),
        ]);
        let r = mirror_store(&a, &range(), &folder, &backup, &mut seen).unwrap();
        assert_eq!(r.added, ["modules/pluck.dkm", "modules/sine.dkm"]);
        assert_eq!(
            fs::read(folder.join("digikit/modules/pluck.dkm")).unwrap(),
            b"PLUCK"
        );
        // nothing changed: nothing written
        let again = mirror_store(&a, &range(), &folder, &backup, &mut seen).unwrap();
        assert_eq!(
            (
                again.added.len(),
                again.replaced.len(),
                again.removed.len(),
                again.unchanged
            ),
            (0, 0, 0, 2)
        );
        // a file replaced on the card (new bytes, same path): the folder follows, the old file is in the backup
        let b = card(&[
            ("modules/pluck.dkm", b"PLUCK2"),
            ("modules/sine.dkm", b"SINE!"),
        ]);
        let r = mirror_store(&b, &range(), &folder, &backup, &mut seen).unwrap();
        assert_eq!(r.replaced, ["modules/pluck.dkm"]);
        assert_eq!(
            fs::read(folder.join("digikit/modules/pluck.dkm")).unwrap(),
            b"PLUCK2"
        );
        assert_eq!(
            fs::read(backup.join("digikit/modules/pluck.dkm")).unwrap(),
            b"PLUCK"
        );
        // a file deleted on the card: removed from the folder (moved to the backup), the other stays
        let c = card(&[("modules/sine.dkm", b"SINE!")]);
        let r = mirror_store(&c, &range(), &folder, &backup, &mut seen).unwrap();
        assert_eq!(r.removed, ["modules/pluck.dkm"]);
        assert!(!folder.join("digikit/modules/pluck.dkm").exists());
        assert!(folder.join("digikit/modules/sine.dkm").is_file());
        assert_eq!(
            fs::read(backup.join("digikit/modules/pluck.dkm")).unwrap(),
            b"PLUCK2"
        );
        // every file deleted: an empty store is a store, the folder empties (and the emptied folder goes)
        let d = card(&[]);
        let r = mirror_store(&d, &range(), &folder, &backup, &mut seen).unwrap();
        assert_eq!(r.removed, ["modules/sine.dkm"]);
        assert!(!folder.join("digikit/modules").exists());
        assert!(seen.is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_session_that_never_saw_a_file_on_the_card_does_not_remove_it_from_the_folder() {
        // the DN2 starts on an empty card: the files an earlier session left in the folder stay until the card has had them
        let root = temp("blank-start");
        let folder = root.join("plus_drive/dn2");
        fs::create_dir_all(folder.join("digikit/modules")).unwrap();
        fs::write(folder.join("digikit/modules/old.dkm"), b"OLD").unwrap();
        fs::write(folder.join("digikit/selection.txt"), b"modules/old.dkm\n").unwrap();
        let blank = SparseImage::new((range().end_sector + 1) * 512);
        let mut seen = BTreeSet::new();
        let r = mirror_store(&blank, &range(), &folder, &root.join("backup"), &mut seen).unwrap();
        assert_eq!(r, MirrorReport::default());
        assert!(folder.join("digikit/modules/old.dkm").is_file());
        // a card built from the folder (the DT2 start): the store as built is `seen`, so a later delete removes the file
        let built = card(&[("modules/old.dkm", b"OLD")]);
        seen.extend(store_files(&built, &range()).unwrap().into_keys());
        let r = mirror_store(&blank, &range(), &folder, &root.join("backup"), &mut seen).unwrap();
        assert_eq!(r.removed, ["modules/old.dkm"]);
        assert!(!folder.join("digikit/modules/old.dkm").exists());
        // the selection file is not a store file: the mirror leaves it alone
        assert!(folder.join("digikit/selection.txt").is_file());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_store_that_does_not_read_is_an_error_to_retry_and_the_folder_is_left_alone() {
        let root = temp("torn");
        let folder = root.join("f");
        fs::create_dir_all(folder.join("digikit/modules")).unwrap();
        fs::write(folder.join("digikit/modules/a.dkm"), b"A").unwrap();
        let r = range();
        let mut bytes = encode_store(
            vec![StoreFile {
                dir: false,
                path: "modules/a.dkm".into(),
                bytes: b"A".to_vec(),
            }],
            r.store_limit(),
        )
        .unwrap();
        bytes[512 + 3] ^= 0xff; // a torn table: the guest is in the middle of an upload
        let mut image = SparseImage::new((r.end_sector + 1) * 512);
        image.write_at(r.first_sector * 512, &bytes).unwrap();
        let mut seen: BTreeSet<String> = ["modules/a.dkm".to_string()].into();
        assert!(mirror_store(&image, &range(), &folder, &root.join("b"), &mut seen).is_err());
        assert!(folder.join("digikit/modules/a.dkm").is_file());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_folder_listing_has_path_size_and_hash_without_the_selection_file() {
        let root = temp("listing");
        let folder = root.join("f");
        fs::create_dir_all(folder.join("digikit/modules")).unwrap();
        fs::write(folder.join("digikit/modules/a.dkm"), b"abc").unwrap();
        fs::write(folder.join("digikit/selection.txt"), b"x").unwrap();
        fs::write(folder.join("digikit/modules/b.dkm.part"), b"x").unwrap();
        let files = folder_files(&folder);
        assert_eq!(
            files,
            vec![FolderFile {
                path: "modules/a.dkm".into(),
                bytes: 3,
                hash: format!("49bec845{}", "0".repeat(56))
            }]
        );
        assert!(folder_files(&root.join("nothing")).is_empty());
        let _ = fs::remove_dir_all(&root);
    }
}
