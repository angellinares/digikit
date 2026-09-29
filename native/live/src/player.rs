//! `LivePlayer`: ties the ring, the frame repeater, a producer thread and
//! the cpal output stream together. This is what both the CLI
//! (`src/bin/live_play.rs`) and the C ABI (`src/abi.rs`) drive.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::audio::{self, OpenedDevice, TARGET_SAMPLE_RATE};
use crate::producer::{Producer, SharedSource, render_one};
use crate::repeater::{FrameQueue, QueueStats};
use crate::resample::RateAdapter;
use crate::ring::{FRAME_LEN, SpscRing, StereoSample};
use crate::sharc_source::RenderLog;
use crate::source::{FrameSource, SilenceSource, ToneSource, WavError, WavSource};

/// Ring capacity is a multiple of the target latency, with a floor so a
/// tiny requested latency still leaves the producer room to work ahead of
/// the callback.
const RING_CAPACITY_MULTIPLE: usize = 4;
const RING_CAPACITY_FLOOR_FRAMES: usize = 256;

#[derive(Clone, Copy, Debug)]
pub struct Stats {
    pub sample_rate: u32,
    pub requested_sample_rate: u32,
    pub channels: u16,
    pub used_fallback: bool,
    pub target_latency_frames: u32,
    pub ring_capacity_frames: u32,
    pub ring_fill_frames: u32,
    pub underruns: u64,
    pub frames_rendered: u64,
    /// The largest callback buffer the device asked for, in frames.
    pub max_callback_frames: u64,
}

/// A player with an output device (a producer thread renders ahead into
/// the ring, paced by the device callback), or offline (no device, no
/// thread: the caller pulls frames with `render_offline`, e.g. a headless
/// check or a host with its own audio clock).
pub struct LivePlayer {
    ring: Arc<SpscRing>,
    underruns: Arc<AtomicU64>,
    callback_frames: Arc<AtomicU64>,
    frames_rendered: Arc<AtomicU64>,
    queue: Arc<FrameQueue>,
    source: SharedSource,
    producer: Option<Producer>,
    device: Option<OpenedDevice>,
    target_latency_frames: u32,
    render_log: Option<Arc<Mutex<RenderLog>>>,
    /// `render_offline`'s input buffer (a Mutex: frames are pushed from
    /// another thread while a caller renders, both through `&self`).
    offline_input: Mutex<Vec<u8>>,
}

impl LivePlayer {
    /// Opens the default output device and starts the producer thread with
    /// silence; `play_wav`/`play_tone` swap in a real source afterwards
    /// (matching the Python wrapper's `open()` then `play(...)`).
    pub fn open(target_latency_frames: u32) -> Result<Self, String> {
        Self::open_with_source(target_latency_frames, Box::new(SilenceSource), None, None)
    }

    /// Opens the default output device with SOURCE already rendering. With
    /// PREFILL, the stream starts only once the producer has filled the
    /// ring (or PREFILL elapsed): the first frames' warm-up (page faults,
    /// cold caches) happens before the device asks for anything. With
    /// SOURCE_RATE, a device running at another rate gets the source
    /// through a [`RateAdapter`] (`src/resample.rs`).
    pub fn open_with_source(
        target_latency_frames: u32,
        source: Box<dyn FrameSource>,
        prefill: Option<Duration>,
        source_rate: Option<u32>,
    ) -> Result<Self, String> {
        let pending = audio::default_output()
            .map_err(|e| format!("live-audio: failed to open output device: {e}"))?;
        let source = match source_rate {
            Some(rate) if rate != pending.sample_rate => {
                Box::new(RateAdapter::new(source, rate, pending.sample_rate))
                    as Box<dyn FrameSource>
            }
            _ => source,
        };
        let ring_capacity = (target_latency_frames as usize * RING_CAPACITY_MULTIPLE)
            .max(RING_CAPACITY_FLOOR_FRAMES);
        let ring = Arc::new(SpscRing::with_capacity(ring_capacity));
        let queue = Arc::new(FrameQueue::new());
        let underruns = Arc::new(AtomicU64::new(0));
        let frames_rendered = Arc::new(AtomicU64::new(0));
        let callback_frames = Arc::new(AtomicU64::new(0));
        let source: SharedSource = Arc::new(Mutex::new(source));

        let producer = Producer::spawn(
            Arc::clone(&source),
            Arc::clone(&queue),
            Arc::clone(&ring),
            Arc::clone(&frames_rendered),
        );

        if let Some(limit) = prefill {
            let deadline = Instant::now() + limit;
            while ring.room() >= crate::ring::FRAME_LEN && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
        }

        let device = pending
            .start(
                Arc::clone(&ring),
                Arc::clone(&underruns),
                Arc::clone(&callback_frames),
                producer.thread_handle(),
            )
            .map_err(|e| format!("live-audio: failed to open output device: {e}"))?;

        Ok(LivePlayer {
            ring,
            underruns,
            callback_frames,
            frames_rendered,
            queue,
            source,
            producer: Some(producer),
            device: Some(device),
            target_latency_frames,
            render_log: None,
            offline_input: Mutex::new(Vec::new()),
        })
    }

