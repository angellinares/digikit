//! The RPC flash route as a sans-io state machine (finding 75, "The RPC route"). No I/O and no clock: a driver (the command line, the
//! Tauri app and the browser through `wasm.rs`) sends what `next` returns, feeds each reply to `on_reply`, and calls `on_timeout` when
//! no reply came in time.
//!
//! The route: `OsUpgradeStart (0x50)`, `OsUpgradeWrite (0x51)` in order, then `OsUpgradeEnd (0x52)`. The device checks the whole image
//! when the last Write arrives and answers status 1. It never asks "UPGRADE OS NOW?" on this route: the host asks, and its answer is
//! End with commit 1 (flash) or 0 (leave the flash untouched).
//!
//! Choices (host side):
//! - One request in flight. The device answers every Write, so the next one goes out only after the reply.
//! - CRC mismatch (status 2): the same chunk is sent again, at most `CRC_RETRIES` times, then the run fails.
//! - No reply in time, status 3, a wrong byte count or a wrong status: the run fails and End with commit 0 is sent. A Write is not sent
//!   again after a timeout. The emulator ignores a Write below the bytes it has taken, so a resend would be safe there, but that was not
//!   seen on a real device and the last Write runs the 2 s validation; a failed run costs only a new start (nothing is flashed).
//! - After End with commit 1 nothing can be cancelled any more.

use crate::syx;

/// Every MidiRpc request starts with this envelope.
pub const ENVELOPE: [u8; 6] = [0xf0, 0x00, 0x20, 0x3c, 0x10, 0x00];
pub const RPC_OS_START: u8 = 0x50;
pub const RPC_OS_WRITE: u8 = 0x51;
pub const RPC_OS_END: u8 = 0x52;

/// Stream bytes per Write. The stock file writes use 2048; the largest size a real device takes is not known.
pub const DEFAULT_CHUNK: usize = 2048;
/// The reply to Start (the device opens the OS UPGRADE screen).
pub const START_TIMEOUT_MS: u32 = 5_000;
/// The reply to a Write: 3 ms in the emulator.
pub const WRITE_TIMEOUT_MS: u32 = 2_000;
/// The reply to the last Write: the device checks the whole image first (1.8 to 2.3 s in the emulator).
pub const LAST_WRITE_TIMEOUT_MS: u32 = 15_000;
/// The reply to End.
pub const END_TIMEOUT_MS: u32 = 5_000;
/// A chunk the device finds damaged (status 2) is sent again this many times.
pub const CRC_RETRIES: u32 = 3;

/// CRC-32 as the device computes it: zlib's `crc32(data, 0xffffffff)`.
pub fn crc32(data: &[u8]) -> u32 {
    let mut c: u32 = 0;
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0xedb8_8320
            } else {
                c >> 1
            };
        }
    }
    !c
}

/// A MidiRpc request: envelope, 8-in-7 packed `seq16 0000 type payload`, `F7`.
pub fn request(ty: u8, seq: u16, payload: &[u8]) -> Vec<u8> {
    let mut body = vec![(seq >> 8) as u8, seq as u8, 0, 0, ty];
    body.extend_from_slice(payload);
    let mut m = ENVELOPE.to_vec();
    m.extend(syx::encode_8in7(&body));
    m.push(0xf7);
    m
}

/// A MidiRpc reply: `(seq, type without the reply bit, payload)`. The unpacked reply is `counter16 seq16 (type | 0x80) payload`: the
/// request's seq is echoed at bytes 2 and 3; bytes 0 and 1 are a counter of the device's own (seen: 2, 3, ... after a boot), not the seq.
pub fn parse_reply(msg: &[u8]) -> Option<(u16, u8, Vec<u8>)> {
    if msg.len() < 13 || msg.last() != Some(&0xf7) || msg[..6] != ENVELOPE {
        return None;
    }
    let body = dkupload::codec::decode_8in7(&msg[6..msg.len() - 1]).ok()?;
    if body.len() < 5 {
        return None;
    }
    Some((
        u16::from_be_bytes([body[2], body[3]]),
        body[4] & 0x7f,
        body[5..].to_vec(),
    ))
}

