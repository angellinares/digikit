use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    thread,
};

use elektron_native_boot::Snapshot;
mod desktop_runtime;
use desktop_runtime::{AudioOptions, DesktopRuntime};
use emmc_card::{Card, DEFAULT_CAPACITY_BLOCKS, RandomAccessRead, SMALL_CAPACITY_BLOCKS};
use serde::Deserialize;
use tauri::State;

const MAX_SYX: usize = 32 * 1024 * 1024;

struct FileBacking {
    file: Mutex<File>,
    len: u64,
    error: Arc<Mutex<Option<String>>>,
}
impl RandomAccessRead for FileBacking {
    fn len(&self) -> u64 {
        self.len
    }
    fn read_at(&self, offset: u64, destination: &mut [u8]) -> usize {
        let mut file = match self.file.lock() {
            Ok(file) => file,
            Err(_) => return 0,
        };
        if let Err(error) = file
            .seek(SeekFrom::Start(offset))
            .and_then(|_| file.read_exact(destination))
        {
            *self.error.lock().expect("backing error mutex") = Some(error.to_string());
            return 0;
        }
        destination.len()
    }
}
fn card(path: PathBuf) -> Result<(Card, Arc<Mutex<Option<String>>>), String> {
    let metadata = std::fs::metadata(&path).map_err(|e| format!("card image: {e}"))?;
    if !metadata.is_file() || metadata.len() % 512 != 0 {
        return Err("card image must be a regular 512-byte aligned file".into());
    }
    let blocks = u32::try_from(metadata.len() / 512).map_err(|_| "card image is too large")?;
    if blocks != DEFAULT_CAPACITY_BLOCKS && blocks != SMALL_CAPACITY_BLOCKS {
        return Err("card image capacity is not a supported identity".into());
    }
    let error = Arc::new(Mutex::new(None));
    let backing = FileBacking {
        file: Mutex::new(File::open(path).map_err(|e| format!("card image: {e}"))?),
        len: metadata.len(),
        error: error.clone(),
    };
    Ok((
        Card::with_backing(blocks, Some(Box::new(backing))).map_err(|e| format!("card: {e:?}"))?,
        error,
    ))
}

fn runtime_from(
    bytes: &[u8],
    image: Option<PathBuf>,
    audio: Option<&AudioOptions>,
) -> Result<(DesktopRuntime, Option<Arc<Mutex<Option<String>>>>), String> {
    match image {
        Some(path) => card(path).and_then(|(card, error)| {
            DesktopRuntime::new(bytes, Some(card), audio).map(|runtime| (runtime, Some(error)))
        }),
        None => DesktopRuntime::new(bytes, None, audio).map(|runtime| (runtime, None)),
    }
}

enum Command {
    Diagnostics {
        session: u64,
        reply: mpsc::Sender<Result<serde_json::Value, String>>,
    },
    Startup {
        reply: Reply,
    },
    Load {
        session: u64,
        bytes: Vec<u8>,
        card: Option<PathBuf>,
        reply: Reply,
    },
    Restart {
        session: u64,
        next_session: u64,
        reply: Reply,
    },
    Step {
        session: u64,
        budget: u32,
        reply: Reply,
    },
    Button {
        session: u64,
        code: u8,
        down: bool,
        reply: Reply,
    },
    Turn {
        session: u64,
        encoder: u8,
        detents: i32,
        reply: Reply,
    },
    Stop {
        session: u64,
        reply: Reply,
    },
}
type Reply = mpsc::Sender<Result<Snapshot, String>>;
struct Host {
    sender: mpsc::Sender<Command>,
}
#[derive(Deserialize)]
struct StartupArgs {
    syx: Option<PathBuf>,
    card_image: Option<PathBuf>,
    #[serde(skip)]
    audio: Option<AudioOptions>,
}