    /// A player with no output device and no producer thread: frames are
    /// rendered only by `render_offline`, at 48 kHz.
    pub fn offline(source: Box<dyn FrameSource>) -> Self {
        LivePlayer {
            ring: Arc::new(SpscRing::with_capacity(RING_CAPACITY_FLOOR_FRAMES)),
            underruns: Arc::new(AtomicU64::new(0)),
            callback_frames: Arc::new(AtomicU64::new(0)),
            frames_rendered: Arc::new(AtomicU64::new(0)),
            queue: Arc::new(FrameQueue::new()),
            source: Arc::new(Mutex::new(source)),
            producer: None,
            device: None,
            target_latency_frames: 0,
            render_log: None,
            offline_input: Mutex::new(Vec::with_capacity(crate::repeater::FRAME_BYTES)),
        }
    }

    /// Offline only: render OUT.len() / 32 frames (the queue's next frames
    /// or repeats, as the producer thread would) into OUT. -> frames
    /// rendered; an error for a player with a device.
    pub fn render_offline(&self, out: &mut [StereoSample]) -> Result<usize, String> {
        if self.device.is_some() {
            return Err("render_offline: this player has an output device".to_string());
        }
        let mut input = self.offline_input.lock().expect("offline input poisoned");
        let mut frame = [StereoSample::default(); FRAME_LEN];
        let mut n = 0;
        for chunk in out.chunks_exact_mut(FRAME_LEN) {
            render_one(&self.source, &self.queue, &mut input, &mut frame);
            chunk.copy_from_slice(&frame);
            n += 1;
        }
        self.frames_rendered
            .fetch_add((n * FRAME_LEN) as u64, Ordering::Relaxed);
        Ok(n)
    }

    /// Swap in a WAV/PCM file as the frame source. Returns the file's
    /// sample count and native sample rate (informational -- this crate
    /// does not resample; a file whose rate differs from the open
    /// device's plays back at the wrong pitch/speed, see `src/source.rs`).
    pub fn play_wav(
        &self,
        path: &std::path::Path,
        looping: bool,
    ) -> Result<(usize, u32), WavError> {
        let mut wav = WavSource::load(path)?;
        wav.looping = looping;
        let info = (wav.total_samples(), wav.sample_rate);
        *self.source.lock().expect("live-audio source lock poisoned") = Box::new(wav);
        Ok(info)
    }

    /// Swap in a sine test tone as the frame source.
    pub fn play_tone(&self, freq_hz: f32, amplitude: f32) {
        let tone = ToneSource::new(self.sample_rate() as f32, freq_hz, amplitude);
        *self.source.lock().expect("live-audio source lock poisoned") = Box::new(tone);
    }

    /// Queue one SPI2 TX frame (wire order) for the frame source (the
    /// emulator's DSPI2 peer calls this for every frame the ColdFire sends;
    /// see `src/repeater.rs`).
    pub fn push_frame(&self, frame: &[u8]) {
        self.queue.write(frame);
    }

    /// The frame queue's counters.
    pub fn frame_stats(&self) -> QueueStats {
        self.queue.stats()
    }

    fn sample_rate(&self) -> u32 {
        self.device
            .as_ref()
            .map_or(TARGET_SAMPLE_RATE, |d| d.sample_rate)
    }

    pub fn stats(&self) -> Stats {
        let d = self.device.as_ref();
        Stats {
            sample_rate: self.sample_rate(),
            requested_sample_rate: TARGET_SAMPLE_RATE,
            channels: d.map_or(2, |d| d.channels),
            used_fallback: d.is_some_and(|d| d.used_fallback),
            target_latency_frames: self.target_latency_frames,
            ring_capacity_frames: self.ring.capacity() as u32,
            ring_fill_frames: self.ring.len() as u32,
            underruns: self.underruns.load(Ordering::Relaxed),
            frames_rendered: self.frames_rendered.load(Ordering::Relaxed),
            max_callback_frames: self.callback_frames.load(Ordering::Relaxed),
        }
    }

    /// Keep the render log of the source this player was opened with
    /// (`sharc_source::CaptureSource::log`), for `render_log`.
    pub fn attach_render_log(&mut self, log: Arc<Mutex<RenderLog>>) {
        self.render_log = Some(log);
    }

    pub fn render_log(&self) -> Option<&Arc<Mutex<RenderLog>>> {
        self.render_log.as_ref()
    }

    pub fn device_name(&self) -> &str {
        self.device.as_ref().map_or("offline", |d| &d.device_name)
    }
}

impl Drop for LivePlayer {
    fn drop(&mut self) {
        if let Some(producer) = self.producer.as_mut() {
            producer.stop();
        }
        // `self.device.stream` (a `cpal::Stream`) stops output on drop.
    }
}
