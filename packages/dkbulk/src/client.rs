//! The host side of the bulk protocol over any `Link`: INFO, a whole-file PUT with two DATA requests in flight, VERIFY, ABORT.
//!
//! Device rules the client relies on (docs/plans/usb-fast.md sections 3.1 and 5): DATA takes the next offset only, so after a refused DATA the
//! requests behind it fail with `E_OFFSET` and wrote nothing; `L_E_BUSY` (the store is in use) is retried after a short wait; the device answers in
//! order. The client keeps the sequence numbers of the requests whose reply it has not read (`owed`): a reply that is not the one awaited is dropped
//! and noted, never an error; after a timeout or a failure the session reads or discards the owed replies before the next request (`resync`).
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::gen_bulk::*;
use crate::link::{Link, LinkError};
use crate::proto::{self, FrameError, Reply};

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Link(LinkError),
    Frame(FrameError),
    /// The device answered a status other than 0 (and not a retried `E_BUSY`).
    Device {
        cmd: u8,
        status: i16,
    },
    /// A short body or the like.
    Protocol(String),
    /// END returned a hash other than the expected one.
    HashMismatch {
        device: u32,
        expected: u32,
    },
    Cancelled,
    /// The stream is corrupt (bad magic, oversize frame): the link was closed and the session is unusable.
    Fatal(String),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Link(e) => e.fmt(f),
            Error::Frame(e) => write!(f, "bad frame: {e:?}"),
            Error::Device { cmd, status } => write!(
                f,
                "the device answered status {status} to command {cmd:#04x}"
            ),
            Error::Protocol(s) => write!(f, "protocol error: {s}"),
            Error::HashMismatch { device, expected } => {
                write!(
                    f,
                    "the device hash {device:#010x} is not the expected {expected:#010x}"
                )
            }
            Error::Cancelled => write!(f, "cancelled"),
            Error::Fatal(s) => write!(f, "the bulk link is unusable: {s}"),
        }
    }
}
impl std::error::Error for Error {}
impl From<LinkError> for Error {
    fn from(e: LinkError) -> Self {
        Error::Link(e)
    }
}

