//! Opt-in recording of ColdFire->DSP DSPI2 frames as a `.dt2cap` file
//! (format: emu/sharc_capture.py). Replies stay zeros, like `ZeroPeer`, so
//! recording does not change guest behaviour.

use std::{cell::RefCell, rc::Rc};

use periph::dspi::Peer;

const REC_TX: u8 = 1;
const REC_RX: u8 = 2;

struct Inner {
    out: Vec<u8>,
    icount: u64,
    frames: u64,
}

/// Host-side handle; the board owns the peer returned by `peer()`.
pub struct Dspi2Capture(Rc<RefCell<Inner>>);

struct CapturePeer(Rc<RefCell<Inner>>);

fn header(device: &str, source_sha256: &str) -> Vec<u8> {
    let body = format!(
        "{{\"frame_bytes\":2748,\"kind\":\"idle\",\"device\":\"{device}\",\"source_sha256\":\"{source_sha256}\",\"recorder\":\"elektron-native-boot\"}}"
    );
    let mut out = b"DT2CAP1\n".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(body.as_bytes());
    out
}

fn record(out: &mut Vec<u8>, kind: u8, icount: u64, payload: &[u8]) {
    out.push(kind);
    out.extend_from_slice(&icount.to_be_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
}

impl Dspi2Capture {
    pub fn new(device: &str, source_sha256: &str) -> Self {
        Self(Rc::new(RefCell::new(Inner {
            out: header(device, source_sha256),
            icount: 0,
            frames: 0,
        })))
    }

    /// The peer to install as `board.dma.peer`.
    pub fn peer(&self) -> Box<dyn Peer> {
        Box::new(CapturePeer(self.0.clone()))
    }

    /// CPU instruction count stamped on the next frames.
    pub fn set_icount(&self, icount: u64) {
        self.0.borrow_mut().icount = icount;
    }

    pub fn frames(&self) -> u64 {
        self.0.borrow().frames
    }

    /// The complete `.dt2cap` file so far.
    pub fn bytes(&self) -> Vec<u8> {
        self.0.borrow().out.clone()
    }
}

impl Peer for CapturePeer {
    fn exchange(&mut self, tx: &[u8]) -> Vec<u8> {
        let mut inner = self.0.borrow_mut();
        let icount = inner.icount;
        let rx = vec![0; tx.len()];
        record(&mut inner.out, REC_TX, icount, tx);
        record(&mut inner.out, REC_RX, icount, &rx);
        inner.frames += 1;
        rx
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_file_layout() {
        let cap = Dspi2Capture::new("dn2", "ab");
        let mut peer = cap.peer();
        cap.set_icount(7);
        assert_eq!(peer.exchange(&[1, 2, 3]), vec![0; 3]);
        cap.set_icount(9);
        assert_eq!(peer.exchange(&[4, 5]), vec![0; 2]);
        assert_eq!(cap.frames(), 2);
        let path = std::env::temp_dir().join(format!("dt2cap-test-{}.dt2cap", std::process::id()));
        std::fs::write(&path, cap.bytes()).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(&bytes[..8], b"DT2CAP1\n");
        let hlen = u32::from_be_bytes(bytes[8..12].try_into().unwrap()) as usize;
        let header = std::str::from_utf8(&bytes[12..12 + hlen]).unwrap();
        assert!(header.contains("\"device\":\"dn2\"") && header.contains("\"frame_bytes\":2748"));
        let rec = &bytes[12 + hlen..];
        assert_eq!(&rec[..13], &[1, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 3]);
        assert_eq!(&rec[13..16], &[1, 2, 3]);
        assert_eq!(&rec[16..29], &[2, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 3]);
        assert_eq!(rec.len(), 62);
    }
}
