//! The upload of one file as a sans-io state machine: BEGIN, DATA in chunks with at most 2 in flight, END.
//! A driver (`Session::put`, or the web app through `wasm.rs`) sends what `next_send` returns, feeds each reply to
//! `on_reply` and calls `on_timeout` when no reply came in time. Replies are matched by `seq`.

use crate::codec::*;
use crate::proto::*;

pub const WINDOW: usize = 2;
pub const MAX_RETRIES: u32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Begin,
    Data,
    End,
    Done,
}

struct Chunk {
    offset: usize,
    len: usize,
    acked: bool,
    pending: Option<u8>,
    retries: u32,
}

pub struct PutMachine {
    path: String,
    data: Vec<u8>,
    hash: [u8; 32],
    seq: u8,
    phase: Phase,
    begin: Option<u8>,
    begin_retries: u32,
    end: Option<u8>,
    end_retries: u32,
    chunks: Vec<Chunk>,
    base: usize,
}

impl PutMachine {
    pub fn new(path: &str, data: Vec<u8>, chunk: usize, seq: u8) -> Result<Self, Error> {
        if !path_ok(path) {
            return Err(Error::Request(format!("not a store path: {path:?}")));
        }
        if u32::try_from(data.len()).is_err() {
            return Err(Error::Request("file larger than 4 GiB".into()));
        }
        // Chunks are a multiple of 512 bytes, at most 32 KiB; only the last may be short.
        let chunk = (chunk.clamp(512, MAX_CHUNK) / 512) * 512;
        let chunks = (0..data.len())
            .step_by(chunk)
            .map(|offset| Chunk {
                offset,
                len: chunk.min(data.len() - offset),
                acked: false,
                pending: None,
                retries: 0,
            })
            .collect();
        let hash = file_hash(&data);
        Ok(PutMachine {
            path: path.into(),
            data,
            hash,
            seq: seq & 0x7f,
            phase: Phase::Begin,
            begin: None,
            begin_retries: 0,
            end: None,
            end_retries: 0,
            chunks,
            base: 0,
        })
    }

    fn take_seq(&mut self) -> u8 {
        let s = self.seq;
        self.seq = (self.seq + 1) & 0x7f;
        s
    }
    /// The next sequence number the driver should use for its own requests.
    pub fn seq(&self) -> u8 {
        self.seq
    }
    pub fn phase(&self) -> Phase {
        self.phase
    }
    pub fn is_done(&self) -> bool {
        self.phase == Phase::Done
    }
    pub fn total(&self) -> u64 {
        self.data.len() as u64
    }
    pub fn acked_bytes(&self) -> u64 {
        self.chunks
            .iter()
            .filter(|c| c.acked)
            .map(|c| c.len as u64)
            .sum()
    }
    pub fn hash(&self) -> [u8; 32] {
        self.hash
    }
    pub fn path(&self) -> &str {
        &self.path
    }
    /// True once END went out at least twice (a lost END reply is told apart from a failed one by the driver).
    pub fn end_retried(&self) -> bool {
        self.end_retries > 0
    }

    /// The next message to send, or `None` when the machine waits for replies (or is done).
    pub fn next_send(&mut self) -> Option<Vec<u8>> {
        match self.phase {
            Phase::Done => None,
            Phase::Begin => {
                if self.begin.is_some() {
                    return None;
                }
                let s = self.take_seq();
                self.begin = Some(s);
                let body = begin_body(&self.path, self.data.len() as u32, &self.hash).ok()?;
                Some(encode_request(CMD_BEGIN, s, &body))
            }
            Phase::Data => {
                let end = (self.base + WINDOW).min(self.chunks.len());
                let idx = (self.base..end)
                    .find(|&i| !self.chunks[i].acked && self.chunks[i].pending.is_none())?;
                let s = self.take_seq();
                let c = &mut self.chunks[idx];
                c.pending = Some(s);
                let body = data_body(c.offset as u32, &self.data[c.offset..c.offset + c.len]);
                Some(encode_request(CMD_DATA, s, &body))
            }
            Phase::End => {
                if self.end.is_some() {
                    return None;
                }
                let s = self.take_seq();
                self.end = Some(s);
                Some(encode_request(CMD_END, s, &[]))
            }
        }
    }

    fn advance(&mut self) {
        while self.base < self.chunks.len() && self.chunks[self.base].acked {
            self.base += 1;
        }
        if self.phase == Phase::Data && self.base == self.chunks.len() {
            self.phase = Phase::End;
        }
    }

    /// Feed one reply. A reply with an unknown `seq` (late, or for an abandoned retry) is ignored.
    pub fn on_reply(&mut self, m: &Message) -> Result<(), Error> {
        if !m.reply {
            return Ok(());
        }
        let result = reply_body(m).map(|_| ());
        if self.begin == Some(m.seq) && m.cmd == CMD_BEGIN {
            result?;
            self.begin = None;
            self.phase = Phase::Data;
            self.advance();
            if self.chunks.is_empty() {
                self.phase = Phase::End;
            }
        } else if self.end == Some(m.seq) && m.cmd == CMD_END {
            result?;
            self.phase = Phase::Done;
        } else if let Some(idx) = self.chunks.iter().position(|c| c.pending == Some(m.seq)) {
            if m.cmd != CMD_DATA {
                return Ok(());
            }
            match result {
                Ok(()) => {
                    // The device takes chunks in order: an ack of chunk k also acks every chunk before it (their
                    // replies may have been lost).
                    for c in &mut self.chunks[..=idx] {
                        c.pending = None;
                        c.acked = true;
                    }
                    self.advance();
                }
                // A later chunk that arrived before the base one was lost: send it again after the base is acked.
                Err(Error::Device {
                    status: Status::Offset,
                    ..
                }) if idx > self.base => {
                    let c = &mut self.chunks[idx];
                    c.pending = None;
                    c.retries += 1;
                    if c.retries > MAX_RETRIES {
                        return result;
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// No reply came in time: send the oldest unanswered request again (same offset, new seq).
    pub fn on_timeout(&mut self) -> Result<(), Error> {
        match self.phase {
            Phase::Begin => {
                self.begin_retries += 1;
                if self.begin_retries > MAX_RETRIES {
                    return Err(Error::Timeout);
                }
                self.begin = None;
            }
            Phase::End => {
                self.end_retries += 1;
                if self.end_retries > MAX_RETRIES {
                    return Err(Error::Timeout);
                }
                self.end = None;
            }
            Phase::Data => {
                if let Some(c) = self.chunks[self.base..]
                    .iter_mut()
                    .find(|c| c.pending.is_some())
                {
                    c.retries += 1;
                    if c.retries > MAX_RETRIES {
                        return Err(Error::Timeout);
                    }
                    c.pending = None;
                }
            }
            Phase::Done => {}
        }
        Ok(())
    }
}
