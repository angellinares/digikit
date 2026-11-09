//! The host folder of each device's card (docs/plans/card-folder.md): the module store is mirrored to it, and the card is built from it
//! at the next load. The folder belongs to the device, not to the process: a load of another device switches to that device's folder.
//!
//! Which folder a device uses, first match:
//! 1. the user's choice for the device (a saved folder that exists; the Card panel's "Choose folder..."),
//! 2. the folder the launcher gave for the first load of the session (`--mirror-dir`),
//! 3. `plus_drive/<device>/` (the root is `$DIGI_PLUS_DRIVE`, else `plus_drive/` of the repository).
//!
//! The choices live in one small JSON file (`{"dn2": "/path", "dt2": "/path"}`) that the host names (the desktop app: its app data
//! folder), so they survive a restart and the controller can read them at load. Without a file the choices last for the session.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use serde_json::{Value, json};

/// What the host gives the controller (`Config::device_folders`).
#[derive(Clone, Debug, Default)]
pub struct DeviceFoldersConfig {
    /// `plus_drive/`; None: `$DIGI_PLUS_DRIVE`, else the repository's.
    pub root: Option<PathBuf>,
    /// The file that keeps the user's choices; None: they last for the session.
    pub choices: Option<PathBuf>,
    /// The module store is mirrored to the folder while the emulator runs and the card's changes are exported to it on quit
    /// (`--no-card-sync` turns both off; the card is still built from the folder).
    pub sync: bool,
}

#[derive(Debug, Default)]
pub struct DeviceFolders {
    root: Option<PathBuf>,
    file: Option<PathBuf>,
    choices: BTreeMap<String, PathBuf>,
    session: BTreeMap<String, PathBuf>,
}

