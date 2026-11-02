//! A real MIDI port for the digikit SysEx protocol (the desktop app's second target next to the emulator). The page speaks the protocol itself
//! in TypeScript (`packages/dk-session/src/dkupload-core.ts`); this file only moves whole SysEx messages: it lists the ports, opens one by name
//! (input and output of the same name), sends a message and waits for the next one. No process is started. Nothing here knows a device: the
//! port is whatever the user picks. The port code itself is the `dkmidi` crate, shared with the `dkmidi` command-line bridge.
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

use dkmidi::{Open, Recv, recv_message};
use midir::MidiInput;
use tauri::State;

/// The open port, and the cancel counter of the pending `midi_recv`: `midi_cancel` raises it, a waiting recv that sees it change ends.
/// Both are behind an `Arc` so a command can hand them to a worker thread (no command does MIDI I/O on the thread that called it).
#[derive(Default)]
pub struct MidiPort {
    open: Arc<Mutex<Option<Open>>>,
    cancel: Arc<AtomicU64>,
}

/// One MIDI client that lives as long as the app and is created on the main thread (`Builder::setup`).
///
/// CoreMIDI tells a process about ports that came or went on the run loop of the thread that created its first client (midir's
/// `MidiInput::new` calls `MIDIClientCreate`; `ports()` only reads CoreMIDI's per-process list). The commands below create their
/// clients on worker threads that never run a run loop, so a first client made there froze the list at its start state: a device plugged in
/// later was not listed until the app restarted. The main thread runs the window's run loop, so a client made there keeps the list current.
pub struct MidiWatch(#[allow(dead_code)] Mutex<Option<MidiInput>>);

impl MidiWatch {
    /// Call on the main thread, before any command can create a client. A failure leaves no client (listing still works as before).
    pub fn start() -> Self {
        Self(Mutex::new(MidiInput::new("digikit watch").ok()))
    }
}

/// The MIDI ports a device could be on: names that exist as input and as output.
#[tauri::command]
pub async fn midi_ports() -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(|| dkmidi::ports("digikit"))
        .await
        .map_err(|e| e.to_string())?
}

/// Open the port called `name` (closing the open one first).
#[tauri::command]
pub async fn midi_open(state: State<'_, MidiPort>, name: String) -> Result<(), String> {
    let slot = state.open.clone();
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let opened = dkmidi::open("digikit", &name)?;
        // the old port closes here, on the worker (closing a CoreMIDI connection can take a moment)
        *slot.lock().map_err(|e| e.to_string())? = Some(opened);
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Close the open port (on a worker: dropping a connection can block).
#[tauri::command]
pub async fn midi_close(state: State<'_, MidiPort>) -> Result<(), String> {
    state.cancel.fetch_add(1, Ordering::AcqRel);
    let slot = state.open.clone();
    tauri::async_runtime::spawn_blocking(move || {
        *slot.lock().map_err(|e| e.to_string())? = None;
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// End the pending `midi_recv` now (it answers as if the time had run out). Nothing pending: no effect.
#[tauri::command]
pub async fn midi_cancel(state: State<'_, MidiPort>) -> Result<(), String> {
    state.cancel.fetch_add(1, Ordering::AcqRel);
    Ok(())
}

/// Send one whole message (the raw body of the request).
#[tauri::command]
pub async fn midi_send(
    state: State<'_, MidiPort>,
    request: tauri::ipc::Request<'_>,
) -> Result<(), String> {
    let bytes = match request.body() {
        tauri::ipc::InvokeBody::Raw(bytes) => bytes.clone(),
        _ => return Err("a MIDI message must use a raw byte body".into()),
    };
    let slot = state.open.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut guard = slot.lock().map_err(|e| e.to_string())?;
        let open = guard.as_mut().ok_or("no MIDI port is open")?;
        open.send(&bytes)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The next complete SysEx message, or an empty reply after `timeout_ms` or when `midi_cancel` was called.
#[tauri::command]
pub async fn midi_recv(
    state: State<'_, MidiPort>,
    timeout_ms: u64,
) -> Result<tauri::ipc::Response, String> {
    let rx = {
        let guard = state.open.lock().map_err(|e| e.to_string())?;
        guard.as_ref().ok_or("no MIDI port is open")?.rx.clone()
    };
    let cancel = state.cancel.clone();
    let seen = cancel.load(Ordering::Acquire);
    tauri::async_runtime::spawn_blocking(move || {
        let rx = rx.lock().map_err(|e| e.to_string())?;
        match recv_message(&rx, Duration::from_millis(timeout_ms), &cancel, seen) {
            Recv::Message(m) => Ok(tauri::ipc::Response::new(m)),
            Recv::Timeout | Recv::Cancelled => Ok(tauri::ipc::Response::new(Vec::new())),
            Recv::Closed => Err("the MIDI port closed".to_string()),
        }
    })
    .await
    .map_err(|e| e.to_string())?
}
