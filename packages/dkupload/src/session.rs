//! A session over a `Transport`: one request at a time, and uploads through `PutMachine`.

use crate::codec::*;
use crate::gen_proto::CAP_FILEHASH;
use crate::proto::*;
use crate::put::{Phase, PutMachine};
use std::time::{Duration, Instant};

pub trait Transport {
    /// Send one whole SysEx message (`F0 .. F7`).
    fn send(&mut self, msg: &[u8]) -> Result<(), String>;
    /// The next whole SysEx message from the device, or `None` after `timeout`.
    fn recv(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>, String>;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    pub done: u64,
    pub total: u64,
}

pub struct Session<T: Transport> {
    pub transport: T,
    seq: u8,
    pub timeout: Duration,
    pub retries: u32,
    pub chunk: usize,
    /// The runtime said it has `CAP_FILEHASH` (asked once, before the first command that carries a file hash).
    file_hash_ok: bool,
}

impl<T: Transport> Session<T> {
    pub fn new(transport: T) -> Self {
        Session {
            transport,
            seq: 0,
            timeout: Duration::from_secs(2),
            retries: 2,
            chunk: MAX_CHUNK,
            file_hash_ok: false,
        }
    }

    fn take_seq(&mut self) -> u8 {
        let s = self.seq;
        self.seq = (self.seq + 1) & 0x7f;
        s
    }

    fn decode_reply(raw: &[u8]) -> Option<Message> {
        decode(raw).ok().filter(|m| m.reply)
    }

    /// One request and its reply body (after the status). Retries on a timeout with a new `seq`.
    pub fn request(&mut self, cmd: u8, body: &[u8]) -> Result<Vec<u8>, Error> {
        for _ in 0..=self.retries {
            let seq = self.take_seq();
            self.transport
                .send(&encode_request(cmd, seq, body))
                .map_err(Error::Transport)?;
            let deadline = Instant::now() + self.timeout;
            while let Some(left) = deadline.checked_duration_since(Instant::now()) {
                let Some(raw) = self.transport.recv(left).map_err(Error::Transport)? else {
                    break;
                };
                if let Some(m) = Self::decode_reply(&raw)
                    && m.seq == seq
                    && m.cmd == cmd
                {
                    return reply_body(&m).map(<[u8]>::to_vec);
                }
            }
        }
        Err(Error::Timeout)
    }

    pub fn info(&mut self) -> Result<Info, Error> {
        parse_info(&self.request(CMD_INFO, &[])?)
    }

    /// Fail at once, with `OLD_STORE`, for a runtime whose store keeps SHA-256.
    fn need_file_hash(&mut self) -> Result<(), Error> {
        if !self.file_hash_ok {
            if !self.info()?.has_cap(CAP_FILEHASH) {
                return Err(Error::Request(OLD_STORE.into()));
            }
            self.file_hash_ok = true;
        }
        Ok(())
    }

    /// All entries, paged.
    pub fn list(&mut self) -> Result<Vec<Entry>, Error> {
        self.list_under(None)
    }

    /// The entries whose path starts with `prefix` (runtime 0.3.0, capability `CAP_FOLDERS`), paged. A folder with nothing in it is an entry of size 0.
    pub fn list_prefix(&mut self, prefix: &str) -> Result<Vec<Entry>, Error> {
        self.list_under(Some(prefix))
    }

    fn list_under(&mut self, prefix: Option<&str>) -> Result<Vec<Entry>, Error> {
        self.need_file_hash()?;
        let mut out: Vec<Entry> = Vec::new();
        loop {
            let body = list_body(out.len() as u32, prefix)?;
            let page = parse_list(&self.request(CMD_LIST, &body)?)?;
            let got = page.entries.len();
            out.extend(page.entries);
            if got == 0 || out.len() as u32 >= page.total {
                return Ok(out);
            }
        }
    }

    /// MKDIR (runtime 0.3.0, capability `CAP_FOLDERS`): make a folder. Ok when it exists; `Path` when the path is a file or inside one.
    pub fn mkdir(&mut self, path: &str) -> Result<(), Error> {
        self.request(CMD_MKDIR, &path_body(path)?).map(drop)
    }

    /// RENAME (runtime 0.5.6, capability `CAP_RENAME`): rename or move a file or an empty folder. The data and its hash stay; the
    /// folder it leaves stays. `NoFile`: no such entry; `Path`: `to` is taken, inside a file or inside `from`, or a path breaks the rules.
    pub fn rename(&mut self, from: &str, to: &str) -> Result<(), Error> {
        let mut body = path_body(from)?;
        body.extend(path_body(to)?);
        self.request(CMD_RENAME, &body).map(drop)
    }

    /// READ (runtime 0.2.4, capability `CAP_READ`): (file size, the bytes) of a store file from `offset`. The device returns at
    /// most `READ_MAX` (768) bytes and none past the end, so the bytes may be fewer than `length`. Errors: `NoFile`,
    /// `Offset` (at or past the end), `Length` (0).
    pub fn read(&mut self, path: &str, offset: u32, length: u32) -> Result<(u32, Vec<u8>), Error> {
        parse_read(&self.request(CMD_READ, &read_body(path, offset, length)?)?)
    }