/// A device name is a short id (`dt2`, `dn2`): it becomes a path part.
pub fn device_ok(device: &str) -> bool {
    !device.is_empty()
        && device
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

impl DeviceFolders {
    pub fn new(root: Option<PathBuf>, file: Option<PathBuf>) -> Self {
        let choices = file.as_deref().map(read_choices).unwrap_or_default();
        Self {
            root,
            file,
            choices,
            session: BTreeMap::new(),
        }
    }

    /// `plus_drive/<device>/`, when there is a root.
    pub fn default_folder(&self, device: &str) -> Option<PathBuf> {
        device_ok(device)
            .then(|| self.root.as_ref().map(|r| r.join(device)))
            .flatten()
    }

    /// The saved choice, when its folder exists.
    pub fn choice(&self, device: &str) -> Option<&Path> {
        self.choices
            .get(device)
            .map(PathBuf::as_path)
            .filter(|p| p.is_dir())
    }

    /// The saved choice whose folder is gone (a removed drive): the default is used instead.
    pub fn missing_choice(&self, device: &str) -> Option<&Path> {
        self.choices
            .get(device)
            .map(PathBuf::as_path)
            .filter(|p| !p.is_dir())
    }

    /// The folder `device` uses now.
    pub fn folder(&self, device: &str) -> Option<PathBuf> {
        self.choice(device)
            .map(Path::to_path_buf)
            .or_else(|| self.session.get(device).cloned())
            .or_else(|| self.default_folder(device))
    }

    /// The launcher's folder for the first load (`--mirror-dir`): used until the user chooses another.
    pub fn set_session(&mut self, device: &str, dir: PathBuf) {
        self.session.insert(device.to_string(), dir);
    }

    /// Save the user's choice for `device` (`None`: back to the default) and write the file. The folder must exist.
    pub fn set_choice(&mut self, device: &str, dir: Option<&Path>) -> Result<(), String> {
        if !device_ok(device) {
            return Err(format!("not a device name: {device:?}"));
        }
        match dir {
            Some(dir) => {
                let dir = fs::canonicalize(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
                if !dir.is_dir() {
                    return Err(format!("{}: not a folder", dir.display()));
                }
                self.choices.insert(device.to_string(), dir);
            }
            None => {
                self.choices.remove(device);
                // the session folder of the launcher is not "the default" the user asked for
                self.session.remove(device);
            }
        }
        self.write()
    }

    fn write(&self) -> Result<(), String> {
        let Some(file) = &self.file else {
            return Ok(());
        };
        if let Some(parent) = file.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let map: serde_json::Map<String, Value> = self
            .choices
            .iter()
            .map(|(device, dir)| (device.clone(), json!(dir.display().to_string())))
            .collect();
        let text = serde_json::to_string_pretty(&Value::Object(map)).map_err(|e| e.to_string())?;
        let partial = file.with_extension("json.part");
        fs::write(&partial, text).map_err(|e| format!("{}: {e}", partial.display()))?;
        fs::rename(&partial, file).map_err(|e| format!("{}: {e}", file.display()))
    }
}

fn read_choices(file: &Path) -> BTreeMap<String, PathBuf> {
    let Ok(text) = fs::read_to_string(file) else {
        return BTreeMap::new();
    };
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&text) else {
        return BTreeMap::new();
    };
    map.into_iter()
        .filter(|(device, _)| device_ok(device))
        .filter_map(|(device, dir)| dir.as_str().map(|d| (device, PathBuf::from(d))))
        .filter(|(_, dir)| dir.is_absolute())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("digi-devfolders-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn each_device_has_its_own_default_folder() {
        let f = DeviceFolders::new(Some("/r/plus_drive".into()), None);
        assert_eq!(f.folder("dn2"), Some("/r/plus_drive/dn2".into()));
        assert_eq!(f.folder("dt2"), Some("/r/plus_drive/dt2".into()));
        assert_eq!(f.folder("../x"), None, "a device name is a path part");
        assert_eq!(DeviceFolders::new(None, None).folder("dt2"), None);
    }

    #[test]
    fn the_choice_beats_the_launchers_folder_beats_the_default_and_use_default_resets() {
        let dir = scratch("order");
        let mine = dir.join("mine");
        fs::create_dir_all(&mine).unwrap();
        let mut f = DeviceFolders::new(Some(dir.join("pd")), None);
        f.set_session("dn2", "/from/launcher".into());
        assert_eq!(f.folder("dn2"), Some("/from/launcher".into()));
        assert_eq!(
            f.folder("dt2"),
            Some(dir.join("pd/dt2")),
            "the first-load folder is for its device only"
        );
        f.set_choice("dn2", Some(&mine)).unwrap();
        assert_eq!(f.folder("dn2"), Some(fs::canonicalize(&mine).unwrap()));
        assert_eq!(
            f.folder("dt2"),
            Some(dir.join("pd/dt2")),
            "a choice is per device"
        );
        f.set_choice("dn2", None).unwrap();
        assert_eq!(f.folder("dn2"), Some(dir.join("pd/dn2")));
        assert!(f.set_choice("dn2", Some(&dir.join("missing"))).is_err());
        assert!(f.set_choice("a/b", None).is_err());
    }

    #[test]
    fn choices_survive_a_restart_and_a_missing_folder_falls_back_to_the_default() {
        let dir = scratch("persist");
        let (a, b) = (dir.join("a"), dir.join("b"));
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        let file = dir.join("config/card-folders.json");
        let mut f = DeviceFolders::new(Some(dir.join("pd")), Some(file.clone()));
        f.set_choice("dn2", Some(&a)).unwrap();
        f.set_choice("dt2", Some(&b)).unwrap();
        let again = DeviceFolders::new(Some(dir.join("pd")), Some(file.clone()));
        assert_eq!(again.folder("dn2"), Some(fs::canonicalize(&a).unwrap()));
        assert_eq!(again.folder("dt2"), Some(fs::canonicalize(&b).unwrap()));
        fs::remove_dir_all(&b).unwrap();
        let gone = DeviceFolders::new(Some(dir.join("pd")), Some(file.clone()));
        assert_eq!(gone.folder("dt2"), Some(dir.join("pd/dt2")));
        assert_eq!(
            gone.missing_choice("dt2"),
            Some(
                fs::canonicalize(&a)
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join("b")
                    .as_path()
            )
        );
        // a damaged file or a relative path is ignored
        fs::write(&file, r#"{"dn2": "relative", "dt2": 3}"#).unwrap();
        assert_eq!(
            DeviceFolders::new(None, Some(file.clone())).folder("dn2"),
            None
        );
        fs::write(&file, "{not json").unwrap();
        assert!(read_choices(&file).is_empty());
    }
}
