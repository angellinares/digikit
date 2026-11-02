//! Export what the firmware wrote to the +Drive back into a `plus_drive/<device>/` folder, byte for byte.
//!
//! Folder layout (plan decision 6, docs/findings/24-plusdrive-export.md): path-filesystem files and folders by their names in
//! the firmware's native format; `.mmcfs/{projects,sounds,kits}/NNNN.bin` for the occupied MmcFs slots;
//! `.mmcfs/boot/SSSSSS.bin` for a 16 MiB region outside the pools (sector in hex: the two boot copies 040000 and
//! 048000) that the guest wrote. The working project (050000) is left out unless `include_working_project` is set:
//! the firmware rewrites it from the boot copy at every start.
//!
//! Nothing is lost: the whole tree is first written to `<out>/export/<stamp>/`; then only files that are new or
//! differ are copied into the folder, and each replaced file is moved to `<out>/backup/<stamp>/` first. Files the
//! firmware deleted are listed and kept, unless `prune` is set (then they are moved to the backup too).

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use emmc_card::{CardOverlaySector, RandomAccessRead};

use crate::{
    CardLayout, Fs, OverlayImage, RegionRole, WavLimits, parse_overlay, read_mmcfs, wav_to_native,
};

#[derive(Clone, Debug)]
pub struct Options {
    /// Holds `export/<stamp>/` and `backup/<stamp>/`.
    pub out_root: PathBuf,
    /// Move the files the firmware deleted to the backup.
    pub prune: bool,
    /// Fixed stamp (tests); default: the UTC time now.
    pub stamp: Option<String>,
    /// Also export the working project (`.mmcfs/boot/050000.bin`). Default: no. The firmware rewrites it from the
    /// boot copy at every start (finding 24), so a copy in the folder is replaced on the card at the next boot
    /// and only makes every later export differ.
    pub include_working_project: bool,
}

impl Options {
    pub fn new(out_root: impl Into<PathBuf>) -> Self {
        Self {
            out_root: out_root.into(),
            prune: false,
            stamp: None,
            include_working_project: false,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub export_dir: PathBuf,
    pub backup_dir: PathBuf,
    /// Relative paths copied into the folder (new files).
    pub added: Vec<String>,
    /// Relative paths whose bytes changed (the old file is in the backup).
    pub replaced: Vec<String>,
    pub unchanged: Vec<String>,
    /// In the folder but not on the card any more.
    pub deleted_by_firmware: Vec<String>,
    /// The subset of `deleted_by_firmware` moved to the backup (with `prune`).
    pub pruned: Vec<String>,
    /// Module store files (`digikit/...`) that changed on the card (an upload, a delete) but were also changed in the folder after
    /// the card was built: the folder file is kept; the card's version is in `export/<stamp>/`.
    pub kept: Vec<String>,
    pub issues: Vec<String>,
}

impl Report {
    pub fn summary(&self) -> String {
        format!(
            "added {} replaced {} unchanged {} deleted_by_firmware {} pruned {} kept {} issues {}\nexport {}\nbackup {}",
            self.added.len(),
            self.replaced.len(),
            self.unchanged.len(),
            self.deleted_by_firmware.len(),
            self.pruned.len(),
            self.kept.len(),
            self.issues.len(),
            self.export_dir.display(),
            self.backup_dir.display()
        )
    }
    pub fn text(&self) -> String {
        let mut out = self.summary();
        for (title, list) in [
            ("added", &self.added),
            ("replaced", &self.replaced),
            ("unchanged", &self.unchanged),
            ("deleted by the firmware", &self.deleted_by_firmware),
            ("pruned", &self.pruned),
            (
                "kept (changed in the folder after the card was built)",
                &self.kept,
            ),
            ("issues", &self.issues),
        ] {
            out.push_str(&format!("\n\n{title} ({})\n", list.len()));
            for line in list {
                out.push_str(&format!("  {line}\n"));
            }
        }
        out
    }
}

// ----- names --------------------------------------------------------------------------------------------------

/// The name the builder gives a sample file: `.wav` dropped, non-ASCII as `?`, at most 255 bytes.
fn builder_name(file_name: &str) -> Vec<u8> {
    let stem = if file_name.len() >= 4
        && file_name.as_bytes()[file_name.len() - 4..].eq_ignore_ascii_case(b".wav")
    {
        &file_name[..file_name.len() - 4]
    } else {
        file_name
    };
    stem.chars()
        .map(|c| if c.is_ascii() { c as u8 } else { b'?' })
        .take(255)
        .collect()
}

fn utc_stamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (days, rem) = (secs / 86400, secs % 86400);
    // civil from days (Howard Hinnant)
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn unique_dir(root: &Path, kind: &str, stamp: &str) -> PathBuf {
    let base = root.join(kind);
    let mut dir = base.join(stamp);
    let mut n = 1;
    while dir.exists() {
        dir = base.join(format!("{stamp}-{n}"));
        n += 1;
    }
    dir
}

fn same_bytes(a: &Path, b: &Path) -> bool {
    let (Ok(ma), Ok(mb)) = (fs::metadata(a), fs::metadata(b)) else {
        return false;
    };
    if !ma.is_file() || !mb.is_file() || ma.len() != mb.len() {
        return false;
    }
    let (Ok(mut fa), Ok(mut fb)) = (fs::File::open(a), fs::File::open(b)) else {
        return false;
    };
    let (mut x, mut y) = (vec![0u8; 1 << 20], vec![0u8; 1 << 20]);
    loop {
        let Ok(n) = fa.read(&mut x) else { return false };
        if n == 0 {
            return true;
        }
        if fb.read_exact(&mut y[..n]).is_err() || x[..n] != y[..n] {
            return false;
        }
    }
}

fn move_file(from: &Path, to: &Path) -> Result<(), String> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    if fs::rename(from, to).is_ok() {
        return Ok(());
    }
    fs::copy(from, to).map_err(|e| format!("{} -> {}: {e}", from.display(), to.display()))?;
    fs::remove_file(from).map_err(|e| format!("{}: {e}", from.display()))
}

fn copy_atomic(from: &Path, to: &Path) -> Result<(), String> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let mut part = to.as_os_str().to_owned();
    part.push(".part");
    let part = PathBuf::from(part);
    fs::copy(from, &part).map_err(|e| format!("{} -> {}: {e}", from.display(), part.display()))?;
    fs::rename(&part, to).map_err(|e| format!("{}: {e}", to.display()))
}

