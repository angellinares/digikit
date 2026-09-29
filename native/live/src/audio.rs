//! The cpal real-time output stream: opens the default output device at
//! 48 kHz stereo when possible, falling back cleanly (and reporting the
//! fallback) otherwise, and drives the callback that pops from the ring.
//!
//! The callback itself (`make_callback` below, instantiated once per
//! sample format) must not allocate or lock: it only does index
//! arithmetic, atomic increments, and `SpscRing::pop_into`/`Thread::
//! unpark`, neither of which locks or allocates (`src/ring.rs`,
//! `src/producer.rs` module docs).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::Thread;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample, StreamConfig};

use crate::ring::{SpscRing, StereoSample};

/// Preferred output rate (`scratchpad/rt-native-core-design.md`: the SHARC
/// frame source targets 48 kHz).
pub const TARGET_SAMPLE_RATE: u32 = 48_000;

/// Largest number of frames handled per callback invocation without
/// allocating: a fixed on-stack scratch buffer, refilled from the ring in
/// chunks if the device asks for more than this in one callback (device
/// buffer sizes are normally far smaller than this).
const CALLBACK_SCRATCH_FRAMES: usize = 4096;

pub struct OpenedDevice {
    pub stream: cpal::Stream,
    pub device_name: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub requested_sample_rate: u32,
    pub used_fallback: bool,
}

/// The default output device and the config it will be opened with, before
/// any stream exists (so a caller can adapt its source to the rate first).
pub struct PendingDevice {
    device: cpal::Device,
    config: StreamConfig,
    sample_format: SampleFormat,
    pub device_name: String,
    pub sample_rate: u32,
    pub used_fallback: bool,
}

pub fn default_output() -> Result<PendingDevice, String> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| "no default output device".to_string())?;
    let device_name = device.to_string();
    let (config, sample_format, used_fallback) = pick_config(&device, TARGET_SAMPLE_RATE)?;
    Ok(PendingDevice {
        sample_rate: config.sample_rate,
        device,
        config,
        sample_format,
        device_name,
        used_fallback,
    })
}

impl PendingDevice {
    pub fn start(
        self,
        ring: Arc<SpscRing>,
        underruns: Arc<AtomicU64>,
        callback_frames: Arc<AtomicU64>,
        producer_thread: Thread,
    ) -> Result<OpenedDevice, String> {
        let stream = build_stream(
            &self.device,
            &self.config,
            self.sample_format,
            ring,
            underruns,
            callback_frames,
            producer_thread,
        )?;
        stream
            .play()
            .map_err(|e| format!("failed to start output stream: {e}"))?;
        Ok(OpenedDevice {
            stream,
            device_name: self.device_name,
            sample_rate: self.config.sample_rate,
            channels: self.config.channels,
            requested_sample_rate: TARGET_SAMPLE_RATE,
            used_fallback: self.used_fallback,
        })
    }
}

pub fn open_output_stream(
    ring: Arc<SpscRing>,
    underruns: Arc<AtomicU64>,
    callback_frames: Arc<AtomicU64>,
    producer_thread: Thread,
) -> Result<OpenedDevice, String> {
    default_output()?.start(ring, underruns, callback_frames, producer_thread)
}

/// Looks for a stereo config at `target_rate` in a format we know how to
/// write (F32/I16/U16). Falls back to the device's own default config,
/// reporting the fallback, if none matches -- "fall back cleanly and
/// report if 48 kHz is unavailable".
fn pick_config(
    device: &cpal::Device,
    target_rate: u32,
) -> Result<(StreamConfig, SampleFormat, bool), String> {
    if let Ok(configs) = device.supported_output_configs() {
        for range in configs {
            let supports_rate =
                range.min_sample_rate() <= target_rate && range.max_sample_rate() >= target_rate;
            let usable_format = matches!(
                range.sample_format(),
                SampleFormat::F32 | SampleFormat::I16 | SampleFormat::U16
            );
            if range.channels() == 2 && supports_rate && usable_format {
                let format = range.sample_format();
                let supported = range.with_sample_rate(target_rate);
                return Ok((supported.config(), format, false));
            }
        }
    }
    let default = device
        .default_output_config()
        .map_err(|e| format!("no usable default output config: {e}"))?;
    Ok((default.config(), default.sample_format(), true))
}

fn build_stream(
    device: &cpal::Device,
    config: &StreamConfig,
    sample_format: SampleFormat,
    ring: Arc<SpscRing>,
    underruns: Arc<AtomicU64>,
    callback_frames: Arc<AtomicU64>,
    producer_thread: Thread,
) -> Result<cpal::Stream, String> {
    let channels = config.channels as usize;
    let err_fn = |err| eprintln!("live-audio: output stream error: {err}");

    macro_rules! build {
        ($t:ty) => {
            device
                .build_output_stream(
                    config.clone(),
                    make_callback::<$t>(
                        channels,
                        ring,
                        underruns,
                        callback_frames,
                        producer_thread,
                    ),
                    err_fn,
                    None,
                )
                .map_err(|e| format!("failed to build output stream: {e}"))
        };
    }

    match sample_format {
        SampleFormat::F32 => build!(f32),
        SampleFormat::I16 => build!(i16),
        SampleFormat::U16 => build!(u16),
        other => Err(format!("unsupported output sample format: {other:?}")),
    }
}

fn make_callback<T>(
    channels: usize,
    ring: Arc<SpscRing>,
    underruns: Arc<AtomicU64>,
    callback_frames: Arc<AtomicU64>,
    producer_thread: Thread,
) -> impl FnMut(&mut [T], &cpal::OutputCallbackInfo)
where
    T: Sample + SizedSample + FromSample<f32>,
{
    // Owned by the callback closure, not re-allocated per call.
    let mut scratch = [StereoSample::default(); CALLBACK_SCRATCH_FRAMES];

    move |data: &mut [T], _info: &cpal::OutputCallbackInfo| {
        let frames_needed = data.len() / channels;
        callback_frames.fetch_max(frames_needed as u64, Ordering::Relaxed);
        let mut frames_written = 0usize;
        let mut had_underrun = false;

        while frames_written < frames_needed {
            let chunk_frames = (frames_needed - frames_written).min(CALLBACK_SCRATCH_FRAMES);
            let popped = ring.pop_into(&mut scratch[..chunk_frames]);
            if popped < chunk_frames {
                had_underrun = true;
                for slot in &mut scratch[popped..chunk_frames] {
                    *slot = StereoSample::default();
                }
            }
            for (i, sample) in scratch[..chunk_frames].iter().copied().enumerate() {
                let out_base = (frames_written + i) * channels;
                data[out_base] = T::from_sample(sample.l);
                if channels >= 2 {
                    data[out_base + 1] = T::from_sample(sample.r);
                    for extra in &mut data[out_base + 2..out_base + channels] {
                        *extra = T::from_sample(0.0f32);
                    }
                } else {
                    // Mono device: average L/R into the one channel.
                    data[out_base] = T::from_sample(0.5 * (sample.l + sample.r));
                }
            }
            frames_written += chunk_frames;
        }

        if had_underrun {
            underruns.fetch_add(1, Ordering::Relaxed);
        }
        // Wake the producer: there is now room (this callback just
        // consumed `frames_needed` frames), whether or not this callback
        // itself underran.
        producer_thread.unpark();
    }
}
