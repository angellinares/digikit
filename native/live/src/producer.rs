//! The producer thread: renders frames from a [`FrameSource`] and pushes
//! them into the ring, paced by the ring's own fill level rather than a
//! wall-clock sleep.
//!
//! Steady state: the real-time callback (`src/audio.rs`) pops samples and
//! then unparks this thread; this thread renders more only when there is
//! room, and parks (not sleeps against a clock) when there is not. A short
//! `park_timeout` is a safety net only, in case an unpark is ever missed
//! (e.g. the very first frame, before the callback has run once) -- it is
//! not how pacing normally happens.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::repeater::FrameQueue;
use crate::ring::{FRAME_LEN, SpscRing, StereoSample};
use crate::source::FrameSource;

/// The producer's current frame source, swappable at any time (e.g. the
/// Python wrapper's `play(...)` after `open()`). Locked once per rendered
/// frame (~1500/s) on the producer thread only -- never on the real-time
/// audio callback, so this is not subject to the ring's lock-free
/// requirement.
pub type SharedSource = Arc<Mutex<Box<dyn FrameSource>>>;

/// Safety-net park timeout: normal operation is woken by the audio
/// callback's `unpark()`, never by this timeout elapsing.
const PARK_SAFETY_NET: Duration = Duration::from_millis(20);

/// The producer's time-constraint policy, ns: (period, computation,
/// constraint); all 0 leaves the thread timeshare. `LIVE_RT=P,C,K` (µs)
/// overrides it, `LIVE_RT=0` turns it off.
fn rt_policy() -> (u64, u64, u64) {
    const DEFAULT: (u64, u64, u64) = (
        RT_PERIOD_US * 1000,
        RT_COMPUTATION_US * 1000,
        RT_CONSTRAINT_US * 1000,
    );
    let Ok(v) = std::env::var("LIVE_RT") else {
        return DEFAULT;
    };
    let f: Vec<u64> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
    match f.as_slice() {
        [p, c, k] => (p * 1000, c * 1000, k * 1000),
        _ => (0, 0, 0),
    }
}

// The producer's own rhythm: the device takes up to 512 frames per
// callback (10.7-11.6 ms at 48/44.1 kHz) and the producer then renders
// ~16 SHARC frames (~8 ms at ~0.46 ms each). Measured with 20 busy
// processes on 16 cores (scratchpad/live-sharc.md): timeshare only, 346
// underruns in 8 s (p99 render 8.9 ms); with this policy none (p99 0.55 ms).
const RT_PERIOD_US: u64 = 11_600;
const RT_COMPUTATION_US: u64 = 8_000;
const RT_CONSTRAINT_US: u64 = 11_600;

/// One frame: the queue's next TX frame (empty before the first write)
/// rendered by SOURCE into OUT. INPUT is the caller's reusable buffer.
pub fn render_one(
    source: &SharedSource,
    queue: &FrameQueue,
    input: &mut Vec<u8>,
    out: &mut [StereoSample; FRAME_LEN],
) {
    queue.take_into(input);
    source
        .lock()
        .expect("live-audio source lock poisoned")
        .render_frame(input, out);
}

pub struct Producer {
    handle: Option<JoinHandle<()>>,
    running: Arc<AtomicBool>,
    thread: thread::Thread,
}

impl Producer {
    /// Spawns the producer thread. `source` renders each frame; `queue`
    /// is read once per frame for the next SPI2 TX frame (repeater
    /// semantics, `src/repeater.rs`); `ring` is where rendered audio goes;
    /// `frames_rendered` is a shared counter the caller can read for
    /// stats.
    pub fn spawn(
        source: SharedSource,
        queue: Arc<FrameQueue>,
        ring: Arc<SpscRing>,
        frames_rendered: Arc<AtomicU64>,
    ) -> Self {
        let running = Arc::new(AtomicBool::new(true));
        let running_thread = Arc::clone(&running);

        let handle = thread::Builder::new()
            .name("live-audio-producer".to_string())
            .spawn(move || {
                // Rendering is on the audio deadline: ask the scheduler for
                // the interactive class (performance cores, ahead of
                // default-class work). A no-op off macOS.
                crate::priority::raise_current_thread();
                let (period, computation, constraint) = rt_policy();
                if period > 0 || computation > 0 {
                    crate::priority::realtime_current_thread(period, computation, constraint);
                }
                let mut scratch = [StereoSample::default(); FRAME_LEN];
                let mut input = Vec::with_capacity(crate::repeater::FRAME_BYTES);
                while running_thread.load(Ordering::Acquire) {
                    if ring.room() >= FRAME_LEN {
                        render_one(&source, &queue, &mut input, &mut scratch);
                        let pushed = ring.push_slice(&scratch);
                        debug_assert_eq!(pushed, FRAME_LEN);
                        frames_rendered.fetch_add(FRAME_LEN as u64, Ordering::Relaxed);
                    } else {
                        thread::park_timeout(PARK_SAFETY_NET);
                    }
                }
            })
            .expect("failed to spawn live-audio producer thread");

        let thread_handle = handle.thread().clone();
        Producer {
            handle: Some(handle),
            running,
            thread: thread_handle,
        }
    }

    /// A cloneable handle the audio callback uses to wake this thread
    /// after popping (see module docs).
    pub fn thread_handle(&self) -> thread::Thread {
        self.thread.clone()
    }

    pub fn stop(&mut self) {
        self.running.store(false, Ordering::Release);
        self.thread.unpark();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for Producer {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::ToneSource;
    use std::sync::atomic::AtomicU64;
    use std::time::Instant;

    fn tone_source() -> SharedSource {
        Arc::new(Mutex::new(
            Box::new(ToneSource::new(48_000.0, 440.0, 0.5)) as Box<dyn FrameSource>
        ))
    }

    #[test]
    fn producer_fills_the_ring_and_stops_at_capacity() {
        let ring = Arc::new(SpscRing::with_capacity(256));
        let queue = Arc::new(FrameQueue::new());
        let frames_rendered = Arc::new(AtomicU64::new(0));
        let source = tone_source();

        let mut producer = Producer::spawn(
            source,
            queue,
            Arc::clone(&ring),
            Arc::clone(&frames_rendered),
        );

        let deadline = Instant::now() + Duration::from_secs(2);
        while ring.room() > 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(ring.room(), 0, "producer should have filled the ring");
        assert!(frames_rendered.load(Ordering::Relaxed) >= ring.capacity() as u64);

        producer.stop();
    }

    #[test]
    fn unpark_wakes_the_producer_to_refill() {
        let ring = Arc::new(SpscRing::with_capacity(64));
        let queue = Arc::new(FrameQueue::new());
        let frames_rendered = Arc::new(AtomicU64::new(0));
        let source = tone_source();

        let mut producer = Producer::spawn(
            source,
            queue,
            Arc::clone(&ring),
            Arc::clone(&frames_rendered),
        );
        let handle = producer.thread_handle();

        let deadline = Instant::now() + Duration::from_secs(2);
        while ring.room() > 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(ring.room(), 0);

        // Simulate the audio callback popping a frame, then waking the
        // producer -- it should refill promptly, not wait for the safety
        // net timeout.
        let mut out = [StereoSample::default(); FRAME_LEN];
        let popped = ring.pop_into(&mut out);
        assert_eq!(popped, FRAME_LEN);
        handle.unpark();

        let refill_deadline = Instant::now() + Duration::from_millis(200);
        while ring.room() != 0 && Instant::now() < refill_deadline {
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            ring.room(),
            0,
            "producer should refill promptly after unpark"
        );

        producer.stop();
    }
}
