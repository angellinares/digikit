//! The transport under the protocol: whole frames in and out, over USB packets. `UsbLink` (nusb) is the real one, `FakeDevice` (fake.rs) stands in for tests.
use std::time::Duration;

use crate::gen_bulk::PACKET;
use crate::proto::{self, FrameError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkError {
    /// No complete frame, or the write did not finish, within the time.
    Timeout,
    /// The device is gone (unplugged, re-enumerated).
    Disconnected,
    /// The endpoint stalled or the OS refused: the text says which.
    Io(String),
    /// The stream is not a bulk stream (bad magic, a length over the bound): fatal, no resync.
    Frame(FrameError),
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LinkError::Timeout => write!(f, "the bulk link timed out"),
            LinkError::Disconnected => write!(f, "the device is gone"),
            LinkError::Io(s) => write!(f, "bulk link: {s}"),
            LinkError::Frame(e) => write!(f, "bulk link: bad frame ({e:?})"),
        }
    }
}
impl std::error::Error for LinkError {}

/// One request or reply frame at a time. `send` ends the USB transfer correctly: a frame whose length is a multiple of the packet size is followed by
/// a zero-length packet (`send_with_zlp`). `recv` reads until a frame is complete (the device sends 64-byte IN packets, each a short packet for the host, and no zero-length packets). A link may queue several sent
/// frames before the first reply is read (two DATA requests in flight).
pub trait Link {
    fn send(&mut self, frame: &[u8], timeout: Duration) -> Result<(), LinkError>;
    /// The next whole frame the device sent.
    fn recv(&mut self, timeout: Duration) -> Result<Vec<u8>, LinkError>;
    /// Let go of the device (after a fatal error). The default does nothing.
    fn close(&mut self) {}
}

/// One OUT transfer of the pipe (a USB bulk transfer: nusb, WebUSB, the fake).
pub trait OutPipe {
    fn out(&mut self, data: &[u8], timeout: Duration) -> Result<(), LinkError>;
}

/// The rule of the plan (section 5): the device completes a multi-page RX buffer only when it is full or on a short packet, so a frame whose
/// length is a multiple of the packet size gets a zero-length OUT transfer after it.
pub fn send_with_zlp<P: OutPipe + ?Sized>(
    pipe: &mut P,
    frame: &[u8],
    timeout: Duration,
) -> Result<(), LinkError> {
    pipe.out(frame, timeout)?;
    if !frame.is_empty() && frame.len().is_multiple_of(PACKET) {
        pipe.out(&[], timeout)?;
    }
    Ok(())
}

/// One IN transfer of the pipe (possibly zero-length).
pub trait InPipe {
    fn read(&mut self, timeout: Duration) -> Result<Vec<u8>, LinkError>;
}

/// Frames out of IN transfers: transfers may split or join frames, zero-length ones add nothing, and a corrupt stream is an error (no resync).
#[derive(Default)]
pub struct FrameReader {
    buf: Vec<u8>,
}

impl FrameReader {
    fn take(&mut self) -> Result<Option<Vec<u8>>, LinkError> {
        if self.buf.len() < crate::gen_bulk::HEADER {
            return Ok(None);
        }
        let total = proto::frame_len(&self.buf).map_err(LinkError::Frame)?;
        if self.buf.len() < total {
            return Ok(None);
        }
        Ok(Some(self.buf.drain(..total).collect()))
    }

    pub fn next<P: InPipe + ?Sized>(
        &mut self,
        pipe: &mut P,
        timeout: Duration,
    ) -> Result<Vec<u8>, LinkError> {
        loop {
            if let Some(f) = self.take()? {
                return Ok(f);
            }
            // a zero-length transfer is "no bytes": read on
            self.buf.extend_from_slice(&pipe.read(timeout)?);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    struct Chunks(VecDeque<Vec<u8>>);
    impl InPipe for Chunks {
        fn read(&mut self, _: Duration) -> Result<Vec<u8>, LinkError> {
            self.0.pop_front().ok_or(LinkError::Timeout)
        }
    }

    #[test]
    fn a_512_byte_reply_followed_by_a_zero_length_transfer_is_one_frame_and_no_error() {
        let a = proto::reply(1, 1, 0, &vec![7; 500]);
        assert_eq!(a.len(), 512);
        let b = proto::reply(2, 2, 0, &[]);
        let mut p = Chunks([a.clone(), vec![], b.clone()].into());
        let mut r = FrameReader::default();
        assert_eq!(r.next(&mut p, Duration::ZERO), Ok(a));
        assert_eq!(r.next(&mut p, Duration::ZERO), Ok(b));
    }

    #[test]
    fn transfers_may_cut_frames_anywhere_and_a_bad_stream_is_fatal() {
        let a = proto::reply(1, 1, 0, &[1, 2, 3]);
        let mut p = Chunks(a.chunks(5).map(<[u8]>::to_vec).collect());
        assert_eq!(FrameReader::default().next(&mut p, Duration::ZERO), Ok(a));
        let mut bad = Chunks([vec![0; 12]].into());
        assert_eq!(
            FrameReader::default().next(&mut bad, Duration::ZERO),
            Err(LinkError::Frame(FrameError::BadMagic))
        );
        let mut big = Chunks([b"DKB1\x44\0\0\0\xff\xff\xff\xff".to_vec()].into());
        assert_eq!(
            FrameReader::default().next(&mut big, Duration::ZERO),
            Err(LinkError::Frame(FrameError::Oversize))
        );
    }
}
