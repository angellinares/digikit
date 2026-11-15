//! The save dialog for the Log drawer and the Profile pane: the raw body is the text to save, `x-name` (percent-encoded) the suggested file name.
use crate::firmware::{body, unescape};

/// Ask where to save the log text in the raw body. The path written, or `None` when the user cancels.
#[tauri::command]
pub async fn log_save(
    app: tauri::AppHandle,
    request: tauri::ipc::Request<'_>,
) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let bytes = body(&request)?;
    let name = request
        .headers()
        .get("x-name")
        .and_then(|v| v.to_str().ok())
        .map(unescape)
        .unwrap_or_else(|| "device-log.log".into());
    // `x-kind`: `profile` for a dk.profile/1 file of the Profile pane; the Log drawer sends none
    let profile = request
        .headers()
        .get("x-kind")
        .and_then(|v| v.to_str().ok())
        == Some("profile");
    let (title, filter, exts): (&str, &str, &[&str]) = if profile {
        ("Save the profile", "Profile", &["json"])
    } else {
        ("Save the log", "Log", &["log", "txt"])
    };
    let dialog = app
        .dialog()
        .file()
        .set_title(title)
        .set_file_name(name)
        .add_filter(filter, exts);
    let picked = tauri::async_runtime::spawn_blocking(move || dialog.blocking_save_file())
        .await
        .map_err(|e| e.to_string())?;
    let Some(path) = picked.and_then(|p| p.into_path().ok()) else {
        return Ok(None);
    };
    std::fs::write(&path, &bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Some(path.display().to_string()))
}
