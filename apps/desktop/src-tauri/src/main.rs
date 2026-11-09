//! The desktop app: the webview's commands are thin messages to the shared
//! emulator controller (`packages/emulator/control`), which also serves the control
//! socket when the app runs with `--control` (the launcher adds it).
use std::{path::PathBuf, process};

use digi_control::{
    Command, Config, Handle, Mode, ScenarioSetup,
    cli::{self, StartupArgs},
    controller::spawn,
    dispatch::Dispatcher,
    load_scenario,
};
use elektron_native_boot::Snapshot;
mod desktop_runtime;
mod firmware;
mod library;
mod local;
mod log_save;
mod midi_port;
use serde_json::Value;
use std::sync::mpsc;
use tauri::State;

struct Host {
    handle: Handle,
    /// Where the webview's chosen sample files are staged before the card is built.
    staging: PathBuf,
    /// Where firmware files picked with Open... are put for `firmware.load` (`<run dir>/open`).
    open_dir: PathBuf,
    /// The .syx the app was started with (`mise run workbench -- FILE`); the console shows it and reloads it
    /// when it changes. The page can read only this file.
    source: Option<PathBuf>,
}

fn parse_args() -> Result<StartupArgs, String> {
    cli::parse_values(std::env::args_os().skip(1), cfg!(feature = "coupled-audio"))
}

async fn send(
    host: State<'_, Host>,
    command: impl FnOnce(digi_control::Reply) -> Command + Send + 'static,
) -> Result<Snapshot, String> {
    let sender = host.handle.sender().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (reply, response) = mpsc::channel();
        sender
            .send(command(reply))
            .map_err(|_| "emulator actor stopped")?;
        response
            .recv()
            .map_err(|_| "emulator actor dropped reply")?
    })
    .await
    .map_err(|e| e.to_string())?
}

async fn send_json(
    host: State<'_, Host>,
    command: impl FnOnce(digi_control::JsonReply) -> Command + Send + 'static,
) -> Result<Value, String> {
    let sender = host.handle.sender().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (reply, response) = mpsc::channel();
        sender
            .send(command(reply))
            .map_err(|_| "emulator actor stopped")?;
        response
            .recv()
            .map_err(|_| "emulator actor dropped reply")?
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Writes an exported diagnostics report to `out/native/diagnostics/` under
/// the working directory (the repository root under `mise run workbench`); the
/// webview cannot save `<a download>` links.
#[tauri::command]
async fn emu_save_diagnostics(name: String, json: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || save_diagnostics(&name, &json))
        .await
        .map_err(|e| e.to_string())?
}

fn save_diagnostics(name: &str, json: &str) -> Result<String, String> {
    let dir = std::env::current_dir()
        .map_err(|e| e.to_string())?
        .join("out/native/diagnostics");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let safe: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    let path = dir.join(format!("{stamp}-{safe}"));
    std::fs::write(&path, json).map_err(|e| e.to_string())?;
    Ok(path.display().to_string())
}

/// The startup .syx: its path, size and modification time (milliseconds since the epoch), or null
/// when the app was started without one.
#[tauri::command]
async fn emu_source_info(host: State<'_, Host>) -> Result<Option<Value>, String> {
    let Some(path) = host.source.clone() else {
        return Ok(None);
    };
    tauri::async_runtime::spawn_blocking(move || source_info(&path))
        .await
        .map_err(|e| e.to_string())?
}

/// How the running binary was built: the PGO state the launcher passed in the environment (null when unknown).
#[tauri::command]
async fn emu_build_info() -> Value {
    serde_json::json!({
        "pgo": std::env::var("DIGI_PGO_STATUS").ok(),
        "pgo_detail": std::env::var("DIGI_PGO_DETAIL").ok(),
    })
}

fn source_info(path: &std::path::Path) -> Result<Option<Value>, String> {
    let fail = |error: &dyn std::fmt::Display| format!("{}: {error}", path.display());
    let metadata = std::fs::metadata(path).map_err(|e| fail(&e))?;
    let modified = metadata.modified().map_err(|e| fail(&e))?;
    let modified_ms = modified
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| fail(&e))?
        .as_millis() as u64;
    Ok(Some(serde_json::json!({
        "path": path.display().to_string(),
        "name": path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        "bytes": metadata.len(),
        "modified_ms": modified_ms,
    })))
}

