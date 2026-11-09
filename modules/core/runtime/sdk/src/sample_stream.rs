//! Allocation-free native mono or stereo PCM16 streaming packer, independent of
//! the small float decoder's frame limit. Packed words are little endian: mono
//! pairs place the even frame low and odd frame high (zero for odd EOF); stereo
//! preserves interleaved left/right samples, with one complete frame per word.
//!
//! Output is staging data: **all output is invalid after any error**, including
//! a later trailer/EOF error. A caller must not publish READY until `finish`
//! succeeds and it independently establishes source consistency (for example,
//! a validated snapshot or an immutable lease with backend integrity checks).
use crate::sample::{HEADER_BYTES, RATE, TRAILER_BYTES};
use crate::{E_FORMAT, E_TOO_BIG};

/// Bytes actually consumed and emitted by one successful `push`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub consumed: usize,
    pub written: usize,
}

/// A single-use parser with a 64-byte header and one pending packed word.
/// Malformed/truncated/extra input returns sticky `E_FORMAT`; a valid header
/// exceeding `max_frames` returns sticky `E_TOO_BIG`, before any output.
/// The canonical file's byte length must fit in `u32`.
pub struct NativePcm16Stream {
    header: [u8; HEADER_BYTES],
    word: [u8; 4],
    header_len: usize,
    word_len: usize,
    channels: u8,
    max_frames: u32,
    frames: u32,
    pcm_left: u32,
    trailer_len: usize,
    error: i32,
    finished: bool,
}

impl NativePcm16Stream {
    /// Accept only canonical mono files, with capacity measured in frames.
    pub const fn new(max_frames: u32) -> Self {
        Self::with_channels(max_frames, 1)
    }

    /// Accept only canonical interleaved stereo files, with capacity in frames.
    /// Output preserves left/right order as two little-endian PCM16 values per word.
    pub const fn new_stereo(max_frames: u32) -> Self {
        Self::with_channels(max_frames, 2)
    }

    /// Configured channel count, available before the header is received.
    /// This describes the required mode, not validated input metadata.
    pub const fn channels(&self) -> u8 {
        self.channels
    }

    const fn with_channels(max_frames: u32, channels: u8) -> Self {
        Self {
            header: [0; HEADER_BYTES],
            word: [0; 4],
            header_len: 0,
            word_len: 0,
            channels,
            max_frames,
            frames: 0,
            pcm_left: 0,
            trailer_len: 0,
            error: 0,
            finished: false,
        }
    }

    fn fail<T>(&mut self, error: i32) -> Result<T, i32> {
        self.error = error;
        Err(error)
    }

    fn validate_header(&mut self) -> Result<(), i32> {
        let pcm = u32::from_be_bytes(self.header[4..8].try_into().unwrap());
        let rate = u32::from_be_bytes(self.header[8..12].try_into().unwrap());
        let bytes_per_frame = u32::from(self.channels) * 2;
        if pcm == 0
            || pcm % bytes_per_frame != 0
            || rate != RATE
            || pcm
                .checked_add((HEADER_BYTES + TRAILER_BYTES) as u32)
                .is_none()
        {
            return self.fail(E_FORMAT);
        }
        for (i, &byte) in self.header.iter().enumerate() {
            let expected = match i {
                1 => self.channels - 1,
                0x14 => 0x7f,
                _ => 0,
            };
            if !(4..12).contains(&i) && byte != expected {
                return self.fail(E_FORMAT);
            }
        }
        if pcm / bytes_per_frame > self.max_frames {
            return self.fail(E_TOO_BIG);
        }
        self.frames = pcm / bytes_per_frame;
        self.pcm_left = pcm;
        Ok(())
    }

