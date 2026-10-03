//! Opt-in playback of emulator PCM through `native/live`'s output path
//! (cargo feature `play`): a bounded lock-free SPSC ring read by the cpal
//! real-time callback (`live_audio::audio`).
//!
//! The emulator runs far slower than real time, so playing the PCM as it is
//! produced is mostly silence. Two modes:
//! - [`Mode::Live`]: open the device at once; whatever the emulator has made
//!   is played, and an empty ring plays silence (an underrun, counted).
//! - [`Mode::Buffer`]: hold the PCM back until N seconds have accumulated
//!   (or the producer is done), then play it in real time.
//!
//! Nothing here blocks the producer: [`PcmPlayer::push`] hands the samples to
//! a feeder thread through an unbounded channel. The feeder resamples
//! (linear) to the device rate and fills the ring; the callback itself only
//! pops. The device is opened on the feeder thread, and failure to open it
//! is reported by [`PcmPlayer::spawn`] without affecting the emulator.

use std::{
    any::Any,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use live_audio::{SpscRing, StereoSample, audio};

/// Source rate of the SHARC SPORT audio.
pub const SOURCE_RATE: u32 = 48_000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    Live,
    /// Start once this many seconds of audio are queued (or at the end).
    Buffer(f64),
}

/// Queue + linear resampler between the producer and the ring. Pure logic
/// (no threads, no device) so it can be tested headless.
pub struct Feeder {
    ring: Arc<SpscRing>,
    /// Source samples not yet fully consumed; `pos` indexes into it.
    buf: Vec<StereoSample>,
    pos: f64,
    /// Source samples per output sample.
    step: f64,
    /// Samples (at the source rate) queued before playback may start.
    start_at: usize,
    started: bool,
}

impl Feeder {
    pub fn new(ring: Arc<SpscRing>, device_rate: u32, mode: Mode) -> Self {
        let start_at = match mode {
            Mode::Live => 0,
            Mode::Buffer(s) => (s * SOURCE_RATE as f64) as usize,
        };
        Feeder {
            ring,
            buf: Vec::new(),
            pos: 0.0,
            step: SOURCE_RATE as f64 / device_rate as f64,
            start_at,
            started: false,
        }
    }

    /// Queue interleaved L/R f32 samples.
    pub fn accept(&mut self, interleaved: &[f32]) {
        self.buf.extend(
            interleaved
                .chunks_exact(2)
                .map(|c| StereoSample { l: c[0], r: c[1] }),
        );
    }

    /// Source samples waiting.
    pub fn queued(&self) -> usize {
        self.buf.len()
    }

    pub fn started(&self) -> bool {
        self.started
    }

    /// True once playback may begin: the start threshold is met, or the
    /// producer is `done` and anything at all is queued.
    pub fn may_start(&self, done: bool) -> bool {
        self.started || self.buf.len() >= self.start_at.max(1) || (done && !self.buf.is_empty())
    }

    /// Move as much as fits into the ring; never blocks. Returns the number
    /// of device-rate samples pushed. Does nothing before `may_start`.
    pub fn pump(&mut self, done: bool) -> usize {
        if !self.may_start(done) {
            return 0;
        }
        self.started = true;
        let mut out = Vec::new();
        let room = self.ring.room();
        while out.len() < room {
            let i = self.pos as usize;
            if i + 1 >= self.buf.len() {
                break;
            }
            let f = (self.pos - i as f64) as f32;
            let (a, b) = (self.buf[i], self.buf[i + 1]);
            out.push(StereoSample {
                l: a.l + (b.l - a.l) * f,
                r: a.r + (b.r - a.r) * f,
            });
            self.pos += self.step;
        }
        // Drop consumed samples, keeping the one the next output starts at.
        let keep = (self.pos as usize).min(self.buf.len());
        if keep > 0 {
            self.buf.drain(..keep);
            self.pos -= keep as f64;
        }
        let n = self.ring.push_slice(&out);
        debug_assert_eq!(n, out.len());
        n
    }

    /// Everything queued has reached the ring (the last source sample stays
    /// as interpolation context).
    pub fn drained(&self) -> bool {
        self.buf.len() <= 1
    }
}

