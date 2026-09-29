//! Frame sources: anything that can render one SHARC audio frame (32
//! stereo samples) given the next SPI2 TX frame bytes.
//!
//! `FrameSource` is the seam the native SHARC core will later fill in
//! (`scratchpad/rt-native-core-design.md` section 3a: `libsharc_dt2_116`
//! loaded in-process, `call()`+`audio_pull()` per frame). For now this
//! crate ships two sources that ignore `input_frame` entirely: a WAV/PCM
//! player and a test tone, both driven through the same ring/producer
//! path a real SHARC source would use.

use crate::q31::f32_to_q31;
use crate::ring::{FRAME_LEN, StereoSample};

/// Something that can render one SHARC frame's worth of audio.
///
/// `input_frame` is the SPI2 TX frame the repeater queue handed the
/// producer this cycle (wire order; already one-shot-cleared on a repeat;
/// empty before the first frame -- see `src/repeater.rs`). The WAV and
/// tone sources below ignore it; `sharc_source::LiveSource` renders it.
pub trait FrameSource: Send {
    fn render_frame(&mut self, input_frame: &[u8], out: &mut [StereoSample; FRAME_LEN]);
}

/// Silence. The default source a [`crate::player::LivePlayer`] opens with,
/// before `play_wav`/`play_tone` swaps in something audible.
pub struct SilenceSource;

impl FrameSource for SilenceSource {
    fn render_frame(&mut self, _input_frame: &[u8], out: &mut [StereoSample; FRAME_LEN]) {
        *out = [StereoSample::default(); FRAME_LEN];
    }
}

/// A fixed-frequency sine test tone, synthesized through the same Q31
/// round trip a real SHARC frame source uses (`src/q31.rs`), so this
/// source exercises the whole conversion path, not just the ring.
pub struct ToneSource {
    sample_rate: f32,
    freq_hz: f32,
    amplitude: f32,
    phase: f32,
}

impl ToneSource {
    pub fn new(sample_rate: f32, freq_hz: f32, amplitude: f32) -> Self {
        ToneSource {
            sample_rate,
            freq_hz,
            amplitude: amplitude.clamp(0.0, 1.0),
            phase: 0.0,
        }
    }
}

impl FrameSource for ToneSource {
    fn render_frame(&mut self, _input_frame: &[u8], out: &mut [StereoSample; FRAME_LEN]) {
        let step = std::f32::consts::TAU * self.freq_hz / self.sample_rate;
        for slot in out.iter_mut() {
            let raw = f32_to_q31(self.amplitude * self.phase.sin());
            let value = crate::q31::q31_to_f32(raw);
            *slot = StereoSample { l: value, r: value };
            self.phase += step;
            if self.phase > std::f32::consts::TAU {
                self.phase -= std::f32::consts::TAU;
            }
        }
    }
}

/// A minimal PCM WAV reader: enough for the mono/stereo, 16/24/32-bit
/// integer or 32-bit float files this project writes
/// (`tools/sharc_dac.py`'s `write_wav_stereo`, `tools/gen_test_samples.py`)
/// and the `out/listen/*.wav` capture outputs. No resampling: a WAV whose
/// sample rate does not match the open device's output rate plays back at
/// the wrong pitch/speed, same as ignoring resampling anywhere else in this
/// project's audio path.
#[derive(Debug)]
pub struct WavSource {
    samples: Vec<StereoSample>,
    pos: usize,
    pub sample_rate: u32,
    pub looping: bool,
}

#[derive(Debug)]
pub enum WavError {
    Io(std::io::Error),
    NotRiffWave,
    MissingFmtChunk,
    MissingDataChunk,
    UnsupportedFormat { audio_format: u16, bits: u16 },
}

impl std::fmt::Display for WavError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WavError::Io(e) => write!(f, "io error: {e}"),
            WavError::NotRiffWave => write!(f, "not a RIFF/WAVE file"),
            WavError::MissingFmtChunk => write!(f, "missing 'fmt ' chunk"),
            WavError::MissingDataChunk => write!(f, "missing 'data' chunk"),
            WavError::UnsupportedFormat { audio_format, bits } => write!(
                f,
                "unsupported WAV format (audio_format={audio_format}, bits_per_sample={bits})"
            ),
        }
    }
}

impl std::error::Error for WavError {}

impl From<std::io::Error> for WavError {
    fn from(e: std::io::Error) -> Self {
        WavError::Io(e)
    }
}

