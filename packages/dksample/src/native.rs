//! The native sample file: a 0x40-byte header, big-endian 16-bit PCM and the 16-byte trailer the firmware adds when it closes a file
//! (docs/findings/14-plus-drive-format.md).

#[cfg(test)]
extern crate std;
#[cfg(test)]
use self::std::{vec, vec::Vec};

pub const NATIVE_HEADER_SIZE: usize = 0x40;
pub const NATIVE_TRAILER_SIZE: usize = 0x10;
/// The sample rate of the native file.
pub const NATIVE_RATE: u32 = 48_000;

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum NativeError {
    /// The PCM is not a whole number of frames, or longer than a `u32`.
    InvalidPcmLength,
}

/// The native file of `pcm_be16` (big-endian int16, interleaved when `stereo`): header, PCM, trailer of zeros.
pub fn build_native_sample(pcm_be16: &[u8], stereo: bool) -> Result<Vec<u8>, NativeError> {
    if pcm_be16.len() % if stereo { 4 } else { 2 } != 0 {
        return Err(NativeError::InvalidPcmLength);
    }
    let len = u32::try_from(pcm_be16.len()).map_err(|_| NativeError::InvalidPcmLength)?;
    let mut out = vec![0; NATIVE_HEADER_SIZE];
    out[1] = u8::from(stereo);
    out[4..8].copy_from_slice(&len.to_be_bytes());
    out[8..12].copy_from_slice(&NATIVE_RATE.to_be_bytes());
    out[0x14] = 0x7f;
    out.extend_from_slice(pcm_be16);
    out.resize(out.len() + NATIVE_TRAILER_SIZE, 0);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_header_and_the_trailer() {
        let n = build_native_sample(&[0, 1, 0, 2], true).unwrap();
        assert_eq!((n.len(), n[1], n[0x14]), (0x54, 1, 0x7f));
        assert_eq!(&n[4..8], &4u32.to_be_bytes());
        assert_eq!(&n[8..12], &48_000u32.to_be_bytes());
        assert_eq!(&n[0x40..0x44], &[0, 1, 0, 2]);
        assert_eq!(&n[0x44..], &[0; 16]);
        assert!(build_native_sample(&[0], false).is_err());
        assert!(build_native_sample(&[0, 1], true).is_err());
    }
}
