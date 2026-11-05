//! The local samples folder of the Transfer page (Samples mode, "On your computer"): the user picks a folder, the page lists it one level
//! at a time and reads files from it. The page cannot do file access itself, so these commands do the three things it needs: pick a
//! folder (the official dialog plugin), list a folder, read a file. Like library.rs, a path that leaves the chosen folder is refused
//! (`..`, an absolute path, an empty part, and a symbolic link that points outside). Nothing is written and no process is started.
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

/// The largest file `local_read` returns (a sample is small; the limit keeps a wrong pick from filling the memory).
const MAX_READ: u64 = 256 * 1024 * 1024;

#[derive(Serialize, Debug, PartialEq)]
pub struct LocalEntry {
    pub name: String,
    /// `file` or `folder`.
    pub kind: &'static str,
    pub size: u64,
}

/// The chosen folder as a canonical directory path.
fn root_dir(root: &str) -> Result<PathBuf, String> {
    let dir = std::fs::canonicalize(root).map_err(|e| format!("{root}: {e}"))?;
    if !dir.is_dir() {
        return Err(format!("{root}: not a folder"));
    }
    Ok(dir)
}

/// `rel` (`/` separated, empty for the folder itself) inside `root`: a canonical path under it, or an error.
fn inside(root: &str, rel: &str) -> Result<PathBuf, String> {
    let dir = root_dir(root)?;
    if !rel.is_empty()
        && rel.split('/').any(|p| {
            p.is_empty() || !matches!(Path::new(p).components().next(), Some(Component::Normal(_)))
        })
    {
        return Err(format!("not a path inside the folder: {rel:?}"));
    }
    let path = std::fs::canonicalize(dir.join(rel)).map_err(|e| format!("{rel}: {e}"))?;
    if !path.starts_with(&dir) {
        return Err(format!("not a path inside the folder: {rel:?}"));
    }
    Ok(path)
}

/// The entries of `rel` inside `root`, one level: folders first, then files, each by name. Names that start with a dot are left out.
pub fn list(root: &str, rel: &str) -> Result<Vec<LocalEntry>, String> {
    let dir = inside(root, rel)?;
    let mut out = vec![];
    for entry in std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        // metadata follows a link; a link that cannot be read or points outside the folder is left out
        let Ok(path) = std::fs::canonicalize(entry.path()) else {
            continue;
        };
        if !path.starts_with(root_dir(root)?) {
            continue;
        }
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            out.push(LocalEntry {
                name,
                kind: "folder",
                size: 0,
            });
        } else if meta.is_file() {
            out.push(LocalEntry {
                name,
                kind: "file",
                size: meta.len(),
            });
        }
    }
    out.sort_by(|a, b| (a.kind != "folder", &a.name).cmp(&(b.kind != "folder", &b.name)));
    Ok(out)
}

/// The bytes of one file inside `root`, or its first `head` bytes (the page labels a file by them without reading it in full).
pub fn read(root: &str, rel: &str, head: Option<u64>) -> Result<Vec<u8>, String> {
    use std::io::Read;
    if rel.is_empty() {
        return Err("no file named".into());
    }
    let path = inside(root, rel)?;
    let meta = std::fs::metadata(&path).map_err(|e| format!("{rel}: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{rel}: not a file"));
    }
    let want = head.unwrap_or(meta.len()).min(meta.len());
    if want > MAX_READ {
        return Err(format!(
            "{rel}: {want} bytes is more than the {MAX_READ} the page reads"
        ));
    }
    let mut out = Vec::with_capacity(want as usize);
    std::fs::File::open(&path)
        .and_then(|f| f.take(want).read_to_end(&mut out))
        .map_err(|e| format!("{rel}: {e}"))?;
    Ok(out)
}

