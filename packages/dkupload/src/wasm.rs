//! The C ABI the web app loads (`apps/web/public/dkupload.wasm`, built by tools/nx/wasm.mjs): the
//! same codec, upload machine and mock device as the desktop app, driven from TypeScript.
//!
//! Results are read after a call from the output buffer (`dk_out_ptr`/`dk_out_len`) or the error buffer
//! (`dk_err_ptr`/`dk_err_len`, a JSON object). Functions return 0 on success and 1 on failure unless said
//! otherwise. Handles are small integers; 0 is never valid.

use crate::codec::*;
use crate::mock::MockDevice;
use crate::proto::*;
use crate::put::PutMachine;
use serde_json::{Value, json};
use std::cell::RefCell;

#[derive(Default)]
struct State {
    out: Vec<u8>,
    err: Vec<u8>,
    puts: Vec<Option<PutMachine>>,
    mocks: Vec<Option<MockDevice>>,
}

thread_local! {
    static ST: RefCell<State> = RefCell::new(State::default());
}

fn error_json(e: &Error) -> Value {
    match e {
        Error::Device { status, free_bytes } => json!({
            "error": e.to_string(), "kind": "device", "status": status.name(), "code": status.code(),
            "free_bytes": free_bytes,
        }),
        Error::Transport(_) => json!({"error": e.to_string(), "kind": "transport"}),
        Error::Timeout => json!({"error": e.to_string(), "kind": "timeout"}),
        Error::Protocol(_) => json!({"error": e.to_string(), "kind": "protocol"}),
        Error::Request(_) => json!({"error": e.to_string(), "kind": "request"}),
        Error::Cancelled => json!({"error": e.to_string(), "kind": "cancelled"}),
    }
}

fn fail(e: Error) -> i32 {
    ST.with(|s| s.borrow_mut().err = error_json(&e).to_string().into_bytes());
    1
}
fn out(bytes: Vec<u8>) -> i32 {
    ST.with(|s| s.borrow_mut().out = bytes);
    0
}
fn out_json(v: Value) -> i32 {
    out(v.to_string().into_bytes())
}

/// # Safety
/// `p..p+n` must be a readable range.
unsafe fn slice<'a>(p: *const u8, n: usize) -> &'a [u8] {
    if n == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(p, n) }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn dk_alloc(n: usize) -> *mut u8 {
    let mut v = Vec::<u8>::with_capacity(n.max(1));
    let p = v.as_mut_ptr();
    std::mem::forget(v);
    p
}

/// # Safety
/// `p` and `n` must be from `dk_alloc(n)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dk_free(p: *mut u8, n: usize) {
    drop(unsafe { Vec::from_raw_parts(p, 0, n.max(1)) });
}

/// Build a single request: `cmd` 1 INFO, 2 LIST (`num` = first index), 0x13 DELETE (`a` = path), 0x14 CLEAR,
/// 0x15 ABORT. The message is in the output buffer.
///
/// # Safety
/// `a..a+a_len` must be readable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dk_cmd(cmd: u32, seq: u32, a: *const u8, a_len: usize, num: u32) -> i32 {
    let a = unsafe { slice(a, a_len) };
    let body = match cmd as u8 {
        CMD_INFO | CMD_ABORT => Ok(vec![]),
        CMD_LIST => Ok(num.to_be_bytes().to_vec()),
        CMD_DELETE => path_body(&String::from_utf8_lossy(a)),
        CMD_CLEAR => Ok(CLEAR_MAGIC.to_be_bytes().to_vec()),
        _ => Err(Error::Request(format!(
            "command {cmd:#x} is not a single request"
        ))),
    };
    match body {
        Ok(b) => out(encode_request(cmd as u8, seq as u8, &b)),
        Err(e) => fail(e),
    }
}

/// Decode a reply to JSON in the output buffer: `{cmd, seq, status, name, ...}` plus `info` or `list`; a non-zero
/// status has `error`, and `free_bytes` for E_FULL. 1 only when the message does not parse.
///
/// # Safety
/// `m..m+n` must be readable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dk_decode(m: *const u8, n: usize) -> i32 {
    let msg = match decode(unsafe { slice(m, n) }) {
        Ok(m) => m,
        Err(e) => return fail(Error::Protocol(e)),
    };
    let mut v = json!({"cmd": msg.cmd, "seq": msg.seq, "reply": msg.reply});
    match reply_body(&msg) {
        Err(e) => {
            let mut j = error_json(&e);
            j["cmd"] = msg.cmd.into();
            j["seq"] = msg.seq.into();
            return out_json(j);
        }
        Ok(body) => {
            v["status"] = 0.into();
            v["name"] = "OK".into();
            match msg.cmd {
                CMD_INFO => match parse_info(body) {
                    Ok(i) => {
                        v["info"] = json!({
                            "protocol": i.protocol, "runtime_version": i.runtime_version, "port": i.port,
                            "free_bytes": i.free_bytes, "max_chunk": i.max_chunk,
                            "safe_mode": i.safe_mode(), "upload_open": i.upload_open(),
                        })
                    }
                    Err(e) => return fail(e),
                },
                CMD_LIST => match parse_list(body) {
                    Ok(l) => {
                        v["list"] = json!({
                            "total": l.total,
                            "entries": l.entries.iter().map(|e| json!({
                                "path": e.path, "size": e.size, "sha256": hex(&e.sha256),
                            })).collect::<Vec<_>>(),
                        })
                    }
                    Err(e) => return fail(e),
                },
                _ => {}
            }
        }
    }
    out_json(v)
}

