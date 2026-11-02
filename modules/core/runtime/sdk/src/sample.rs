//! Bounded native mono sample decoder. The canonical file layout is shared with
//! `packages/dksample/src/native.rs`. Output words are IEEE float32 bits; decoding
//! uses integer arithmetic only, including on ColdFire.
use crate::{E_FORMAT, E_TOO_BIG};

pub const MAX_FRAMES: usize = 8192;
pub const HEADER_BYTES: usize = 64;
pub const TRAILER_BYTES: usize = 16;
pub const MAX_FILE_BYTES: usize = HEADER_BYTES + MAX_FRAMES * 2 + TRAILER_BYTES;
pub const RATE: u32 = 48_000;

fn be32(bytes: &[u8], at: usize) -> u32 {
    ((bytes[at] as u32) << 24) | ((bytes[at + 1] as u32) << 16) | ((bytes[at + 2] as u32) << 8) | bytes[at + 3] as u32
}

/// Validate the complete canonical native file, rejecting empty/stereo/non-48 kHz
/// files and nonzero reserved header or trailer bytes. No partial success.
pub fn native_frames(input: &[u8]) -> Result<u32, i32> {
    if input.len() > MAX_FILE_BYTES {
        return Err(E_TOO_BIG);
    }
    if input.len() < HEADER_BYTES + TRAILER_BYTES || input[1] != 0 || be32(input, 8) != RATE {
        return Err(E_FORMAT);
    }
    let pcm = be32(input, 4) as usize;
    if pcm == 0 || pcm & 1 != 0 || pcm != input.len() - HEADER_BYTES - TRAILER_BYTES {
        return Err(E_FORMAT);
    }
    let mut i = 0;
    while i < HEADER_BYTES {
        if !(4..12).contains(&i) && input[i] != if i == 0x14 { 0x7f } else { 0 } {
            return Err(E_FORMAT);
        }
        i += 1;
    }
    i = HEADER_BYTES + pcm;
    while i < input.len() {
        if input[i] != 0 {
            return Err(E_FORMAT);
        }
        i += 1;
    }
    Ok((pcm / 2) as u32)
}

/// Header-only check of a native file of `size` bytes whose first `HEADER_BYTES` bytes are `header`, for the per-track table (mono or
/// stereo, any length that fits the pool): returns `(frames, channels)`. Checks only what the stock loader reads (finding 14): byte 1 is 0
/// for mono and 1 for stereo (interleaved PCM16, finding 81 arithmetic: pcm bytes = frames * 2 * channels), the PCM length (bytes 4..8)
/// and the rate (8..12, 48 kHz). Files written by Elektron Transfer carry other header bytes, so those are not checked. The trailer is
/// not required: the size must only hold header + pcm.
pub fn native_header(header: &[u8], size: u32) -> Result<(u32, u32), i32> {
    if header.len() < HEADER_BYTES || header[1] > 1 || be32(header, 8) != RATE {
        return Err(E_FORMAT);
    }
    let channels = header[1] as u32 + 1;
    let pcm = be32(header, 4);
    if pcm == 0 || pcm % (2 * channels) != 0 || (size as u64) < HEADER_BYTES as u64 + pcm as u64 {
        return Err(E_FORMAT);
    }
    Ok((pcm / (2 * channels), channels))
}

