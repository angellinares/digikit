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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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

/// Frames of linear fade at each rebuffer stop and resume.
pub const FADE_FRAMES: usize = 128;

/// What the output callback does when the ring runs dry.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Underrun {
    /// Play silence for the missing frames and carry on.
    #[default]
    Gap,
    /// Fade out, play silence until `resume_frames` are queued, fade back in.
    /// Below real time this plays in bursts instead of a stream full of gaps.
    Rebuffer { resume_frames: usize },
}

/// Counters the callback keeps; read from any thread.
#[derive(Debug, Default)]
pub struct SinkCounters {
    /// Frames handed to the device, sound or silence.
    pub played_frames: AtomicU64,
    /// Of those, frames of silence written because the ring was dry or refilling.
    pub silent_frames: AtomicU64,
    /// True while a rebuffering callback waits for the ring to refill.
    pub waiting: AtomicBool,
}

/// Options for one output stream.
#[derive(Clone, Default)]
pub struct SinkOptions {
    pub underrun: Underrun,
    pub counters: Arc<SinkCounters>,
    /// Every played frame is also pushed here (dropped when full), for a
    /// capture of what the device actually received.
    pub monitor: Option<Arc<SpscRing>>,
}

/// The ring-to-frames half of the output callback, independent of the
/// device sample format.
#[derive(Debug, Default)]
pub struct SinkRender {
    underrun: Underrun,
    waiting: bool,
    fade_in: usize,
}

impl SinkRender {
    pub fn new(underrun: Underrun) -> Self {
        Self {
            underrun,
            waiting: false,
            fade_in: 0,
        }
    }

    /// True while waiting for the ring to refill (rebuffer only).
    pub fn waiting(&self) -> bool {
        self.waiting
    }

    /// Fill `out` from `ring`. Returns (an underrun started here, frames of
    /// silence written).
    pub fn fill(&mut self, ring: &SpscRing, out: &mut [StereoSample]) -> (bool, usize) {
        if let Underrun::Rebuffer { resume_frames } = self.underrun
            && self.waiting
            && ring.len() >= resume_frames.clamp(1, ring.capacity())
        {
            self.waiting = false;
            self.fade_in = FADE_FRAMES;
        }
        let got = if self.waiting { 0 } else { ring.pop_into(out) };
        for s in &mut out[..got] {
            if self.fade_in == 0 {
                break;
            }
            let gain = 1.0 - self.fade_in as f32 / FADE_FRAMES as f32;
            s.l *= gain;
            s.r *= gain;
            self.fade_in -= 1;
        }
        let short = got < out.len();
        let started = short && !self.waiting;
        if short {
            if matches!(self.underrun, Underrun::Rebuffer { .. }) && !self.waiting {
                let n = got.min(FADE_FRAMES);
                for (i, s) in out[got - n..got].iter_mut().enumerate() {
                    let gain = 1.0 - (i + 1) as f32 / n as f32;
                    s.l *= gain;
                    s.r *= gain;
                }
                self.waiting = true;
                self.fade_in = 0;
            }
            out[got..].fill(StereoSample::default());
        }
        (started, out.len() - got)
    }
}

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

/// Rare output-error observations; no locks or allocations when recording.
#[derive(Clone, Default)]
pub struct StreamErrors(Arc<AtomicU64>);
impl StreamErrors {
    pub fn record(&self) {
        self.0.fetch_add(1, Ordering::Release);
    }
    pub fn error(&self) -> Option<String> {
        let count = self.0.load(Ordering::Acquire);
        (count != 0).then(|| format!("output stream error ({count} events; details in stderr)"))
    }
}