/// What a device opener returns: the device rate, a label and a keep-alive
/// (the stream; dropping it stops playback).
pub type Opened = (u32, String, Box<dyn Any>);

/// Opens the output for a given ring. Runs on the feeder thread, twice at
/// most: [`Opener::probe`] before any audio exists, [`Opener::start`] when
/// playback begins.
pub trait Opener {
    /// Device rate and name, without starting a stream.
    fn probe(&mut self) -> Result<(u32, String), String>;
    fn start(&mut self, ring: Arc<SpscRing>, wake: thread::Thread) -> Result<Opened, String>;
    /// Underrun callbacks and maximum callback size (not a rendered-frame count).
    fn counters(&self) -> (u64, u64) {
        (0, 0)
    }
    fn error(&self) -> Option<String> {
        None
    }
}

/// The default cpal output device through `live_audio::audio`.
#[derive(Default)]
pub struct CpalOpener {
    pending: Option<audio::PendingDevice>,
    underruns: Arc<AtomicU64>,
    callback_max_frames: Arc<AtomicU64>,
    stream_errors: audio::StreamErrors,
}

impl Opener for CpalOpener {
    fn probe(&mut self) -> Result<(u32, String), String> {
        let p = audio::default_output()?;
        let info = (p.sample_rate, p.device_name.clone());
        self.pending = Some(p);
        Ok(info)
    }
    fn start(&mut self, ring: Arc<SpscRing>, wake: thread::Thread) -> Result<Opened, String> {
        let p = self.pending.take().ok_or("device not probed")?;
        let opened = p.start_with_errors(
            ring,
            self.underruns.clone(),
            self.callback_max_frames.clone(),
            wake,
            self.stream_errors.clone(),
        )?;
        Ok((
            opened.sample_rate,
            opened.device_name.clone(),
            Box::new(opened),
        ))
    }
    fn counters(&self) -> (u64, u64) {
        (
            self.underruns.load(Ordering::Relaxed),
            self.callback_max_frames.load(Ordering::Relaxed),
        )
    }
    fn error(&self) -> Option<String> {
        self.stream_errors.error()
    }
}

/// Feeder snapshots; no mutex or observation work runs in the audio callback.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct PlaybackStats {
    pub stream_started: bool,
    pub source_received_frames: u64,
    pub feeder_pending_source_frames: u64,
    pub queued_device_frames: u64,
    pub underrun_events: u64,
    pub callback_max_frames: u64,
    pub error: Option<String>,
}

pub struct PcmPlayer {
    tx: Option<mpsc::Sender<Vec<f32>>>,
    worker: Option<thread::JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    stats: Arc<Mutex<PlaybackStats>>,
    pub device: String,
    pub device_rate: u32,
}

impl PcmPlayer {
    /// Start playback on the default output device. `Err` (no device, no
    /// usable config) leaves nothing running.
    pub fn spawn(mode: Mode) -> Result<Self, String> {
        Self::spawn_with(CpalOpener::default, mode)
    }

    /// `make` builds the opener on the feeder thread (device handles need
    /// not be `Send`).
    pub fn spawn_with<O, F>(make: F, mode: Mode) -> Result<Self, String>
    where
        O: Opener,
        F: FnOnce() -> O + Send + 'static,
    {
        let (tx, rx) = mpsc::channel::<Vec<f32>>();
        let (info_tx, info_rx) = mpsc::channel::<Result<(u32, String), String>>();
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(Mutex::new(PlaybackStats::default()));
        let worker_stop = stop.clone();
        let worker_stats = stats.clone();
        let worker = thread::Builder::new()
            .name("pcm-feeder".into())
            .spawn(move || feeder_thread(make(), mode, rx, info_tx, worker_stop, worker_stats))
            .map_err(|e| format!("spawn feeder: {e}"))?;
        match info_rx.recv() {
            Ok(Ok((device_rate, device))) => Ok(PcmPlayer {
                tx: Some(tx),
                worker: Some(worker),
                device,
                device_rate,
                stop,
                stats,
            }),
            Ok(Err(e)) => {
                let _ = worker.join();
                Err(e)
            }
            Err(_) => Err("feeder thread died".into()),
        }
    }