// ----- the export ---------------------------------------------------------------------------------------------

/// What the firmware's card holds, as relative paths below the folder.
struct Staged {
    /// Relative path (with `/`) of every file written to the export directory.
    files: BTreeSet<String>,
    dirs: BTreeSet<String>,
    /// MmcFs was present on the card (so a missing slot file means a freed slot).
    mmcfs: bool,
}

fn rel_string(parts: &[Vec<u8>]) -> String {
    parts
        .iter()
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// The selection the card holds: both copies read, the valid one with the higher `seq`. `None`: no store or no valid record.
fn card_selection(img: &dyn RandomAccessRead, layout: &CardLayout) -> Option<Vec<String>> {
    let store = layout.store.as_ref()?;
    if store.selection_sectors < 2 {
        return None;
    }
    let copy = |sector: u64| {
        let mut buf = [0u8; crate::SECTOR];
        img.read_at(sector * crate::SECTOR as u64, &mut buf);
        crate::decode_selection(&buf)
    };
    crate::current_selection(copy(store.end_sector - 2), copy(store.end_sector - 1))
        .map(|s| s.paths)
}

fn stage_file(dir: &Path, rel: &str, bytes: &[u8]) -> Result<(), String> {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    fs::write(&path, bytes).map_err(|e| format!("{}: {e}", path.display()))
}

/// Write the card's tree and slots to `export_dir`.
fn stage(
    view: &OverlayImage<'_>,
    layout: &CardLayout,
    export_dir: &Path,
    include_working_project: bool,
    report: &mut Report,
) -> Result<Staged, String> {
    let mut staged = Staged {
        files: BTreeSet::new(),
        dirs: BTreeSet::new(),
        mmcfs: false,
    };
    fs::create_dir_all(export_dir).map_err(|e| format!("{}: {e}", export_dir.display()))?;
    // A device without a path filesystem (DN2) has no tree to export.
    let path_fs = layout.path_fs.as_ref().map(|_| Fs::open(view));
    match path_fs {
        None => {}
        Some(Err(e)) => report.issues.push(e.to_string()),
        Some(Ok(fs_view)) => {
            let walk = fs_view.walk();
            report.issues.extend(walk.issues.iter().cloned());
            // The firmware links its RAM-resident "factory" tree into the root at every mount: expected, not on the card.
            for name in walk
                .ram_entries
                .iter()
                .filter(|n| n.as_slice() != b"factory")
            {
                report.issues.push(format!(
                    "{}: RAM-resident entry (not on the card), skipped",
                    String::from_utf8_lossy(name)
                ));
            }
            // Names that collide on a case-insensitive disk, or with the reserved folders, get the id appended.
            let mut taken: BTreeSet<String> = BTreeSet::new();
            let mut rename: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
            for node in &walk.nodes {
                let parts: Vec<&[u8]> = node.path.split(|&b| b == b'/').collect();
                let mut mapped: Vec<Vec<u8>> = Vec::new();
                for i in 0..parts.len() {
                    let prefix = node
                        .path
                        .split(|&b| b == b'/')
                        .take(i + 1)
                        .collect::<Vec<_>>()
                        .join(&b'/');
                    if let Some(known) = rename.get(&prefix) {
                        mapped.push(known.clone());
                        continue;
                    }
                    let last = i + 1 == parts.len();
                    let mut name: Vec<u8> = parts[i]
                        .iter()
                        .map(|&b| {
                            if b == 0 || b == b'/' || b == b'\\' {
                                b'_'
                            } else {
                                b
                            }
                        })
                        .collect();
                    if name.is_empty() {
                        name = b"_".to_vec();
                    }
                    let full = |n: &[u8]| {
                        let mut p = mapped.clone();
                        p.push(n.to_vec());
                        rel_string(&p).to_lowercase()
                    };
                    let reserved = i == 0
                        && (name.starts_with(b".mmcfs")
                            || name == b".preview"
                            || name == b"digikit");
                    if (reserved || taken.contains(&full(&name))) && last {
                        let id = node.record.id;
                        name.extend_from_slice(format!("~{id}").as_bytes());
                        report.issues.push(format!(
                            "{}: name collides (case or reserved); exported as {}",
                            String::from_utf8_lossy(&node.path),
                            String::from_utf8_lossy(&name)
                        ));
                    }
                    taken.insert(full(&name));
                    rename.insert(prefix, name.clone());
                    mapped.push(name);
                }
                let rel = rel_string(&mapped);
                if node.record.is_dir() {
                    fs::create_dir_all(export_dir.join(&rel)).map_err(|e| format!("{rel}: {e}"))?;
                    staged.dirs.insert(rel);
                    continue;
                }
                let mut issues = Vec::new();
                match fs_view.read_content(&node.record, &mut issues) {
                    Ok(bytes) => {
                        stage_file(export_dir, &rel, &bytes)?;
                        staged.files.insert(rel.clone());
                    }
                    Err(e) => report.issues.push(format!("{rel}: {e}")),
                }
                report
                    .issues
                    .extend(issues.into_iter().map(|i| format!("{rel}: {i}")));
            }
        }
    }
    // The runtime's selection record (docs/design/digikit-store.md): the copy it would use goes to `digikit/selection.txt`.
    if let Some(paths) = card_selection(view, layout) {
        let rel = format!("digikit/{}", crate::SELECTION_FILE);
        stage_file(
            export_dir,
            &rel,
            crate::format_selection_txt(&paths).as_bytes(),
        )?;
        staged.files.insert(rel);
    }
    let mmc = read_mmcfs(view, layout, Some(view));
    report.issues.extend(mmc.issues.iter().cloned());
    staged.mmcfs = mmc.present;
    for slot in &mmc.slots {
        let rel = format!(".mmcfs/{}/{:04}.bin", slot.pool.dir_name(), slot.index);
        stage_file(export_dir, &rel, &slot.bytes)?;
        staged.files.insert(rel);
    }
    for (sector, bytes) in &mmc.boot {
        if !include_working_project
            && layout
                .region_at(*sector)
                .is_some_and(|r| r.role == RegionRole::WorkingProject)
        {
            continue;
        }
        let rel = format!(".mmcfs/boot/{sector:06x}.bin");
        stage_file(export_dir, &rel, bytes)?;
        staged.files.insert(rel);
    }
    Ok(staged)
}

fn list_files(folder: &Path, rel: &str, out: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(folder.join(rel)) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') && name != ".mmcfs" {
            continue;
        }
        // `digikit/` is the module store's: `store_pass` handles it, three-way, and the selection record is staged by itself.
        if rel.is_empty() && name == "digikit" {
            continue;
        }
        let child = if rel.is_empty() {
            name
        } else {
            format!("{rel}/{name}")
        };
        match entry.file_type() {
            Ok(t) if t.is_dir() => list_files(folder, &child, out),
            Ok(t) if t.is_file() => out.push(child),
            _ => {}
        }
    }
}

