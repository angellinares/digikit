//! Bounded RIFF/WAVE conversion to the native 48 kHz sample wrapper.
use crate::{FormatError, build_native_sample};

pub const MAX_SOURCE_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_OUTPUT_PCM: usize = 128 * 1024 * 1024;
#[derive(Clone, Copy, Debug)]
pub struct WavLimits {
    pub max_source_bytes: usize,
    pub max_output_pcm: usize,
    pub max_phases: u32,
}
impl Default for WavLimits {
    fn default() -> Self {
        Self {
            max_source_bytes: MAX_SOURCE_BYTES,
            max_output_pcm: MAX_OUTPUT_PCM,
            max_phases: 48_000,
        }
    }
}
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum WavError {
    TooLarge,
    NotWave,
    TruncatedChunk,
    MissingChunk,
    InvalidFormat(&'static str),
    UnsupportedEncoding,
    NonFinite,
    OutputTooLarge,
    Format(FormatError),
}
impl From<FormatError> for WavError {
    fn from(e: FormatError) -> Self {
        Self::Format(e)
    }
}
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct NativeInfo {
    pub src_rate: u32,
    pub channels: u16,
    pub src_frames: usize,
    pub frames: usize,
    pub data_len: usize,
}
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct NativeSample {
    pub bytes: Vec<u8>,
    pub info: NativeInfo,
}
fn le16(b: &[u8]) -> u16 {
    u16::from_le_bytes(b.try_into().unwrap())
}
fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes(b.try_into().unwrap())
}
fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        let r = a % b;
        a = b;
        b = r
    }
    a
}
fn bessel(x: f64) -> f64 {
    let (mut total, mut term, mut k) = (1.0, 1.0, 1u32);
    while term > 1e-12 * total {
        term *= (x / (2.0 * k as f64)).powi(2);
        total += term;
        k += 1
    }
    total
}
fn decode(fmt: &[u8], data: &[u8]) -> Result<(u32, Vec<Vec<f64>>), WavError> {
    if fmt.len() < 16 {
        return Err(WavError::InvalidFormat("short fmt"));
    }
    let (mut tag, ch, rate, block, bits) = (
        le16(&fmt[0..2]),
        le16(&fmt[2..4]),
        le32(&fmt[4..8]),
        le16(&fmt[12..14]),
        le16(&fmt[14..16]),
    );
    if tag == 0xfffe {
        if fmt.len() < 40 {
            return Err(WavError::InvalidFormat("short extensible fmt"));
        }
        let subtype = le16(&fmt[24..26]);
        if fmt[26..40] != [0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71][..] {
            return Err(WavError::InvalidFormat("noncanonical extensible GUID"));
        }
        tag = subtype
    }
    if !(1..=2).contains(&ch) || rate == 0 {
        return Err(WavError::InvalidFormat("channels or rate"));
    }
    let width = usize::from(bits) / 8;
    if width == 0 || usize::from(block) != usize::from(ch) * width {
        return Err(WavError::InvalidFormat("block alignment"));
    }
    if data.len() % usize::from(block) != 0 {
        return Err(WavError::InvalidFormat("partial frame"));
    }
    let frames = data.len() / usize::from(block);
    let mut out = (0..ch)
        .map(|_| Vec::with_capacity(frames))
        .collect::<Vec<_>>();
    for f in 0..frames {
        for c in 0..usize::from(ch) {
            let p = f * usize::from(block) + c * width;
            let v = match (tag, bits) {
                (1, 8) => (f64::from(data[p]) - 128.0) / 128.0,
                (1, 16) => {
                    f64::from(i16::from_le_bytes(data[p..p + 2].try_into().unwrap())) / 32768.0
                }
                (1, 24) => {
                    let x = i32::from(data[p])
                        | (i32::from(data[p + 1]) << 8)
                        | (i32::from(data[p + 2]) << 16);
                    f64::from(if x & 0x800000 != 0 { x | !0xffffff } else { x }) / 8388608.0
                }
                (1, 32) => {
                    f64::from(i32::from_le_bytes(data[p..p + 4].try_into().unwrap())) / 2147483648.0
                }
                (3, 32) => f64::from(f32::from_le_bytes(data[p..p + 4].try_into().unwrap())),
                (3, 64) => f64::from_le_bytes(data[p..p + 8].try_into().unwrap()),
                _ => return Err(WavError::UnsupportedEncoding),
            };
            if !v.is_finite() {
                return Err(WavError::NonFinite);
            }
            out[c].push(v)
        }
    }
    Ok((rate, out))
}
fn resample(input: &[f64], src: u32, limit: &WavLimits) -> Result<Vec<f64>, WavError> {
    if src == 48_000 {
        return Ok(input.to_vec());
    }
    let g = gcd(src, 48_000);
    let (up, down) = (48_000 / g, src / g);
    if up > limit.max_phases {
        return Err(WavError::InvalidFormat("phase count"));
    }
    let fc = (48_000.0 / src as f64).min(1.0) * 0.95;
    let i0 = bessel(8.6);
    let mut phases = Vec::with_capacity(up as usize);
    for p in 0..up {
        let mut taps = Vec::with_capacity(64);
        for k in 0..64 {
            let t = p as f64 / up as f64 + (31 - k) as f64;
            let x = fc * t;
            let sinc = if x == 0.0 {
                1.0
            } else {
                (core::f64::consts::PI * x).sin() / (core::f64::consts::PI * x)
            };
            let r = t / 32.0;
            let win = if r * r < 1.0 {
                bessel(8.6 * (1.0 - r * r).sqrt()) / i0
            } else {
                0.0
            };
            taps.push(fc * sinc * win)
        }
        phases.push(taps)
    }
    let count = input
        .len()
        .checked_mul(up as usize)
        .ok_or(WavError::OutputTooLarge)?
        .div_ceil(down as usize);
    let mut padded = vec![0.0; 32];
    padded.extend_from_slice(input);
    padded.resize(padded.len() + 32, 0.0);
    let mut out = Vec::with_capacity(count);
    for n in 0..count {
        let i = n * down as usize / up as usize;
        let p = (n * down as usize) % up as usize;
        let mut sum = 0.0;
        for k in 0..64 {
            sum += phases[p][k] * padded[i + 1 + k]
        }
        out.push(sum)
    }
    Ok(out)
}
pub fn wav_to_native(bytes: &[u8], limits: WavLimits) -> Result<NativeSample, WavError> {
    if bytes.len() > limits.max_source_bytes {
        return Err(WavError::TooLarge);
    }
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(WavError::NotWave);
    }
    let (mut pos, mut fmt, mut data) = (12, None, None);
    while pos + 8 <= bytes.len() {
        let size = le32(&bytes[pos + 4..pos + 8]) as usize;
        let end = pos
            .checked_add(8)
            .and_then(|x| x.checked_add(size))
            .ok_or(WavError::TruncatedChunk)?;
        if end > bytes.len() {
            return Err(WavError::TruncatedChunk);
        }
        match &bytes[pos..pos + 4] {
            b"fmt " => fmt = Some(&bytes[pos + 8..end]),
            b"data" => data = Some(&bytes[pos + 8..end]),
            _ => {}
        }
        pos = end + (size & 1);
        if pos > bytes.len() {
            return Err(WavError::TruncatedChunk);
        }
    }
    let (rate, channels) = decode(
        fmt.ok_or(WavError::MissingChunk)?,
        data.ok_or(WavError::MissingChunk)?,
    )?;
    let src_frames = channels[0].len();
    let converted = channels
        .iter()
        .map(|c| resample(c, rate, &limits))
        .collect::<Result<Vec<_>, _>>()?;
    let frames = converted[0].len();
    let data_len = frames
        .checked_mul(converted.len())
        .and_then(|x| x.checked_mul(2))
        .ok_or(WavError::OutputTooLarge)?;
    if data_len > limits.max_output_pcm {
        return Err(WavError::OutputTooLarge);
    }
    let mut pcm = Vec::with_capacity(data_len);
    for i in 0..frames {
        for c in &converted {
            let s = (c[i] * 32768.0).round_ties_even().clamp(-32768.0, 32767.0) as i16;
            pcm.extend_from_slice(&s.to_be_bytes())
        }
    }
    let stereo = converted.len() == 2;
    Ok(NativeSample {
        bytes: build_native_sample(&pcm, stereo)?,
        info: NativeInfo {
            src_rate: rate,
            channels: converted.len() as u16,
            src_frames,
            frames,
            data_len,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn wav(tag: u16, bits: u16, rate: u32, ch: u16, data: &[u8]) -> Vec<u8> {
        let mut x = Vec::new();
        x.extend_from_slice(b"RIFF");
        x.extend_from_slice(&0u32.to_le_bytes());
        x.extend_from_slice(b"WAVEfmt ");
        x.extend_from_slice(&16u32.to_le_bytes());
        x.extend_from_slice(&tag.to_le_bytes());
        x.extend_from_slice(&ch.to_le_bytes());
        x.extend_from_slice(&rate.to_le_bytes());
        x.extend_from_slice(&(rate * u32::from(ch) * u32::from(bits) / 8).to_le_bytes());
        x.extend_from_slice(&(ch * bits / 8).to_le_bytes());
        x.extend_from_slice(&bits.to_le_bytes());
        x.extend_from_slice(b"data");
        x.extend_from_slice(&(data.len() as u32).to_le_bytes());
        x.extend_from_slice(data);
        if data.len() & 1 != 0 {
            x.push(0);
        }
        let n = (x.len() - 8) as u32;
        x[4..8].copy_from_slice(&n.to_le_bytes());
        x
    }
    #[test]
    fn pcm_and_float() {
        let a = wav(1, 16, 48000, 1, &[0, 0, 0xff, 0x7f]);
        let n = wav_to_native(&a, WavLimits::default()).unwrap();
        assert_eq!(
            (n.info.frames, n.info.data_len, &n.bytes[0x40..0x44]),
            (2, 4, &[0, 0, 0x7f, 0xff][..])
        );
        let f = wav(3, 32, 48000, 1, &0.5f32.to_le_bytes());
        assert_eq!(
            &wav_to_native(&f, WavLimits::default()).unwrap().bytes[0x40..0x42],
            &[0x40, 0]
        );
        for (tag, bits, bytes) in [
            (1, 8, vec![128]),
            (1, 24, vec![0, 0, 0]),
            (1, 32, vec![0; 4]),
            (3, 64, vec![0; 8]),
        ] {
            assert_eq!(
                wav_to_native(&wav(tag, bits, 48000, 1, &bytes), WavLimits::default())
                    .unwrap()
                    .info
                    .data_len,
                2
            );
        }
        let zero = wav(1, 16, 44100, 2, &[0; 8]);
        assert_eq!(
            wav_to_native(&zero, WavLimits::default())
                .unwrap()
                .info
                .frames,
            3
        );
    }
    #[test]
    fn rejects_bad() {
        assert!(wav_to_native(b"RIFF", WavLimits::default()).is_err());
        let mut bad = wav(1, 16, 48000, 1, &[0]);
        assert!(wav_to_native(&bad, WavLimits::default()).is_err());
        bad = wav(3, 32, 48000, 1, &f32::NAN.to_le_bytes());
        assert_eq!(
            wav_to_native(&bad, WavLimits::default()),
            Err(WavError::NonFinite)
        );
    }
}
