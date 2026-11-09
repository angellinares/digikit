//! The Firmware page's two native parts: `patchkit::flash` in process (the page cannot patch and check a firmware image itself here, the
//! browser build does it in patchkit.wasm) and the system's save dialog for the image. The MIDI side is `midi_port.rs`; the page paces the
//! messages with the Rust sender through patchkit.wasm, like the site.
use std::path::Path;

/// The answer of `firmware_prepare`: the length of the summary JSON as 4 bytes big endian, the JSON (`Prepared::summary_json`), the image.
pub fn pack(summary: &str, image: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + summary.len() + image.len());
    out.extend_from_slice(&(summary.len() as u32).to_be_bytes());
    out.extend_from_slice(summary.as_bytes());
    out.extend_from_slice(image);
    out
}

/// Patch and check `stock` (or, with `restore`, check the stock file itself) with the checkout's modules, ports and device files.
pub fn prepare(root: &Path, stock: &[u8], restore: bool, set: &str) -> Result<Vec<u8>, String> {
    let p = patchkit::flash::prepare_in_repo(root, stock, restore, set).map_err(|e| e.0)?;
    Ok(pack(&p.summary_json().to_string(), &p.image))
}

pub(crate) fn body(request: &tauri::ipc::Request<'_>) -> Result<Vec<u8>, String> {
    match request.body() {
        tauri::ipc::InvokeBody::Raw(bytes) => Ok(bytes.clone()),
        _ => Err("the file must be sent as a raw byte body".into()),
    }
}

/// `flash::prepare` or `flash::restore` (header `x-restore: 1`) on the raw body, the stock `.syx`. Fails with patchkit's text for a file that
/// is no known stock image; a failed check is in the summary.
#[tauri::command]
pub async fn firmware_prepare(
    request: tauri::ipc::Request<'_>,
) -> Result<tauri::ipc::Response, String> {
    let stock = body(&request)?;
    let restore = request.headers().get("x-restore").is_some_and(|v| v == "1");
    // the patch set: `device` or `device-introblank` (the Custom boot screen toggle); anything else is refused by the device file
    let set = request
        .headers()
        .get("x-set")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .unwrap_or("device")
        .to_string();
    tauri::async_runtime::spawn_blocking(move || {
        let root = digi_control::runtime_patch::find_root(None)?;
        prepare(&root, &stock, restore, &set).map(tauri::ipc::Response::new)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The save dialog for the image in the raw body; `x-name` (percent-encoded) is the suggested file name. The path written, or `None` when the
/// user cancels.
#[tauri::command]
pub async fn firmware_save(
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
        .unwrap_or_else(|| "firmware.syx".into());
    let dialog = app
        .dialog()
        .file()
        .set_title("Save the firmware image")
        .set_file_name(name)
        .add_filter("SysEx", &["syx"]);
    let picked = tauri::async_runtime::spawn_blocking(move || dialog.blocking_save_file())
        .await
        .map_err(|e| e.to_string())?;
    let Some(path) = picked.and_then(|p| p.into_path().ok()) else {
        return Ok(None);
    };
    std::fs::write(&path, &bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Some(path.display().to_string()))
}

/// `%XX` escapes of a header value (a header carries ASCII only).
pub(crate) fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(h) = s
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(h);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_answer_is_the_summary_then_the_image() {
        let out = pack("{\"ok\":true}", &[1, 2, 3]);
        assert_eq!(&out[..4], &11u32.to_be_bytes());
        assert_eq!(&out[4..15], b"{\"ok\":true}");
        assert_eq!(&out[15..], &[1, 2, 3]);
    }

    #[test]
    fn a_header_name_is_unescaped() {
        assert_eq!(
            unescape("Digitone_II_OS1.12%2Bdigikit-0.5.2.syx"),
            "Digitone_II_OS1.12+digikit-0.5.2.syx"
        );
        assert_eq!(unescape("plain.syx"), "plain.syx");
        assert_eq!(unescape("bad%zz"), "bad%zz");
    }
}