/// Export the card (`base` plus `overlay`) into `folder`; see the module notes.
pub fn export(
    base: &dyn RandomAccessRead,
    overlay: &[CardOverlaySector],
    folder: &Path,
    layout: &CardLayout,
    options: &Options,
) -> Result<Report, String> {
    let stamp = options.stamp.clone().unwrap_or_else(utc_stamp);
    let view = OverlayImage::new(base, overlay);
    let export_dir = unique_dir(&options.out_root, "export", &stamp);
    let backup_dir = unique_dir(&options.out_root, "backup", &stamp);
    let mut report = Report {
        export_dir: export_dir.clone(),
        backup_dir: backup_dir.clone(),
        ..Report::default()
    };
    let staged = stage(
        &view,
        layout,
        &export_dir,
        options.include_working_project,
        &mut report,
    )?;
    fs::create_dir_all(folder).map_err(|e| format!("{}: {e}", folder.display()))?;

    // Sample sources in the folder: `x.wav` stands for the native file `x` when it converts to the same bytes.
    let mut existing = Vec::new();
    list_files(folder, "", &mut existing);
    let mut wav_for: BTreeMap<String, String> = BTreeMap::new();
    for rel in &existing {
        if rel.to_ascii_lowercase().ends_with(".wav") {
            let (dir, file) = rel.rsplit_once('/').unwrap_or(("", rel));
            let key = String::from_utf8_lossy(&builder_name(file)).into_owned();
            wav_for.insert(
                if dir.is_empty() {
                    key
                } else {
                    format!("{dir}/{key}")
                },
                rel.clone(),
            );
        }
    }
    let mut covered_wavs: BTreeSet<String> = BTreeSet::new();

    for rel in &staged.dirs {
        fs::create_dir_all(folder.join(rel)).map_err(|e| format!("{rel}: {e}"))?;
    }
    for rel in &staged.files {
        let from = export_dir.join(rel);
        let to = folder.join(rel);
        if *rel == format!("digikit/{}", crate::SELECTION_FILE) {
            // the same list in a file with the user's comments and spacing is unchanged
            let want = fs::read_to_string(&from)
                .ok()
                .and_then(|t| crate::parse_selection_txt(&t).ok());
            let have = fs::read_to_string(&to)
                .ok()
                .and_then(|t| crate::parse_selection_txt(&t).ok());
            if want.is_some() && want == have {
                report.unchanged.push(rel.clone());
                continue;
            }
        }
        if let Some(wav_rel) = wav_for.get(rel) {
            // The folder holds the source WAV: unchanged when it still builds to these bytes.
            let converted = fs::read(folder.join(wav_rel))
                .ok()
                .and_then(|w| wav_to_native(&w, WavLimits::default()).ok())
                .map(|n| n.bytes);
            covered_wavs.insert(wav_rel.clone());
            if converted.is_some() && converted == fs::read(&from).ok() {
                report.unchanged.push(format!("{rel} (from {wav_rel})"));
                continue;
            }
            // The firmware changed the sample: the WAV would win over the native file at the next build.
            move_file(&folder.join(wav_rel), &backup_dir.join(wav_rel))?;
            report
                .replaced
                .push(format!("{wav_rel} (now native {rel})"));
            copy_atomic(&from, &to)?;
            continue;
        }
        if to.is_dir() {
            report.issues.push(format!(
                "{rel}: a folder is in the way of the file; left alone"
            ));
            continue;
        }
        if to.is_file() {
            if same_bytes(&from, &to) {
                report.unchanged.push(rel.clone());
                continue;
            }
            move_file(&to, &backup_dir.join(rel))?;
            copy_atomic(&from, &to)?;
            report.replaced.push(rel.clone());
        } else {
            // a file where the card has a folder in the path: back it up first
            let mut at = PathBuf::new();
            for part in Path::new(rel)
                .parent()
                .into_iter()
                .flat_map(Path::components)
            {
                at.push(part);
                let p = folder.join(&at);
                if p.is_file() {
                    move_file(&p, &backup_dir.join(&at))?;
                    report.replaced.push(at.to_string_lossy().into_owned());
                }
            }
            copy_atomic(&from, &to)?;
            report.added.push(rel.clone());
        }
    }

    for rel in &existing {
        if staged.files.contains(rel) || covered_wavs.contains(rel) {
            continue;
        }
        if rel.starts_with(".mmcfs/") && !staged.mmcfs {
            continue;
        }
        // a boot project copy the guest did not touch is not re-exported; it is not deleted either
        if rel.starts_with(".mmcfs/boot/") {
            continue;
        }
        // a WAV whose native sample is on the card was handled above; one with no card file was deleted
        report.deleted_by_firmware.push(rel.clone());
        if options.prune {
            move_file(&folder.join(rel), &backup_dir.join(rel))?;
            report.pruned.push(rel.clone());
        }
    }
    store_pass(
        base,
        &view,
        layout,
        folder,
        &export_dir,
        &backup_dir,
        options.prune,
        &mut report,
    )?;
    fs::write(export_dir.join("REPORT.txt"), report.text())
        .map_err(|e| format!("REPORT.txt: {e}"))?;
    Ok(report)
}