/// One protocol method of the controller (docs/CONTROL.md: the same table as the control socket), for the webview. `session` is the
/// webview's session token: `firmware.load` and `restart` start the session it names, every other method must carry the current
/// one, else the answer is "stale emulator session" and nothing runs. Not offered: `quit` (the app owns its lifetime).
#[tauri::command]
async fn emu_call(
    host: State<'_, Host>,
    method: String,
    params: Option<Value>,
    session: Option<u64>,
) -> Result<Value, String> {
    let handle = host.handle.clone();
    tauri::async_runtime::spawn_blocking(move || call_method(&handle, &method, params, session))
        .await
        .map_err(|e| e.to_string())?
}

fn call_method(
    handle: &Handle,
    method: &str,
    params: Option<Value>,
    session: Option<u64>,
) -> Result<Value, String> {
    if method == "quit" {
        return Err("quit is not available to the webview".into());
    }
    let mut params = params.unwrap_or_else(|| serde_json::json!({}));
    let Some(fields) = params.as_object_mut() else {
        return Err("params must be an object".into());
    };
    if let Some(session) = session {
        fields.insert("session".into(), session.into());
    }
    handle.call(method, params).map_err(|e| e.message)
}

/// Put the bytes of a firmware file the webview picked (Open...) where the controller can read them: `<run dir>/open/<name>`
/// (the name is the percent-encoded `x-name`). Returns the path for `firmware.load {syx}`.
#[tauri::command]
async fn emu_stage_syx(
    host: State<'_, Host>,
    request: tauri::ipc::Request<'_>,
) -> Result<String, String> {
    let name = request
        .headers()
        .get("x-name")
        .and_then(|value| value.to_str().ok())
        .and_then(percent_decode)
        .unwrap_or_default();
    let safe: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect();
    let safe = if safe.is_empty() {
        "opened.syx".to_string()
    } else {
        safe
    };
    let bytes = match request.body() {
        tauri::ipc::InvokeBody::Raw(bytes) => bytes.clone(),
        _ => return Err("firmware must use a raw byte body".into()),
    };
    let dir = host.open_dir.clone();
    tauri::async_runtime::spawn_blocking(move || {
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join(safe);
        std::fs::write(&path, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(path.display().to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn emu_audio_status(host: State<'_, Host>, session_id: u64) -> Result<Value, String> {
    send_json(host, move |reply| Command::AudioStatus {
        session: session_id,
        reply,
    })
    .await
}

#[tauri::command]
async fn emu_startup(host: State<'_, Host>) -> Result<Snapshot, String> {
    send(host, |reply| Command::Startup { reply }).await
}
#[tauri::command]
async fn emu_step(host: State<'_, Host>, session_id: u64, budget: u32) -> Result<Snapshot, String> {
    send(host, move |reply| Command::Step {
        session: session_id,
        budget,
        reply,
    })
    .await
}
#[tauri::command]
async fn emu_tap(
    host: State<'_, Host>,
    session_id: u64,
    code: u8,
    hold: Option<u32>,
) -> Result<Snapshot, String> {
    send(host, move |reply| Command::Tap {
        session: session_id,
        code,
        hold: hold.unwrap_or(40_000_000),
        reply,
    })
    .await
}
#[tauri::command]
async fn emu_stop(host: State<'_, Host>, session_id: u64) -> Result<(), String> {
    match send(host, move |reply| Command::Stop {
        session: session_id,
        reply,
    })
    .await
    {
        Ok(_) => Ok(()),
        Err(error) if error == "No firmware selected" => Ok(()),
        Err(error) => Err(error),
    }
}

/// A header value the webview percent-encoded (`encodeURIComponent`).
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = text.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Forget the staged sample files (the start of a new folder choice).
#[tauri::command]
async fn emu_card_reset(host: State<'_, Host>) -> Result<(), String> {
    let dir = host.staging.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// A staged file's relative path (`/`-separated) is safe to join to the staging directory: no empty, `.` or `..`
/// part, no backslash or NUL, so nothing escapes it.
fn staged_path_ok(name: &str) -> bool {
    !name.is_empty()
        && !name.contains(['\\', '\0'])
        && name
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// Stage one file of the chosen +Drive-style folder: the raw body, its path relative to the folder in `x-name`
/// (nested folders and `.mmcfs/` are kept; the controller builds the card with the +Drive builder).
#[tauri::command]
async fn emu_card_stage(
    host: State<'_, Host>,
    request: tauri::ipc::Request<'_>,
) -> Result<(), String> {
    let name = request
        .headers()
        .get("x-name")
        .and_then(|value| value.to_str().ok())
        .and_then(percent_decode)
        .ok_or("missing x-name")?;
    if !staged_path_ok(&name) {
        return Err(format!("not a file path inside the folder: {name}"));
    }
    let bytes = match request.body() {
        tauri::ipc::InvokeBody::Raw(bytes) => bytes.clone(),
        _ => return Err("staged files must use a raw byte body".into()),
    };
    let dir = host.staging.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let target = dir.join(&name);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(target, bytes).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Build the card of the staged files and restart the DT2 on it; progress is
/// read with `emu_call("card.status")`.
#[tauri::command]
async fn emu_card_load(host: State<'_, Host>, name: Option<String>) -> Result<Value, String> {
    let (handle, dir) = (host.handle.clone(), host.staging.clone());
    tauri::async_runtime::spawn_blocking(move || {
        handle
            .call(
                "card.load",
                serde_json::json!({
                    "dir": dir.display().to_string(), "wait": false, "name": name,
                }),
            )
            .map_err(|e| e.message)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The desktop `--scenario` mode ends the app when the controller reports the
/// scenario done: write the report (to `--report` or stdout) and exit.
fn exit_on_scenario_done(handle: &Handle, report: Option<PathBuf>) {
    let events = handle
        .events()
        .subscribe(Some(vec!["scenario.done".into()]));
    std::thread::spawn(move || {
        let Ok(event) = events.recv() else { return };
        let text = match serde_json::to_string_pretty(&event["report"]) {
            Ok(text) => text,
            Err(error) => {
                eprintln!("report: {error}");
                process::exit(1)
            }
        };
        match report {
            Some(path) => {
                let written = path
                    .parent()
                    .map_or(Ok(()), std::fs::create_dir_all)
                    .and_then(|()| std::fs::write(&path, text));
                if let Err(error) = written {
                    eprintln!("report: {error}");
                    process::exit(1)
                }
            }
            None => println!("{text}"),
        }
        process::exit(0)
    });
}

/// The app data folder of the app (where Tauri's path resolver puts `app_data_dir`): the system's data folder and the app identifier.
/// The controller starts before the Tauri app is built, so it is worked out here from the environment.
fn app_data_dir(identifier: &str) -> Option<PathBuf> {
    app_data_dir_in(
        identifier,
        std::env::var_os("HOME"),
        std::env::var_os("XDG_DATA_HOME"),
        std::env::var_os("APPDATA"),
    )
}

fn app_data_dir_in(
    identifier: &str,
    home: Option<std::ffi::OsString>,
    xdg_data_home: Option<std::ffi::OsString>,
    appdata: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    let non_empty = |v: Option<std::ffi::OsString>| v.filter(|v| !v.is_empty()).map(PathBuf::from);
    let base = if cfg!(target_os = "macos") {
        non_empty(home)?.join("Library/Application Support")
    } else if cfg!(windows) {
        non_empty(appdata)?
    } else {
        non_empty(xdg_data_home).or_else(|| non_empty(home).map(|h| h.join(".local/share")))?
    };
    Some(base.join(identifier))
}

/// `firmware.load` of the startup .syx as session 1. Without a usable module library or port for the runtime patch (a build outside
/// the repository, a firmware that has no port) the image loads as it is, with a note on stderr.
fn load_startup(
    handle: &Handle,
    syx: &std::path::Path,
    card: Option<&std::path::Path>,
    runtime: bool,
) -> Result<(), String> {
    let mut params = serde_json::json!({"syx": syx.display().to_string(), "session": 1});
    if let Some(card) = card {
        params["card"] = card.display().to_string().into();
    }
    if runtime {
        params["runtime"] = true.into();
    }
    match handle.call("firmware.load", params.clone()) {
        Err(error) if runtime && error.message.starts_with("runtime patch:") => {
            eprintln!(
                "{}; loading the image without the digikit runtime",
                error.message
            );
            params["runtime"] = false.into();
            handle
                .call("firmware.load", params)
                .map(drop)
                .map_err(|e| e.message)
        }
        other => other.map(drop).map_err(|e| e.message),
    }
}

fn main() {
    #[cfg(feature = "coupled-audio")]
    if std::env::args_os()
        .nth(1)
        .is_some_and(|a| a == "--pgo-train")
    {
        match desktop_runtime::pgo_training_workload() {
            Ok(()) => {
                println!("pgo_train=ok");
                process::exit(0)
            }
            Err(error) => {
                println!("pgo_train=failed: {error}");
                process::exit(1)
            }
        }
    }
    let context = tauri::generate_context!();
    let identifier = context.config().identifier.clone();
    let startup = parse_args().unwrap_or_else(|error| {
        eprintln!("{error}");
        process::exit(2)
    });
    let run_dir = startup
        .run_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from("out/native/run"));
    let mut config = Config::new(startup.audio.clone());
    config.audio_alt = startup.audio_alt.clone();
    config.mode = if startup.paused {
        Mode::Paused
    } else if startup.scenario.is_some() {
        // Scenario ticks run in the webview's `emu_step`.
        Mode::Ui
    } else {
        Mode::Free
    };
    config.run_dir = run_dir.clone();
    config.mirror_dir = startup.mirror_dir.clone();
    // One folder per device; the user's choices are kept in the app data folder (the controller reads them at every load).
    config.device_folders = startup
        .device_folders
        .then(|| digi_control::DeviceFoldersConfig {
            root: None,
            choices: app_data_dir(&identifier).map(|dir| dir.join("card-folders.json")),
            sync: startup.card_sync,
        });
    config.card_folder = startup
        .card_folder
        .clone()
        .map(|dir| digi_control::CardSource {
            dir,
            sync: startup.card_sync,
            out: None,
        });
    // Mount the last card again at start (not for scenarios, which must not depend on earlier runs).
    config.remember_card = startup.scenario.is_none();
    // A ColdFire-only runtime (a DT2 opened in a DN2-audio session) runs at the wall clock, not at the CPU's speed.
    config.pace_free = true;
    let run_dir_for_staging = run_dir.clone();
    config.on_quit = Some(Box::new(|| {
        // Let the socket thread write the reply first.
        std::thread::sleep(std::time::Duration::from_millis(200));
        process::exit(0)
    }));
    config.scenario = startup.scenario.as_deref().map(|spec| ScenarioSetup {
        scenario: load_scenario(spec).unwrap_or_else(|error| {
            eprintln!("{error}");
            process::exit(2)
        }),
    });
    let has_scenario = config.scenario.is_some();
    let (handle, _controller) = spawn(config);
    if has_scenario {
        exit_on_scenario_done(&handle, startup.report.clone());
    }
    if let Some(path) = startup.syx.clone() {
        // The first session is 1 (`emu_startup` and the webview's pump use it). The digikit runtime is patched in unless
        // `--no-runtime` or a scenario run (scenarios are stock).
        let runtime = startup.runtime.unwrap_or(!has_scenario);
        if runtime {
            eprintln!(
                "emulator startup: patching the digikit runtime in; a first boot of a new image takes a while, later ones come from the cache"
            );
        }
        if let Err(error) = load_startup(&handle, &path, startup.card_image.as_deref(), runtime) {
            eprintln!("emulator startup: {error}");
            process::exit(2);
        }
    }
    #[cfg(unix)]
    // The control socket is on unless `--no-control` (design question 3).
    let _server = if startup.control != Some(false) {
        match digi_control::server::serve(
            Dispatcher::new(handle.clone(), "digiemu-desktop"),
            digi_control::server::ServerOptions { run_dir },
            "digiemu-desktop",
        ) {
            Ok(server) => {
                eprintln!("control socket: {}", server.socket.display());
                Some(server)
            }
            Err(error) => {
                // The emulator still runs; only scripted control is lost.
                eprintln!("control socket: {error}");
                None
            }
        }
    } else {
        None
    };
    #[cfg(not(unix))]
    let _ = (&run_dir, Dispatcher::new);
    let exit_handle = handle.clone();
    let host = Host {
        handle,
        staging: run_dir_for_staging.join("cards").join("staging"),
        open_dir: run_dir_for_staging.join("open"),
        source: startup
            .syx
            .clone()
            .map(|path| std::fs::canonicalize(&path).unwrap_or(path)),
    };
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(host)
        .manage(midi_port::MidiPort::default())
        // on the main thread, before any command: CoreMIDI refreshes the port list on the run loop of the thread of its first client
        .setup(|app| {
            use tauri::Manager;
            app.manage(midi_port::MidiWatch::start());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            emu_call,
            emu_stage_syx,
            emu_save_diagnostics,
            emu_source_info,
            emu_build_info,
            emu_audio_status,
            emu_startup,
            emu_step,
            emu_tap,
            emu_stop,
            emu_card_reset,
            emu_card_stage,
            emu_card_load,
            card_open_folder,
            firmware::firmware_prepare,
            firmware::firmware_save,
            log_save::log_save,
            library::lib_scan,
            library::lib_read,
            library::sample_wasm,
            library::patchkit_wasm,
            library::device_storage,
            local::local_pick_folder,
            local::local_list,
            local::local_read,
            midi_port::midi_ports,
            midi_port::midi_open,
            midi_port::midi_close,
            midi_port::midi_send,
            midi_port::midi_recv,
            midi_port::midi_cancel
        ])
        .build(context)
        .expect("tauri build error");
    // The window closed: let the controller end the session (`quit`: the module store is mirrored once more and the card's changes
    // are exported to the +Drive folder) before the process goes. Without it the controller thread is cut off mid-export.
    app.run(move |_app, event| {
        if let tauri::RunEvent::Exit = event {
            let _ = exit_handle.call("quit", serde_json::json!({}));
        }
    });
}

/// Open the module store's folder (`card.folder`: `plus_drive/<device>/`) in the system's file manager, through the official opener plugin.
/// Only that folder can be opened: the path comes from the controller, not from the page.
#[tauri::command]
async fn card_open_folder(app: tauri::AppHandle, host: State<'_, Host>) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let handle = host.handle.clone();
    let folder = tauri::async_runtime::spawn_blocking(move || {
        let v = handle
            .call("card.folder", serde_json::json!({}))
            .map_err(|e| e.message)?;
        v["folder"]
            .as_str()
            .map(PathBuf::from)
            .ok_or_else(|| "no folder is set for the module store".to_string())
    })
    .await
    .map_err(|e| e.to_string())??;
    let made = folder.clone();
    tauri::async_runtime::spawn_blocking(move || {
        std::fs::create_dir_all(&made).map_err(|e| format!("{}: {e}", made.display()))
    })
    .await
    .map_err(|e| e.to_string())??;
    app.opener()
        .open_path(folder.display().to_string(), None::<&str>)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod cli_tests {
    use super::*;
    use desktop_runtime::{AudioMode, AudioOptions};

    /// Tauri runs a plain `fn` command on the main thread: any wait or file read there freezes the window. Every command is `async`
    /// (its blocking work goes to `spawn_blocking`).
    #[test]
    fn every_command_is_async() {
        let sources = [
            ("main.rs", include_str!("main.rs")),
            ("library.rs", include_str!("library.rs")),
            ("local.rs", include_str!("local.rs")),
            ("midi_port.rs", include_str!("midi_port.rs")),
        ];
        let mut found = 0;
        for (file, text) in sources {
            let lines: Vec<&str> = text.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                if line.trim() != "#[tauri::command]" {
                    continue;
                }
                found += 1;
                let next = lines[i + 1..].iter().find(|l| {
                    !l.trim_start().starts_with("///") && !l.trim_start().starts_with("#[")
                });
                let next = next.expect("a function follows").trim_start();
                assert!(
                    next.starts_with("async fn ") || next.starts_with("pub async fn "),
                    "{file}: a command on the main thread: {next}"
                );
            }
        }
        assert!(found >= 20, "the scan found {found} commands");
    }

    fn parse(args: &[&str]) -> Result<StartupArgs, String> {
        cli::parse_values(
            args.iter().map(std::ffi::OsString::from),
            cfg!(feature = "coupled-audio"),
        )
    }

    #[test]
    fn app_data_dir_is_the_systems_data_folder_and_the_identifier() {
        let os = |s: &str| Some(std::ffi::OsString::from(s));
        let dir = app_data_dir_in(
            "dev.digiemu.panel",
            os("/home/u"),
            os("/data"),
            os("C:/Roaming"),
        )
        .unwrap();
        if cfg!(target_os = "macos") {
            assert_eq!(
                dir,
                PathBuf::from("/home/u/Library/Application Support/dev.digiemu.panel")
            );
        } else if cfg!(windows) {
            assert_eq!(dir, PathBuf::from("C:/Roaming/dev.digiemu.panel"));
        } else {
            assert_eq!(dir, PathBuf::from("/data/dev.digiemu.panel"));
        }
        assert_eq!(app_data_dir_in("x", None, None, None), None);
    }

    #[test]
    fn default_is_cf_only_and_card_path_is_preserved() {
        assert!(parse(&[]).unwrap().audio.is_none());
        let args = parse(&["file.syx", "--card-image", "card.img"]).unwrap();
        assert_eq!(args.syx, Some("file.syx".into()));
        assert_eq!(args.card_image, Some("card.img".into()));
        assert!(args.audio.is_none());
    }
    #[test]
    fn coupled_requires_all_private_inputs_and_valid_buffer() {
        for args in [
            &["--coupled"][..],
            &["x", "--coupled"],
            &["x", "--dsp-state", "state"],
            &["--unknown"],
            &["x", "--audio-buffer", "NaN"],
            &["x", "--audio-buffer", "-1"],
            &["x", "--audio-buffer", "11"],
        ] {
            assert!(parse(args).is_err(), "{args:?}");
        }
        let parsed = parse(&[
            "x",
            "--coupled",
            "--dsp-image",
            "image",
            "--dsp-state",
            "state",
            "--cf-snapshot",
            "snapshot",
            "--no-audio",
            "--audio-buffer",
            "0.1",
        ])
        .unwrap();
        let audio = parsed.audio.unwrap();
        assert_eq!(audio.image, PathBuf::from("image"));
        assert_eq!(audio.buffer_seconds, 0.1);
        assert!(!audio.playback);
    }

    #[test]
    fn audio_mode_parses_and_requires_coupled() {
        let base = [
            "x",
            "--auto-coupled",
            "--dsp-image",
            "image",
            "--dsp-state",
            "state",
            "--cf-snapshot",
            "snapshot",
        ];
        let with = |mode: &'static str| {
            let mut args = base.to_vec();
            args.extend(["--audio-mode", mode]);
            parse(&args)
        };
        assert_eq!(
            with("stretch").unwrap().audio.unwrap().audio_mode,
            AudioMode::Stretch
        );
        assert!(with("loud").is_err());
        assert!(parse(&["x", "--audio-mode", "gap"]).is_err());
    }

    #[test]
    fn tap_holds_for_the_requested_guest_instructions_across_chunks() {
        let syx = std::env::var_os("DIGI_COUPLED_SYX")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("../../../firmware/Digitone_II_OS1.11.syx"));
        let Ok(bytes) = std::fs::read(syx) else {
            return;
        };
        let (handle, _join) = spawn(Config::new(None));
        let sender = handle.sender().clone();
        let (load_reply, load_response) = mpsc::channel();
        sender
            .send(Command::Load {
                session: 1,
                bytes,
                card: None,
                reply: load_reply,
            })
            .unwrap();
        let before = load_response.recv().unwrap().unwrap().status.icount;
        let (tap_reply, tap_response) = mpsc::channel();
        sender
            .send(Command::Tap {
                session: 1,
                code: 25,
                hold: 250_001,
                reply: tap_reply,
            })
            .unwrap();
        let after = tap_response.recv().unwrap().unwrap().status.icount;
        assert!(after - before >= 250_001);
    }

    #[test]
    fn audio_status_validates_session_and_reports_no_coupled_audio_as_null() {
        let (handle, controller) = spawn(Config::new(None));
        let sender = handle.sender().clone();
        let (missing_reply, missing_response) = mpsc::channel();
        sender
            .send(Command::AudioStatus {
                session: 0,
                reply: missing_reply,
            })
            .unwrap();
        assert_eq!(
            missing_response.recv().unwrap().unwrap_err(),
            "No firmware selected"
        );
        let (stale_reply, stale_response) = mpsc::channel();
        sender
            .send(Command::AudioStatus {
                session: 1,
                reply: stale_reply,
            })
            .unwrap();
        assert_eq!(
            stale_response.recv().unwrap().unwrap_err(),
            "stale emulator session"
        );

        let syx = std::env::var_os("DIGI_COUPLED_SYX")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("../../../firmware/Digitone_II_OS1.11.syx"));
        let Ok(bytes) = std::fs::read(syx) else {
            drop((sender, handle));
            controller.join().unwrap();
            return;
        };
        let (load_reply, load_response) = mpsc::channel();
        sender
            .send(Command::Load {
                session: 1,
                bytes,
                card: None,
                reply: load_reply,
            })
            .unwrap();
        load_response.recv().unwrap().unwrap();
        let (status_reply, status_response) = mpsc::channel();
        sender
            .send(Command::AudioStatus {
                session: 1,
                reply: status_reply,
            })
            .unwrap();
        assert_eq!(status_response.recv().unwrap().unwrap(), Value::Null);
        drop((sender, handle));
        controller.join().unwrap();
    }

    #[test]
    #[ignore = "private DN2 ready fixture and generated DSP"]
    fn startup_returns_the_coupled_restore_frame_once() {
        let (Some(fixture_dir), Some(syx)) = (
            std::env::var_os("DIGI_COUPLED_FIXTURES").map(PathBuf::from),
            std::env::var_os("DIGI_COUPLED_SYX").map(PathBuf::from),
        ) else {
            return;
        };
        let Ok(bytes) = std::fs::read(syx) else {
            return;
        };
        let audio = AudioOptions {
            image: fixture_dir.join("digi-audio-dn2-image.bin"),
            state: fixture_dir.join("digi-audio-m5.snap.dsp"),
            snapshot: fixture_dir.join("digi-audio-m5.snap"),
            buffer_seconds: 0.0,
            playback: false,
            automatic: false,
            expected_syx: None,
            link_timing: false,
            audio_mode: AudioMode::default(),
            dsp_patch: None,
            zero: false,
        };
        let (handle, controller) = spawn(Config::new(Some(audio)));
        let sender = handle.sender().clone();
        let (load_reply, load_response) = mpsc::channel();
        sender
            .send(Command::Load {
                session: 1,
                bytes,
                card: None,
                reply: load_reply,
            })
            .unwrap();
        let loaded = load_response.recv().unwrap().unwrap();
        let frame = loaded
            .frame
            .clone()
            .expect("load should observe restored LCD");
        assert!(frame.iter().any(|byte| *byte != 0));
        let revision = loaded.status.frame_revision;
        let (startup_reply, startup_response) = mpsc::channel();
        sender
            .send(Command::Startup {
                reply: startup_reply,
            })
            .unwrap();
        let startup = startup_response.recv().unwrap().unwrap();
        assert_eq!(startup.frame, Some(frame));
        assert_eq!(startup.status.frame_revision, revision);
        let (second_reply, second_response) = mpsc::channel();
        sender
            .send(Command::Startup {
                reply: second_reply,
            })
            .unwrap();
        assert!(second_response.recv().unwrap().unwrap().frame.is_none());
        drop((sender, handle));
        controller.join().unwrap();
    }
}

#[cfg(test)]
mod emu_call_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn methods_round_trip_and_the_session_token_is_checked_first() {
        let (handle, _controller) = spawn(Config::new(None));
        // A method round trip: no firmware yet.
        assert!(call_method(&handle, "status", None, None).unwrap()["firmware"].is_null());
        // The current session (0 before the first load) passes; another one is stale and nothing runs.
        assert!(call_method(&handle, "status", None, Some(0)).is_ok());
        assert_eq!(
            call_method(&handle, "diagnostics", None, Some(4)).unwrap_err(),
            "stale emulator session"
        );
        // `firmware.load` and `restart` start a session: the token must be newer than the current one.
        assert_eq!(
            call_method(&handle, "restart", None, Some(0)).unwrap_err(),
            "stale emulator session"
        );
        // A start that fails does not take the token.
        let missing = json!({"syx": "/nonexistent/none.syx"});
        assert!(call_method(&handle, "firmware.load", Some(missing), Some(7)).is_err());
        assert!(call_method(&handle, "status", None, Some(0)).is_ok());
        // What the webview may not do, and bad params.
        assert!(call_method(&handle, "quit", None, None).is_err());
        assert!(call_method(&handle, "status", Some(json!([1])), None).is_err());
    }
}