#[derive(Clone, Debug)]
pub struct Options {
    /// Per reply. A DATA reply can take as long as the card needs for 64 KiB.
    pub timeout: Duration,
    /// How long `resync` waits for each reply still owed.
    pub resync_timeout: Duration,
    /// DATA requests in flight.
    pub window: usize,
    /// Data bytes of one DATA request: a multiple of `ALIGN`, at most `DATA_MAX`.
    pub chunk: usize,
    /// How often a request that got `E_BUSY` is sent again in a row (reset by every acknowledged DATA), and the wait before each.
    pub busy_retries: u32,
    pub busy_wait: Duration,
}
impl Default for Options {
    fn default() -> Self {
        Options {
            timeout: Duration::from_secs(5),
            resync_timeout: Duration::from_millis(500),
            window: 2,
            chunk: DATA_MAX,
            busy_retries: 150,
            busy_wait: Duration::from_millis(20),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Info {
    pub version: u8,
    pub flags: u8,
    pub data_max: u32,
    pub align: u32,
    /// What SET_NONCE left in the device (all zero before).
    pub nonce: [u8; NONCE_LEN],
}

static NEVER: AtomicBool = AtomicBool::new(false);

pub struct Client<L: Link> {
    pub link: L,
    pub opts: Options,
    seq: u8,
    /// Sequence numbers of the requests sent whose reply has not been read, oldest first.
    owed: VecDeque<u8>,
    fatal: Option<String>,
    /// What the client dropped or did on its own (late replies, resyncs), for the log.
    pub notes: Vec<String>,
}

impl<L: Link> Client<L> {
    pub fn new(link: L, opts: Options) -> Self {
        assert!(
            opts.window >= 1
                && opts.chunk >= ALIGN
                && opts.chunk <= DATA_MAX
                && opts.chunk.is_multiple_of(ALIGN)
        );
        Client {
            link,
            opts,
            seq: 0,
            owed: VecDeque::new(),
            fatal: None,
            notes: Vec::new(),
        }
    }

    fn fatal(&mut self, why: String) -> Error {
        self.link.close();
        self.fatal = Some(why.clone());
        Error::Fatal(why)
    }

    fn live(&self) -> Result<(), Error> {
        match &self.fatal {
            Some(w) => Err(Error::Fatal(w.clone())),
            None => Ok(()),
        }
    }

    fn send(&mut self, cmd: u8, flags: u16, body: &[u8]) -> Result<u8, Error> {
        self.live()?;
        let seq = self.seq;
        self.seq = self.seq.wrapping_add(1);
        self.link
            .send(&proto::request(cmd, seq, flags, body), self.opts.timeout)?;
        self.owed.push_back(seq);
        Ok(seq)
    }

    /// The next frame: waits in slices so `cancel` is seen within 50 ms. A corrupt stream is fatal.
    fn recv_frame(&mut self, wait: Duration, cancel: &AtomicBool) -> Result<Vec<u8>, Error> {
        self.live()?;
        let end = Instant::now() + wait;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let slice = end
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(50));
            match self.link.recv(slice) {
                Ok(f) => return Ok(f),
                Err(LinkError::Timeout) if Instant::now() < end => {}
                Err(LinkError::Frame(e)) => return Err(self.fatal(format!("{e:?}"))),
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// A reply read: it leaves the owed list, with every request before it (the device answers in order: those replies were lost).
    fn settle(&mut self, r: &Reply) {
        if let Some(at) = self.owed.iter().position(|&s| s == r.seq) {
            self.owed.drain(..=at);
        }
    }

    /// The reply to request `want`. Replies to other requests (late ones from a request that timed out) are dropped and noted.
    fn recv_reply(&mut self, cmd: u8, want: u8, cancel: &AtomicBool) -> Result<Reply, Error> {
        let end = Instant::now() + self.opts.timeout;
        loop {
            // a stream of replies that never match is a timeout too
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(LinkError::Timeout.into());
            }
            let frame = self.recv_frame(left, cancel)?;
            let r = match proto::parse_reply(&frame) {
                Ok(r) => r,
                Err(e) => return Err(self.fatal(format!("{e:?}"))),
            };
            self.settle(&r);
            if r.seq == want && r.cmd == cmd {
                return Ok(r);
            }
            self.notes.push(format!(
                "dropped a reply to command {:#04x} seq {} while waiting for seq {want}",
                r.cmd, r.seq
            ));
        }
    }

    /// Read or discard the replies still owed (late ones of a timed-out request, the DATA requests in flight of a cancelled one). A reply that never
    /// comes ends the wait: the rest is forgotten (its late reply is dropped when it arrives).
    fn resync(&mut self) -> Result<(), Error> {
        let end = Instant::now() + self.opts.resync_timeout * (self.owed.len() as u32 + 1);
        while !self.owed.is_empty() {
            let slice = if Instant::now() >= end {
                Duration::ZERO
            } else {
                self.opts.resync_timeout
            };
            match self.recv_frame(slice, &NEVER) {
                Ok(f) => match proto::parse_reply(&f) {
                    Ok(r) => {
                        self.notes.push(format!(
                            "resync: late reply to command {:#04x} seq {}",
                            r.cmd, r.seq
                        ));
                        self.settle(&r);
                    }
                    Err(e) => return Err(self.fatal(format!("{e:?}"))),
                },
                Err(Error::Link(LinkError::Timeout)) => {
                    self.notes
                        .push(format!("resync: {} replies never came", self.owed.len()));
                    self.owed.clear();
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// One request and its reply, sent again after `E_BUSY` up to `retries` times. A status other than 0 is an `Error::Device`.
    fn call_with(
        &mut self,
        cmd: u8,
        flags: u16,
        body: &[u8],
        retries: u32,
    ) -> Result<Vec<u8>, Error> {
        self.resync()?;
        let mut tries = 0;
        loop {
            let seq = self.send(cmd, flags, body)?;
            let r = self.recv_reply(cmd, seq, &NEVER)?;
            match r.status {
                0 => return Ok(r.body),
                L_E_BUSY if tries < retries => {
                    tries += 1;
                    std::thread::sleep(self.opts.busy_wait);
                }
                status => return Err(Error::Device { cmd, status }),
            }
        }
    }

    fn call(&mut self, cmd: u8, flags: u16, body: &[u8]) -> Result<Vec<u8>, Error> {
        self.call_with(cmd, flags, body, self.opts.busy_retries)
    }

    pub fn info(&mut self) -> Result<Info, Error> {
        let b = self.call(CMD_INFO, 0, &[])?;
        if b.len() < 12 + NONCE_LEN {
            return Err(Error::Protocol(format!("INFO body of {} bytes", b.len())));
        }
        Ok(Info {
            version: b[0],
            flags: b[1],
            data_max: u32::from_be_bytes(b[4..8].try_into().unwrap()),
            align: u32::from_be_bytes(b[8..12].try_into().unwrap()),
            nonce: b[12..12 + NONCE_LEN].try_into().unwrap(),
        })
    }

    /// Drop the device's write session: the owed replies are read or discarded first, then ABORT, and its reply is confirmed.
    pub fn abort(&mut self) -> Result<(), Error> {
        self.call(CMD_ABORT, 0, &[]).map(|_| ())
    }

    /// The device's hash of `len` bytes at `offset` of the open or the last file.
    pub fn verify(&mut self, offset: u32, len: u32) -> Result<u32, Error> {
        hash_of(
            CMD_VERIFY,
            &self.call(CMD_VERIFY, 0, &proto::verify_body(offset, len))?,
        )
    }

    /// Write the file `path` under `root`. `replace`: the destination may exist and is replaced atomically at END (the REPLACE flag of BEGIN), a failed
    /// upload keeps the old file; clear: an existing file gives `L_E_EXISTS`. `expected`: the file hash (`hash|1`) the device must reach; it checks it at
    /// END and the client compares what END returns. `cancel` is read between requests and while waiting: when set, the outstanding replies are
    /// read or discarded, the file aborted and `Error::Cancelled` returned. A failed PUT aborts the device's session. Returns the device's hash from END.
    #[allow(clippy::too_many_arguments)]
    pub fn put(
        &mut self,
        root: u32,
        path: &str,
        data: &[u8],
        expected: Option<u32>,
        replace: bool,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(usize, usize),
    ) -> Result<u32, Error> {
        let size =
            u32::try_from(data.len()).map_err(|_| Error::Protocol("a file over 4 GiB".into()))?;
        let flags = if replace { FLAG_REPLACE } else { 0 };
        let body = proto::begin_body(root, path, size, expected);
        match self.call(CMD_BEGIN, flags, &body) {
            Ok(_) => {}
            // still busy after the retries: a session of ours may be left open (an earlier upload that failed): drop it once, try once more
            Err(Error::Device {
                status: L_E_BUSY, ..
            }) => {
                self.notes
                    .push("BEGIN stayed busy: ABORT, then BEGIN once more".into());
                let _ = self.abort();
                self.call_with(CMD_BEGIN, flags, &body, 0)?;
            }
            Err(e) => return Err(e),
        }
        if let Err(e) = self.transfer(data, cancel, progress) {
            let _ = self.abort_session();
            return Err(e);
        }
        let hash = match self
            .call(CMD_END, 0, &[])
            .and_then(|b| hash_of(CMD_END, &b))
        {
            Ok(h) => h,
            Err(e) => {
                let _ = self.abort_session();
                return Err(e);
            }
        };
        if let Some(expected) = expected
            && hash != expected
        {
            return Err(Error::HashMismatch {
                device: hash,
                expected,
            });
        }
        Ok(hash)
    }

    /// `abort` unless the link is already unusable.
    fn abort_session(&mut self) -> Result<(), Error> {
        self.live()?;
        self.abort()
    }

    /// DATA requests with `window` in flight. A refused request ends the pipeline: the ones behind it were refused too (`E_OFFSET`) and the next
    /// request goes out at the first byte that is not acknowledged. A request leaves `owed` only when its reply is read, so an error here leaves
    /// every outstanding reply (the head included) for the caller's `abort`.
    fn transfer(
        &mut self,
        data: &[u8],
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(usize, usize),
    ) -> Result<(), Error> {
        let total = data.len();
        let (mut next, mut acked) = (0usize, 0usize);
        let mut inflight: VecDeque<(u8, usize)> = VecDeque::new(); // (seq, length)
        let (mut busy, mut tries) = (false, 0u32);
        progress(0, total);
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            while !busy && inflight.len() < self.opts.window && next < total {
                let len = self.opts.chunk.min(total - next);
                let seq = self.send(
                    CMD_DATA,
                    0,
                    &proto::data_body(next as u32, &data[next..next + len]),
                )?;
                inflight.push_back((seq, len));
                next += len;
            }
            let Some(&(seq, len)) = inflight.front() else {
                if busy {
                    tries += 1;
                    if tries > self.opts.busy_retries {
                        return Err(Error::Device {
                            cmd: CMD_DATA,
                            status: L_E_BUSY,
                        });
                    }
                    std::thread::sleep(self.opts.busy_wait);
                    busy = false;
                    next = acked;
                    continue;
                }
                return Ok(());
            };
            let r = self.recv_reply(CMD_DATA, seq, cancel)?;
            inflight.pop_front();
            if r.status == 0 && !busy {
                acked += len;
                tries = 0; // the budget is for a run of busy answers, not for the whole file
                progress(acked, total);
            } else if r.status == L_E_BUSY {
                busy = true;
            } else if !busy {
                return Err(Error::Device {
                    cmd: CMD_DATA,
                    status: r.status,
                });
            } // else: behind a busy reply, refused and wrote nothing
        }
    }
}

fn hash_of(cmd: u8, body: &[u8]) -> Result<u32, Error> {
    match body {
        [a, b, c, d, ..] => Ok(u32::from_be_bytes([*a, *b, *c, *d])),
        _ => Err(Error::Protocol(format!(
            "the reply to {cmd:#04x} has {} body bytes, not a hash",
            body.len()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::FakeDevice;
    use crate::link::OutPipe;

    fn bytes(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 131 + 7) as u8).collect()
    }
    fn hash(d: &[u8]) -> u32 {
        dksample::hash::content_hash(d) | 1
    }
    fn opts() -> Options {
        Options {
            timeout: Duration::from_millis(20),
            resync_timeout: Duration::from_millis(5),
            busy_wait: Duration::ZERO,
            chunk: 4096,
            ..Options::default()
        }
    }
    fn client(dev: FakeDevice) -> Client<FakeDevice> {
        Client::new(dev, opts())
    }
    static NO: AtomicBool = AtomicBool::new(false);
    fn put(c: &mut Client<FakeDevice>, path: &str, d: &[u8], replace: bool) -> Result<u32, Error> {
        c.put(1, path, d, Some(hash(d)), replace, &NO, &mut |_, _| {})
    }

    #[test]
    fn a_put_runs_two_data_requests_in_flight_and_returns_the_device_hash() {
        let data = bytes(10 * 4096 + 100);
        let mut c = client(FakeDevice::new());
        let mut seen = Vec::new();
        let h = c
            .put(
                2,
                "samples/a.wav",
                &data,
                Some(hash(&data)),
                false,
                &NO,
                &mut |a, t| seen.push((a, t)),
            )
            .unwrap();
        assert_eq!(h, hash(&data));
        assert_eq!(c.link.files["samples/a.wav"], data);
        assert_eq!(c.link.max_inflight, 2);
        assert_eq!(
            c.link.applied,
            (0..=10).map(|i| i * 4096).collect::<Vec<u32>>()
        );
        assert_eq!(seen.first(), Some(&(0, data.len())));
        assert_eq!(seen.last(), Some(&(data.len(), data.len())));
        assert!(!c.link.session_open());
    }

    #[test]
    fn the_default_chunk_is_a_64_kib_data_request() {
        let data = bytes(150_000);
        let mut c = Client::new(
            FakeDevice::new(),
            Options {
                timeout: Duration::from_millis(20),
                ..Options::default()
            },
        );
        c.put(1, "x", &data, None, false, &NO, &mut |_, _| {})
            .unwrap();
        assert_eq!(c.link.applied, vec![0, 65536, 131072]);
    }

    #[test]
    fn busy_data_is_sent_again_from_the_first_unacknowledged_byte() {
        let data = bytes(12 * 4096);
        let mut dev = FakeDevice::new();
        dev.busy.insert(CMD_DATA, 3);
        let mut c = client(dev);
        put(&mut c, "f", &data, false).unwrap();
        assert_eq!(c.link.files["f"], data);
        // each offset is applied once, in order: the requests behind a busy one were refused, not applied twice
        assert_eq!(
            c.link.applied,
            (0..12).map(|i| i * 4096).collect::<Vec<u32>>()
        );
    }

    #[test]
    fn the_busy_budget_is_reset_by_every_acknowledged_data() {
        // after each applied DATA two requests are busy: 10 busy answers in all, more than the budget of 3 in a row
        let data = bytes(6 * 4096);
        let mut dev = FakeDevice::new();
        dev.busy_bursts = (2, 5);
        let mut c = client(dev);
        c.opts.busy_retries = 3;
        put(&mut c, "f", &data, false).unwrap();
        assert_eq!(c.link.files["f"], data);
        // a stuck device still gives up
        let mut dev = FakeDevice::new();
        dev.busy.insert(CMD_DATA, 1000);
        let mut c = client(dev);
        c.opts.busy_retries = 5;
        let e = put(&mut c, "g", &bytes(1000), false).unwrap_err();
        assert_eq!(
            e,
            Error::Device {
                cmd: CMD_DATA,
                status: L_E_BUSY
            }
        );
        assert!(!c.link.session_open() && !c.link.files.contains_key("g"));
    }

    #[test]
    fn busy_end_is_retried() {
        let mut dev = FakeDevice::new();
        dev.busy.insert(CMD_END, 2);
        let mut c = client(dev);
        put(&mut c, "f", &bytes(1000), false).unwrap();
    }

    #[test]
    fn begin_that_stays_busy_gets_one_abort_and_one_more_try() {
        // an upload of ours failed and its ABORT never arrived: the session is open on the device
        let mut dev = FakeDevice::new();
        dev.mute.insert(CMD_DATA);
        dev.mute.insert(CMD_ABORT);
        let mut c = client(dev);
        c.opts.busy_retries = 2;
        assert_eq!(
            put(&mut c, "a", &bytes(8192), false).unwrap_err(),
            Error::Link(LinkError::Timeout)
        );
        assert!(c.link.session_open());
        c.link.mute.clear();
        put(&mut c, "b", &bytes(8192), false).unwrap();
        assert_eq!(c.link.files["b"], bytes(8192));
        assert!(c.notes.iter().any(|n| n.contains("ABORT, then BEGIN")));
        // the second BEGIN is sent once, with no retries of its own: an open session of someone else stays an error
        let mut c = client(FakeDevice::new());
        c.opts.busy_retries = 1;
        c.link.busy.insert(CMD_BEGIN, 100);
        let e = put(&mut c, "x", &bytes(512), false).unwrap_err();
        assert_eq!(
            e,
            Error::Device {
                cmd: CMD_BEGIN,
                status: L_E_BUSY
            }
        );
        let begins = c.link.log.iter().filter(|l| l.0 == CMD_BEGIN).count();
        assert_eq!(begins, 2 + 1); // two tries, then one more after ABORT
    }

    #[test]
    fn a_wrong_expected_hash_is_refused_by_the_device_and_leaves_no_file() {
        let data = bytes(5000);
        let mut c = client(FakeDevice::new());
        let e = c
            .put(
                1,
                "f",
                &data,
                Some(hash(&data) ^ 2),
                false,
                &NO,
                &mut |_, _| {},
            )
            .unwrap_err();
        assert_eq!(
            e,
            Error::Device {
                cmd: CMD_END,
                status: E_HASH
            }
        );
        assert!(c.link.files.is_empty() && !c.link.session_open());
    }

    #[test]
    fn an_end_hash_that_is_not_the_expected_one_is_a_mismatch() {
        let data = bytes(5000);
        let mut dev = FakeDevice::new();
        dev.corrupt_end_hash = true;
        let mut c = client(dev);
        let e = put(&mut c, "f", &data, false).unwrap_err();
        assert_eq!(
            e,
            Error::HashMismatch {
                device: hash(&data) ^ 2,
                expected: hash(&data)
            }
        );
    }

    #[test]
    fn replace_swaps_the_file_at_end_and_keeps_the_old_one_when_the_upload_fails() {
        let mut dev = FakeDevice::new();
        dev.files.insert("f".into(), vec![1, 2, 3]);
        let mut c = client(dev);
        let data = bytes(5000);
        let e = put(&mut c, "f", &data, false).unwrap_err();
        assert_eq!(
            e,
            Error::Device {
                cmd: CMD_BEGIN,
                status: L_E_EXISTS
            }
        );
        c.link.mute.insert(CMD_DATA);
        assert_eq!(
            put(&mut c, "f", &data, true).unwrap_err(),
            Error::Link(LinkError::Timeout)
        );
        assert_eq!(c.link.files["f"], vec![1, 2, 3]);
        assert!(!c.link.session_open());
        c.link.mute.clear();
        let e = c
            .put(
                1,
                "f",
                &data,
                Some(hash(&data) ^ 2),
                true,
                &NO,
                &mut |_, _| {},
            )
            .unwrap_err();
        assert_eq!(
            e,
            Error::Device {
                cmd: CMD_END,
                status: E_HASH
            }
        );
        assert_eq!(c.link.files["f"], vec![1, 2, 3]);
        put(&mut c, "f", &data, true).unwrap();
        assert_eq!(c.link.files["f"], data);
        assert!(
            c.link
                .log
                .iter()
                .any(|l| l.0 == CMD_BEGIN && l.1 == FLAG_REPLACE)
        );
    }

    #[test]
    fn a_late_reply_after_a_timeout_is_dropped_and_the_next_request_is_answered() {
        let mut c = client(FakeDevice::new());
        c.link.hold = true;
        assert_eq!(c.info().unwrap_err(), Error::Link(LinkError::Timeout));
        c.link.release(); // the reply to the timed-out INFO arrives late
        let i = c.info().unwrap();
        assert_eq!(i.align, 512);
        assert!(c.notes.iter().any(|n| n.contains("late reply")));
        // and a late reply that arrives in the middle of the next wait is dropped, not an error
        c.link.hold = true;
        assert!(c.info().is_err());
        c.link.release();
        put(&mut c, "g", &bytes(512), false).unwrap();
    }

    #[test]
    fn cancel_during_a_wait_ends_with_cancelled_and_the_link_stays_in_step() {
        let mut dev = FakeDevice::new();
        dev.mute.insert(CMD_DATA); // never answered: the put waits in recv
        let mut c = Client::new(
            dev,
            Options {
                timeout: Duration::from_secs(30),
                resync_timeout: Duration::from_millis(5),
                chunk: 4096,
                ..Options::default()
            },
        );
        let stop = AtomicBool::new(false);
        let t0 = Instant::now();
        let e = std::thread::scope(|s| {
            s.spawn(|| {
                std::thread::sleep(Duration::from_millis(60));
                stop.store(true, Ordering::Relaxed);
            });
            c.put(
                1,
                "f",
                &bytes(20 * 4096),
                None,
                false,
                &stop,
                &mut |_, _| {},
            )
            .unwrap_err()
        });
        assert_eq!(e, Error::Cancelled);
        assert!(t0.elapsed() < Duration::from_secs(5));
        assert!(!c.link.session_open() && c.link.files.is_empty());
        c.link.mute.clear();
        assert_eq!(c.info().unwrap().align, 512);
        put(&mut c, "g", &bytes(512), false).unwrap();
    }

    #[test]
    fn cancel_mid_file_reads_the_outstanding_replies_including_the_head() {
        let data = bytes(20 * 4096);
        let stop = AtomicBool::new(false);
        let mut c = client(FakeDevice::new());
        let e = c
            .put(1, "f", &data, None, false, &stop, &mut |a, _| {
                if a >= 5 * 4096 {
                    stop.store(true, Ordering::Relaxed)
                }
            })
            .unwrap_err();
        assert_eq!(e, Error::Cancelled);
        assert!(!c.link.session_open() && c.link.files.is_empty());
        assert!(c.owed.is_empty());
        assert_eq!(c.info().unwrap().align, 512);
    }

    #[test]
    fn a_failed_abort_then_a_new_put() {
        let mut dev = FakeDevice::new();
        dev.mute.insert(CMD_DATA);
        dev.mute.insert(CMD_ABORT);
        let mut c = client(dev);
        let e = put(&mut c, "f", &bytes(8192), false).unwrap_err();
        assert_eq!(e, Error::Link(LinkError::Timeout));
        assert!(c.link.session_open()); // the ABORT was lost
        c.link.mute.clear();
        c.opts.busy_retries = 1;
        put(&mut c, "g", &bytes(8192), false).unwrap();
        assert!(!c.link.session_open());
    }

    #[test]
    fn a_silent_device_times_out_and_the_session_is_aborted() {
        let mut dev = FakeDevice::new();
        dev.mute.insert(CMD_DATA);
        let mut c = client(dev);
        let e = put(&mut c, "f", &bytes(8192), false).unwrap_err();
        assert_eq!(e, Error::Link(LinkError::Timeout));
        assert!(!c.link.session_open());
    }

    #[test]
    fn empty_and_short_files_and_verify() {
        let mut c = client(FakeDevice::new());
        put(&mut c, "empty", &[], false).unwrap();
        put(&mut c, "short", &bytes(7), false).unwrap();
        assert!(c.link.files["empty"].is_empty() && c.link.files["short"].len() == 7);
        let data = bytes(3 * 4096);
        put(&mut c, "f", &data, false).unwrap();
        assert_eq!(c.verify(4096, 4096).unwrap(), hash(&data[4096..8192]));
        assert_eq!(
            c.verify(0, 3 * 4096 + 1).unwrap_err(),
            Error::Device {
                cmd: CMD_VERIFY,
                status: E_READ
            }
        );
    }

    #[test]
    fn info_carries_the_nonce_and_survives_a_512_byte_reply_with_its_zero_length_transfer() {
        let mut dev = FakeDevice::new();
        dev.nonce = [1, 2, 3, 4, 5, 6, 7, 8];
        dev.info_pad = 512 - HEADER - 12 - NONCE_LEN; // a reply of exactly 512 bytes
        let mut c = client(dev);
        assert_eq!(c.info().unwrap().nonce, [1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(c.info().unwrap().align, 512); // the ZLT after the first reply did not break the second
    }

    #[test]
    fn a_frame_that_is_a_multiple_of_512_bytes_needs_the_zero_length_packet() {
        // BEGIN of exactly 512 bytes: 12 header + 4 root + path + NUL + 4 size + 4 hash
        let path: String = "p".repeat(512 - HEADER - 4 - 1 - 8);
        let frame = proto::request(CMD_BEGIN, 0, 0, &proto::begin_body(1, &path, 0, None));
        assert_eq!(frame.len(), 512);
        // without the ZLP the device never sees the request
        let mut dev = FakeDevice::new();
        dev.out(&frame, Duration::ZERO).unwrap();
        assert!(dev.log.is_empty());
        assert_eq!(dev.pending_out(), 512);
        dev.out(&[], Duration::ZERO).unwrap();
        assert_eq!(dev.log.len(), 1);
        // the client's link sends the ZLP itself
        let mut c = client(FakeDevice::new());
        put(&mut c, &path, &[], false).unwrap();
        assert!(c.link.files.contains_key(&path));
        assert_eq!(c.link.bad_frames, 0);
    }

    #[test]
    fn a_corrupt_stream_is_fatal() {
        struct Bad(u32);
        impl Link for Bad {
            fn send(&mut self, _: &[u8], _: Duration) -> Result<(), LinkError> {
                Ok(())
            }
            fn recv(&mut self, _: Duration) -> Result<Vec<u8>, LinkError> {
                Err(LinkError::Frame(FrameError::BadMagic))
            }
            fn close(&mut self) {
                self.0 += 1;
            }
        }
        let mut c = Client::new(Bad(0), opts());
        assert!(matches!(c.info(), Err(Error::Fatal(_))));
        assert_eq!(c.link.0, 1);
        assert!(matches!(c.info(), Err(Error::Fatal(_)))); // no further request
    }

    #[test]
    fn the_device_rules_refuse_a_skipped_offset_and_a_short_piece_before_the_end() {
        let mut dev = FakeDevice::new();
        let t = Duration::from_millis(1);
        let mut say = |cmd, seq, body: &[u8]| {
            dev.send(&proto::request(cmd, seq, 0, body), t).unwrap();
            proto::parse_reply(&dev.recv(t).unwrap()).unwrap().status
        };
        assert_eq!(say(CMD_BEGIN, 0, &proto::begin_body(1, "f", 2048, None)), 0);
        assert_eq!(
            say(CMD_DATA, 1, &proto::data_body(512, &[0; 512])),
            E_OFFSET
        );
        assert_eq!(say(CMD_DATA, 2, &proto::data_body(0, &[0; 100])), E_LENGTH);
        assert_eq!(say(CMD_END, 3, &[]), E_LENGTH);
        assert_eq!(say(CMD_DATA, 4, &proto::data_body(0, &[0; 512])), E_STATE);
    }
}
