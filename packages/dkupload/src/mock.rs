//! An in-memory device that follows the state rules of the spec (framing, upload state, offset and retry,
//! length, E_FULL, E_HASH). It does not model the store layout. For tests of the hosts, in Rust and (through
//! `wasm.rs`) in node.

use crate::codec::*;
use crate::gen_proto::{CAP_FILEHASH, CAP_FOLDERS, CAP_READ, CAP_VERIFY};
use crate::proto::{READ_MAX, Status};
use crate::session::Transport;
use std::collections::VecDeque;
use std::time::Duration;

struct Upload {
    path: String,
    size: usize,
    hash: [u8; 32],
    data: Vec<u8>,
    last: Option<(usize, usize)>,
}

pub struct MockDevice {
    pub port: String,
    pub capacity: u64,
    used: u64,
    /// Sorted by path.
    pub files: Vec<(String, Vec<u8>)>,
    /// Folders with nothing in them (made by MKDIR); a file stored inside one removes it.
    pub dirs: Vec<String>,
    upload: Option<Upload>,
    pub safe_mode: bool,
    pub no_store: bool,
    /// A runtime of the old store format: its INFO has no `CAP_FILEHASH`.
    pub old_store: bool,
    /// Fault injection: the next n requests are lost before they are handled / the next n replies are lost.
    pub drop_requests: u32,
    pub drop_replies: u32,
    /// Commands handled, in order (cmd without the reply bit), for tests that check the traffic.
    pub log: Vec<u8>,
}

/// Room kept for the table, as the real store keeps it after the staged data.
const TABLE_ROOM: u64 = 4096;

impl MockDevice {
    pub fn new(capacity: u64) -> Self {
        MockDevice {
            port: "mock-0.0".into(),
            capacity,
            used: 0,
            files: Vec::new(),
            dirs: Vec::new(),
            upload: None,
            safe_mode: false,
            no_store: false,
            old_store: false,
            drop_requests: 0,
            drop_replies: 0,
            log: Vec::new(),
        }
    }

    pub fn free_bytes(&self) -> u32 {
        self.capacity
            .saturating_sub(self.used + TABLE_ROOM)
            .min(u32::MAX as u64) as u32
    }

    fn reply(cmd: u8, seq: u8, status: Status, rest: &[u8]) -> Vec<u8> {
        let mut body = vec![status.code() as u8];
        body.extend(rest);
        encode(cmd, true, seq, &body)
    }

    /// Handle one request message; the reply, or `None` when it was dropped (or is not for this protocol).
    pub fn handle(&mut self, raw: &[u8]) -> Option<Vec<u8>> {
        if raw.len() < 8 || raw[..4] != HEAD[..4] || *raw.last()? != 0xF7 {
            return None;
        }
        if self.drop_requests > 0 {
            self.drop_requests -= 1;
            return None;
        }
        let (cmd, seq) = (raw[5], raw[6]);
        let reply = if raw[4] != HEAD[4] {
            Self::reply(cmd, seq, Status::Proto, &[])
        } else {
            match decode(raw) {
                Ok(m) if !m.reply => {
                    self.log.push(m.cmd);
                    self.dispatch(m.cmd, m.seq, &m.body)
                }
                _ => Self::reply(cmd, seq, Status::Msg, &[]),
            }
        };
        if self.drop_replies > 0 {
            self.drop_replies -= 1;
            return None;
        }
        Some(reply)
    }