impl WavSource {
    pub fn load(path: &std::path::Path) -> Result<Self, WavError> {
        let bytes = std::fs::read(path)?;
        Self::from_bytes(&bytes)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, WavError> {
        if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
            return Err(WavError::NotRiffWave);
        }
        let mut pos = 12usize;
        let mut channels: u16 = 0;
        let mut sample_rate: u32 = 0;
        let mut bits_per_sample: u16 = 0;
        let mut audio_format: u16 = 0;
        let mut data: &[u8] = &[];

        while pos + 8 <= bytes.len() {
            let id = &bytes[pos..pos + 4];
            let size = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
            let body_start = pos + 8;
            let body_end = (body_start + size).min(bytes.len());
            let body = &bytes[body_start..body_end];

            if id == b"fmt " {
                if body.len() < 16 {
                    return Err(WavError::MissingFmtChunk);
                }
                audio_format = u16::from_le_bytes(body[0..2].try_into().unwrap());
                channels = u16::from_le_bytes(body[2..4].try_into().unwrap());
                sample_rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
                bits_per_sample = u16::from_le_bytes(body[14..16].try_into().unwrap());
            } else if id == b"data" {
                data = body;
            }
            // Chunks are word-aligned: an odd-sized chunk has a pad byte.
            pos = body_start + size + (size & 1);
        }

        if channels == 0 || sample_rate == 0 {
            return Err(WavError::MissingFmtChunk);
        }
        if data.is_empty() {
            return Err(WavError::MissingDataChunk);
        }

        type SampleDecoder = Box<dyn Fn(&[u8]) -> f32>;
        let to_f32: SampleDecoder = match (audio_format, bits_per_sample) {
            (1, 16) => Box::new(|b: &[u8]| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0),
            (1, 24) => Box::new(|b: &[u8]| {
                let raw = i32::from_le_bytes([b[0], b[1], b[2], 0]) << 8; // sign-extend
                (raw >> 8) as f32 / 8_388_608.0
            }),
            (1, 32) => Box::new(|b: &[u8]| {
                i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0
            }),
            (3, 32) => Box::new(|b: &[u8]| f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
            (0xFFFE, 16) => Box::new(|b: &[u8]| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0),
            _ => {
                return Err(WavError::UnsupportedFormat {
                    audio_format,
                    bits: bits_per_sample,
                });
            }
        };
        let bytes_per_sample = (bits_per_sample / 8) as usize;
        let frame_bytes = bytes_per_sample * channels as usize;
        if frame_bytes == 0 {
            return Err(WavError::UnsupportedFormat {
                audio_format,
                bits: bits_per_sample,
            });
        }

        let mut samples = Vec::with_capacity(data.len() / frame_bytes);
        let mut off = 0usize;
        while off + frame_bytes <= data.len() {
            let l = to_f32(&data[off..off + bytes_per_sample]);
            let r = if channels >= 2 {
                to_f32(&data[off + bytes_per_sample..off + 2 * bytes_per_sample])
            } else {
                l
            };
            samples.push(StereoSample { l, r });
            off += frame_bytes;
        }

        Ok(WavSource {
            samples,
            pos: 0,
            sample_rate,
            looping: false,
        })
    }

    pub fn total_samples(&self) -> usize {
        self.samples.len()
    }

    pub fn is_finished(&self) -> bool {
        !self.looping && self.pos >= self.samples.len()
    }
}

impl FrameSource for WavSource {
    fn render_frame(&mut self, _input_frame: &[u8], out: &mut [StereoSample; FRAME_LEN]) {
        for slot in out.iter_mut() {
            if self.pos >= self.samples.len() {
                if self.looping && !self.samples.is_empty() {
                    self.pos = 0;
                } else {
                    *slot = StereoSample::default();
                    continue;
                }
            }
            *slot = self.samples[self.pos];
            self.pos += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_pcm16_wav(path: &std::path::Path, sample_rate: u32, samples: &[(i16, i16)]) {
        let mut data = Vec::new();
        for (l, r) in samples {
            data.extend_from_slice(&l.to_le_bytes());
            data.extend_from_slice(&r.to_le_bytes());
        }
        let byte_rate = sample_rate * 2 * 2;
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&((36 + data.len()) as u32).to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // PCM
        out.extend_from_slice(&2u16.to_le_bytes()); // stereo
        out.extend_from_slice(&sample_rate.to_le_bytes());
        out.extend_from_slice(&byte_rate.to_le_bytes());
        out.extend_from_slice(&4u16.to_le_bytes()); // block align
        out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&data);
        std::fs::write(path, out).unwrap();
    }

    #[test]
    fn tone_source_produces_bounded_nonzero_samples() {
        let mut tone = ToneSource::new(48_000.0, 440.0, 0.5);
        let mut out = [StereoSample::default(); FRAME_LEN];
        tone.render_frame(&[], &mut out);
        assert!(out.iter().any(|s| s.l != 0.0));
        for s in &out {
            assert!(s.l.abs() <= 0.5001);
            assert_eq!(s.l, s.r);
        }
    }

    #[test]
    fn tone_source_amplitude_is_clamped() {
        let tone = ToneSource::new(48_000.0, 440.0, 2.0);
        assert!(tone.amplitude <= 1.0);
    }

    #[test]
    fn wav_source_reads_pcm16_stereo() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("live_audio_test_{}.wav", std::process::id()));
        write_pcm16_wav(&path, 48_000, &[(16384, -16384), (0, 32767)]);

        let mut src = WavSource::load(&path).unwrap();
        assert_eq!(src.sample_rate, 48_000);
        assert_eq!(src.total_samples(), 2);

        let mut out = [StereoSample::default(); FRAME_LEN];
        src.render_frame(&[], &mut out);
        assert!((out[0].l - 0.5).abs() < 1e-3);
        assert!((out[0].r + 0.5).abs() < 1e-3);
        assert!((out[1].l - 0.0).abs() < 1e-3);
        assert!((out[1].r - 1.0).abs() < 1e-2);
        // Past the end of a non-looping source: silence.
        assert_eq!(out[2], StereoSample::default());
        assert!(src.is_finished());

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn wav_source_loops_when_enabled() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("live_audio_test_loop_{}.wav", std::process::id()));
        write_pcm16_wav(&path, 48_000, &[(32767, 32767)]);

        let mut src = WavSource::load(&path).unwrap();
        src.looping = true;
        let mut out = [StereoSample::default(); FRAME_LEN];
        src.render_frame(&[], &mut out);
        // Every sample should be the single looped source sample (~1.0),
        // never silence, since looping never runs out.
        for s in &out {
            assert!(s.l > 0.9, "looping source produced silence: {s:?}");
        }
        assert!(!src.is_finished());

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn rejects_non_riff_input() {
        let err = WavSource::from_bytes(b"not a wav file").unwrap_err();
        assert!(matches!(err, WavError::NotRiffWave));
    }
}