/// Why a run ended without a flash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailCode {
    /// Start was refused (another upgrade is open, or the device does not take the file).
    Refused,
    /// The device found the same chunk damaged more often than `CRC_RETRIES` allows.
    Crc,
    /// No reply in time.
    Timeout,
    /// The device reported an error (status 3), or End was refused.
    Device,
    /// The reply does not fit the route (wrong byte count, wrong status).
    Protocol,
    /// The last Write did not report the image complete.
    Incomplete,
    /// The user cancelled, or answered no at the question.
    Cancelled,
}

impl FailCode {
    pub fn name(self) -> &'static str {
        match self {
            FailCode::Refused => "refused",
            FailCode::Crc => "crc",
            FailCode::Timeout => "timeout",
            FailCode::Device => "device",
            FailCode::Protocol => "protocol",
            FailCode::Incomplete => "incomplete",
            FailCode::Cancelled => "cancelled",
        }
    }
}

/// A run that ended without a flash: a code and one plain sentence (or two) for the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub code: FailCode,
    pub message: String,
    /// Bytes of the image the device had taken when the run ended.
    pub taken: usize,
}

/// What the driver does next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Send these bytes (one whole SysEx message) and wait at most `timeout_ms` for the reply. Feed it to `on_reply`, or call
    /// `on_timeout`.
    Send { bytes: Vec<u8>, timeout_ms: u32 },
    /// The device has accepted the whole image. Ask the user, then call `confirm`.
    AskConfirm,
    /// End with commit 1 was accepted: the device erases, writes and restarts by itself. The driver now watches for the restart.
    WaitReboot,
    /// The run is finished (after `WaitReboot`).
    Done,
    /// The run ended without a flash (End with commit 0 was sent where it applies).
    Failed(Failure),
}

#[derive(Clone, Debug)]
enum St {
    Start,
    AwaitStart,
    Write,
    AwaitWrite,
    Confirm,
    Commit,
    AwaitCommit,
    /// End with commit 0 is to be sent; the run then fails with this.
    Abort(Failure),
    AwaitAbort(Failure),
    Reboot,
    Finished,
    Failed(Failure),
}

pub struct RpcFlash {
    image: Vec<u8>,
    chunk: usize,
    st: St,
    seq: u16,
    /// The request in flight: its type, seq, and the bytes and timeout (for a repeated `next`).
    inflight: Option<(u8, u16, Vec<u8>, u32)>,
    /// Stream offset of the chunk being sent, and the bytes the device has taken.
    offset: usize,
    taken: usize,
    retries: u32,
    /// Was Start sent (so a cancel has something to cancel)?
    started: bool,
    /// The reply End with commit 0 waits for may be the validation of the last Write.
    abort_timeout: u32,
}

impl RpcFlash {
    /// `image` is the whole `.syx` stream. `chunk` is the number of stream bytes per Write (0: `DEFAULT_CHUNK`).
    pub fn new(image: Vec<u8>, chunk: usize) -> RpcFlash {
        let empty = image.is_empty();
        let mut m = RpcFlash {
            image,
            chunk: if chunk == 0 { DEFAULT_CHUNK } else { chunk },
            st: St::Start,
            seq: 0,
            inflight: None,
            offset: 0,
            taken: 0,
            retries: 0,
            started: false,
            abort_timeout: END_TIMEOUT_MS,
        };
        if empty {
            m.st = St::Failed(m.fail(FailCode::Protocol, "There is no image to send.".into()));
        }
        m
    }

    pub fn total(&self) -> usize {
        self.image.len()
    }
    /// Bytes of the image the device has acknowledged.
    pub fn taken(&self) -> usize {
        self.taken
    }
    pub fn chunk(&self) -> usize {
        self.chunk
    }
    /// The run is over (done, or failed).
    pub fn finished(&self) -> bool {
        matches!(self.st, St::Finished | St::Failed(_))
    }

    fn fail(&self, code: FailCode, message: String) -> Failure {
        Failure {
            code,
            message,
            taken: self.taken,
        }
    }

    fn take_seq(&mut self) -> u16 {
        self.seq = self.seq.wrapping_add(1).max(1);
        self.seq
    }