    fn dispatch(&mut self, cmd: u8, seq: u8, body: &[u8]) -> Vec<u8> {
        let r = |s: Status| Self::reply(cmd, seq, s, &[]);
        match cmd {
            CMD_INFO => {
                let mut b = vec![0, 1, 0, 1];
                b.extend(self.port.as_bytes());
                b.push(0);
                b.extend(self.free_bytes().to_be_bytes());
                b.extend((MAX_CHUNK as u32).to_be_bytes());
                b.push(self.safe_mode as u8 | (self.upload.is_some() as u8) << 1);
                // the log range, the runtime version and the capability bits
                b.extend([0; 8]);
                b.extend(0x0001_0000u32.to_be_bytes());
                b.extend(
                    (CAP_VERIFY
                        | CAP_READ
                        | CAP_FOLDERS
                        | if self.old_store { 0 } else { CAP_FILEHASH })
                    .to_be_bytes(),
                );
                Self::reply(cmd, seq, Status::Ok, &b)
            }
            CMD_CLEAR => {
                if body.len() != 4
                    || u32::from_be_bytes([body[0], body[1], body[2], body[3]]) != CLEAR_MAGIC
                {
                    return r(Status::Msg);
                }
                self.files.clear();
                self.used = 0;
                self.upload = None;
                self.no_store = false;
                r(Status::Ok)
            }
            _ if self.no_store && cmd != CMD_ABORT => r(Status::NoStore),
            CMD_LIST => {
                if body.len() < 4
                    || (body.len() > 4 && (body.len() < 6 || body[body.len() - 1] != 0))
                {
                    return r(Status::Msg);
                }
                let first = u32::from_be_bytes([body[0], body[1], body[2], body[3]]) as usize;
                let prefix = (body.len() > 4).then(|| &body[4..body.len() - 1]);
                // files and empty folders, in the store's order (the mock's: by path)
                let mut all: Vec<(&str, usize, [u8; 32])> = self
                    .files
                    .iter()
                    .map(|(p, d)| (p.as_str(), d.len(), file_hash(d)))
                    .chain(self.dirs.iter().map(|p| (p.as_str(), 0, [0u8; 32])))
                    .filter(|(p, _, _)| prefix.is_none_or(|x| p.as_bytes().starts_with(x)))
                    .collect();
                all.sort_by(|a, b| a.0.cmp(b.0));
                let mut entries = Vec::new();
                let mut n = 0u8;
                for (p, size, hash) in all.iter().skip(first) {
                    let mut e = p.as_bytes().to_vec();
                    e.push(0);
                    e.extend((*size as u32).to_be_bytes());
                    e.extend(hash);
                    // 1 KiB on the wire: status + total + n (6 bytes) plus the entries, packed 8 for 7.
                    if encode(cmd, true, 0, &[&[0u8; 6][..], &entries, &e].concat()).len()
                        > MAX_REPLY_WIRE
                        || n == u8::MAX
                    {
                        break;
                    }
                    entries.extend(e);
                    n += 1;
                }
                let mut b = (all.len() as u32).to_be_bytes().to_vec();
                b.push(n);
                b.extend(entries);
                Self::reply(cmd, seq, Status::Ok, &b)
            }
            CMD_BEGIN => {
                let Some(nul) = body.iter().position(|&b| b == 0) else {
                    return r(Status::Msg);
                };
                if body.len() != nul + 1 + 4 + 32 {
                    return r(Status::Msg);
                }
                let Ok(path) = std::str::from_utf8(&body[..nul]) else {
                    return r(Status::Path);
                };
                if !path_ok(path) {
                    return r(Status::Path);
                }
                let size = u32::from_be_bytes(body[nul + 1..nul + 5].try_into().unwrap()) as usize;
                let mut hash = [0u8; 32];
                hash.copy_from_slice(&body[nul + 5..]);
                self.upload = None;
                let need = size.div_ceil(512) as u64 * 512;
                if need > self.free_bytes() as u64 {
                    return Self::reply(cmd, seq, Status::Full, &self.free_bytes().to_be_bytes());
                }
                self.upload = Some(Upload {
                    path: path.into(),
                    size,
                    hash,
                    data: Vec::new(),
                    last: None,
                });
                r(Status::Ok)
            }
            CMD_DATA => {
                let Some(up) = self.upload.as_mut() else {
                    return r(Status::State);
                };
                if body.len() < 4 {
                    return r(Status::Msg);
                }
                let offset = u32::from_be_bytes([body[0], body[1], body[2], body[3]]) as usize;
                let bytes = &body[4..];
                let len = bytes.len();
                if len == 0 || len > MAX_CHUNK {
                    return r(Status::Length);
                }
                if up.last == Some((offset, len)) {
                    return r(Status::Ok);
                }
                if offset != up.data.len() {
                    return r(Status::Offset);
                }
                if offset + len > up.size || (!len.is_multiple_of(512) && offset + len != up.size) {
                    return r(Status::Length);
                }
                up.data.extend(bytes);
                up.last = Some((offset, len));
                r(Status::Ok)
            }
            CMD_END => {
                let Some(up) = self.upload.take() else {
                    return r(Status::State);
                };
                if up.data.len() != up.size {
                    self.upload = Some(up);
                    return r(Status::Length);
                }
                if file_hash(&up.data) != up.hash {
                    return r(Status::Hash);
                }
                self.files.retain(|(p, _)| *p != up.path);
                // a folder entry goes when something is stored inside it
                self.dirs.retain(|d| !up.path.starts_with(&format!("{d}/")));
                self.files.push((up.path, up.data));
                self.files.sort_by(|a, b| a.0.cmp(&b.0));
                self.used = self
                    .files
                    .iter()
                    .map(|(_, d)| d.len().div_ceil(512) as u64 * 512)
                    .sum();
                r(Status::Ok)
            }
            CMD_DELETE => {
                let Some(nul) = body.iter().position(|&b| b == 0) else {
                    return r(Status::Msg);
                };
                let Ok(path) = std::str::from_utf8(&body[..nul]) else {
                    return r(Status::Path);
                };
                if !path_ok(path) {
                    return r(Status::Path);
                }
                let before = self.files.len() + self.dirs.len();
                self.files.retain(|(p, _)| p != path);
                self.dirs.retain(|p| p != path);
                if self.files.len() + self.dirs.len() == before {
                    r(Status::NoFile)
                } else {
                    // the space of a deleted file is free again
                    self.used = self
                        .files
                        .iter()
                        .map(|(_, d)| d.len().div_ceil(512) as u64 * 512)
                        .sum();
                    r(Status::Ok)
                }
            }
            CMD_MKDIR => {
                let Some(nul) = body.iter().position(|&b| b == 0) else {
                    return r(Status::Msg);
                };
                let Ok(path) = std::str::from_utf8(&body[..nul]) else {
                    return r(Status::Path);
                };
                if nul + 1 != body.len() || !path_ok(path) {
                    return r(Status::Path);
                }
                let inside = format!("{path}/");
                let in_file = self
                    .files
                    .iter()
                    .any(|(p, _)| p == path || path.starts_with(&format!("{p}/")));
                if in_file {
                    return r(Status::Path);
                }
                let exists = self
                    .dirs
                    .iter()
                    .any(|d| d == path || d.starts_with(&inside))
                    || self.files.iter().any(|(p, _)| p.starts_with(&inside));
                if !exists {
                    // a folder that gets a folder inside it has something in it: only the innermost entry is kept
                    self.dirs.retain(|d| !path.starts_with(&format!("{d}/")));
                    self.dirs.push(path.into());
                }
                r(Status::Ok)
            }
            CMD_RENAME => {
                // from NUL to NUL, as the runtime's `store::plan_rename` checks it
                let mut parts = body.splitn(3, |&b| b == 0);
                let (Some(f), Some(t), Some(rest)) = (parts.next(), parts.next(), parts.next())
                else {
                    return r(Status::Msg);
                };
                let (Ok(from), Ok(to)) = (std::str::from_utf8(f), std::str::from_utf8(t)) else {
                    return r(Status::Path);
                };
                if !rest.is_empty() || !path_ok(from) || !path_ok(to) || from == to {
                    return r(Status::Path);
                }
                let (fi, di) = (
                    self.files.iter().position(|(p, _)| p == from),
                    self.dirs.iter().position(|d| d == from),
                );
                let inside = |outer: &str, inner: &str| inner.starts_with(&format!("{outer}/"));
                let taken = self
                    .files
                    .iter()
                    .any(|(p, _)| p == to || inside(to, p) || inside(p, to))
                    || self.dirs.iter().any(|d| d == to || inside(to, d));
                if taken {
                    return r(Status::Path);
                }
                if fi.is_none() && di.is_none() {
                    return r(Status::NoFile);
                }
                if inside(from, to) {
                    return r(Status::Path);
                }
                match (fi, di) {
                    (Some(i), _) => {
                        self.files[i].0 = to.into();
                        self.files.sort_by(|a, b| a.0.cmp(&b.0));
                    }
                    (_, Some(i)) => {
                        self.dirs.remove(i);
                        self.dirs.push(to.into());
                    }
                    _ => {}
                }
                // a folder entry that `to` is inside goes; the folder `from` leaves stays
                self.dirs.retain(|d| !inside(d, to));
                if let Some(cut) = from.rfind('/') {
                    let parent = &from[..cut];
                    let others = self.files.iter().any(|(p, _)| inside(parent, p))
                        || self.dirs.iter().any(|d| inside(parent, d));
                    if !others {
                        self.dirs.push(parent.into());
                    }
                }
                r(Status::Ok)
            }
            CMD_READ => {
                let Some(nul) = body.iter().position(|&b| b == 0) else {
                    return r(Status::Msg);
                };
                if body.len() != nul + 1 + 8 {
                    return r(Status::Msg);
                }
                let Ok(path) = std::str::from_utf8(&body[..nul]) else {
                    return r(Status::Path);
                };
                if !path_ok(path) {
                    return r(Status::Path);
                }
                let at = u32::from_be_bytes(body[nul + 1..nul + 5].try_into().unwrap()) as usize;
                let len = u32::from_be_bytes(body[nul + 5..].try_into().unwrap()) as usize;
                let Some((_, d)) = self.files.iter().find(|(p, _)| p == path) else {
                    return r(Status::NoFile);
                };
                if len == 0 {
                    return r(Status::Length);
                }
                if at >= d.len() {
                    return r(Status::Offset);
                }
                let n = len.min(READ_MAX as usize).min(d.len() - at);
                let mut b = (d.len() as u32).to_be_bytes().to_vec();
                b.extend(&d[at..at + n]);
                Self::reply(cmd, seq, Status::Ok, &b)
            }
            CMD_VERIFY => {
                let Some(nul) = body.iter().position(|&b| b == 0) else {
                    return r(Status::Msg);
                };
                if body.len() != nul + 1 && body.len() != nul + 1 + 8 {
                    return r(Status::Msg);
                }
                let Ok(path) = std::str::from_utf8(&body[..nul]) else {
                    return r(Status::Path);
                };
                if !path_ok(path) {
                    return r(Status::Path);
                }
                let Some((_, d)) = self.files.iter().find(|(p, _)| p == path) else {
                    return r(Status::NoFile);
                };
                if body.len() == nul + 1 {
                    return Self::reply(cmd, seq, Status::Ok, &file_hash(d));
                }
                let at = u32::from_be_bytes(body[nul + 1..nul + 5].try_into().unwrap()) as usize;
                let len = u32::from_be_bytes(body[nul + 5..].try_into().unwrap()) as usize;
                if !at.is_multiple_of(512) || at >= d.len() {
                    return r(Status::Offset);
                }
                if len == 0 || len > d.len() - at {
                    return r(Status::Length);
                }
                Self::reply(cmd, seq, Status::Ok, &file_hash(&d[at..at + len]))
            }
            CMD_ABORT => {
                self.upload = None;
                r(Status::Ok)
            }
            _ => r(Status::Proto),
        }
    }
}

/// A `Transport` onto a `MockDevice`: a lost request or reply shows as a timeout at once.
pub struct MockTransport {
    pub dev: MockDevice,
    queue: VecDeque<Vec<u8>>,
}
impl MockTransport {
    pub fn new(dev: MockDevice) -> Self {
        MockTransport {
            dev,
            queue: VecDeque::new(),
        }
    }
}
impl Transport for MockTransport {
    fn send(&mut self, msg: &[u8]) -> Result<(), String> {
        if let Some(r) = self.dev.handle(msg) {
            self.queue.push_back(r);
        }
        Ok(())
    }
    fn recv(&mut self, _timeout: Duration) -> Result<Option<Vec<u8>>, String> {
        Ok(self.queue.pop_front())
    }
}