    /// Accept arbitrary byte chunks, stopping for output backpressure. Only
    /// complete four-byte words are written; leftover output bytes are untouched.
    /// Even a zero-length output may consume the header and one pending word.
    /// Retry the unconsumed input with fresh output space, including an empty
    /// input to drain a pending word. An error invalidates all staging output.
    pub fn push(&mut self, input: &[u8], output: &mut [u8]) -> Result<Progress, i32> {
        if self.error != 0 {
            return Err(self.error);
        }
        if self.finished {
            return if input.is_empty() {
                Ok(Progress::default())
            } else {
                self.fail(E_FORMAT)
            };
        }
        let mut progress = Progress::default();
        loop {
            if self.header_len < HEADER_BYTES {
                let take = core::cmp::min(
                    HEADER_BYTES - self.header_len,
                    input.len() - progress.consumed,
                );
                self.header[self.header_len..self.header_len + take]
                    .copy_from_slice(&input[progress.consumed..progress.consumed + take]);
                self.header_len += take;
                progress.consumed += take;
                if self.header_len < HEADER_BYTES {
                    break;
                }
                self.validate_header()?;
            }
            if self.word_len == 4 || (self.pcm_left == 0 && self.word_len != 0) {
                if output.len() - progress.written < 4 {
                    break;
                }
                let packed = [self.word[1], self.word[0], self.word[3], self.word[2]];
                output[progress.written..progress.written + 4].copy_from_slice(&packed);
                progress.written += 4;
                self.word = [0; 4];
                self.word_len = 0;
            }
            if self.pcm_left != 0 {
                let take = core::cmp::min(
                    core::cmp::min(4 - self.word_len, self.pcm_left as usize),
                    input.len() - progress.consumed,
                );
                if take == 0 {
                    break;
                }
                self.word[self.word_len..self.word_len + take]
                    .copy_from_slice(&input[progress.consumed..progress.consumed + take]);
                self.word_len += take;
                self.pcm_left -= take as u32;
                progress.consumed += take;
                continue;
            }
            if progress.consumed == input.len() {
                break;
            }
            if self.trailer_len == TRAILER_BYTES || input[progress.consumed] != 0 {
                return self.fail(E_FORMAT);
            }
            self.trailer_len += 1;
            progress.consumed += 1;
        }
        Ok(progress)
    }

    /// Assert exact EOF after all payload output and sixteen zero trailer bytes.
    /// Returns frames, not the number of individual channel samples.
    /// Calling too early permanently fails, including with a word still pending.
    /// Successful empty pushes and repeated finishes are idempotent.
    pub fn finish(&mut self) -> Result<u32, i32> {
        if self.error != 0 {
            return Err(self.error);
        }
        if self.header_len != HEADER_BYTES
            || self.pcm_left != 0
            || self.word_len != 0
            || self.trailer_len != TRAILER_BYTES
        {
            return self.fail(E_FORMAT);
        }
        self.finished = true;
        Ok(self.frames)
    }
}