    fn send(&mut self, ty: u8, payload: &[u8], timeout_ms: u32) -> Action {
        let seq = self.take_seq();
        let bytes = request(ty, seq, payload);
        self.inflight = Some((ty, seq, bytes.clone(), timeout_ms));
        Action::Send { bytes, timeout_ms }
    }

    fn is_last(&self) -> bool {
        self.offset + self.chunk >= self.image.len()
    }

    /// Fail now if nothing is in the device that needs a cancel, else after End with commit 0.
    fn abort(&mut self, f: Failure) {
        self.inflight = None;
        self.st = if self.started {
            St::Abort(f)
        } else {
            St::Failed(f)
        };
    }

    /// The next thing to do. Call it at the start and after each `on_reply`, `on_timeout`, `confirm` and `cancel`. While a request is
    /// in flight it returns that request again.
    #[allow(
        clippy::should_implement_trait,
        reason = "a state machine step, not an iterator"
    )]
    pub fn next(&mut self) -> Action {
        match self.st.clone() {
            St::Start => {
                let mut p = (self.image.len() as u32).to_be_bytes().to_vec();
                p.extend_from_slice(b"sysex\0");
                // flag 0: the device waits for End (the host asks the question)
                p.push(0);
                self.started = true;
                self.st = St::AwaitStart;
                self.send(RPC_OS_START, &p, START_TIMEOUT_MS)
            }
            St::Write => {
                let end = (self.offset + self.chunk).min(self.image.len());
                let data = &self.image[self.offset..end];
                let mut p = crc32(data).to_be_bytes().to_vec();
                p.extend_from_slice(&(data.len() as u32).to_be_bytes());
                p.extend_from_slice(&(self.offset as u32).to_be_bytes());
                p.extend_from_slice(data);
                let t = if self.is_last() {
                    LAST_WRITE_TIMEOUT_MS
                } else {
                    WRITE_TIMEOUT_MS
                };
                self.st = St::AwaitWrite;
                self.send(RPC_OS_WRITE, &p, t)
            }
            St::Commit => {
                self.st = St::AwaitCommit;
                self.send(RPC_OS_END, &[1], END_TIMEOUT_MS)
            }
            St::Abort(f) => {
                self.st = St::AwaitAbort(f);
                let t = self.abort_timeout;
                self.send(RPC_OS_END, &[0], t)
            }
            St::AwaitStart | St::AwaitWrite | St::AwaitCommit | St::AwaitAbort(_) => {
                match &self.inflight {
                    Some((_, _, bytes, t)) => Action::Send {
                        bytes: bytes.clone(),
                        timeout_ms: *t,
                    },
                    None => Action::Done,
                }
            }
            St::Confirm => Action::AskConfirm,
            St::Reboot => {
                self.st = St::Finished;
                Action::WaitReboot
            }
            St::Finished => Action::Done,
            St::Failed(f) => Action::Failed(f),
        }
    }

    /// A message from the device. Returns false when it is not the reply to the request in flight (another message, an old reply): the
    /// driver goes on waiting for the rest of the timeout.
    pub fn on_reply(&mut self, msg: &[u8]) -> bool {
        let Some((ty, seq, _, _)) = self.inflight.clone() else {
            return false;
        };
        let Some((rseq, rty, payload)) = parse_reply(msg) else {
            return false;
        };
        if rseq != seq || rty != ty {
            return false;
        }
        self.inflight = None;
        match self.st.clone() {
            St::AwaitStart => self.start_reply(&payload),
            St::AwaitWrite => self.write_reply(&payload),
            St::AwaitCommit => self.commit_reply(&payload),
            St::AwaitAbort(f) => self.st = St::Failed(f),
            _ => {}
        }
        true
    }

    /// No reply came in `timeout_ms`.
    pub fn on_timeout(&mut self) {
        let waited = self.inflight.as_ref().map(|i| i.3 / 1000);
        let secs = waited.unwrap_or(0);
        match self.st.clone() {
            St::AwaitStart => {
                let f = self.fail(
                    FailCode::Timeout,
                    format!("The device did not answer in {secs} s. Check the cable and that the device is on and not busy."),
                );
                self.abort(f);
            }
            St::AwaitWrite => {
                let f = self.fail(
                    FailCode::Timeout,
                    format!(
                        "The device stopped answering at byte {} of {} (no reply in {secs} s). Nothing was written.",
                        self.taken,
                        self.image.len()
                    ),
                );
                self.abort_timeout = if self.is_last() {
                    LAST_WRITE_TIMEOUT_MS
                } else {
                    END_TIMEOUT_MS
                };
                self.abort(f);
            }
            St::AwaitCommit => {
                // End 1 was sent and may have been taken: no End 0 now.
                self.inflight = None;
                self.st = St::Failed(self.fail(
                    FailCode::Timeout,
                    "The device did not answer the last command. Look at its screen: if it shows ERASING FLASH it is flashing; do not touch it or the cable.".into(),
                ));
            }
            St::AwaitAbort(f) => {
                self.inflight = None;
                self.st = St::Failed(f);
            }
            _ => {}
        }
    }

    /// The answer to `AskConfirm`: yes sends End with commit 1, no sends End with commit 0 (the run then fails with `Cancelled`).
    pub fn confirm(&mut self, yes: bool) {
        if !matches!(self.st, St::Confirm) {
            return;
        }
        if yes {
            self.st = St::Commit;
        } else {
            let f = self.fail(
                FailCode::Cancelled,
                "Cancelled. Nothing was written.".into(),
            );
            self.abort(f);
        }
    }

    /// Stop the run. Before End with commit 1 the device gets End with commit 0 and its flash stays as it was. Returns false when it is
    /// too late (End with commit 1 was sent) or the run is over.
    pub fn cancel(&mut self) -> bool {
        match self.st {
            St::Start | St::Write | St::Confirm => {}
            St::AwaitStart => {}
            St::AwaitWrite => {
                if self.is_last() {
                    self.abort_timeout = LAST_WRITE_TIMEOUT_MS;
                }
            }
            _ => return false,
        }
        let f = self.fail(
            FailCode::Cancelled,
            "Cancelled. Nothing was written.".into(),
        );
        self.abort(f);
        true
    }

    fn start_reply(&mut self, p: &[u8]) {
        let status = p.first().copied().unwrap_or(1);
        if status == 0 {
            self.st = St::Write;
            return;
        }
        let why = text_of(p.get(1..).unwrap_or(&[]));
        let f = self.fail(
            FailCode::Refused,
            if why.is_empty() {
                "The device refused the upgrade.".into()
            } else {
                format!("The device refused the upgrade: {why}.")
            },
        );
        self.abort(f);
    }

    fn write_reply(&mut self, p: &[u8]) {
        if p.len() < 5 {
            let f = self.fail(
                FailCode::Protocol,
                "The device sent a reply this program does not understand.".into(),
            );
            return self.abort(f);
        }
        let taken = u32::from_be_bytes([p[0], p[1], p[2], p[3]]) as usize;
        let status = p[4];
        let why = text_of(&p[5..]);
        let len = (self.offset + self.chunk).min(self.image.len()) - self.offset;
        let last = self.is_last();
        match status {
            0 | 1 => {
                if taken != self.offset + len {
                    let f = self.fail(
                        FailCode::Protocol,
                        format!(
                            "The device says it has {taken} bytes, expected {}. Nothing was written.",
                            self.offset + len
                        ),
                    );
                    return self.abort(f);
                }
                self.taken = taken;
                self.retries = 0;
                if last {
                    if status == 1 {
                        self.st = St::Confirm;
                    } else {
                        let f = self.fail(
                            FailCode::Incomplete,
                            "The device took the last bytes but did not report the image complete. Nothing was written.".into(),
                        );
                        self.abort(f);
                    }
                } else if status == 1 {
                    let f = self.fail(
                        FailCode::Protocol,
                        "The device reported the image complete before the last byte. Nothing was written.".into(),
                    );
                    self.abort(f);
                } else {
                    self.offset += len;
                    self.st = St::Write;
                }
            }
            2 => {
                self.retries += 1;
                if self.retries > CRC_RETRIES {
                    let f = self.fail(
                        FailCode::Crc,
                        format!(
                            "The device found the data damaged at byte {} and the same {CRC_RETRIES} resends did not help. Check the cable and try again. Nothing was written.",
                            self.offset
                        ),
                    );
                    self.abort(f);
                } else {
                    self.st = St::Write;
                }
            }
            3 => {
                let f = self.fail(
                    FailCode::Device,
                    if why.is_empty() {
                        "The device reported an error. Nothing was written.".into()
                    } else {
                        format!("The device reported an error: {why}. Nothing was written.")
                    },
                );
                self.abort(f);
            }
            s => {
                let f = self.fail(
                    FailCode::Protocol,
                    format!(
                        "The device answered with the unknown status {s}. Nothing was written."
                    ),
                );
                self.abort(f);
            }
        }
    }

    fn commit_reply(&mut self, p: &[u8]) {
        if p.first() == Some(&0) {
            self.st = St::Reboot;
        } else {
            let why = text_of(p.get(1..).unwrap_or(&[]));
            self.st = St::Failed(self.fail(
                FailCode::Device,
                if why.is_empty() {
                    "The device refused to flash. Look at its screen.".into()
                } else {
                    format!("The device refused to flash: {why}. Look at its screen.")
                },
            ));
        }
    }
}