/// The module store (docs/design/digikit-store.md, "+Drive round trip"): the files the card holds now go to `digikit/<path>`, so an
/// upload survives the next card build. Three-way: BASE is the card as it was built, VIEW is the card now, the folder is what the
/// user has. Only what the device changed is acted on: a store file that is new or different on the card is added to the folder or
/// replaces the folder's copy when that still equals the built one (the old file goes to the backup); when the folder's copy was
/// changed after the card was built it is kept and reported in `kept`. A store file the card no longer has is listed in
/// `deleted_by_firmware` when the folder still holds the built version (moved to the backup with `prune`), and in `kept` when the
/// folder's copy differs from it.
#[allow(clippy::too_many_arguments)]
fn store_pass(
    base: &dyn RandomAccessRead,
    view: &OverlayImage<'_>,
    layout: &CardLayout,
    folder: &Path,
    export_dir: &Path,
    backup_dir: &Path,
    prune: bool,
    report: &mut Report,
) -> Result<(), String> {
    let Some(range) = layout.store.as_ref() else {
        return Ok(());
    };
    let size = range.store_limit();
    let read = |img: &dyn RandomAccessRead, what: &str, report: &mut Report| {
        let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        let mut dirs: Vec<String> = Vec::new();
        match crate::read_store(img, range.first_sector, size, range.header_b()) {
            Ok(None) => {}
            Err(e) => report.issues.push(format!("module store ({what}): {e}")),
            Ok(Some(store)) => {
                for entry in &store.entries {
                    if !crate::valid_store_path(&entry.path)
                        || entry.path.contains('\\')
                        || entry.path == crate::SELECTION_FILE
                    {
                        report.issues.push(format!(
                            "module store ({what}): {:?} is not a usable folder path, skipped",
                            entry.path
                        ));
                        continue;
                    }
                    if entry.is_dir {
                        dirs.push(entry.path.clone());
                        continue;
                    }
                    match crate::read_store_file(img, range.first_sector, entry) {
                        Ok(bytes) => {
                            files.insert(entry.path.clone(), bytes);
                        }
                        Err(e) => report.issues.push(e.to_string()),
                    }
                }
            }
        }
        (files, dirs)
    };
    let (built, built_dirs) = read(base, "as built", report);
    let (now, now_dirs) = read(view, "now", report);
    // a folder with nothing in it that the card made: the folder exists on disk too
    for d in &now_dirs {
        if !built_dirs.contains(d) {
            let to = folder.join("digikit").join(d);
            if !to.exists() {
                fs::create_dir_all(&to).map_err(|e| format!("{}: {e}", to.display()))?;
            }
        }
    }
    let on_disk = |rel: &str| fs::read(folder.join("digikit").join(rel)).ok();
    for (path, bytes) in &now {
        let rel = format!("digikit/{path}");
        let device_changed = built.get(path) != Some(bytes);
        if !device_changed {
            continue;
        }
        stage_file(export_dir, &rel, bytes)?;
        let to = folder.join(&rel);
        match on_disk(path) {
            None if to.is_dir() => report.issues.push(format!(
                "{rel}: a folder is in the way of the file; left alone"
            )),
            None => {
                copy_atomic(&export_dir.join(&rel), &to)?;
                report.added.push(rel);
            }
            Some(have) if &have == bytes => report.unchanged.push(rel),
            Some(have) if built.get(path) == Some(&have) => {
                move_file(&to, &backup_dir.join(&rel))?;
                copy_atomic(&export_dir.join(&rel), &to)?;
                report.replaced.push(rel);
            }
            Some(_) => report.kept.push(rel),
        }
    }
    for (path, was) in &built {
        if now.contains_key(path) {
            continue;
        }
        let rel = format!("digikit/{path}");
        match on_disk(path) {
            None => {}
            Some(have) if &have == was => {
                report.deleted_by_firmware.push(rel.clone());
                if prune {
                    move_file(&folder.join(&rel), &backup_dir.join(&rel))?;
                    report.pruned.push(rel);
                }
            }
            Some(_) => report.kept.push(rel),
        }
    }
    Ok(())
}

// ----- command line -------------------------------------------------------------------------------------------

/// A card image file read through `RandomAccessRead`.
pub struct FileImage {
    file: Mutex<fs::File>,
    len: u64,
}
impl FileImage {
    pub fn open(path: &Path) -> Result<Self, String> {
        let meta = fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let file = fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Self {
            file: Mutex::new(file),
            len: meta.len(),
        })
    }
}
impl RandomAccessRead for FileImage {
    fn len(&self) -> u64 {
        self.len
    }
    fn read_at(&self, offset: u64, out: &mut [u8]) -> usize {
        if offset >= self.len {
            return 0;
        }
        let n = usize::try_from((self.len - offset).min(out.len() as u64)).unwrap_or(out.len());
        let Ok(mut file) = self.file.lock() else {
            return 0;
        };
        file.seek(SeekFrom::Start(offset))
            .and_then(|_| file.read_exact(&mut out[..n]))
            .map(|_| n)
            .unwrap_or(0)
    }
}

/// Run `f` on the card the guest sees: the image file, with the writes of a `card.overlay` file over it when given.
pub fn with_overlay<T>(
    image: &Path,
    overlay: Option<&Path>,
    f: impl FnOnce(&dyn RandomAccessRead) -> Result<T, String>,
) -> Result<T, String> {
    let base = FileImage::open(image)?;
    let Some(path) = overlay else { return f(&base) };
    let sectors = parse_overlay(&fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?)
        .map_err(|e| e.to_string())?;
    f(&OverlayImage::new(&base, &sectors))
}

