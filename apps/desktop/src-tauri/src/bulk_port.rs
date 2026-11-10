//! The USB bulk link of the usb-fast module (docs/plans/usb-fast.md section 7), the second transport next to `midi_port.rs`. The page speaks the
//! bulk protocol itself in TypeScript (`packages/dk-session/src/bulk.ts`); this file only moves whole frames: it lists the devices that offer the
//! interface, opens one by its bus address, sends one request frame and waits for the next reply frame. The USB code is the `dkbulk` crate, shared with
//! its command line. Several Elektron devices can be attached and their USB serial strings are the same, so the page opens each candidate and proves
//! which one is on its MIDI port with the nonce handshake.
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use dkbulk::LinkError;
use dkbulk::usb::{self, UsbRx, UsbTx};
use serde::Serialize;
use tauri::State;

/// The open link: the two halves (a send and a recv wait at once), what keeps the interface claimed, and the cancel token of the pending
/// `bulk_recv`: `bulk_cancel` sets it and that recv ends. A recv has its own token, so a cancel meant for an earlier one cannot end a later one.
#[derive(Default)]
pub struct BulkPort {
    tx: Arc<Mutex<Option<UsbTx>>>,
    rx: Arc<Mutex<Option<UsbRx>>>,
    keep: Mutex<Option<Box<dyn std::any::Any + Send>>>,
    pending: Mutex<Option<Arc<AtomicBool>>>,
}

#[derive(Serialize)]
pub struct BulkCandidate {
    id: String,
    name: String,
}

/// The devices on the bus that offer the bulk interface (usb-fast loaded): empty means the page stays on SysEx.
#[tauri::command]
pub async fn bulk_list() -> Result<Vec<BulkCandidate>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        usb::list().map(|v| {
            v.into_iter()
                .map(|c| BulkCandidate {
                    id: c.id,
                    name: c.name,
                })
                .collect()
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Let go of the open link: the old interface is released before another is claimed (on a worker: dropping cancels transfers).
async fn release(state: &BulkPort) -> Result<(), String> {
    if let Some(t) = state.pending.lock().map_err(|e| e.to_string())?.take() {
        t.store(true, Ordering::Release);
    }
    let (tx, rx) = (state.tx.clone(), state.rx.clone());
    let keep = state.keep.lock().map_err(|e| e.to_string())?.take();
    tauri::async_runtime::spawn_blocking(move || {
        *tx.lock().map_err(|e| e.to_string())? = None;
        *rx.lock().map_err(|e| e.to_string())? = None;
        drop(keep);
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Claim the interface of the device `id` (`bus:address` of `bulk_list`), after releasing the open one.
#[tauri::command]
pub async fn bulk_open(state: State<'_, BulkPort>, id: String) -> Result<(), String> {
    release(&state).await?;
    let link = tauri::async_runtime::spawn_blocking(move || usb::UsbLink::open(Some(&id)))
        .await
        .map_err(|e| e.to_string())??;
    let (utx, urx, keep) = link.split();
    *state.tx.lock().map_err(|e| e.to_string())? = Some(utx);
    *state.rx.lock().map_err(|e| e.to_string())? = Some(urx);
    *state.keep.lock().map_err(|e| e.to_string())? = Some(keep);
    Ok(())
}

/// Release the interface.
#[tauri::command]
pub async fn bulk_close(state: State<'_, BulkPort>) -> Result<(), String> {
    release(&state).await
}

/// End the pending `bulk_recv` now (it answers as if the time had run out). Nothing pending: no effect.
#[tauri::command]
pub async fn bulk_cancel(state: State<'_, BulkPort>) -> Result<(), String> {
    if let Some(t) = state.pending.lock().map_err(|e| e.to_string())?.as_ref() {
        t.store(true, Ordering::Release);
    }
    Ok(())
}

/// Send one whole request frame (the raw body of the request); the zero-length packet after a 512-multiple frame is sent here. Returns when done.
#[tauri::command]
pub async fn bulk_send(
    state: State<'_, BulkPort>,
    request: tauri::ipc::Request<'_>,
) -> Result<(), String> {
    let bytes = match request.body() {
        tauri::ipc::InvokeBody::Raw(bytes) => bytes.clone(),
        _ => return Err("a bulk frame must use a raw byte body".into()),
    };
    let slot = state.tx.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut guard = slot.lock().map_err(|e| e.to_string())?;
        let tx = guard.as_mut().ok_or("no bulk link is open")?;
        tx.send(&bytes, Duration::from_secs(10))
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The next reply frame, or an empty reply after `timeout_ms` or when `bulk_cancel` was called. A zero-length IN transfer is skipped inside `recv`.
#[tauri::command]
pub async fn bulk_recv(
    state: State<'_, BulkPort>,
    timeout_ms: u64,
) -> Result<tauri::ipc::Response, String> {
    let slot = state.rx.clone();
    let token = Arc::new(AtomicBool::new(false));
    *state.pending.lock().map_err(|e| e.to_string())? = Some(token.clone());
    tauri::async_runtime::spawn_blocking(move || {
        let mut guard = slot.lock().map_err(|e| e.to_string())?;
        let rx = guard.as_mut().ok_or("no bulk link is open")?;
        let end = Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            // waits in slices so a cancel is seen within 50 ms
            let slice = end
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(50));
            match rx.recv(slice) {
                Ok(frame) => return Ok(tauri::ipc::Response::new(frame)),
                Err(LinkError::Timeout) => {
                    if token.load(Ordering::Acquire) || Instant::now() >= end {
                        return Ok(tauri::ipc::Response::new(Vec::new()));
                    }
                }
                Err(e) => return Err(e.to_string()),
            }
        }
    })
    .await
    .map_err(|e| e.to_string())?
}