/// The zero-terminated string at the start of `b`, with the closing `.` or `!` taken off (the caller ends the sentence).
fn text_of(b: &[u8]) -> String {
    let end = b.iter().position(|&x| x == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end])
        .trim()
        .trim_end_matches(['.', '!'])
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A device as our machine sees it: it answers by the rules of finding 75, and can be told to misbehave.
    #[derive(Default)]
    struct Fake {
        stream: Vec<u8>,
        len: usize,
        started: bool,
        ended: Option<u8>,
        /// Corrupt (answer 2 to) this many Writes in a row at the given offset.
        crc_fail: Option<(usize, u32)>,
        /// Answer 3 to the Write at this offset.
        error_at: Option<usize>,
        /// Do not answer the Write at this offset.
        silent_at: Option<usize>,
        /// Answer 0 instead of 1 to the last Write.
        no_complete: bool,
        writes: usize,
    }

    /// A request as the device reads it: `(seq, type, payload)`.
    fn parse_request(msg: &[u8]) -> (u16, u8, Vec<u8>) {
        let body = dkupload::codec::decode_8in7(&msg[6..msg.len() - 1]).unwrap();
        (
            u16::from_be_bytes([body[0], body[1]]),
            body[4],
            body[5..].to_vec(),
        )
    }

    /// The device's reply: its own counter first (not the seq), then the echoed seq, as the emulated firmware does.
    fn reply(req: &[u8], payload: &[u8]) -> Vec<u8> {
        let (seq, ty, _) = parse_request(req);
        let mut body = vec![0, 0x2a, (seq >> 8) as u8, seq as u8, ty | 0x80];
        body.extend_from_slice(payload);
        let mut m = ENVELOPE.to_vec();
        m.extend(syx::encode_8in7(&body));
        m.push(0xf7);
        m
    }

    impl Fake {
        /// The device's answer to one request, or None for no answer.
        fn handle(&mut self, req: &[u8]) -> Option<Vec<u8>> {
            let (_, ty, p) = parse_request(req);
            match ty {
                RPC_OS_START => {
                    self.len = u32::from_be_bytes(p[..4].try_into().unwrap()) as usize;
                    assert_eq!(&p[4..10], b"sysex\0");
                    assert_eq!(p[10], 0, "flag 0: the device waits for End");
                    self.started = true;
                    Some(reply(req, &[0, b'O', b'K', 0]))
                }
                RPC_OS_WRITE => {
                    assert!(self.started);
                    self.writes += 1;
                    let crc = u32::from_be_bytes(p[..4].try_into().unwrap());
                    let n = u32::from_be_bytes(p[4..8].try_into().unwrap()) as usize;
                    let off = u32::from_be_bytes(p[8..12].try_into().unwrap()) as usize;
                    let data = &p[12..];
                    assert_eq!(data.len(), n);
                    assert_eq!(crc, crc32(data));
                    if self.silent_at == Some(off) {
                        return None;
                    }
                    if self.error_at == Some(off) {
                        return Some(reply(
                            req,
                            &[&(off as u32).to_be_bytes()[..], &[3], b"Checksum error!\0"].concat(),
                        ));
                    }
                    if let Some((at, left)) = self.crc_fail.as_mut()
                        && *at == off
                        && *left > 0
                    {
                        *left -= 1;
                        return Some(reply(
                            req,
                            &[&(off as u32).to_be_bytes()[..], &[2], b"CRC\0"].concat(),
                        ));
                    }
                    assert_eq!(off, self.stream.len(), "in order");
                    self.stream.extend_from_slice(data);
                    let done = self.stream.len() == self.len && !self.no_complete;
                    Some(reply(
                        req,
                        &[
                            &(self.stream.len() as u32).to_be_bytes()[..],
                            &[done as u8],
                            b"\0",
                        ]
                        .concat(),
                    ))
                }
                RPC_OS_END => {
                    self.ended = Some(p[0]);
                    Some(reply(req, &[if p[0] == 1 { 0 } else { 2 }, 0]))
                }
                t => panic!("request {t:#x}"),
            }
        }
    }

    /// Drive the machine against the fake until it fails or finishes. `answer` is the user's answer at the question.
    fn drive(m: &mut RpcFlash, d: &mut Fake, answer: bool) -> (Vec<Action>, Option<Failure>) {
        let mut seen = vec![];
        for _ in 0..100_000 {
            let a = m.next();
            seen.push(a.clone());
            match a {
                Action::Send { bytes, .. } => match d.handle(&bytes) {
                    Some(r) => assert!(m.on_reply(&r)),
                    None => m.on_timeout(),
                },
                Action::AskConfirm => m.confirm(answer),
                Action::WaitReboot => {}
                Action::Done => return (seen, None),
                Action::Failed(f) => return (seen, Some(f)),
            }
        }
        panic!("no end");
    }

    fn image(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 31 + i / 7) as u8).collect()
    }

    #[test]
    fn a_reply_is_matched_by_the_echoed_seq_not_by_the_devices_counter() {
        // seen in the emulator: Start with seq 1 answered `0002 0001 d0 00 00` (counter 2, echoed seq 1)
        let req = request(RPC_OS_START, 1, &[]);
        let msg = [
            &ENVELOPE[..],
            &[0x04, 0x00, 0x02, 0x00, 0x01, 0x50, 0x00, 0x00, 0xf7],
        ]
        .concat();
        assert_eq!(parse_reply(&msg), Some((1, RPC_OS_START, vec![0, 0])));
        assert_eq!(parse_request(&req).0, 1);
    }

    #[test]
    fn crc_is_zlib_with_init_ffffffff() {
        // python: zlib.crc32(b"abc", 0xffffffff)
        assert_eq!(crc32(b"abc"), 0x359a_672f);
        assert_eq!(crc32(b""), 0xffff_ffff);
        assert_eq!(crc32(&(0..200u8).collect::<Vec<_>>()), 0xdb86_3609);
    }

    #[test]
    fn a_run_sends_the_image_in_order_asks_and_commits() {
        let img = image(5000);
        let mut m = RpcFlash::new(img.clone(), 2048);
        let mut d = Fake::default();
        let (seen, fail) = drive(&mut m, &mut d, true);
        assert!(fail.is_none());
        assert_eq!(d.stream, img);
        assert_eq!(d.writes, 3);
        assert_eq!(d.ended, Some(1));
        // Start, three Writes, the question, End 1, then the reboot
        assert!(seen.contains(&Action::AskConfirm));
        assert_eq!(seen.iter().filter(|a| **a == Action::WaitReboot).count(), 1);
        assert_eq!(m.taken(), 5000);
        // only the last Write gets the long timeout
        let timeouts: Vec<u32> = seen
            .iter()
            .filter_map(|a| match a {
                Action::Send { timeout_ms, .. } => Some(*timeout_ms),
                _ => None,
            })
            .collect();
        assert_eq!(
            timeouts,
            vec![
                START_TIMEOUT_MS,
                WRITE_TIMEOUT_MS,
                WRITE_TIMEOUT_MS,
                LAST_WRITE_TIMEOUT_MS,
                END_TIMEOUT_MS
            ]
        );
    }

    #[test]
    fn nothing_is_committed_before_the_user_says_yes() {
        let mut m = RpcFlash::new(image(3000), 2048);
        let mut d = Fake::default();
        loop {
            match m.next() {
                Action::Send { bytes, .. } => assert!(m.on_reply(&d.handle(&bytes).unwrap())),
                Action::AskConfirm => break,
                a => panic!("{a:?}"),
            }
        }
        assert_eq!(d.ended, None);
        // asking again does not move on
        assert_eq!(m.next(), Action::AskConfirm);
    }

    #[test]
    fn a_damaged_chunk_is_sent_again_and_the_run_goes_on() {
        let img = image(5000);
        let mut m = RpcFlash::new(img.clone(), 2048);
        let mut d = Fake {
            crc_fail: Some((2048, CRC_RETRIES)),
            ..Fake::default()
        };
        let (_, fail) = drive(&mut m, &mut d, true);
        assert!(fail.is_none());
        assert_eq!(d.stream, img);
        assert_eq!(d.writes, 3 + CRC_RETRIES as usize);
        assert_eq!(d.ended, Some(1));
    }

    #[test]
    fn when_the_retries_run_out_the_run_fails_and_the_flash_is_not_committed() {
        let mut m = RpcFlash::new(image(5000), 2048);
        let mut d = Fake {
            crc_fail: Some((2048, 99)),
            ..Fake::default()
        };
        let (_, fail) = drive(&mut m, &mut d, true);
        let f = fail.unwrap();
        assert_eq!(f.code, FailCode::Crc);
        assert_eq!(d.writes, 2 + CRC_RETRIES as usize);
        assert_eq!(d.ended, Some(0));
        assert!(f.message.contains("Nothing was written"), "{}", f.message);
    }

    #[test]
    fn a_missing_reply_fails_with_end_0_and_the_write_is_not_repeated() {
        let mut m = RpcFlash::new(image(5000), 2048);
        let mut d = Fake {
            silent_at: Some(2048),
            ..Fake::default()
        };
        let (_, fail) = drive(&mut m, &mut d, true);
        let f = fail.unwrap();
        assert_eq!(f.code, FailCode::Timeout);
        assert_eq!(d.writes, 2, "no blind resend");
        assert_eq!(d.ended, Some(0));
        assert_eq!(f.taken, 2048);
    }

    #[test]
    fn a_device_error_is_shown_with_its_message() {
        let mut m = RpcFlash::new(image(5000), 2048);
        let mut d = Fake {
            error_at: Some(4096),
            ..Fake::default()
        };
        let (_, fail) = drive(&mut m, &mut d, true);
        let f = fail.unwrap();
        assert_eq!(f.code, FailCode::Device);
        assert!(f.message.contains("Checksum error"), "{}", f.message);
        assert_eq!(d.ended, Some(0));
    }

    #[test]
    fn a_last_write_that_is_not_complete_never_asks() {
        let mut m = RpcFlash::new(image(5000), 2048);
        let mut d = Fake {
            no_complete: true,
            ..Fake::default()
        };
        let (seen, fail) = drive(&mut m, &mut d, true);
        assert_eq!(fail.unwrap().code, FailCode::Incomplete);
        assert!(!seen.contains(&Action::AskConfirm));
        assert_eq!(d.ended, Some(0));
    }

    #[test]
    fn answering_no_sends_end_0() {
        let mut m = RpcFlash::new(image(5000), 2048);
        let mut d = Fake::default();
        let (_, fail) = drive(&mut m, &mut d, false);
        assert_eq!(fail.unwrap().code, FailCode::Cancelled);
        assert_eq!(d.ended, Some(0));
        assert_eq!(d.stream.len(), 5000, "the device had the whole image");
    }

    #[test]
    fn cancel_in_the_middle_sends_end_0_and_the_late_reply_is_ignored() {
        let mut m = RpcFlash::new(image(10_000), 2048);
        let mut d = Fake::default();
        // Start and two Writes
        for _ in 0..3 {
            let Action::Send { bytes, .. } = m.next() else {
                panic!()
            };
            assert!(m.on_reply(&d.handle(&bytes).unwrap()));
        }
        // the third Write is in flight when the user cancels
        let Action::Send { bytes: late, .. } = m.next() else {
            panic!()
        };
        let late_reply = d.handle(&late).unwrap();
        assert!(m.cancel());
        let Action::Send { bytes, .. } = m.next() else {
            panic!()
        };
        // the reply to the cancelled Write arrives now: it is not the reply to End
        assert!(!m.on_reply(&late_reply));
        assert!(m.on_reply(&d.handle(&bytes).unwrap()));
        let Action::Failed(f) = m.next() else {
            panic!()
        };
        assert_eq!(f.code, FailCode::Cancelled);
        assert_eq!(d.ended, Some(0));
        assert!(!m.cancel(), "over");
    }

    #[test]
    fn cancel_before_the_start_sends_nothing_and_after_the_commit_is_refused() {
        let mut m = RpcFlash::new(image(100), 2048);
        assert!(m.cancel());
        assert!(matches!(m.next(), Action::Failed(f) if f.code == FailCode::Cancelled));

        let mut m = RpcFlash::new(image(100), 2048);
        let mut d = Fake::default();
        loop {
            match m.next() {
                Action::Send { bytes, .. } => assert!(m.on_reply(&d.handle(&bytes).unwrap())),
                Action::AskConfirm => m.confirm(true),
                Action::WaitReboot => break,
                a => panic!("{a:?}"),
            }
        }
        assert!(!m.cancel(), "End 1 was sent");
        assert_eq!(m.next(), Action::Done);
    }

    #[test]
    fn a_refused_start_fails_and_clears_the_device_with_end_0() {
        let mut m = RpcFlash::new(image(100), 2048);
        let Action::Send { bytes, .. } = m.next() else {
            panic!()
        };
        assert!(m.on_reply(&reply(&bytes, b"\x01OS Upgrade is already in progress\0")));
        let Action::Send { bytes, .. } = m.next() else {
            panic!()
        };
        assert_eq!(parse_request(&bytes).1, RPC_OS_END);
        assert_eq!(parse_request(&bytes).2, vec![0]);
        assert!(m.on_reply(&reply(&bytes, &[2, 0])));
        let Action::Failed(f) = m.next() else {
            panic!()
        };
        assert_eq!(f.code, FailCode::Refused);
        assert!(f.message.contains("already in progress"), "{}", f.message);
    }

    #[test]
    fn a_wrong_byte_count_is_a_failure() {
        let mut m = RpcFlash::new(image(5000), 2048);
        let mut d = Fake::default();
        let Action::Send { bytes, .. } = m.next() else {
            panic!()
        };
        assert!(m.on_reply(&d.handle(&bytes).unwrap()));
        let Action::Send { bytes, .. } = m.next() else {
            panic!()
        };
        // the device claims 100 bytes taken after a 2048 byte Write
        assert!(m.on_reply(&reply(
            &bytes,
            &[&100u32.to_be_bytes()[..], &[0, 0]].concat()
        )));
        assert!(matches!(m.next(), Action::Send { .. }), "End 0");
    }

    #[test]
    fn a_reply_with_another_seq_or_type_is_not_ours() {
        let mut m = RpcFlash::new(image(100), 2048);
        let Action::Send { bytes, .. } = m.next() else {
            panic!()
        };
        let mut other = bytes.clone();
        // a SoftwareVersion reply with the same seq, and a garbage message
        let r = reply(&request(0x02, parse_request(&bytes).0, &[]), &[0]);
        assert!(!m.on_reply(&r));
        other.truncate(5);
        assert!(!m.on_reply(&other));
        // asking again repeats the request in flight
        assert_eq!(
            m.next(),
            Action::Send {
                bytes,
                timeout_ms: START_TIMEOUT_MS
            }
        );
    }
}
