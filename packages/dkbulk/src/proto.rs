//! The framing of the bulk link (docs/plans/usb-fast.md section 5; numbers in `gen_bulk.rs`, from `sdk/sdk.toml [protocol.bulk]`).
//!
//! request: `magic | cmd u8 | seq u8 | flags u16 | len u32 | body[len]`, reply: `magic | cmd|0x40 | seq | status i16 | len u32 | body[len]`. Big-endian.
use crate::gen_bulk::*;

/// A reply: the command it answers (without the reply flag), the sequence number, the status (0 = ok) and the body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply {
    pub cmd: u8,
    pub seq: u8,
    pub status: i16,
    pub body: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// Fewer bytes than the header, or than the header's `len` says.
    Short,
    BadMagic,
    /// A request where a reply is expected (the reply flag is clear).
    NotAReply,
    /// A length field above `MAX_FRAME`: a corrupt stream, fatal.
    Oversize,
}

/// One request frame.
pub fn request(cmd: u8, seq: u8, flags: u16, body: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(HEADER + body.len());
    v.extend_from_slice(&MAGIC);
    v.push(cmd);
    v.push(seq);
    v.extend_from_slice(&flags.to_be_bytes());
    v.extend_from_slice(&(body.len() as u32).to_be_bytes());
    v.extend_from_slice(body);
    v
}

/// One reply frame (the device side; the fake device and the tests use it).
pub fn reply(cmd: u8, seq: u8, status: i16, body: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(HEADER + body.len());
    v.extend_from_slice(&MAGIC);
    v.push(cmd | REPLY_FLAG);
    v.push(seq);
    v.extend_from_slice(&status.to_be_bytes());
    v.extend_from_slice(&(body.len() as u32).to_be_bytes());
    v.extend_from_slice(body);
    v
}

/// The largest frame: a DATA request with its header and the slack for BEGIN's path and the like.
pub const MAX_FRAME: usize = DATA_MAX + 4 + HEADER + FRAME_SLACK;

/// The total length of the frame that starts `head` (at least `HEADER` bytes), or an error when it is not a frame.
pub fn frame_len(head: &[u8]) -> Result<usize, FrameError> {
    if head.len() < HEADER {
        return Err(FrameError::Short);
    }
    if head[..4] != MAGIC {
        return Err(FrameError::BadMagic);
    }
    let len = u32::from_be_bytes(head[8..12].try_into().unwrap()) as usize;
    if HEADER + len > MAX_FRAME {
        return Err(FrameError::Oversize);
    }
    Ok(HEADER + len)
}

pub fn parse_reply(frame: &[u8]) -> Result<Reply, FrameError> {
    let total = frame_len(frame)?;
    if frame.len() < total {
        return Err(FrameError::Short);
    }
    if frame[4] & REPLY_FLAG == 0 {
        return Err(FrameError::NotAReply);
    }
    Ok(Reply {
        cmd: frame[4] & !REPLY_FLAG,
        seq: frame[5],
        status: i16::from_be_bytes([frame[6], frame[7]]),
        body: frame[HEADER..total].to_vec(),
    })
}

/// A request as the device sees it: (cmd, seq, flags, body).
pub fn parse_request(frame: &[u8]) -> Result<(u8, u8, u16, Vec<u8>), FrameError> {
    let total = frame_len(frame)?;
    if frame.len() < total {
        return Err(FrameError::Short);
    }
    Ok((
        frame[4],
        frame[5],
        u16::from_be_bytes([frame[6], frame[7]]),
        frame[HEADER..total].to_vec(),
    ))
}

/// BEGIN body: root u32, path, NUL, size u32, expected hash (4 bytes; 0: none).
pub fn begin_body(root: u32, path: &str, size: u32, hash: Option<u32>) -> Vec<u8> {
    let mut b = root.to_be_bytes().to_vec();
    b.extend_from_slice(path.as_bytes());
    b.push(0);
    b.extend_from_slice(&size.to_be_bytes());
    b.extend_from_slice(&hash.unwrap_or(0).to_be_bytes());
    b
}

/// DATA body: offset u32, then the bytes.
pub fn data_body(offset: u32, bytes: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(4 + bytes.len());
    b.extend_from_slice(&offset.to_be_bytes());
    b.extend_from_slice(bytes);
    b
}

/// VERIFY body: offset u32, length u32.
pub fn verify_body(offset: u32, len: u32) -> Vec<u8> {
    let mut b = offset.to_be_bytes().to_vec();
    b.extend_from_slice(&len.to_be_bytes());
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_has_the_header_of_the_plan() {
        let r = request(CMD_DATA, 7, 0, &[1, 2, 3]);
        assert_eq!(&r[..12], b"DKB1\x03\x07\x00\x00\x00\x00\x00\x03");
        assert_eq!(&r[12..], &[1, 2, 3]);
        assert_eq!(frame_len(&r), Ok(15));
    }

    #[test]
    fn a_reply_carries_the_flag_and_a_signed_status() {
        let r = reply(CMD_END, 9, -56, &[0xaa]);
        assert_eq!(&r[..12], b"DKB1\x44\x09\xff\xc8\x00\x00\x00\x01");
        let p = parse_reply(&r).unwrap();
        assert_eq!(
            (p.cmd, p.seq, p.status, p.body),
            (CMD_END, 9, -56, vec![0xaa])
        );
    }

    #[test]
    fn bad_frames_are_named() {
        assert_eq!(parse_reply(b"DKB1"), Err(FrameError::Short));
        assert_eq!(
            frame_len(b"XXXX\0\0\0\0\0\0\0\0"),
            Err(FrameError::BadMagic)
        );
        assert_eq!(
            parse_reply(&request(1, 0, 0, &[])),
            Err(FrameError::NotAReply)
        );
        let mut r = reply(1, 0, 0, &[1, 2]);
        r.pop();
        assert_eq!(parse_reply(&r), Err(FrameError::Short));
    }

    #[test]
    fn begin_body_layout() {
        let b = begin_body(2, "a/b", 512, Some(0x01020305));
        assert_eq!(b, [0, 0, 0, 2, b'a', b'/', b'b', 0, 0, 0, 2, 0, 1, 2, 3, 5]);
    }
}
