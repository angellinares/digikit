//! In-process +Drive cards: a `plus_drive/<device>/`-style folder becomes a card image (no Python, no
//! subprocess), cached by a hash of the firmware and the folder's files. The DT2 card has a path filesystem and a
//! seeded project; a card of another device (the DN2) takes only `.mmcfs/` and `digikit/`.
//!
//! The folder contract is the builder's (`plusdrive_format::folder`): nested folders and plain files in the path
//! filesystem, WAVs as native samples, `.mmcfs/` slots and boot regions, `digikit/` in the module store. A folder of
//! WAV files, as the desktop "Choose samples" stages it, is the simplest such folder.
//!
//! Layout under the run directory:
//!
//! ```text
//! <run_dir>/cards/<key12>/<device>.img   sparse card image (4 GB logical)
//! <run_dir>/cards/<key12>/ready/         digi-audio-m5.snap(.dsp), the ready state
//! <run_dir>/cards/<key12>/SOURCE.json    what the card was built from
//! ```
//!
//! Everything under `cards/` is derived from the firmware and the user's
//! samples: it stays in the git-ignored `out/` tree and is never committed.

use std::{
    fs,
    path::{Path, PathBuf},
};

use plusdrive_format::{
    Dt2Seed, TreeOptions, build_tree_image, card_layout, folder_files, scan_folder,
    write_image_file,
};
use serde_json::json;
use sha2::{Digest, Sha256};

/// Bump when the card layout or the track mapping changes, so old caches are
/// not reused: it is part of the key.
pub const FORMAT_TAG: &str = "dt2-card-v3-plusdrive-folder";

/// The files of `dir` a build reads (relative `/` paths, sorted), or why there is nothing to build.
pub fn list_files(dir: &Path) -> Result<Vec<String>, String> {
    if !dir.is_dir() {
        return Err(format!("{} is not a directory", dir.display()));
    }
    let files = folder_files(dir)?;
    if files.is_empty() {
        return Err(format!("no files in {}", dir.display()));
    }
    Ok(files)
}

/// The files of `dir` that a card of `device` takes: every file for a device with a path filesystem, else only `.mmcfs/` and
/// `digikit/`. Empty: the folder holds nothing this device's card can use (the card is then not built from it).
pub fn device_files(device: &str, dir: &Path) -> Result<Vec<String>, String> {
    let layout = card_layout(device)?;
    let files = folder_files(dir)?;
    Ok(if layout.path_fs.is_some() {
        files
    } else {
        files
            .into_iter()
            .filter(|f| f.starts_with(".mmcfs/") || f.starts_with("digikit/"))
            .collect()
    })
}

/// Where a card lives and whether its parts exist.
#[derive(Clone, Debug)]
pub struct CardPaths {
    pub key: String,
    pub dir: PathBuf,
    pub image: PathBuf,
    pub ready: PathBuf,
}

impl CardPaths {
    /// The DT2's card (`last.json` names a card by key only).
    pub fn new(run_dir: &Path, key: &str) -> Self {
        Self::for_device(run_dir, key, "dt2")
    }
    pub fn for_device(run_dir: &Path, key: &str, device: &str) -> Self {
        let dir = run_dir.join("cards").join(&key[..12.min(key.len())]);
        Self {
            key: key.to_string(),
            image: dir.join(format!("{device}.img")),
            ready: dir.join("ready"),
            dir,
        }
    }
    pub fn snapshot(&self) -> PathBuf {
        self.ready.join("digi-audio-m5.snap")
    }
    pub fn dsp_state(&self) -> PathBuf {
        self.ready.join("digi-audio-m5.snap.dsp")
    }
    /// Both ready files exist (the cold boot writes the snapshot last).
    pub fn ready_cached(&self) -> bool {
        self.snapshot().is_file() && self.dsp_state().is_file()
    }
}