pub const EXPORT_USAGE: &str = "usage: plusdrive export IMAGE [--overlay FILE] -o FOLDER [--out DIR] [--prune] [--with-working-project] [--device dt2|dn2]\n  \
     IMAGE   card image (sparse file); --overlay: a `card.overlay` file of the guest's writes\n  \
     -o      the plus_drive/<device> folder to bring up to date\n  \
     --out   holds export/<stamp>/ and backup/<stamp>/ (default out/plusdrive)\n  \
     --prune move files the firmware deleted to the backup (default: list and keep)\n  \
     --with-working-project  also export .mmcfs/boot/050000.bin (default: no; the firmware rewrites it at every start)\n  \
     --device the card's device (default dt2)";

pub fn cli(args: Vec<String>) -> Result<(), String> {
    let (mut image, mut overlay, mut folder, mut out, mut prune) =
        (None, None, None, PathBuf::from("out/plusdrive"), false);
    let mut device = String::from("dt2");
    let mut working = false;
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--overlay" => {
                overlay = Some(PathBuf::from(it.next().ok_or("missing --overlay value")?))
            }
            "-o" => folder = Some(PathBuf::from(it.next().ok_or("missing -o value")?)),
            "--out" => out = PathBuf::from(it.next().ok_or("missing --out value")?),
            "--prune" => prune = true,
            "--with-working-project" => working = true,
            "--device" => device = it.next().ok_or("missing --device value")?,
            _ if image.is_none() && !arg.starts_with('-') => image = Some(PathBuf::from(arg)),
            _ => return Err(EXPORT_USAGE.into()),
        }
    }
    let (image, folder) = (image.ok_or(EXPORT_USAGE)?, folder.ok_or(EXPORT_USAGE)?);
    let base = FileImage::open(&image)?;
    let sectors = match overlay {
        Some(path) => {
            parse_overlay(&fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?)
                .map_err(|e| e.to_string())?
        }
        None => Vec::new(),
    };
    let mut options = Options::new(out);
    options.prune = prune;
    options.include_working_project = working;
    let layout = crate::card_layout(&device)?;
    let report = export(&base, &sectors, &folder, &layout, &options)?;
    println!("{}", report.text());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NamedSample, Pool, SparseImage, build_sample_image, card_layout, dt2_layout};

    fn wav(frames: usize, seed: u8) -> Vec<u8> {
        let data: Vec<u8> = (0..frames)
            .flat_map(|i| (((i as i32 * 37 + seed as i32) % 2000 - 1000) as i16).to_le_bytes())
            .collect();
        let mut out = b"RIFF".to_vec();
        out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        for field in [16u32, 0x0001_0001, 48_000, 96_000, 0x0010_0002] {
            out.extend_from_slice(&field.to_le_bytes());
        }
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&data);
        out
    }

    #[test]
    fn with_overlay_reads_the_image_under_the_masked_overlay_bytes() {
        let dir = temp("with-overlay");
        let (image, overlay) = (dir.join("card.img"), dir.join("overlay.bin"));
        fs::write(&image, vec![7u8; 1024]).unwrap();
        let mut sector = CardOverlaySector {
            sector: 1,
            written: [0; 64],
            data: [9; 512],
        };
        sector.written[0] = 0b11; // bytes 0 and 1 only
        fs::write(&overlay, crate::encode_overlay(&[sector])).unwrap();
        let read = |overlay: Option<&Path>| {
            with_overlay(&image, overlay, |view| {
                let mut out = [0u8; 4];
                view.read_at(510, &mut out);
                Ok(out)
            })
            .unwrap()
        };
        assert_eq!(read(None), [7, 7, 7, 7]);
        assert_eq!(read(Some(&overlay)), [7, 7, 9, 9]);
    }

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("plusdrive-export-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn card_of(wavs: &[(&str, Vec<u8>)]) -> SparseImage {
        let samples = wavs
            .iter()
            .map(|(n, w)| NamedSample {
                file_name: (*n).into(),
                sample: wav_to_native(w, WavLimits::default()).unwrap(),
            })
            .collect();
        // the whole card: the store is at its end
        crate::build_sample_image_with_capacity(samples, dt2_layout().total_sectors as u32)
            .unwrap()
            .image
    }

    fn area() -> device_profile::StoreLayout {
        dt2_layout().store.unwrap()
    }

    #[test]
    fn export_of_a_built_card_matches_the_input_and_keeps_wavs() {
        let root = temp("roundtrip");
        let folder = root.join("plus_drive/dt2");
        fs::create_dir_all(&folder).unwrap();
        let wavs = [("kick.wav", wav(3000, 1)), ("snare.WAV", wav(40_000, 2))];
        for (n, w) in &wavs {
            fs::write(folder.join(n), w).unwrap();
        }
        let image = card_of(&wavs);
        let mut options = Options::new(root.join("out"));
        options.stamp = Some("t1".into());
        let report = export(&image, &[], &folder, &dt2_layout(), &options).unwrap();
        assert!(report.issues.is_empty(), "{:?}", report.issues);
        assert_eq!(report.unchanged.len(), 2, "{report:?}");
        assert!(
            report.added.is_empty()
                && report.replaced.is_empty()
                && report.deleted_by_firmware.is_empty()
        );
        // the staged tree holds the native files, byte for byte
        for (n, w) in &wavs {
            let stem = builder_name(n);
            let native = wav_to_native(w, WavLimits::default()).unwrap().bytes;
            assert_eq!(
                fs::read(report.export_dir.join(String::from_utf8(stem).unwrap())).unwrap(),
                native
            );
        }
        // exporting into an empty folder writes the native files
        let empty = root.join("empty");
        let mut options = Options::new(root.join("out"));
        options.stamp = Some("t2".into());
        let report = export(&image, &[], &empty, &dt2_layout(), &options).unwrap();
        assert_eq!(report.added, ["kick", "snare"]);
        assert_eq!(
            fs::read(empty.join("kick")).unwrap(),
            fs::read(report.export_dir.join("kick")).unwrap()
        );
        // and a second export changes nothing
        options.stamp = Some("t3".into());
        let again = export(&image, &[], &empty, &dt2_layout(), &options).unwrap();
        assert!(again.added.is_empty() && again.replaced.is_empty());
        assert_eq!(again.unchanged.len(), 2);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_unchanged_store_is_not_reported_and_dn2_has_no_tree() {
        let root = temp("digikit");
        let folder = root.join("plus_drive/dt2");
        fs::create_dir_all(folder.join("digikit/sub")).unwrap();
        fs::write(folder.join("digikit/sub/m.dkm"), b"module").unwrap();
        let mut image = card_of(&[("kick.wav", wav(3000, 1))]);
        let store = crate::encode_store(
            vec![crate::StoreFile {
                dir: false,
                path: "sub/m.dkm".into(),
                bytes: b"module".to_vec(),
            }],
            area().store_limit(),
        )
        .unwrap();
        image.write_at(area().first_sector * 512, &store).unwrap();
        let mut options = Options::new(root.join("out"));
        options.stamp = Some("t1".into());
        let report = export(&image, &[], &folder, &dt2_layout(), &options).unwrap();
        let all = [
            &report.added,
            &report.replaced,
            &report.unchanged,
            &report.deleted_by_firmware,
            &report.pruned,
        ];
        assert!(
            all.iter()
                .all(|list| list.iter().all(|f| !f.contains("digikit"))),
            "{report:?}"
        );
        assert!(folder.join("digikit/sub/m.dkm").is_file());
        assert!(!report.export_dir.join("digikit").exists());
        // DN2: no path filesystem, so no tree and no issue about one.
        options.stamp = Some("t2".into());
        let dn2 = card_layout("dn2").unwrap();
        let report = export(&image, &[], &root.join("dn2"), &dn2, &options).unwrap();
        assert!(
            report.added.is_empty() && report.issues.is_empty(),
            "{report:?}"
        );
    }

    #[test]
    fn the_selection_record_round_trips_with_the_folder() {
        use crate::{Selection, Tree, TreeOptions, build_tree_image, encode_selection};
        let root = temp("selection");
        let folder = root.join("plus_drive/dt2");
        let layout = dt2_layout();
        let end = layout.store.as_ref().unwrap().end_sector;
        let built = build_tree_image(
            Tree {
                selection: Some(vec!["modules/a.dkm".into(), "modules/b.dkm".into()]),
                ..Default::default()
            },
            TreeOptions::default(),
            None,
        )
        .unwrap();
        let mut options = Options::new(root.join("out"));
        // 1. the builder's own record: the export writes the list into the folder
        options.stamp = Some("s1".into());
        let report = export(&built.image, &[], &folder, &layout, &options).unwrap();
        assert_eq!(report.added, ["digikit/selection.txt"], "{report:?}");
        let text = fs::read_to_string(folder.join("digikit/selection.txt")).unwrap();
        assert_eq!(
            crate::parse_selection_txt(&text).unwrap(),
            ["modules/a.dkm", "modules/b.dkm"]
        );
        // 2. the user's file with comments and other spacing holds the same list: unchanged
        fs::write(
            folder.join("digikit/selection.txt"),
            "# mine\n modules/a.dkm\n\nmodules/b.dkm # not a comment, part of the path\n",
        )
        .unwrap();
        options.stamp = Some("s2".into());
        let report = export(&built.image, &[], &folder, &layout, &options).unwrap();
        assert_eq!(report.replaced, ["digikit/selection.txt"], "{report:?}");
        fs::write(
            folder.join("digikit/selection.txt"),
            "# mine\n modules/a.dkm\n\nmodules/b.dkm\n",
        )
        .unwrap();
        options.stamp = Some("s3".into());
        let report = export(&built.image, &[], &folder, &layout, &options).unwrap();
        assert!(
            report.replaced.is_empty() && report.added.is_empty(),
            "{report:?}"
        );
        assert!(
            report
                .unchanged
                .contains(&"digikit/selection.txt".to_string())
        );
        // 3. the runtime writes copy B with a higher seq (what a menu selection does): the folder follows, the old file is kept
        let record = encode_selection(&Selection {
            seq: 5,
            boot_counter: 1,
            flags: 0,
            paths: vec!["modules/c.dkm".into()],
        })
        .unwrap();
        let overlay = [CardOverlaySector {
            sector: end - 1,
            written: [0xff; 64],
            data: record,
        }];
        options.stamp = Some("s4".into());
        let report = export(&built.image, &overlay, &folder, &layout, &options).unwrap();
        assert_eq!(report.replaced, ["digikit/selection.txt"], "{report:?}");
        let new = fs::read_to_string(folder.join("digikit/selection.txt")).unwrap();
        assert_eq!(crate::parse_selection_txt(&new).unwrap(), ["modules/c.dkm"]);
        assert!(
            fs::read_to_string(report.backup_dir.join("digikit/selection.txt"))
                .unwrap()
                .contains("# mine")
        );
        // 4. a torn copy B (bad CRC) loses to A; no valid copy leaves the folder alone
        let mut torn = overlay[0].clone();
        torn.data[40] ^= 1;
        options.stamp = Some("s5".into());
        let report = export(&built.image, &[torn.clone()], &folder, &layout, &options).unwrap();
        assert_eq!(report.replaced, ["digikit/selection.txt"], "{report:?}");
        let both_bad = [
            torn,
            CardOverlaySector {
                sector: end - 2,
                written: [0xff; 64],
                data: [0u8; 512],
            },
        ];
        options.stamp = Some("s6".into());
        let report = export(&built.image, &both_bad, &folder, &layout, &options).unwrap();
        assert!(!report.export_dir.join("digikit").exists(), "{report:?}");
        assert!(folder.join("digikit/selection.txt").is_file());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_synthetic_overlay_replaces_a_file_and_the_old_one_is_backed_up() {
        let root = temp("overlay");
        let folder = root.join("f");
        fs::create_dir_all(&folder).unwrap();
        let w = wav(2000, 3);
        let image = card_of(&[("a.wav", w.clone())]);
        // first export into the folder: native file `a`
        let mut options = Options::new(root.join("out"));
        options.stamp = Some("s1".into());
        export(&image, &[], &folder, &dt2_layout(), &options).unwrap();
        let before = fs::read(folder.join("a")).unwrap();
        // the overlay rewrites the first PCM bytes of that file's page
        let fs_view = Fs::open(&image).unwrap();
        let walk = fs_view.walk();
        let rec = &walk.nodes[0].record;
        let physical = fs_view.extents(rec).unwrap()[0].2;
        let sector = crate::page_sector(physical).unwrap();
        let mut written = [0u8; 64];
        let mut data = [0u8; 512];
        written[8] = 0xff; // bytes 0x40..0x48 (PCM start)
        data[0x40..0x48].copy_from_slice(&[9, 9, 9, 9, 9, 9, 9, 9]);
        let overlay = [CardOverlaySector {
            sector,
            written,
            data,
        }];
        options.stamp = Some("s2".into());
        let report = export(&image, &overlay, &folder, &dt2_layout(), &options).unwrap();
        assert_eq!(report.replaced, ["a"], "{report:?}");
        let after = fs::read(folder.join("a")).unwrap();
        assert_eq!(after.len(), before.len());
        assert_eq!(&after[0x40..0x48], &[9; 8]);
        assert_eq!(fs::read(report.backup_dir.join("a")).unwrap(), before);
        // a file the folder has and the card lacks is listed and kept, then pruned on request
        fs::write(folder.join("gone"), b"x").unwrap();
        options.stamp = Some("s3".into());
        let report = export(&image, &overlay, &folder, &dt2_layout(), &options).unwrap();
        assert_eq!(report.deleted_by_firmware, ["gone"]);
        assert!(folder.join("gone").exists());
        options.stamp = Some("s4".into());
        options.prune = true;
        let report = export(&image, &overlay, &folder, &dt2_layout(), &options).unwrap();
        assert_eq!(report.pruned, ["gone"]);
        assert!(!folder.join("gone").exists());
        assert_eq!(fs::read(report.backup_dir.join("gone")).unwrap(), b"x");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn mmcfs_slots_are_exported_by_their_content() {
        let root = temp("mmcfs");
        let mut image = build_sample_image(Vec::new()).unwrap().image;
        // table sector: project slot 3, sound slot 5, kit slot 9
        let mut table = [0u8; 512];
        table[0] = 1 << 3;
        table[16] = 1 << 5;
        table[272 + 1] = 1 << 1;
        image.replace_at(0x800 * 512, &table).unwrap();
        let mut slot_bytes = vec![0u8; 512 * 3];
        slot_bytes[0] = 0xab;
        slot_bytes[1024] = 0xcd; // last non-zero sector is the third
        image
            .write_at(
                dt2_layout().slot_sector(Pool::Projects, 3) * 512,
                &slot_bytes,
            )
            .unwrap();
        image
            .write_at(dt2_layout().slot_sector(Pool::Sounds, 5) * 512, &[7; 512])
            .unwrap();
        image
            .write_at(dt2_layout().slot_sector(Pool::Kits, 9) * 512, &[8; 512])
            .unwrap();
        let folder = root.join("f");
        let mut options = Options::new(root.join("out"));
        options.stamp = Some("m".into());
        let report = export(&image, &[], &folder, &dt2_layout(), &options).unwrap();
        assert_eq!(
            report.added,
            [
                ".mmcfs/kits/0009.bin",
                ".mmcfs/projects/0003.bin",
                ".mmcfs/sounds/0005.bin"
            ]
        );
        assert_eq!(
            fs::read(folder.join(".mmcfs/projects/0003.bin")).unwrap(),
            slot_bytes
        );
        // A boot project copy (COKi) is exported only when the guest wrote it.
        options.stamp = Some("m2".into());
        let report = export(&image, &[], &folder, &dt2_layout(), &options).unwrap();
        assert!(!folder.join(".mmcfs/boot").exists(), "{report:?}");
        let mut written = [0u8; 64];
        written[0] = 0xff;
        let mut data = [0u8; 512];
        data[..4].copy_from_slice(b"COKj");
        let overlay = [CardOverlaySector {
            sector: 0x40000,
            written,
            data,
        }];
        options.stamp = Some("m3".into());
        let report = export(&image, &overlay, &folder, &dt2_layout(), &options).unwrap();
        assert_eq!(report.added, [".mmcfs/boot/040000.bin"]);
        assert_eq!(
            &fs::read(folder.join(".mmcfs/boot/040000.bin")).unwrap()[..4],
            b"COKj"
        );
        // The working project (0x50000) is rewritten by the firmware at every start: left out unless asked for.
        let working = [
            overlay[0].clone(),
            CardOverlaySector {
                sector: 0x50000,
                written,
                data,
            },
        ];
        options.stamp = Some("m4".into());
        let report = export(&image, &working, &folder, &dt2_layout(), &options).unwrap();
        assert!(report.added.is_empty(), "{report:?}");
        assert!(!folder.join(".mmcfs/boot/050000.bin").exists());
        options.include_working_project = true;
        options.stamp = Some("m5".into());
        let report = export(&image, &working, &folder, &dt2_layout(), &options).unwrap();
        assert_eq!(report.added, [".mmcfs/boot/050000.bin"]);
        let _ = fs::remove_dir_all(&root);
    }

    // ---- the module store round trip (an upload survives the next card build) ----

    fn store_bytes(files: &[(&str, &[u8])], seq: u32) -> Vec<u8> {
        let mut bytes = crate::encode_store(
            files
                .iter()
                .map(|(p, b)| crate::StoreFile {
                    dir: false,
                    path: (*p).into(),
                    bytes: b.to_vec(),
                })
                .collect(),
            area().store_limit(),
        )
        .unwrap();
        if seq != 1 {
            let mut h = crate::parse_header(&bytes[..512]).unwrap();
            h.seq = seq;
            bytes[..512].copy_from_slice(&crate::encode_header(&h));
        }
        bytes
    }

    /// A card as built (store of `built`) and the same card after the guest wrote the store of `now` over it (both header copies).
    fn card_with_store(
        built: &[(&str, &[u8])],
        now: &[(&str, &[u8])],
    ) -> (SparseImage, Vec<CardOverlaySector>) {
        let mut image = card_of(&[("kick.wav", wav(3000, 1))]);
        let first = built_store(&mut image, built);
        let bytes = store_bytes(now, 2);
        let mut overlay = Vec::new();
        for (i, chunk) in bytes.chunks(512).enumerate() {
            let mut data = [0u8; 512];
            data[..chunk.len()].copy_from_slice(chunk);
            overlay.push(CardOverlaySector {
                sector: first + i as u64,
                written: [0xff; 64],
                data,
            });
        }
        overlay.push(CardOverlaySector {
            sector: area().header_b(),
            written: [0xff; 64],
            data: bytes[..512].try_into().unwrap(),
        });
        (image, overlay)
    }

    fn built_store(image: &mut SparseImage, files: &[(&str, &[u8])]) -> u64 {
        let bytes = store_bytes(files, 1);
        image.write_at(area().first_sector * 512, &bytes).unwrap();
        image
            .write_at(area().header_b() * 512, &bytes[..512])
            .unwrap();
        area().first_sector
    }

    #[test]
    fn an_uploaded_module_is_written_back_to_the_folder_and_the_second_export_changes_nothing() {
        let root = temp("upload");
        let folder = root.join("plus_drive/dt2");
        fs::create_dir_all(folder.join("digikit/modules")).unwrap();
        fs::write(folder.join("digikit/modules/a.dkm"), b"first").unwrap();
        let (image, overlay) = card_with_store(
            &[("modules/a.dkm", b"first")],
            &[("modules/a.dkm", b"first"), ("modules/b.dkm", b"uploaded")],
        );
        let mut options = Options::new(root.join("out"));
        options.stamp = Some("u1".into());
        let report = export(&image, &overlay, &folder, &dt2_layout(), &options).unwrap();
        assert_eq!(
            report.added,
            ["kick", "digikit/modules/b.dkm"],
            "{report:?}"
        );
        assert!(report.kept.is_empty() && report.deleted_by_firmware.is_empty());
        assert_eq!(
            fs::read(folder.join("digikit/modules/b.dkm")).unwrap(),
            b"uploaded"
        );
        options.stamp = Some("u2".into());
        let again = export(&image, &overlay, &folder, &dt2_layout(), &options).unwrap();
        assert!(again.added.is_empty() && again.replaced.is_empty() && again.kept.is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_replaced_module_replaces_the_folder_copy_unless_the_folder_changed_after_the_build() {
        let root = temp("upload-replace");
        let folder = root.join("plus_drive/dt2");
        fs::create_dir_all(folder.join("digikit/modules")).unwrap();
        fs::write(folder.join("digikit/modules/a.dkm"), b"first").unwrap();
        fs::write(
            folder.join("digikit/modules/c.dkm"),
            b"mine, edited after the build",
        )
        .unwrap();
        let (image, overlay) = card_with_store(
            &[("modules/a.dkm", b"first"), ("modules/c.dkm", b"built")],
            &[
                ("modules/a.dkm", b"second"),
                ("modules/c.dkm", b"uploaded over built"),
            ],
        );
        let mut options = Options::new(root.join("out"));
        options.stamp = Some("r1".into());
        let report = export(&image, &overlay, &folder, &dt2_layout(), &options).unwrap();
        assert_eq!(report.replaced, ["digikit/modules/a.dkm"], "{report:?}");
        assert_eq!(report.kept, ["digikit/modules/c.dkm"]);
        assert_eq!(
            fs::read(folder.join("digikit/modules/a.dkm")).unwrap(),
            b"second"
        );
        assert_eq!(
            fs::read(report.backup_dir.join("digikit/modules/a.dkm")).unwrap(),
            b"first"
        );
        assert_eq!(
            fs::read(folder.join("digikit/modules/c.dkm")).unwrap(),
            b"mine, edited after the build"
        );
        // the card's version of the kept file is in the export directory
        assert_eq!(
            fs::read(report.export_dir.join("digikit/modules/c.dkm")).unwrap(),
            b"uploaded over built"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_module_deleted_on_the_card_is_listed_and_pruned_only_on_request() {
        let root = temp("upload-delete");
        let folder = root.join("plus_drive/dt2");
        fs::create_dir_all(folder.join("digikit/modules")).unwrap();
        fs::write(folder.join("digikit/modules/a.dkm"), b"first").unwrap();
        fs::write(folder.join("digikit/modules/b.dkm"), b"edited").unwrap();
        let (image, overlay) = card_with_store(
            &[("modules/a.dkm", b"first"), ("modules/b.dkm", b"built")],
            &[],
        );
        let mut options = Options::new(root.join("out"));
        options.stamp = Some("d1".into());
        let report = export(&image, &overlay, &folder, &dt2_layout(), &options).unwrap();
        assert_eq!(
            report.deleted_by_firmware,
            ["digikit/modules/a.dkm"],
            "{report:?}"
        );
        assert_eq!(report.kept, ["digikit/modules/b.dkm"]);
        assert!(folder.join("digikit/modules/a.dkm").is_file());
        options.stamp = Some("d2".into());
        options.prune = true;
        let report = export(&image, &overlay, &folder, &dt2_layout(), &options).unwrap();
        assert_eq!(report.pruned, ["digikit/modules/a.dkm"]);
        assert!(!folder.join("digikit/modules/a.dkm").exists());
        assert!(folder.join("digikit/modules/b.dkm").is_file());
        assert!(report.backup_dir.join("digikit/modules/a.dkm").is_file());
        let _ = fs::remove_dir_all(&root);
    }
}