fn actor(receiver: mpsc::Receiver<Command>, audio: Option<AudioOptions>) {
    let mut runtime: Option<DesktopRuntime> = None;
    let mut session = 0u64;
    let mut cached: Option<(Vec<u8>, Option<PathBuf>)> = None;
    let mut read_error: Option<Arc<Mutex<Option<String>>>> = None;
    while let Ok(command) = receiver.recv() {
        let command = match command {
            Command::Diagnostics {
                session: token,
                reply,
            } => {
                let result = if token != session {
                    Err("stale emulator session".into())
                } else {
                    runtime
                        .as_mut()
                        .map(DesktopRuntime::diagnostics)
                        .ok_or_else(|| "No firmware selected".into())
                };
                let _ = reply.send(result);
                continue;
            }
            command => command,
        };
        let (request_session, reply) = match &command {
            Command::Diagnostics { .. } => unreachable!("diagnostics handled above"),
            Command::Startup { reply } => (session, reply.clone()),
            Command::Load { session, reply, .. }
            | Command::Restart { session, reply, .. }
            | Command::Step { session, reply, .. }
            | Command::Button { session, reply, .. }
            | Command::Turn { session, reply, .. }
            | Command::Stop { session, reply } => (*session, reply.clone()),
        };
        if request_session != session
            && !matches!(command, Command::Load { .. } | Command::Stop { .. })
        {
            let _ = reply.send(Err("stale emulator session".into()));
            continue;
        }
        let result = match command {
            Command::Diagnostics { .. } => unreachable!("diagnostics handled above"),
            Command::Startup { .. } => runtime
                .as_mut()
                .ok_or_else(|| "No startup firmware selected".to_string())
                .map(|runtime| runtime.snapshot()),
            Command::Load {
                session: next,
                bytes,
                card: image,
                ..
            } => {
                if next <= session {
                    Err("stale emulator session".into())
                } else {
                    runtime = None;
                    read_error = None;
                    session = next;
                    cached = None;
                    if bytes.len() > MAX_SYX {
                        Err("firmware image exceeds 32 MiB cache limit".into())
                    } else {
                        match runtime_from(&bytes, image.clone(), audio.as_ref()) {
                            Ok((next_runtime, error)) => {
                                cached = Some((bytes, image));
                                read_error = error;
                                runtime = Some(next_runtime);
                                Ok(runtime.as_mut().expect("new runtime").snapshot())
                            }
                            Err(error) => Err(error),
                        }
                    }
                }
            }
            Command::Restart { next_session, .. } => match cached.clone() {
                Some((bytes, image)) if next_session > session => {
                    match runtime_from(&bytes, image, audio.as_ref()) {
                        Ok((next, error)) => {
                            session = next_session;
                            read_error = error;
                            runtime = Some(next);
                            Ok(runtime.as_mut().expect("restarted runtime").snapshot())
                        }
                        Err(error) => Err(error),
                    }
                }
                Some(_) => Err("stale emulator session".into()),
                None => Err("No firmware selected".into()),
            },
            Command::Step { budget, .. } => runtime
                .as_mut()
                .ok_or_else(|| "No firmware selected".into())
                .and_then(|runtime| {
                    let snapshot = runtime.step_chunk(budget.min(250_000));
                    if let Some(error) = read_error
                        .as_ref()
                        .and_then(|error| error.lock().ok().and_then(|mut slot| slot.take()))
                    {
                        Err(format!("card image read: {error}"))
                    } else {
                        Ok(snapshot)
                    }
                }),
            Command::Button { code, down, .. } => runtime
                .as_mut()
                .ok_or_else(|| "No firmware selected".into())
                .and_then(|runtime| {
                    runtime.button(code, down)?;
                    Ok(runtime.snapshot())
                }),
            Command::Turn {
                encoder, detents, ..
            } => runtime
                .as_mut()
                .ok_or_else(|| "No firmware selected".into())
                .and_then(|runtime| {
                    runtime.turn(encoder, detents)?;
                    Ok(runtime.snapshot())
                }),
            Command::Stop { .. } => {
                if request_session < session {
                    Err("stale emulator session".into())
                } else {
                    session = request_session;
                    let result = runtime
                        .as_mut()
                        .ok_or_else(|| "No firmware selected".to_string())
                        .map(|runtime| runtime.snapshot());
                    if result.is_ok() {
                        runtime = None;
                        cached = None;
                    }
                    result
                }
            }
        };
        let _ = reply.send(result);
    }
}
async fn send(
    host: State<'_, Host>,
    command: impl FnOnce(Reply) -> Command + Send + 'static,
) -> Result<Snapshot, String> {
    let sender = host.sender.clone();
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
#[tauri::command]
async fn emu_diagnostics(
    host: State<'_, Host>,
    session_id: u64,
) -> Result<serde_json::Value, String> {
    let sender = host.sender.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (reply, response) = mpsc::channel();
        sender
            .send(Command::Diagnostics {
                session: session_id,
                reply,
            })
            .map_err(|_| "emulator actor stopped")?;
        response
            .recv()
            .map_err(|_| "emulator actor dropped reply")?
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn emu_load(
    host: State<'_, Host>,
    request: tauri::ipc::Request<'_>,
) -> Result<Snapshot, String> {
    let session_id = request
        .headers()
        .get("x-session-id")
        .and_then(|value| value.to_str().ok())
        .ok_or("missing x-session-id")?
        .parse::<u64>()
        .map_err(|_| "invalid x-session-id")?;
    let bytes = match request.body() {
        tauri::ipc::InvokeBody::Raw(bytes) => bytes.clone(),
        _ => return Err("firmware must use a raw byte body".into()),
    };
    send(host, move |reply| Command::Load {
        session: session_id,
        bytes,
        card: None,
        reply,
    })
    .await
}
#[tauri::command]
async fn emu_startup(host: State<'_, Host>) -> Result<Snapshot, String> {
    send(host, |reply| Command::Startup { reply }).await
}
#[tauri::command]
async fn emu_restart(
    host: State<'_, Host>,
    session_id: u64,
    next_session_id: u64,
) -> Result<Snapshot, String> {
    send(host, move |reply| Command::Restart {
        session: session_id,
        next_session: next_session_id,
        reply,
    })
    .await
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
async fn emu_button(
    host: State<'_, Host>,
    session_id: u64,
    code: u8,
    down: bool,
) -> Result<Snapshot, String> {
    send(host, move |reply| Command::Button {
        session: session_id,
        code,
        down,
        reply,
    })
    .await
}
#[tauri::command]
async fn emu_turn(
    host: State<'_, Host>,
    session_id: u64,
    encoder: u8,
    detents: i32,
) -> Result<Snapshot, String> {
    send(host, move |reply| Command::Turn {
        session: session_id,
        encoder,
        detents,
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
fn main() {
    let startup = parse_args().unwrap_or_else(|error| {
        eprintln!("{error}");
        std::process::exit(2)
    });
    let (sender, receiver) = mpsc::channel();
    let audio = startup.audio.clone();
    thread::spawn(move || actor(receiver, audio));
    if let Some(path) = startup.syx {
        let bytes = std::fs::read(&path).unwrap_or_else(|error| {
            eprintln!("firmware input: {error}");
            std::process::exit(2)
        });
        let (reply, response) = mpsc::channel();
        sender
            .send(Command::Load {
                session: 1,
                bytes,
                card: startup.card_image,
                reply,
            })
            .unwrap_or_else(|error| {
                eprintln!("emulator startup: {error}");
                std::process::exit(2)
            });
        if let Err(error) = response
            .recv()
            .unwrap_or_else(|error| Err(error.to_string()))
        {
            eprintln!("emulator startup: {error}");
            std::process::exit(2);
        }
    }
    let host = Host { sender };
    tauri::Builder::default()
        .manage(host)
        .invoke_handler(tauri::generate_handler![
            emu_diagnostics,
            emu_startup,
            emu_load,
            emu_restart,
            emu_step,
            emu_button,
            emu_turn,
            emu_stop
        ])
        .run(tauri::generate_context!())
        .expect("tauri runtime error");
}
fn parse_args() -> Result<StartupArgs, String> {
    parse_values(std::env::args_os().skip(1))
}
fn parse_values(
    values: impl IntoIterator<Item = std::ffi::OsString>,
) -> Result<StartupArgs, String> {
    let mut values = values.into_iter();
    let (mut syx, mut card_image, mut image, mut state, mut snapshot) =
        (None, None, None, None, None);
    let mut coupled = false;
    let mut buffer = None;
    let mut no_audio = false;
    while let Some(value) = values.next() {
        let mut path = |label: &str| -> Result<PathBuf, String> {
            values
                .next()
                .map(Into::into)
                .ok_or_else(|| format!("{label} requires a file"))
        };
        if value == "--card-image" {
            card_image = Some(path("--card-image")?);
        } else if value == "--coupled" {
            coupled = true;
        } else if value == "--dsp-image" {
            image = Some(path("--dsp-image")?);
        } else if value == "--dsp-state" {
            state = Some(path("--dsp-state")?);
        } else if value == "--cf-snapshot" {
            snapshot = Some(path("--cf-snapshot")?);
        } else if value == "--no-audio" {
            no_audio = true;
        } else if value == "--audio-buffer" {
            let seconds = values.next().ok_or("--audio-buffer requires seconds")?;
            let seconds: f64 = seconds
                .to_str()
                .ok_or("invalid --audio-buffer")?
                .parse()
                .map_err(|_| "invalid --audio-buffer")?;
            if !seconds.is_finite() || !(0.0..=10.0).contains(&seconds) {
                return Err("--audio-buffer must be finite and between 0 and 10 seconds".into());
            }
            buffer = Some(seconds);
        } else if value.to_string_lossy().starts_with("--") {
            return Err(format!("unknown option: {}", value.to_string_lossy()));
        } else if syx.is_none() {
            syx = Some(value.into());
        } else {
            return Err("usage: digiemu [SYX] [--card-image FILE] [--coupled --dsp-image FILE --dsp-state FILE --cf-snapshot FILE [--audio-buffer SECONDS] [--no-audio]]".into());
        }
    }
    let audio = if coupled {
        if syx.is_none() {
            return Err("--coupled requires a startup SYX".into());
        }
        Some(AudioOptions {
            image: image.ok_or("--coupled requires --dsp-image")?,
            state: state.ok_or("--coupled requires --dsp-state")?,
            snapshot: snapshot.ok_or("--coupled requires --cf-snapshot")?,
            buffer_seconds: buffer.unwrap_or(0.0),
            playback: !no_audio,
        })
    } else {
        if image.is_some() || state.is_some() || snapshot.is_some() || buffer.is_some() || no_audio
        {
            return Err("audio options require --coupled".into());
        }
        None
    };
    Ok(StartupArgs {
        syx,
        card_image,
        audio,
    })
}
#[cfg(test)]
mod cli_tests {
    use super::*;
    fn parse(args: &[&str]) -> Result<StartupArgs, String> {
        parse_values(args.iter().map(std::ffi::OsString::from))
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
}