/// Decode signed PCM16 normalized by 32768 into float32 words. The entire file
/// and destination capacity are checked before any output word is written.
pub fn decode_native(input: &[u8], output: &mut [u32]) -> Result<u32, i32> {
    let frames = native_frames(input)?;
    if output.len() < frames as usize {
        return Err(E_TOO_BIG);
    }
    let mut i = 0;
    while i < frames as usize {
        let at = HEADER_BYTES + i * 2;
        let signed = (((input[at] as u16) << 8) | input[at + 1] as u16) as i16 as i32;
        let magnitude = if signed < 0 { -signed } else { signed } as u32;
        let mut bits = 0;
        if magnitude != 0 {
            let top = 31 - magnitude.leading_zeros();
            bits = ((top + 112) << 23) | ((magnitude << (23 - top)) & 0x7fffff);
            if signed < 0 {
                bits |= 0x80000000;
            }
        }
        output[i] = bits;
        i += 1;
    }
    Ok(frames)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec;
    use std::vec::Vec;

    fn native(pcm: &[i16]) -> Vec<u8> {
        let mut bytes = vec![0; HEADER_BYTES];
        bytes[4..8].copy_from_slice(&(pcm.len() as u32 * 2).to_be_bytes());
        bytes[8..12].copy_from_slice(&48_000u32.to_be_bytes());
        bytes[0x14] = 0x7f;
        for value in pcm {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        bytes.resize(bytes.len() + TRAILER_BYTES, 0);
        bytes
    }

    #[test]
    fn sample_decode_every_pcm16_value_matches_independent_float_reference() {
        for start in (i16::MIN as i32..=i16::MAX as i32).step_by(MAX_FRAMES) {
            let pcm: Vec<i16> = (start..start + MAX_FRAMES as i32).map(|n| n as i16).collect();
            let mut out = vec![0; MAX_FRAMES + 1];
            out[MAX_FRAMES] = 0xdeadbeef;
            assert_eq!(decode_native(&native(&pcm), &mut out), Ok(MAX_FRAMES as u32));
            for (value, bits) in pcm.iter().zip(&out) {
                assert_eq!(*bits, (*value as f32 / 32768.0).to_bits(), "{value}");
            }
            assert_eq!(out[MAX_FRAMES], 0xdeadbeef);
        }
    }

    #[test]
    fn header_check_reads_only_the_stock_fields() {
        let file = |channels: u8, pcm: u32| {
            let mut bytes = vec![0u8; HEADER_BYTES + pcm as usize + TRAILER_BYTES];
            bytes[1] = channels;
            bytes[4..8].copy_from_slice(&pcm.to_be_bytes());
            bytes[8..12].copy_from_slice(&48_000u32.to_be_bytes());
            bytes[0x14] = 0x7f;
            bytes
        };
        let m = file(0, 20);
        assert_eq!(native_header(&m, m.len() as u32), Ok((10, 1)));
        let s = file(1, 20);
        assert_eq!(native_header(&s, s.len() as u32), Ok((5, 2)));
        assert_eq!(native_header(&s[..HEADER_BYTES], s.len() as u32), Ok((5, 2)));
        // larger than the 8192-frame bank limit is fine here
        let big = file(1, 4 * 100_000);
        assert_eq!(native_header(&big[..HEADER_BYTES], big.len() as u32), Ok((100_000, 2)));
        assert_eq!(native_header(&s, s.len() as u32 + 1), Ok((5, 2)), "extra bytes after the trailer");
        assert_eq!(native_header(&s, 64 + 20), Ok((5, 2)), "no trailer");
        assert!(native_header(&s, 64 + 20 - 1).is_err(), "pcm cut short");
        assert!(native_header(&file(1, 18), 64 + 18 + 16).is_err(), "a partial stereo frame");
        assert!(native_header(&file(2, 20), 100).is_err(), "channels");
        assert!(native_header(&file(0, 0), 80).is_err(), "empty");
        for at in [0usize, 2, 3, 12, 0x13, 0x14, 63] {
            let mut h = s.clone();
            h[at] ^= 1;
            assert_eq!(native_header(&h, s.len() as u32), Ok((5, 2)), "byte {at}");
        }
        let mut rate = s.clone();
        rate[8..12].copy_from_slice(&44_100u32.to_be_bytes());
        assert!(native_header(&rate, s.len() as u32).is_err());
        assert!(native_header(&s[..40], s.len() as u32).is_err());
    }

    #[test]
    fn sample_rejects_malformed_and_unsupported_files_before_writing() {
        let valid = native(&[-32768, 0, 32767]);
        let mut cases = vec![vec![], native(&[])];
        for at in [0, 1, 2, 3, 7, 8, 11, 12, 20, 63, valid.len() - 1] {
            let mut bytes = valid.clone();
            bytes[at] ^= 1;
            cases.push(bytes);
        }
        cases.push(valid[..valid.len() - 1].to_vec());
        let mut extra = valid.clone();
        extra.push(0);
        cases.push(extra);
        cases.push(native(&vec![1; MAX_FRAMES + 1]));
        for bytes in cases {
            let mut out = [0xdeadbeef; 4];
            assert!(decode_native(&bytes, &mut out).is_err());
            assert_eq!(out, [0xdeadbeef; 4]);
        }
        let mut out = [0xdeadbeef; 2];
        assert_eq!(decode_native(&valid, &mut out), Err(E_TOO_BIG));
        assert_eq!(out, [0xdeadbeef; 2]);
    }
}
