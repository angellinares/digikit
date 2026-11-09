//! Framing and 8-in-7 packing (docs/design/digikit-upload.md, "Framing").

pub const HEAD: [u8; 5] = [0xF0, 0x7D, 0x64, 0x6B, 0x01];
pub const REPLY_BIT: u8 = 0x40;

pub use crate::gen_proto::{
    CMD_ABORT, CMD_BEGIN, CMD_CLEAR, CMD_DATA, CMD_DELETE, CMD_END, CMD_INFO, CMD_LIST, CMD_MKDIR,
    CMD_READ, CMD_VERIFY,
};
/// `MAX_CHUNK`: a PUT chunk. `MAX_REQUEST_WIRE` and `MAX_REPLY_WIRE`: the most a request (40 KiB) and a reply (1 KiB) take on the wire.
pub use crate::gen_proto::{MAX_CHUNK, MAX_REPLY_WIRE, MAX_REQUEST_WIRE};
pub const CLEAR_MAGIC: u32 = 0x434c_5221;

/// Pack: each group of up to 7 bytes becomes a collector byte (bit `6 - n` is the top bit of byte `n`) and the 7 bytes
/// with bit 7 cleared.
pub fn encode_8in7(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 7 + 1);
    for group in data.chunks(7) {
        let mut marker = 0u8;
        for (n, &b) in group.iter().enumerate() {
            if b & 0x80 != 0 {
                marker |= 1 << (6 - n);
            }
        }
        out.push(marker);
        out.extend(group.iter().map(|b| b & 0x7f));
    }
    out
}

/// Unpack. Fails on a byte with bit 7 set, or a lone collector byte (a group has 1 to 7 data bytes).
pub fn decode_8in7(packed: &[u8]) -> Result<Vec<u8>, String> {
    if let Some(i) = packed.iter().position(|b| b & 0x80 != 0) {
        return Err(format!("byte {i} has bit 7 set"));
    }
    let mut out = Vec::with_capacity(packed.len());
    for group in packed.chunks(8) {
        if group.len() < 2 {
            return Err("collector byte without data".into());
        }
        let marker = group[0];
        for (n, &b) in group[1..].iter().enumerate() {
            out.push(b | (((marker >> (6 - n)) & 1) << 7));
        }
    }
    Ok(out)
}

/// A decoded message: `cmd` is without the reply bit, `reply` says which direction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub cmd: u8,
    pub reply: bool,
    pub seq: u8,
    pub body: Vec<u8>,
}

pub fn encode(cmd: u8, reply: bool, seq: u8, body: &[u8]) -> Vec<u8> {
    let mut out = HEAD.to_vec();
    out.push(if reply { cmd | REPLY_BIT } else { cmd });
    out.push(seq & 0x7f);
    out.extend(encode_8in7(body));
    out.push(0xF7);
    out
}

pub fn encode_request(cmd: u8, seq: u8, body: &[u8]) -> Vec<u8> {
    encode(cmd, false, seq, body)
}

/// Decode a whole SysEx message (`F0 .. F7`). The protocol version byte is not checked here: the device answers
/// `E_PROTO` to a request with another one, and a host sees a wrong reply version as a decode error.
pub fn decode(msg: &[u8]) -> Result<Message, String> {
    if msg.len() < 8 || msg[..4] != HEAD[..4] || msg[msg.len() - 1] != 0xF7 {
        return Err("not a digikit message".into());
    }
    if msg[4] != HEAD[4] {
        return Err(format!("protocol version {}", msg[4]));
    }
    let c = msg[5];
    Ok(Message {
        cmd: c & !REPLY_BIT,
        reply: c & REPLY_BIT != 0,
        seq: msg[6],
        body: decode_8in7(&msg[7..msg.len() - 1])?,
    })
}

pub use dksample::file_hash;

/// The store's path rules: 1 to 159 bytes, `/` separated, no empty, `.` or `..` part, no NUL.
pub fn path_ok(path: &str) -> bool {
    let b = path.as_bytes();
    !b.is_empty()
        && b.len() <= 159
        && !b.contains(&0)
        && path
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != "..")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_examples() {
        assert_eq!(
            encode_8in7(&[0x80, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06]),
            [0x40, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06]
        );
        assert_eq!(encode_8in7(&[0xFF]), [0x40, 0x7F]);
        assert_eq!(decode_8in7(&[0x40, 0x7F]).unwrap(), [0xFF]);
    }

    #[test]
    fn round_trip_all_lengths() {
        let data: Vec<u8> = (0..300u32).map(|i| (i * 37 + 11) as u8).collect();
        for n in 0..data.len() {
            let p = encode_8in7(&data[..n]);
            assert!(p.iter().all(|b| b & 0x80 == 0));
            assert_eq!(decode_8in7(&p).unwrap(), &data[..n]);
        }
    }

    #[test]
    fn decode_rejects_bad_input() {
        assert!(decode_8in7(&[0x40]).is_err());
        assert!(decode_8in7(&[0x00, 0x80]).is_err());
    }

    #[test]
    fn message_round_trip_and_wire_size() {
        let body: Vec<u8> = (0..32772u32).map(|i| i as u8).collect();
        let m = encode_request(CMD_DATA, 5, &body);
        assert!(m.len() <= MAX_REQUEST_WIRE);
        assert_eq!(&m[..7], &[0xF0, 0x7D, 0x64, 0x6B, 0x01, 0x11, 5]);
        let d = decode(&m).unwrap();
        assert_eq!((d.cmd, d.reply, d.seq), (CMD_DATA, false, 5));
        assert_eq!(d.body, body);
        let r = encode(CMD_INFO, true, 9, &[0]);
        assert_eq!(r[5], 0x41);
        assert!(decode(&r).unwrap().reply);
    }

    #[test]
    fn paths() {
        assert!(path_ok("modules/a.dkm"));
        assert!(!path_ok("") && !path_ok("a//b") && !path_ok("../a") && !path_ok("a/./b"));
        assert!(path_ok(&"x".repeat(159)) && !path_ok(&"x".repeat(160)));
    }
}