/// The folder dialog of the system; `None` when the user cancels. Starts at `start` when it is a folder; `title` is the dialog's
/// title (default: the samples folder of the Transfer page).
#[tauri::command]
pub async fn local_pick_folder(
    app: tauri::AppHandle,
    start: Option<String>,
    title: Option<String>,
) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let mut dialog = app.dialog().file().set_title(
        title
            .as_deref()
            .unwrap_or("Choose the folder with your samples"),
    );
    if let Some(dir) = start.filter(|s| Path::new(s).is_dir()) {
        dialog = dialog.set_directory(dir);
    }
    let picked = tauri::async_runtime::spawn_blocking(move || dialog.blocking_pick_folder())
        .await
        .map_err(|e| e.to_string())?;
    Ok(picked
        .and_then(|p| p.into_path().ok())
        .map(|p| p.display().to_string()))
}

/// One level of the chosen folder (`rel` is `/` separated; empty is the folder itself).
#[tauri::command]
pub async fn local_list(root: String, rel: String) -> Result<Vec<LocalEntry>, String> {
    tauri::async_runtime::spawn_blocking(move || list(&root, &rel))
        .await
        .map_err(|e| e.to_string())?
}

/// One file of the chosen folder as raw bytes (`head`: only the first bytes).
#[tauri::command]
pub async fn local_read(
    root: String,
    rel: String,
    head: Option<u64>,
) -> Result<tauri::ipc::Response, String> {
    tauri::async_runtime::spawn_blocking(move || {
        read(&root, &rel, head).map(tauri::ipc::Response::new)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> (PathBuf, PathBuf) {
        let base =
            std::env::temp_dir().join(format!("digi-local-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("samples");
        std::fs::create_dir_all(root.join("drums")).unwrap();
        std::fs::write(root.join("kick.wav"), b"KICK").unwrap();
        std::fs::write(root.join("drums/snare.wav"), b"SNARE!").unwrap();
        std::fs::write(root.join(".DS_Store"), b"x").unwrap();
        std::fs::write(base.join("secret.txt"), b"outside").unwrap();
        (base, root)
    }

    #[test]
    fn the_listing_is_one_level_folders_first_and_hides_dot_files() {
        let (base, root) = folder("list");
        let r = root.to_str().unwrap();
        assert_eq!(
            list(r, "").unwrap(),
            vec![
                LocalEntry {
                    name: "drums".into(),
                    kind: "folder",
                    size: 0
                },
                LocalEntry {
                    name: "kick.wav".into(),
                    kind: "file",
                    size: 4
                }
            ]
        );
        assert_eq!(
            list(r, "drums").unwrap(),
            vec![LocalEntry {
                name: "snare.wav".into(),
                kind: "file",
                size: 6
            }]
        );
        assert_eq!(read(r, "drums/snare.wav", None).unwrap(), b"SNARE!");
        assert_eq!(read(r, "drums/snare.wav", Some(3)).unwrap(), b"SNA");
        assert_eq!(read(r, "kick.wav", Some(99)).unwrap(), b"KICK");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_path_stays_inside_the_chosen_folder() {
        let (base, root) = folder("safe");
        let r = root.to_str().unwrap();
        for bad in [
            "../secret.txt",
            "drums/../../secret.txt",
            "/etc/passwd",
            "a//b",
            "./kick.wav",
            "drums/..",
        ] {
            assert!(read(r, bad, None).is_err(), "{bad}");
            assert!(list(r, bad).is_err(), "{bad}");
        }
        assert!(read(r, "", None).is_err(), "a folder is not a file");
        assert!(read(r, "drums", None).is_err());
        assert!(read(r, "nothing.wav", None).is_err());
        assert!(list("/nonexistent/folder", "").is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_that_points_outside_is_refused_and_not_listed() {
        let (base, root) = folder("link");
        std::os::unix::fs::symlink(base.join("secret.txt"), root.join("link.txt")).unwrap();
        std::os::unix::fs::symlink(&base, root.join("up")).unwrap();
        let r = root.to_str().unwrap();
        assert!(read(r, "link.txt", None).is_err());
        assert!(list(r, "up").is_err());
        let names: Vec<_> = list(r, "").unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["drums", "kick.wav"]);
        let _ = std::fs::remove_dir_all(&base);
    }
}