#[cfg(test)]
#[path = "../../../../../packages/dksample/src/native.rs"]
mod canonical;

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::{vec, vec::Vec};

    fn native(values: &[i16]) -> Vec<u8> {
        let pcm: Vec<u8> = values.iter().flat_map(|v| v.to_be_bytes()).collect();
        canonical::build_native_sample(&pcm, false).unwrap()
    }

    fn expected(values: &[i16]) -> Vec<u8> {
        values
            .chunks(2)
            .flat_map(|pair| {
                let low = pair[0] as u16 as u32;
                let high = pair.get(1).copied().unwrap_or(0) as u16 as u32;
                (low | (high << 16)).to_le_bytes()
            })
            .collect()
    }

    fn feed(
        parser: &mut NativePcm16Stream,
        mut input: &[u8],
        output_size: usize,
        packed: &mut Vec<u8>,
    ) {
        loop {
            let mut output = [0xa5; 19];
            let p = parser.push(input, &mut output[..output_size]).unwrap();
            assert!(p.consumed <= input.len());
            assert!(p.written <= output_size && p.written % 4 == 0);
            assert!(output[p.written..].iter().all(|&v| v == 0xa5));
            packed.extend_from_slice(&output[..p.written]);
            input = &input[p.consumed..];
            if input.is_empty() && p.written == 0 {
                break;
            }
            assert!(p.consumed != 0 || p.written != 0);
        }
    }

    #[test]
    fn all_fragment_boundaries_pack_extremes_and_odd_final_frame() {
        for values in [
            &[-32768, -1, 0, 1, 32767][..],
            &[32767, -32768][..],
            &[-1][..],
        ] {
            let file = native(values);
            for split in 0..=file.len() {
                for output_size in 4..=11 {
                    let mut parser = NativePcm16Stream::new(480_000);
                    let mut packed = Vec::new();
                    feed(&mut parser, &file[..split], output_size, &mut packed);
                    feed(&mut parser, &file[split..], output_size, &mut packed);
                    assert_eq!(parser.finish(), Ok(values.len() as u32));
                    assert_eq!(packed, expected(values));
                    assert_eq!(parser.finish(), Ok(values.len() as u32));
                    assert_eq!(parser.push(&[], &mut []), Ok(Progress::default()));
                }
            }
            let mut parser = NativePcm16Stream::new(480_000);
            let mut packed = Vec::new();
            for byte in &file {
                feed(&mut parser, &[*byte], 4, &mut packed);
            }
            assert_eq!(parser.finish(), Ok(values.len() as u32));
            assert_eq!(packed, expected(values));
        }
    }

    fn stereo(values: &[i16]) -> Vec<u8> {
        let pcm: Vec<u8> = values.iter().flat_map(|v| v.to_be_bytes()).collect();
        canonical::build_native_sample(&pcm, true).unwrap()
    }

    #[test]
    fn stereo_fragmentation_preserves_channels_and_backpressure() {
        let values = [-32768, 32767, -1, 42, 1234, -2345];
        let file = stereo(&values);
        for split in 0..=file.len() {
            for output_size in 4..=11 {
                let mut parser = NativePcm16Stream::new_stereo(3);
                assert_eq!(parser.channels(), 2);
                let mut packed = Vec::new();
                feed(&mut parser, &file[..split], output_size, &mut packed);
                feed(&mut parser, &file[split..], output_size, &mut packed);
                assert_eq!(parser.finish(), Ok(3));
                assert_eq!(packed, expected(&values));
            }
        }
        for size in 0..4 {
            let mut parser = NativePcm16Stream::new_stereo(3);
            let mut output = [0xa5; 3];
            let first = parser.push(&file, &mut output[..size]).unwrap();
            assert_eq!(
                first,
                Progress {
                    consumed: 68,
                    written: 0
                }
            );
            assert_eq!(output, [0xa5; 3]);
            assert_eq!(
                parser.push(&file[68..], &mut output[..size]),
                Ok(Progress::default())
            );
            let mut packed = Vec::new();
            feed(&mut parser, &[], 4, &mut packed);
            for byte in &file[68..] {
                feed(&mut parser, &[*byte], 5, &mut packed);
            }
            assert_eq!(parser.finish(), Ok(3));
            assert_eq!(packed, expected(&values));
        }
    }

    #[test]
    fn strict_modes_and_stereo_frame_alignment_reject_before_output() {
        assert_eq!(NativePcm16Stream::new(1).channels(), 1);
        let valid = stereo(&[-1, 2]);
        let mut cases = Vec::new();
        cases.push(native(&[-1, 2]));
        for flag in [2, 255] {
            let mut bad = valid.clone();
            bad[1] = flag;
            cases.push(bad);
        }
        for pcm in [0u32, 1, 2, 3, 5, 6, u32::MAX, u32::MAX - 3, u32::MAX - 79] {
            let mut bad = valid.clone();
            bad[4..8].copy_from_slice(&pcm.to_be_bytes());
            cases.push(bad);
        }
        for bad in cases {
            let mut parser = NativePcm16Stream::new_stereo(u32::MAX);
            let mut output = [0xa5; 16];
            assert_eq!(parser.push(&bad, &mut output), Err(E_FORMAT));
            assert_eq!(output, [0xa5; 16]);
            sticky(&mut parser, E_FORMAT);
        }
        let mut mono = NativePcm16Stream::new(2);
        let mut output = [0xa5; 16];
        assert_eq!(mono.push(&valid, &mut output), Err(E_FORMAT));
        assert_eq!(output, [0xa5; 16]);
        sticky(&mut mono, E_FORMAT);
    }

    #[test]
    fn stereo_capacity_counts_frames_and_accepts_long_headers_without_allocation() {
        let file = stereo(&[-1, 2, 3, -4]);
        let mut exact = NativePcm16Stream::new_stereo(2);
        let mut packed = Vec::new();
        feed(&mut exact, &file, 4, &mut packed);
        assert_eq!(exact.finish(), Ok(2));
        assert_eq!(packed, expected(&[-1, 2, 3, -4]));
        // Header acceptance proves parser arithmetic, not allocatable device RAM.
        for frames in [1u32, 480_001, 48_000 * 60 * 60, (400 * 1024 * 1024) / 4] {
            let mut header = [0; HEADER_BYTES];
            header.copy_from_slice(&file[..HEADER_BYTES]);
            header[4..8].copy_from_slice(&(frames * 4).to_be_bytes());
            let mut output = [0xa5; 4];
            let mut parser = NativePcm16Stream::new_stereo(frames);
            assert_eq!(
                parser.push(&header, &mut output),
                Ok(Progress {
                    consumed: HEADER_BYTES,
                    written: 0
                })
            );
            assert_eq!(parser.frames, frames);
            assert_eq!(parser.pcm_left, frames * 4);
            assert_eq!(output, [0xa5; 4]);
            let mut too_small = NativePcm16Stream::new_stereo(frames - 1);
            assert_eq!(too_small.push(&header, &mut output), Err(E_TOO_BIG));
            assert_eq!(output, [0xa5; 4]);
            sticky(&mut too_small, E_TOO_BIG);
        }
    }

    #[test]
    fn stereo_truncation_and_late_trailer_errors_are_sticky() {
        let file = stereo(&[-1, 2, 3, -4]);
        for end in 0..file.len() {
            let mut parser = NativePcm16Stream::new_stereo(2);
            let mut packed = Vec::new();
            feed(&mut parser, &file[..end], 4, &mut packed);
            assert_eq!(parser.finish(), Err(E_FORMAT));
            sticky(&mut parser, E_FORMAT);
        }
        for at in file.len() - TRAILER_BYTES..file.len() {
            let mut bad = file.clone();
            bad[at] = 1;
            let mut parser = NativePcm16Stream::new_stereo(2);
            let mut output = [0xa5; 8];
            assert_eq!(parser.push(&bad, &mut output), Err(E_FORMAT));
            assert_eq!(output.as_slice(), expected(&[-1, 2, 3, -4]));
            sticky(&mut parser, E_FORMAT);
        }
    }

    #[test]
    fn zero_and_tiny_output_hold_one_word_then_drain() {
        let file = native(&[-32768, 32767, -1]);
        for size in 0..4 {
            let mut parser = NativePcm16Stream::new(3);
            let mut small = [0xa5; 3];
            let first = parser.push(&file, &mut small[..size]).unwrap();
            assert_eq!(
                first,
                Progress {
                    consumed: 68,
                    written: 0
                }
            );
            assert_eq!(small, [0xa5; 3]);
            assert_eq!(
                parser.push(&file[first.consumed..], &mut small[..size]),
                Ok(Progress::default())
            );
            let mut packed = Vec::new();
            feed(&mut parser, &[], 4, &mut packed);
            feed(&mut parser, &file[first.consumed..], 5, &mut packed);
            assert_eq!(parser.finish(), Ok(3));
            assert_eq!(packed, expected(&[-32768, 32767, -1]));
        }
    }

    #[test]
    fn ten_second_stream_uses_fixed_output_buffer() {
        let values: Vec<i16> = (0..480_000).map(|i| i as i16).collect();
        let file = native(&values);
        let mut parser = NativePcm16Stream::new(480_000);
        let mut consumed = 0;
        let mut frame = 0;
        let mut output = [0xa5; 260];
        while consumed < file.len() {
            let end = core::cmp::min(consumed + 137, file.len());
            let p = parser
                .push(&file[consumed..end], &mut output[..259])
                .unwrap();
            consumed += p.consumed;
            for word in output[..p.written].chunks_exact(4) {
                let packed = u32::from_le_bytes(word.try_into().unwrap());
                assert_eq!(packed as u16 as i16, values[frame]);
                assert_eq!((packed >> 16) as u16 as i16, values[frame + 1]);
                frame += 2;
            }
            assert_eq!(output[259], 0xa5);
            assert!(p.consumed != 0 || p.written != 0);
        }
        assert_eq!(frame, 480_000);
        assert_eq!(parser.finish(), Ok(480_000));
    }

    fn sticky(parser: &mut NativePcm16Stream, code: i32) {
        let mut output = [0xa5; 16];
        assert_eq!(parser.push(&[], &mut output), Err(code));
        assert_eq!(parser.push(&native(&[1]), &mut output), Err(code));
        assert_eq!(parser.finish(), Err(code));
        assert_eq!(output, [0xa5; 16]);
    }

    #[test]
    fn malformed_headers_and_capacity_never_emit() {
        let valid = native(&[1]);
        let mut cases = vec![native(&[])];
        for at in 0..64 {
            let mut bad = valid.clone();
            bad[at] ^= 1;
            // PCM length and rate have explicit cases below.
            if !(4..12).contains(&at) {
                cases.push(bad);
            }
        }
        for pcm in [0, 1, 3, u32::MAX, u32::MAX - 1, u32::MAX - 79] {
            let mut bad = valid.clone();
            bad[4..8].copy_from_slice(&pcm.to_be_bytes());
            cases.push(bad);
        }
        let mut rate = valid.clone();
        rate[8..12].copy_from_slice(&44_100u32.to_be_bytes());
        cases.push(rate);
        for bad in cases {
            let mut parser = NativePcm16Stream::new(u32::MAX);
            let mut output = [0xa5; 16];
            assert_eq!(parser.push(&bad, &mut output), Err(E_FORMAT));
            assert_eq!(output, [0xa5; 16]);
            sticky(&mut parser, E_FORMAT);
        }
        for (limit, frames) in [(0, 1), (2, 3), (480_000, 480_001)] {
            let mut header = valid[..64].to_vec();
            header[4..8].copy_from_slice(&(frames * 2u32).to_be_bytes());
            let mut parser = NativePcm16Stream::new(limit);
            let mut output = [0xa5; 16];
            assert_eq!(parser.push(&header, &mut output), Err(E_TOO_BIG));
            assert_eq!(output, [0xa5; 16]);
            sticky(&mut parser, E_TOO_BIG);
        }
    }

    #[test]
    fn truncation_late_trailer_and_extra_data_are_sticky() {
        let file = native(&[-1, 2, 3]);
        for end in 0..file.len() {
            let mut parser = NativePcm16Stream::new(3);
            let mut packed = Vec::new();
            feed(&mut parser, &file[..end], 4, &mut packed);
            assert_eq!(parser.finish(), Err(E_FORMAT));
            sticky(&mut parser, E_FORMAT);
        }
        for at in file.len() - 16..file.len() {
            let mut bad = file.clone();
            bad[at] = 1;
            let mut parser = NativePcm16Stream::new(3);
            let mut output = [0xa5; 16];
            assert_eq!(parser.push(&bad, &mut output), Err(E_FORMAT));
            assert_eq!(&output[..8], &expected(&[-1, 2, 3]));
            sticky(&mut parser, E_FORMAT);
        }
        for finish_first in [false, true] {
            let mut parser = NativePcm16Stream::new(3);
            parser.push(&file, &mut [0; 8]).unwrap();
            if finish_first {
                assert_eq!(parser.finish(), Ok(3));
            }
            assert_eq!(parser.push(&[0], &mut [0; 4]), Err(E_FORMAT));
            sticky(&mut parser, E_FORMAT);
        }
        let mut parser = NativePcm16Stream::new(3);
        parser.push(&file, &mut []).unwrap();
        assert_eq!(parser.finish(), Err(E_FORMAT));
        sticky(&mut parser, E_FORMAT);
    }
}
