//! `digi-emud`: the emulator without a window. The controller plus the control
//! socket; start it, then drive it from `tools/control/digi_client.py`.
//!
//!   digi-emud [SYX] [--card-image FILE] [--auto-coupled ...] [--free]
//!
//! The guest starts paused: a client steps it (`step`, `run_until`) or sets
//! `run.mode free`. The socket is `out/native/run/<pid>.sock`; the discovery
//! file `out/native/run/current.json` names it.

use std::path::PathBuf;

use digi_control::{
    Config, Mode,
    cli::{self, StartupArgs},
    controller::spawn,
    dispatch::Dispatcher,
    server::{self, ServerOptions},
};
use serde_json::json;

fn main() {
    let mut free = false;
    let args: Vec<_> = std::env::args_os()
        .skip(1)
        .filter(|arg| {
            let is_free = arg == "--free";
            free |= is_free;
            !is_free
        })
        .collect();
    let startup: StartupArgs = cli::parse_values(args, cfg!(feature = "coupled-audio"))
        .unwrap_or_else(|error| {
            eprintln!("{error}");
            std::process::exit(2)
        });
    let run_dir = startup
        .run_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from("out/native/run"));
    let mut config = Config::new(startup.audio.clone());
    config.audio_alt = startup.audio_alt.clone();
    config.mode = if free { Mode::Free } else { Mode::Paused };
    config.run_dir = run_dir.clone();
    config.mirror_dir = startup.mirror_dir.clone();
    // The daemon keeps the user's folder choices for the session only (no app data folder).
    config.device_folders = startup
        .device_folders
        .then(|| digi_control::DeviceFoldersConfig {
            root: None,
            choices: None,
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
    if startup.scenario.is_some() {
        eprintln!(
            "digi-emud: --scenario is a desktop option (scenario.run over the socket is not built yet)"
        );
        std::process::exit(2);
    }
    let (handle, join) = spawn(config);
    let dispatcher = Dispatcher::new(handle.clone(), "digi-emud");
    if let Some(syx) = &startup.syx {
        let mut params = json!({"syx": syx.display().to_string()});
        if let Some(card) = &startup.card_image {
            params["card"] = card.display().to_string().into();
        }
        if startup.runtime == Some(true) {
            params["runtime"] = true.into();
        }
        if let Err(error) = handle.call("firmware.load", params) {
            eprintln!("digi-emud: {error}");
            std::process::exit(2);
        }
    }
    let server = match server::serve(dispatcher, ServerOptions { run_dir }, "digi-emud") {
        Ok(server) => server,
        Err(error) => {
            eprintln!("digi-emud: control socket: {error}");
            std::process::exit(2);
        }
    };
    eprintln!("digi-emud: listening on {}", server.socket.display());
    let _ = join.join();
    // Let the socket thread write the reply to `quit` before the files go.
    std::thread::sleep(std::time::Duration::from_millis(200));
    drop(server);
}