fn put_with<R>(h: u32, f: impl FnOnce(&mut PutMachine) -> R) -> Option<R> {
    ST.with(|s| s.borrow_mut().puts.get_mut(h as usize)?.as_mut().map(f))
}

/// Start an upload. Returns the handle (>= 1), or 0 with the error buffer set.
///
/// # Safety
/// The ranges must be readable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dk_put_new(
    path: *const u8,
    path_len: usize,
    data: *const u8,
    data_len: usize,
    chunk: u32,
    seq: u32,
) -> u32 {
    let (path, data) = unsafe { (slice(path, path_len), slice(data, data_len)) };
    match PutMachine::new(
        &String::from_utf8_lossy(path),
        data.to_vec(),
        chunk as usize,
        seq as u8,
    ) {
        Ok(m) => ST.with(|s| {
            let mut s = s.borrow_mut();
            if s.puts.is_empty() {
                s.puts.push(None);
            }
            s.puts.push(Some(m));
            (s.puts.len() - 1) as u32
        }),
        Err(e) => {
            fail(e);
            0
        }
    }
}

/// 1 and the message in the output buffer when there is one to send, else 0.
#[unsafe(no_mangle)]
pub extern "C" fn dk_put_next(h: u32) -> i32 {
    match put_with(h, |m| m.next_send()).flatten() {
        Some(msg) => {
            out(msg);
            1
        }
        None => 0,
    }
}

/// # Safety
/// `m..m+n` must be readable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dk_put_reply(h: u32, m: *const u8, n: usize) -> i32 {
    let Ok(msg) = decode(unsafe { slice(m, n) }) else {
        return 0;
    }; // not ours: ignore
    match put_with(h, |p| p.on_reply(&msg)) {
        Some(Ok(())) => 0,
        Some(Err(e)) => fail(e),
        None => fail(Error::Request("no such upload".into())),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn dk_put_timeout(h: u32) -> i32 {
    match put_with(h, |p| p.on_timeout()) {
        Some(Ok(())) => 0,
        Some(Err(e)) => fail(e),
        None => fail(Error::Request("no such upload".into())),
    }
}

/// `{phase, done, total, seq, end_retried}` in the output buffer.
#[unsafe(no_mangle)]
pub extern "C" fn dk_put_state(h: u32) -> i32 {
    match put_with(h, |p| {
        json!({
            "phase": format!("{:?}", p.phase()).to_lowercase(), "done": p.acked_bytes(), "total": p.total(),
            "seq": p.seq(), "end_retried": p.end_retried(), "sha256": hex(&p.sha256()),
        })
    }) {
        Some(v) => out_json(v),
        None => fail(Error::Request("no such upload".into())),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn dk_put_free(h: u32) {
    ST.with(|s| {
        if let Some(slot) = s.borrow_mut().puts.get_mut(h as usize) {
            *slot = None;
        }
    });
}

fn mock_with<R>(h: u32, f: impl FnOnce(&mut MockDevice) -> R) -> Option<R> {
    ST.with(|s| s.borrow_mut().mocks.get_mut(h as usize)?.as_mut().map(f))
}

/// A mock device (for tests); returns its handle.
#[unsafe(no_mangle)]
pub extern "C" fn dk_mock_new(capacity: u32) -> u32 {
    ST.with(|s| {
        let mut s = s.borrow_mut();
        if s.mocks.is_empty() {
            s.mocks.push(None);
        }
        s.mocks.push(Some(MockDevice::new(capacity as u64)));
        (s.mocks.len() - 1) as u32
    })
}

/// 1 and the reply in the output buffer, or 0 when the request was dropped.
///
/// # Safety
/// `m..m+n` must be readable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dk_mock_handle(h: u32, m: *const u8, n: usize) -> i32 {
    let raw = unsafe { slice(m, n) }.to_vec();
    match mock_with(h, |d| d.handle(&raw)).flatten() {
        Some(r) => {
            out(r);
            1
        }
        None => 0,
    }
}

/// Fault injection: kind 0 drop the next `n` requests, 1 drop the next `n` replies, 2 no store (n != 0), 3 safe mode.
#[unsafe(no_mangle)]
pub extern "C" fn dk_mock_ctl(h: u32, kind: u32, n: u32) {
    mock_with(h, |d| match kind {
        0 => d.drop_requests = n,
        1 => d.drop_replies = n,
        2 => d.no_store = n != 0,
        _ => d.safe_mode = n != 0,
    });
}

/// `[{path, size, sha256}]` of the mock's files in the output buffer.
#[unsafe(no_mangle)]
pub extern "C" fn dk_mock_files(h: u32) -> i32 {
    let v = mock_with(h, |d| {
        d.files
            .iter()
            .map(|(p, b)| json!({"path": p, "size": b.len(), "sha256": hex(&sha256(b))}))
            .collect::<Vec<_>>()
    });
    out_json(json!(v.unwrap_or_default()))
}

macro_rules! getter {
    ($ptr:ident, $len:ident, $field:ident) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn $ptr() -> *const u8 {
            ST.with(|s| s.borrow().$field.as_ptr())
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn $len() -> usize {
            ST.with(|s| s.borrow().$field.len())
        }
    };
}
getter!(dk_out_ptr, dk_out_len, out);
getter!(dk_err_ptr, dk_err_len, err);