pub fn default_output() -> Result<PendingDevice, String> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| "no default output device".to_string())?;
    // CPAL's Device Display returns fmt::Error if the OS rejects the name
    // query, which makes ToString panic. A label must not prevent playback.
    let device_name = device
        .description()
        .map(|description| description.name().to_owned())
        .unwrap_or_else(|_| "Default output device".to_owned());
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
        self.start_with_errors(
            ring,
            underruns,
            callback_frames,
            producer_thread,
            StreamErrors::default(),
        )
    }

    pub fn start_with_errors(
        self,
        ring: Arc<SpscRing>,
        underruns: Arc<AtomicU64>,
        callback_frames: Arc<AtomicU64>,
        producer_thread: Thread,
        stream_errors: StreamErrors,
    ) -> Result<OpenedDevice, String> {
        self.start_with_options(
            ring,
            underruns,
            callback_frames,
            producer_thread,
            stream_errors,
            SinkOptions::default(),
        )
    }

    /// Like `start_with_errors`, with an underrun policy, counters and an optional monitor ring.
    pub fn start_with_options(
        self,
        ring: Arc<SpscRing>,
        underruns: Arc<AtomicU64>,
        callback_frames: Arc<AtomicU64>,
        producer_thread: Thread,
        stream_errors: StreamErrors,
        options: SinkOptions,
    ) -> Result<OpenedDevice, String> {
        let stream = build_stream(
            &self.device,
            &self.config,
            self.sample_format,
            ring,
            underruns,
            callback_frames,
            producer_thread,
            stream_errors,
            options,
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

#[allow(clippy::too_many_arguments)]
fn build_stream(
    device: &cpal::Device,
    config: &StreamConfig,
    sample_format: SampleFormat,
    ring: Arc<SpscRing>,
    underruns: Arc<AtomicU64>,
    callback_frames: Arc<AtomicU64>,
    producer_thread: Thread,
    stream_errors: StreamErrors,
    options: SinkOptions,
) -> Result<cpal::Stream, String> {
    let channels = config.channels as usize;
    let err_fn = move |err| {
        stream_errors.record();
        eprintln!("live-audio: output stream error: {err}");
    };

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
                        options.clone(),
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
    options: SinkOptions,
) -> impl FnMut(&mut [T], &cpal::OutputCallbackInfo)
where
    T: Sample + SizedSample + FromSample<f32>,
{
    // Owned by the callback closure, not re-allocated per call.
    let mut scratch = [StereoSample::default(); CALLBACK_SCRATCH_FRAMES];
    let mut render = SinkRender::new(options.underrun);

    move |data: &mut [T], _info: &cpal::OutputCallbackInfo| {
        let frames_needed = data.len() / channels;
        callback_frames.fetch_max(frames_needed as u64, Ordering::Relaxed);
        let mut frames_written = 0usize;
        let mut had_underrun = false;
        let mut silent = 0usize;

        while frames_written < frames_needed {
            let chunk_frames = (frames_needed - frames_written).min(CALLBACK_SCRATCH_FRAMES);
            let (started, quiet) = render.fill(&ring, &mut scratch[..chunk_frames]);
            had_underrun |= started;
            silent += quiet;
            if let Some(monitor) = &options.monitor {
                monitor.push_slice(&scratch[..chunk_frames]);
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
        options
            .counters
            .played_frames
            .fetch_add(frames_needed as u64, Ordering::Relaxed);
        options
            .counters
            .silent_frames
            .fetch_add(silent as u64, Ordering::Relaxed);
        options
            .counters
            .waiting
            .store(render.waiting(), Ordering::Relaxed);
        // Wake the producer: there is now room (this callback just
        // consumed `frames_needed` frames), whether or not this callback
        // itself underran.
        producer_thread.unpark();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ones(n: usize) -> Vec<StereoSample> {
        vec![StereoSample { l: 1.0, r: 1.0 }; n]
    }

    #[test]
    fn gap_mode_zero_fills_the_shortfall_and_keeps_the_rest() {
        let ring = SpscRing::with_capacity(64);
        ring.push_slice(&ones(10));
        let mut render = SinkRender::new(Underrun::Gap);
        let mut out = [StereoSample { l: 9.0, r: 9.0 }; 16];
        assert_eq!(render.fill(&ring, &mut out), (true, 6));
        assert!(out[..10].iter().all(|s| s.l == 1.0));
        assert!(out[10..].iter().all(|s| *s == StereoSample::default()));
        assert!(!render.waiting());
    }

    #[test]
    fn rebuffer_fades_out_waits_for_the_threshold_then_fades_in() {
        let ring = SpscRing::with_capacity(256);
        ring.push_slice(&ones(10));
        let mut render = SinkRender::new(Underrun::Rebuffer { resume_frames: 20 });
        let mut out = [StereoSample::default(); 16];
        assert_eq!(render.fill(&ring, &mut out), (true, 6));
        assert!(render.waiting());
        assert_eq!(out[9].l, 0.0, "the last played frame is faded to silence");
        assert!(out[0].l > 0.0 && out[0].l < 1.0);
        ring.push_slice(&ones(5));
        assert_eq!(render.fill(&ring, &mut out), (false, 16), "still refilling");
        assert_eq!(ring.len(), 5, "nothing is consumed while refilling");
        ring.push_slice(&ones(20));
        assert_eq!(render.fill(&ring, &mut out), (false, 0));
        assert!(!render.waiting());
        assert_eq!(out[0].l, 0.0, "the fade-in starts from silence");
        assert!(out[15].l > out[1].l);
    }
}