    /// Queue interleaved L/R samples at 48 kHz. Never blocks.
    pub fn push(&self, interleaved: &[f32]) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(interleaved.to_vec());
        }
    }

    /// Snapshot feeder/device observations. Queues exclude channel messages not yet received.
    pub fn stats(&self) -> PlaybackStats {
        let mut stats = self.stats.lock().expect("playback stats mutex").clone();
        if self
            .worker
            .as_ref()
            .is_some_and(thread::JoinHandle::is_finished)
            && stats.error.is_none()
        {
            stats.error = Some("feeder thread stopped".into());
        }
        stats
    }

    /// No more audio: play what is queued (even below the buffer threshold)
    /// and return when it has been played out.
    pub fn finish(mut self) {
        self.tx = None;
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

/// Dropping a session cancels immediately rather than draining its queued audio.
impl Drop for PcmPlayer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.tx = None;
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
    }
}

fn feeder_thread<O: Opener>(
    mut opener: O,
    mode: Mode,
    rx: mpsc::Receiver<Vec<f32>>,
    info_tx: mpsc::Sender<Result<(u32, String), String>>,
    stop: Arc<AtomicBool>,
    stats: Arc<Mutex<PlaybackStats>>,
) {
    let (rate, _name) = match opener.probe() {
        Ok(i) => {
            let _ = info_tx.send(Ok(i.clone()));
            i
        }
        Err(e) => {
            let _ = info_tx.send(Err(e));
            return;
        }
    };
    // One second of device-rate audio (rounded up to a power of two).
    let ring = Arc::new(SpscRing::with_capacity(rate as usize));
    let mut feeder = Feeder::new(ring.clone(), rate, mode);
    let mut stream: Option<Opened> = None;
    let mut done = false;
    let mut received = 0u64;
    loop {
        if stop.load(Ordering::Acquire) {
            return;
        }
        // Intake: wait briefly for audio so the loop also services the ring.
        match rx.recv_timeout(Duration::from_millis(5)) {
            Ok(v) => {
                received += (v.len() / 2) as u64;
                feeder.accept(&v);
                while let Ok(v) = rx.try_recv() {
                    if stop.load(Ordering::Acquire) {
                        return;
                    }
                    received += (v.len() / 2) as u64;
                    feeder.accept(&v);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => done = true,
        }
        if stop.load(Ordering::Acquire) {
            return;
        }
        if stream.is_none() && feeder.may_start(done) {
            match opener.start(ring.clone(), thread::current()) {
                Ok(s) => stream = Some(s),
                Err(e) => {
                    stats.lock().expect("playback stats mutex").error = Some(e.clone());
                    eprintln!("pcm_play: cannot start output: {e}");
                    return;
                }
            }
        }
        feeder.pump(done);
        if let Some(error) = opener.error() {
            stats.lock().expect("playback stats mutex").error = Some(error);
            return;
        }
        let (underrun_events, callback_max_frames) = opener.counters();
        *stats.lock().expect("playback stats mutex") = PlaybackStats {
            stream_started: stream.is_some(),
            source_received_frames: received,
            feeder_pending_source_frames: feeder.queued().saturating_sub(1) as u64,
            queued_device_frames: ring.len() as u64,
            underrun_events,
            callback_max_frames,
            error: None,
        };
        if done && feeder.drained() {
            break;
        }
        if done {
            // Ring full and the callback consumes it: wait for room.
            thread::park_timeout(Duration::from_millis(5));
        }
    }
    // Let the device play the ring out (a ring is at most 1 s).
    while !ring.is_empty() {
        if stop.load(Ordering::Acquire) {
            return;
        }
        if let Some(error) = opener.error() {
            stats.lock().expect("playback stats mutex").error = Some(error);
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    for _ in 0..10 {
        if stop.load(Ordering::Acquire) {
            return;
        }
        if let Some(error) = opener.error() {
            stats.lock().expect("playback stats mutex").error = Some(error);
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    drop(stream);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(n: usize) -> Vec<f32> {
        (0..n).flat_map(|i| [i as f32, -(i as f32)]).collect()
    }

    fn drain(ring: &SpscRing) -> Vec<StereoSample> {
        let mut v = vec![StereoSample::default(); ring.len()];
        let n = ring.pop_into(&mut v);
        v.truncate(n);
        v
    }

    #[test]
    fn buffer_mode_waits_for_threshold_then_streams_in_order() {
        let ring = Arc::new(SpscRing::with_capacity(1024));
        let mut f = Feeder::new(ring.clone(), 48_000, Mode::Buffer(0.01)); // 480 samples
        f.accept(&ramp(400));
        assert!(!f.may_start(false));
        assert_eq!(f.pump(false), 0);
        assert!(ring.is_empty());
        f.accept(&ramp(400)[..200]);
        assert!(f.may_start(false));
        // 500 queued, the last one is interpolation context only.
        assert_eq!(f.pump(false), 499);
        let got = drain(&ring);
        assert_eq!(got.len(), 499);
        assert_eq!(got[0], StereoSample { l: 0.0, r: -0.0 });
        assert_eq!(got[10], StereoSample { l: 10.0, r: -10.0 });
    }

    #[test]
    fn never_exceeds_ring_room_and_keeps_the_rest() {
        let ring = Arc::new(SpscRing::with_capacity(64));
        let mut f = Feeder::new(ring.clone(), 48_000, Mode::Live);
        f.accept(&ramp(300));
        assert_eq!(f.pump(false), 64);
        assert_eq!(f.pump(false), 0); // full: no block, no loss
        let a = drain(&ring);
        assert_eq!(a.last().unwrap().l, 63.0);
        assert_eq!(f.pump(false), 64);
        assert_eq!(drain(&ring)[0].l, 64.0);
    }

    #[test]
    fn short_run_plays_at_the_end_and_resamples() {
        let ring = Arc::new(SpscRing::with_capacity(4096));
        let mut f = Feeder::new(ring.clone(), 24_000, Mode::Buffer(60.0));
        f.accept(&ramp(101));
        assert_eq!(f.pump(false), 0);
        assert!(f.may_start(true));
        let n = f.pump(true);
        assert_eq!(n, 50); // 48k -> 24k halves, last sample is context
        let got = drain(&ring);
        assert_eq!(got[3].l, 6.0);
        assert!(f.drained() || f.queued() <= 2);
    }

    struct FakeOpener {
        rate: u32,
        started: Arc<Mutex<Option<Arc<SpscRing>>>>,
    }
    impl Opener for FakeOpener {
        fn probe(&mut self) -> Result<(u32, String), String> {
            Ok((self.rate, "fake".into()))
        }
        fn start(&mut self, ring: Arc<SpscRing>, _wake: thread::Thread) -> Result<Opened, String> {
            *self.started.lock().unwrap() = Some(ring);
            Ok((self.rate, "fake".into(), Box::new(())))
        }
    }

    struct NoDevice;
    impl Opener for NoDevice {
        fn probe(&mut self) -> Result<(u32, String), String> {
            Err("no default output device".into())
        }
        fn start(&mut self, _: Arc<SpscRing>, _: thread::Thread) -> Result<Opened, String> {
            unreachable!()
        }
    }

    #[test]
    fn player_thread_end_to_end_headless() {
        let slot = Arc::new(Mutex::new(None));
        let p = PcmPlayer::spawn_with(
            {
                let slot = slot.clone();
                move || FakeOpener {
                    rate: 48_000,
                    started: slot,
                }
            },
            Mode::Buffer(0.02), // 960 samples
        )
        .unwrap();
        assert_eq!((p.device.as_str(), p.device_rate), ("fake", 48_000));
        // 500 samples: below the threshold, the device must not start.
        p.push(&ramp(500));
        thread::sleep(Duration::from_millis(60));
        assert!(slot.lock().unwrap().is_none());
        p.push(&ramp(2000)[..1000]);
        // The fake device consumes like the callback would.
        let consumer = {
            let slot = slot.clone();
            thread::spawn(move || {
                let mut got = Vec::new();
                let t0 = std::time::Instant::now();
                while got.len() < 999 && t0.elapsed() < Duration::from_secs(5) {
                    let ring = slot.lock().unwrap().clone();
                    if let Some(r) = ring {
                        let mut b = [StereoSample::default(); 64];
                        let n = r.pop_into(&mut b);
                        got.extend_from_slice(&b[..n]);
                    }
                    thread::sleep(Duration::from_millis(1));
                }
                got
            })
        };
        p.finish();
        let got = consumer.join().unwrap();
        assert_eq!(got.len(), 999);
        // 500 ramp samples then the first 500 again: order preserved.
        assert_eq!(got[499].l, 499.0);
        assert_eq!(got[500].l, 0.0);
    }

    #[test]
    fn missing_device_fails_gracefully() {
        let r = PcmPlayer::spawn_with(|| NoDevice, Mode::Live);
        assert_eq!(r.err().as_deref(), Some("no default output device"));
    }
    #[test]
    fn drop_cancels_a_full_fake_device_and_joins_the_feeder() {
        let slot = Arc::new(Mutex::new(None));
        let player = PcmPlayer::spawn_with(
            {
                let slot = slot.clone();
                move || FakeOpener {
                    rate: 48_000,
                    started: slot,
                }
            },
            Mode::Live,
        )
        .unwrap();
        player.push(&ramp(100_000));
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !player.stats().stream_started && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        let stats = player.stats();
        assert!(stats.stream_started);
        assert_eq!(stats.source_received_frames, 100_000);
        assert!(stats.queued_device_frames > 0);
        let start = std::time::Instant::now();
        drop(player); // The fake device never consumes: draining would hang.
        assert!(start.elapsed() < Duration::from_secs(1));
        assert_eq!(
            Arc::strong_count(&slot),
            1,
            "feeder/opener must have been dropped"
        );
    }
    #[test]
    fn drop_before_buffer_threshold_never_starts_the_device() {
        let slot = Arc::new(Mutex::new(None));
        let player = PcmPlayer::spawn_with(
            {
                let slot = slot.clone();
                move || FakeOpener {
                    rate: 48_000,
                    started: slot,
                }
            },
            Mode::Buffer(2.0),
        )
        .unwrap();
        player.push(&ramp(64));
        drop(player);
        assert!(slot.lock().unwrap().is_none());
        assert_eq!(Arc::strong_count(&slot), 1);
    }
    struct BadStart;
    impl Opener for BadStart {
        fn probe(&mut self) -> Result<(u32, String), String> {
            Ok((48_000, "fake".into()))
        }
        fn start(&mut self, _: Arc<SpscRing>, _: thread::Thread) -> Result<Opened, String> {
            Err("fake start failure".into())
        }
    }
    #[test]
    fn late_device_failure_is_observable() {
        let player = PcmPlayer::spawn_with(|| BadStart, Mode::Live).unwrap();
        player.push(&ramp(64));
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while player.stats().error.is_none() && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(player.stats().error.as_deref(), Some("fake start failure"));
    }
    struct PostStartError {
        errors: audio::StreamErrors,
    }
    impl Opener for PostStartError {
        fn probe(&mut self) -> Result<(u32, String), String> {
            Ok((48_000, "fake".into()))
        }
        fn start(&mut self, _: Arc<SpscRing>, _: thread::Thread) -> Result<Opened, String> {
            Ok((48_000, "fake".into(), Box::new(())))
        }
        fn error(&self) -> Option<String> {
            self.errors.error()
        }
    }
    #[test]
    fn error_after_successful_start_is_latched_and_finish_does_not_hang() {
        let errors = audio::StreamErrors::default();
        let player = PcmPlayer::spawn_with(
            {
                let errors = errors.clone();
                move || PostStartError { errors }
            },
            Mode::Live,
        )
        .unwrap();
        player.push(&ramp(100_000));
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !player.stats().stream_started && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(player.stats().stream_started);
        assert!(player.stats().error.is_none());
        errors.record(); // Exactly the same signal used by the CPAL error callback.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while player.stats().error.is_none() && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            player.stats().error.as_deref(),
            Some("output stream error (1 events; details in stderr)")
        );
        thread::sleep(Duration::from_millis(15));
        assert!(
            player.stats().error.is_some(),
            "error must remain latched after worker exits"
        );
        let start = std::time::Instant::now();
        player.finish(); // Fake stream never drains its ring; error must terminate it.
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}