    /// VERIFY (runtime 0.2.4, capability `CAP_VERIFY`): the file hash slot the device computes from a fresh read of the file on the card.
    /// Errors: `NoFile`, `Read`.
    pub fn verify(&mut self, path: &str) -> Result<[u8; 32], Error> {
        self.need_file_hash()?;
        let body = self.request(CMD_VERIFY, &path_body(path)?)?;
        body.try_into()
            .map_err(|_| Error::Protocol("verify: the reply is not 32 bytes".into()))
    }

    /// VERIFY of a range (runtime 0.3.0): the hash slot of `length` bytes from `offset` (a multiple of 512), so a large file is checked in pieces.
    /// Errors: `NoFile`, `Read`, `Offset`, `Length`.
    pub fn verify_range(
        &mut self,
        path: &str,
        offset: u32,
        length: u32,
    ) -> Result<[u8; 32], Error> {
        self.need_file_hash()?;
        let mut b = path_body(path)?;
        b.extend(offset.to_be_bytes());
        b.extend(length.to_be_bytes());
        let body = self.request(CMD_VERIFY, &b)?;
        body.try_into()
            .map_err(|_| Error::Protocol("verify: the reply is not 32 bytes".into()))
    }

    /// The first 0xa0 bytes of a store file (a .dkm header; fewer for a shorter file).
    pub fn header(&mut self, path: &str) -> Result<Vec<u8>, Error> {
        self.read(path, 0, HEADER_BYTES).map(|(_, b)| b)
    }

    pub fn delete(&mut self, path: &str) -> Result<(), Error> {
        self.request(CMD_DELETE, &path_body(path)?).map(drop)
    }
    pub fn clear(&mut self) -> Result<(), Error> {
        self.request(CMD_CLEAR, &CLEAR_MAGIC.to_be_bytes())
            .map(drop)
    }
    pub fn abort(&mut self) -> Result<(), Error> {
        self.request(CMD_ABORT, &[]).map(drop)
    }

    /// Upload one file. `progress` is called after every reply and returns `false` to cancel (the upload is
    /// aborted on the device).
    pub fn put(
        &mut self,
        path: &str,
        data: Vec<u8>,
        progress: &mut dyn FnMut(Progress) -> bool,
    ) -> Result<(), Error> {
        self.need_file_hash()?;
        let mut m = PutMachine::new(path, data, self.chunk, self.seq)?;
        let result = self.drive(&mut m, progress);
        self.seq = m.seq();
        match result {
            Ok(()) => Ok(()),
            // A lost reply to END, then E_STATE on the second END: the first one committed. Look the file up.
            Err(Error::Device {
                status: Status::State,
                ..
            }) if m.phase() == Phase::End && m.end_retried() => {
                let hash = m.hash();
                if self
                    .list()?
                    .iter()
                    .any(|e| e.path == path && e.hash == hash)
                {
                    Ok(())
                } else {
                    Err(Error::Device {
                        status: Status::State,
                        free_bytes: None,
                    })
                }
            }
            Err(e) => {
                // Best effort: drop the open upload. Device errors other than E_STATE leave it open.
                if !matches!(e, Error::Transport(_)) {
                    let _ = self.abort();
                }
                Err(e)
            }
        }
    }

    fn drive(
        &mut self,
        m: &mut PutMachine,
        progress: &mut dyn FnMut(Progress) -> bool,
    ) -> Result<(), Error> {
        let total = m.total();
        loop {
            while let Some(msg) = m.next_send() {
                self.transport.send(&msg).map_err(Error::Transport)?;
            }
            if m.is_done() {
                progress(Progress { done: total, total });
                return Ok(());
            }
            match self
                .transport
                .recv(self.timeout)
                .map_err(Error::Transport)?
            {
                Some(raw) => {
                    if let Some(r) = Self::decode_reply(&raw) {
                        m.on_reply(&r)?;
                    }
                }
                None => m.on_timeout()?,
            }
            if !progress(Progress {
                done: m.acked_bytes(),
                total,
            }) {
                return Err(Error::Cancelled);
            }
        }
    }

    /// Repack: CLEAR, then upload `files` again. The device keeps old file data until CLEAR, so this is how space
    /// comes back. The caller must hold every file the store should keep (`repack_missing`).
    pub fn repack(
        &mut self,
        files: Vec<(String, Vec<u8>)>,
        progress: &mut dyn FnMut(usize, Progress) -> bool,
    ) -> Result<(), Error> {
        self.clear()?;
        for (i, (path, data)) in files.into_iter().enumerate() {
            self.put(&path, data, &mut |p| progress(i, p))?;
        }
        Ok(())
    }
}

/// The store entries that no file in `library` (path, hash) matches: a repack would lose them.
pub fn repack_missing(listing: &[Entry], library: &[(String, [u8; 32])]) -> Vec<String> {
    listing
        .iter()
        .filter(|e| !library.iter().any(|(p, s)| *p == e.path && *s == e.hash))
        .map(|e| e.path.clone())
        .collect()
}
