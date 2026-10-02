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
    sync::{Arc, atomic::AtomicU64, mpsc},
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
}

/// The default cpal output device through `live_audio::audio`.
#[derive(Default)]
pub struct CpalOpener {
    pending: Option<audio::PendingDevice>,
    underruns: Arc<AtomicU64>,
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
        let opened = p.start(
            ring,
            self.underruns.clone(),
            Arc::new(AtomicU64::new(0)),
            wake,
        )?;
        Ok((
            opened.sample_rate,
            opened.device_name.clone(),
            Box::new(opened),
        ))
    }
}

pub struct PcmPlayer {
    tx: Option<mpsc::Sender<Vec<f32>>>,
    worker: Option<thread::JoinHandle<()>>,
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
        let worker = thread::Builder::new()
            .name("pcm-feeder".into())
            .spawn(move || feeder_thread(make(), mode, rx, info_tx))
            .map_err(|e| format!("spawn feeder: {e}"))?;
        match info_rx.recv() {
            Ok(Ok((device_rate, device))) => Ok(PcmPlayer {
                tx: Some(tx),
                worker: Some(worker),
                device,
                device_rate,
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

    /// No more audio: play what is queued (even below the buffer threshold)
    /// and return when it has been played out.
    pub fn finish(mut self) {
        self.tx = None;
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

fn feeder_thread<O: Opener>(
    mut opener: O,
    mode: Mode,
    rx: mpsc::Receiver<Vec<f32>>,
    info_tx: mpsc::Sender<Result<(u32, String), String>>,
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
    loop {
        // Intake: wait briefly for audio so the loop also services the ring.
        match rx.recv_timeout(Duration::from_millis(5)) {
            Ok(v) => {
                feeder.accept(&v);
                while let Ok(v) = rx.try_recv() {
                    feeder.accept(&v);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => done = true,
        }
        if stream.is_none() && feeder.may_start(done) {
            match opener.start(ring.clone(), thread::current()) {
                Ok(s) => stream = Some(s),
                Err(e) => {
                    eprintln!("pcm_play: cannot start output: {e}");
                    return;
                }
            }
        }
        feeder.pump(done);
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
        thread::sleep(Duration::from_millis(10));
    }
    thread::sleep(Duration::from_millis(100));
    drop(stream);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

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
}
