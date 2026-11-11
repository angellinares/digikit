//! A device on the other end of the USB pipes, with the device rules of the plan (docs/plans/usb-fast.md sections 3.1 and 5), for host tests: one write
//! session, DATA at the next offset only (a multiple of 512 except the last piece), `E_BUSY` while the store is in use, the END hash check, ABORT, VERIFY,
//! REPLACE (the old file stays until END commits), the nonce of the identity handshake. It models the USB rules too: an OUT transfer that is a multiple of
//! 512 bytes is not delivered until a short transfer ends it, and the device sends 64-byte IN packets, each a short packet for the host, with no zero-length packets (readers loop until a frame is complete).
//! Faults are switched on by field.
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;

use crate::gen_bulk::*;
use crate::link::{FrameReader, InPipe, Link, LinkError, OutPipe, send_with_zlp};
use crate::proto;

struct Session {
    path: String,
    size: usize,
    hash: u32,
    data: Vec<u8>,
}

#[derive(Default)]
pub struct FakeDevice {
    /// The files: path -> bytes.
    pub files: HashMap<String, Vec<u8>>,
    /// The last file's bytes, for VERIFY after END.
    last: Vec<u8>,
    session: Option<Session>,
    /// IN transfers waiting for the host (frames in pieces of at most 512 bytes, zero-length ones after a 512-multiple frame).
    in_chunks: VecDeque<Vec<u8>>,
    reader: FrameReader,
    /// OUT bytes of transfers that were a multiple of 512 bytes, waiting for a short transfer.
    partial: Vec<u8>,
    /// Answer `L_E_BUSY` to the next n requests of a command (nothing is applied).
    pub busy: HashMap<u8, u32>,
    /// After each applied DATA, answer `L_E_BUSY` to the next `.0` DATA requests, at most `.1` times.
    pub busy_bursts: (u32, u32),
    burst_left: u32,
    /// END returns a hash that is wrong by one.
    pub corrupt_end_hash: bool,
    /// Requests of these commands are dropped: no effect, no reply.
    pub mute: HashSet<u8>,
    /// Hold the replies back until `release`.
    pub hold: bool,
    held: Vec<Vec<u8>>,
    /// Bytes added to the INFO reply body (to make the reply a multiple of 512 bytes).
    pub info_pad: usize,
    pub nonce: [u8; NONCE_LEN],
    /// The device keeps ZLT on for EP2 IN (the real one does).
    pub zlt: bool,
    /// The requests seen, as (cmd, flags, offset-or-0, body length).
    pub log: Vec<(u8, u16, u32, usize)>,
    /// Requests delivered and not yet answered to the host, at most (the window the client used).
    pub max_inflight: usize,
    inflight: usize,
    /// Every DATA offset the device applied.
    pub applied: Vec<u32>,
    /// OUT transfers that ended a frame without being a short packet first (a frame lost for want of the ZLP shows as `partial`).
    pub bad_frames: u32,
}

impl FakeDevice {
    pub fn new() -> Self {
        FakeDevice {
            zlt: true,
            ..Self::default()
        }
    }

    pub fn session_open(&self) -> bool {
        self.session.is_some()
    }

    /// Bytes of an OUT transfer sequence that no short packet has completed yet.
    pub fn pending_out(&self) -> usize {
        self.partial.len()
    }

    /// Deliver the held replies.
    pub fn release(&mut self) {
        for r in std::mem::take(&mut self.held) {
            self.queue_reply(r);
        }
        self.hold = false;
    }

    fn queue_reply(&mut self, r: Vec<u8>) {
        for piece in r.chunks(PACKET) {
            self.in_chunks.push_back(piece.to_vec());
        }
        if self.zlt && r.len().is_multiple_of(PACKET) {
            self.in_chunks.push_back(Vec::new());
        }
    }

    fn deliver(&mut self, frame: Vec<u8>) {
        let Ok((cmd, seq, flags, body)) = proto::parse_request(&frame) else {
            self.bad_frames += 1;
            return;
        };
        let off = if cmd == CMD_DATA && body.len() >= 4 {
            u32::from_be_bytes(body[..4].try_into().unwrap())
        } else {
            0
        };
        self.log.push((cmd, flags, off, body.len()));
        self.inflight += 1;
        self.max_inflight = self.max_inflight.max(self.inflight);
        if self.mute.contains(&cmd) {
            return;
        }
        if let Some(r) = self.handle(cmd, seq, flags, &body) {
            if self.hold {
                self.held.push(r);
            } else {
                self.queue_reply(r);
            }
        }
    }