/// The cache key: firmware, format tag, and every file's relative path and bytes.
pub fn card_key(device: &str, syx: &[u8], dir: &Path, files: &[String]) -> Result<String, String> {
    let mut hash = Sha256::new();
    if device != "dt2" {
        // the DT2's keys stay what they were (cached cards are reused)
        hash.update(b"device\0");
        hash.update(device.as_bytes());
        hash.update(b"\0");
    }
    hash.update(b"firmware\0");
    hash.update(Sha256::digest(syx));
    hash.update(b"\0format\0");
    hash.update(FORMAT_TAG.as_bytes());
    for rel in files {
        let path = dir.join(rel);
        let bytes = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        hash.update(b"\0file\0");
        hash.update(rel.as_bytes());
        hash.update(b"\0");
        hash.update(Sha256::digest(&bytes));
    }
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// What a card build produced.
#[derive(Clone, Debug)]
pub struct CardInfo {
    pub samples: Vec<String>,
    pub image_bytes_written: u64,
}

/// Build the seeded image of the folder `dir` and write it to `image` (through a `.part` file, so a crash never
/// leaves a half image under the final name). `progress(done, total)` is called after each file is read.
pub fn build_card(
    device: &str,
    syx: &[u8],
    dir: &Path,
    image: &Path,
    mut progress: impl FnMut(usize, usize),
) -> Result<CardInfo, String> {
    // only the DT2 has a project to seed (`Dt2Seed`)
    let seed = (device == "dt2")
        .then(|| Dt2Seed::from_syx(syx))
        .transpose()?;
    let layout = card_layout(device)?;
    let scanned = scan_folder(dir, &layout, &mut progress)?;
    for line in &scanned.skipped {
        eprintln!("card: skipped {line}");
    }
    let built = build_tree_image(
        scanned.tree,
        TreeOptions {
            capacity_blocks: plusdrive_format::DEFAULT_CAPACITY_BLOCKS,
            layout,
            max_extent_pages: u32::MAX,
        },
        seed.as_ref().map(Dt2Seed::seed),
    )
    .map_err(|e| format!("card image: {e:?}"))?;
    let written = write_image_file(&built.image, image)?;
    Ok(CardInfo {
        samples: built
            .files
            .iter()
            .map(|file| file.file_name.clone())
            .collect(),
        image_bytes_written: written,
    })
}

/// Build the card of `dir` unless it is cached; the paths, whether the image was already there, and the number of
/// samples on it.
pub fn ensure_card(
    device: &str,
    run_dir: &Path,
    syx: &[u8],
    dir: &Path,
    files: &[String],
    progress: impl FnMut(usize, usize),
) -> Result<(CardPaths, bool, usize), String> {
    let key = card_key(device, syx, dir, files)?;
    let paths = CardPaths::for_device(run_dir, &key, device);
    if paths.image.is_file() {
        let samples = fs::read_to_string(paths.dir.join("SOURCE.json"))
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .and_then(|v| v["samples"].as_array().map(Vec::len))
            .unwrap_or(0);
        return Ok((paths, true, samples));
    }
    let info = build_card(device, syx, dir, &paths.image, progress)?;
    let source = json!({
        "key": paths.key,
        "format": FORMAT_TAG,
        "firmware_sha256": sha256_hex(syx),
        "samples": info.samples,
        "files": files.len(),
    });
    let _ = fs::write(
        paths.dir.join("SOURCE.json"),
        serde_json::to_string_pretty(&source).unwrap_or_default(),
    );
    let samples = info.samples.len();
    Ok((paths, false, samples))
}

/// The card the app mounts on its own at the next DT2 start
/// (`<run_dir>/cards/last.json`): which cached card, for which firmware, and
/// what to call it in the UI.
#[derive(Clone, Debug, PartialEq)]
pub struct LastCard {
    pub key: String,
    pub name: String,
    pub samples: usize,
    pub firmware_sha256: String,
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn last_path(run_dir: &Path) -> PathBuf {
    run_dir.join("cards").join("last.json")
}

/// Remember `last` (through a `.part` file, so a crash keeps the old one).
pub fn write_last(run_dir: &Path, last: &LastCard) -> Result<(), String> {
    let path = last_path(run_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(&json!({
        "key": last.key, "name": last.name, "samples": last.samples,
        "firmware_sha256": last.firmware_sha256,
    }))
    .map_err(|e| e.to_string())?;
    let partial = path.with_extension("json.part");
    fs::write(&partial, text).map_err(|e| format!("{}: {e}", partial.display()))?;
    fs::rename(&partial, &path).map_err(|e| format!("{}: {e}", path.display()))
}

/// The remembered card, if `last.json` is readable.
pub fn read_last(run_dir: &Path) -> Option<LastCard> {
    let value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(last_path(run_dir)).ok()?).ok()?;
    Some(LastCard {
        key: value["key"].as_str()?.to_string(),
        name: value["name"].as_str()?.to_string(),
        samples: usize::try_from(value["samples"].as_u64()?).ok()?,
        firmware_sha256: value["firmware_sha256"].as_str()?.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(frequency: f64, frames: usize) -> Vec<u8> {
        let mut data = Vec::new();
        for i in 0..frames {
            let value = (0.5
                * (std::f64::consts::TAU * frequency * i as f64 / 48_000.0).sin()
                * 32767.0) as i16;
            data.extend_from_slice(&value.to_le_bytes());
        }
        let mut out = b"RIFF".to_vec();
        out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&48_000u32.to_le_bytes());
        out.extend_from_slice(&96_000u32.to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&data);
        out
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("digi-cards-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn lists_every_file_of_the_folder_sorted() {
        let dir = scratch("list");
        fs::write(dir.join("b.wav"), wav(440.0, 100)).unwrap();
        fs::write(dir.join("A.WAV"), wav(440.0, 100)).unwrap();
        fs::write(dir.join(".DS_Store"), b"x").unwrap();
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("sub/c.wav"), wav(440.0, 100)).unwrap();
        fs::create_dir_all(dir.join(".mmcfs/projects")).unwrap();
        fs::write(dir.join(".mmcfs/projects/0000.bin"), b"p").unwrap();
        assert_eq!(
            list_files(&dir).unwrap(),
            [".mmcfs/projects/0000.bin", "A.WAV", "b.wav", "sub/c.wav"]
        );
        assert!(list_files(&dir.join("sub/none")).is_err());
        let empty = scratch("empty");
        assert!(list_files(&empty).unwrap_err().contains("no files"));
    }

    #[test]
    fn key_follows_firmware_names_and_bytes() {
        let dir = scratch("key");
        fs::write(dir.join("a.wav"), wav(220.0, 480)).unwrap();
        fs::write(dir.join("b.wav"), wav(880.0, 480)).unwrap();
        let files = list_files(&dir).unwrap();
        let key = card_key("dt2", b"fw1", &dir, &files).unwrap();
        assert_eq!(key, card_key("dt2", b"fw1", &dir, &files).unwrap());
        assert_ne!(key, card_key("dt2", b"fw2", &dir, &files).unwrap());
        fs::write(dir.join("b.wav"), wav(881.0, 480)).unwrap();
        assert_ne!(key, card_key("dt2", b"fw1", &dir, &files).unwrap());
        let paths = CardPaths::new(Path::new("run"), &key);
        assert!(
            paths
                .image
                .ends_with(format!("cards/{}/dt2.img", &key[..12]))
        );
        assert!(!paths.ready_cached());
        // another device: its own key and image name
        let dn2 = card_key("dn2", b"fw1", &dir, &files).unwrap();
        assert_ne!(key, dn2);
        assert!(
            CardPaths::for_device(Path::new("run"), &dn2, "dn2")
                .image
                .ends_with(format!("cards/{}/dn2.img", &dn2[..12]))
        );
    }

    #[test]
    fn a_device_without_a_path_filesystem_takes_only_mmcfs_and_digikit() {
        let dir = scratch("device-files");
        fs::write(dir.join("a.wav"), wav(220.0, 480)).unwrap();
        assert_eq!(device_files("dt2", &dir).unwrap(), ["a.wav"]);
        assert!(device_files("dn2", &dir).unwrap().is_empty());
        fs::create_dir_all(dir.join("digikit/modules")).unwrap();
        fs::write(dir.join("digikit/modules/m.dkm"), b"m").unwrap();
        fs::create_dir_all(dir.join(".mmcfs/projects")).unwrap();
        fs::write(dir.join(".mmcfs/projects/0000.bin"), b"p").unwrap();
        assert_eq!(
            device_files("dn2", &dir).unwrap(),
            [".mmcfs/projects/0000.bin", "digikit/modules/m.dkm"]
        );
        assert!(device_files("nope", &dir).is_err());
    }

    /// With the DT2 1.17 .syx (`DT2_SYX`, or the one in `firmware/`): a plus_drive-style folder (three WAVs, a
    /// nested folder, a saved project slot, a boot copy and a module file) builds a card whose track N sample is
    /// file N, and a second call reuses the cache.
    #[test]
    fn builds_a_seeded_card_from_a_plus_drive_folder_and_caches_it() {
        let Some(syx) = std::env::var_os("DT2_SYX")
            .map(PathBuf::from)
            .or_else(|| {
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .ancestors()
                    .map(|dir| dir.join("firmware").join("Digitakt_II_OS1.17.syx"))
                    .find(|path| path.exists())
            })
            .and_then(|path| fs::read(path).ok())
        else {
            return;
        };
        let dir = scratch("build");
        let samples = dir.join("samples");
        fs::create_dir_all(&samples).unwrap();
        for (name, frequency) in [("1-a220.wav", 220.0), ("2-b880.wav", 880.0)] {
            fs::write(samples.join(name), wav(frequency, 4800)).unwrap();
        }
        fs::write(samples.join("3-c3001.wav"), wav(3001.0, 4800)).unwrap();
        fs::create_dir_all(samples.join("kicks")).unwrap();
        fs::write(samples.join("kicks/k.wav"), wav(60.0, 4800)).unwrap();
        fs::create_dir_all(samples.join(".mmcfs/projects")).unwrap();
        fs::write(samples.join(".mmcfs/projects/0002.bin"), b"project").unwrap();
        fs::create_dir_all(samples.join("digikit")).unwrap();
        fs::write(samples.join("digikit/m.dkm"), b"module").unwrap();
        let files = list_files(&samples).unwrap();
        let mut seen = Vec::new();
        let (paths, cached, count) = ensure_card(
            "dt2",
            &dir.join("run"),
            &syx,
            &samples,
            &files,
            |done, total| seen.push((done, total)),
        )
        .unwrap();
        assert!(!cached);
        assert_eq!(count, 4);
        assert_eq!(seen.last(), Some(&(4, 6)), "{seen:?}");
        assert_eq!(fs::metadata(&paths.image).unwrap().len() % 512, 0);
        struct Image(fs::File, u64);
        impl emmc_card::RandomAccessRead for Image {
            fn len(&self) -> u64 {
                self.1
            }
            fn read_at(&self, offset: u64, out: &mut [u8]) -> usize {
                use std::os::unix::fs::FileExt;
                self.0.read_at(out, offset).unwrap_or(0)
            }
        }
        let file = fs::File::open(&paths.image).unwrap();
        let len = file.metadata().unwrap().len();
        let image = Image(file, len);
        let listing = plusdrive_format::list_image(&image).unwrap();
        let names: Vec<_> = listing
            .iter()
            .map(|entry| String::from_utf8_lossy(&entry.name).into_owned())
            .collect();
        for name in ["1-a220", "2-b880", "3-c3001", "kicks"] {
            assert!(names.iter().any(|n| n == name), "{names:?}");
        }
        let area = plusdrive_format::card_layout("dt2").unwrap().store.unwrap();
        let store = plusdrive_format::read_store(
            &image,
            area.first_sector,
            area.store_limit(),
            area.header_b(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(store.entries.len(), 1);
        let mut slot = [0u8; 7];
        emmc_card::RandomAccessRead::read_at(&image, (0x58000 + 2 * 0x8000) * 512, &mut slot);
        assert_eq!(&slot, b"project");
        let (again, cached, count) =
            ensure_card("dt2", &dir.join("run"), &syx, &samples, &files, |_, _| {}).unwrap();
        assert!(cached);
        assert_eq!(count, 4);
        assert_eq!(again.image, paths.image);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn last_card_round_trips_and_ignores_garbage() {
        let dir = scratch("last");
        assert_eq!(read_last(&dir), None);
        let last = LastCard {
            key: "ab".repeat(32),
            name: "Kick drums".into(),
            samples: 7,
            firmware_sha256: sha256_hex(b"fw"),
        };
        write_last(&dir, &last).unwrap();
        assert_eq!(read_last(&dir), Some(last));
        fs::write(dir.join("cards/last.json"), b"{not json").unwrap();
        assert_eq!(read_last(&dir), None);
        let _ = fs::remove_dir_all(&dir);
    }
}