    fn handle(&mut self, cmd: u8, seq: u8, flags: u16, body: &[u8]) -> Option<Vec<u8>> {
        if let Some(n) = self.busy.get_mut(&cmd)
            && *n > 0
        {
            *n -= 1;
            return Some(proto::reply(cmd, seq, L_E_BUSY, &[]));
        }
        if cmd == CMD_DATA && self.burst_left > 0 {
            self.burst_left -= 1;
            return Some(proto::reply(cmd, seq, L_E_BUSY, &[]));
        }
        let ok = |b: &[u8]| Some(proto::reply(cmd, seq, 0, b));
        let err = |s: i16| Some(proto::reply(cmd, seq, s, &[]));
        match cmd {
            CMD_INFO => {
                let mut b = vec![1, 0, 0, 0];
                b.extend_from_slice(&(DATA_MAX as u32).to_be_bytes());
                b.extend_from_slice(&(ALIGN as u32).to_be_bytes());
                b.extend_from_slice(&self.nonce);
                b.extend(std::iter::repeat_n(0, self.info_pad));
                ok(&b)
            }
            CMD_BEGIN => {
                if self.session.is_some() {
                    return err(L_E_BUSY);
                }
                let Some(nul) = body.get(4..).and_then(|t| t.iter().position(|&c| c == 0)) else {
                    return err(E_MSG);
                };
                let at = 4 + nul + 1;
                if body.len() != at + 8 {
                    return err(E_MSG);
                }
                let path = String::from_utf8_lossy(&body[4..4 + nul]).into_owned();
                if self.files.contains_key(&path) && flags & FLAG_REPLACE == 0 {
                    return err(L_E_EXISTS);
                }
                let size = u32::from_be_bytes(body[at..at + 4].try_into().unwrap()) as usize;
                let hash = u32::from_be_bytes(body[at + 4..at + 8].try_into().unwrap());
                self.session = Some(Session {
                    path,
                    size,
                    hash,
                    data: Vec::new(),
                });
                ok(&[])
            }
            CMD_DATA => {
                let Some(s) = self.session.as_mut() else {
                    return err(E_STATE);
                };
                if body.len() < 4 {
                    return err(E_MSG);
                }
                let offset = u32::from_be_bytes(body[..4].try_into().unwrap());
                let bytes = &body[4..];
                if offset as usize != s.data.len() {
                    return err(E_OFFSET);
                }
                let last = s.data.len() + bytes.len() == s.size;
                if bytes.is_empty()
                    || s.data.len() + bytes.len() > s.size
                    || bytes.len() > DATA_MAX
                    || (!bytes.len().is_multiple_of(ALIGN) && !last)
                {
                    return err(E_LENGTH);
                }
                s.data.extend_from_slice(bytes);
                self.applied.push(offset);
                if self.busy_bursts.1 > 0 {
                    self.busy_bursts.1 -= 1;
                    self.burst_left = self.busy_bursts.0;
                }
                ok(&[])
            }
            CMD_END => {
                let Some(s) = self.session.take() else {
                    return err(E_STATE);
                };
                if s.data.len() != s.size {
                    return err(E_LENGTH);
                }
                let h = dksample::hash::content_hash(&s.data) | 1;
                if s.hash != 0 && s.hash != h {
                    return err(E_HASH);
                }
                self.last = s.data.clone();
                self.files.insert(s.path, s.data);
                ok(&(if self.corrupt_end_hash { h ^ 2 } else { h }).to_be_bytes())
            }
            CMD_ABORT => {
                self.session = None;
                ok(&[])
            }
            CMD_VERIFY => {
                if body.len() != 8 {
                    return err(E_MSG);
                }
                let off = u32::from_be_bytes(body[..4].try_into().unwrap()) as usize;
                let len = u32::from_be_bytes(body[4..].try_into().unwrap()) as usize;
                match self.last.get(off..off + len) {
                    Some(r) => ok(&(dksample::hash::content_hash(r) | 1).to_be_bytes()),
                    None => err(E_READ),
                }
            }
            _ => err(E_PROTO),
        }
    }
}

impl OutPipe for FakeDevice {
    /// One OUT transfer. A transfer that is a multiple of 512 bytes ends no frame (the device waits for more); a short or zero-length one does.
    fn out(&mut self, data: &[u8], _timeout: Duration) -> Result<(), LinkError> {
        self.partial.extend_from_slice(data);
        if !data.is_empty() && data.len().is_multiple_of(PACKET) {
            return Ok(());
        }
        if !self.partial.is_empty() {
            let frame = std::mem::take(&mut self.partial);
            self.deliver(frame);
        }
        Ok(())
    }
}

struct Chunks<'a>(&'a mut VecDeque<Vec<u8>>);
impl InPipe for Chunks<'_> {
    fn read(&mut self, _: Duration) -> Result<Vec<u8>, LinkError> {
        self.0.pop_front().ok_or(LinkError::Timeout)
    }
}

impl Link for FakeDevice {
    fn send(&mut self, frame: &[u8], timeout: Duration) -> Result<(), LinkError> {
        send_with_zlp(self, frame, timeout)
    }

    fn recv(&mut self, timeout: Duration) -> Result<Vec<u8>, LinkError> {
        let Self {
            reader, in_chunks, ..
        } = self;
        let f = reader.next(&mut Chunks(in_chunks), timeout)?;
        self.inflight = self.inflight.saturating_sub(1);
        Ok(f)
    }
}
