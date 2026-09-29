//! A lock-free single-producer/single-consumer ring buffer of stereo
//! samples.
//!
//! One producer thread (paced by [`SpscRing::room`], not wall-clock sleep --
//! see `src/producer.rs`) pushes rendered audio; one consumer -- the cpal
//! real-time callback (`src/audio.rs`) -- pops it. The callback must not
//! allocate or lock, so this type never does either on the hot path: the
//! backing store is a fixed-size, pre-allocated slice, and `head`/`tail`
//! are plain atomics with acquire/release ordering (the standard SPSC
//! pattern: only the producer writes `tail`, only the consumer writes
//! `head`, and the data slot at index `i` is only touched by the producer
//! until it publishes `tail = i+1`, and only by the consumer after it reads
//! that publish and before it publishes `head = i+1`).

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicUsize, Ordering};

/// One interleaved stereo sample, already converted to `f32` (the producer
/// converts Q31 -> f32, never the callback -- see `src/q31.rs`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StereoSample {
    pub l: f32,
    pub r: f32,
}

/// Stereo samples per SHARC audio frame (ring A half, `tools/sharc_dac.py`
/// `SAMPLES_PER_HALF`).
pub const FRAME_LEN: usize = 32;

/// Smallest power of two that is `>= n` (capacity must be a power of two so
/// the index mask `capacity - 1` replaces a modulo on the hot path).
fn next_pow2(n: usize) -> usize {
    let mut p = 1usize;
    while p < n {
        p <<= 1;
    }
    p
}

pub struct SpscRing {
    data: Box<[UnsafeCell<StereoSample>]>,
    mask: usize,
    head: AtomicUsize, // next slot to read; owned by the consumer
    tail: AtomicUsize, // next slot to write; owned by the producer
}

// SAFETY: `data` is only ever accessed through `head`/`tail`-gated slots,
// each touched by exactly one side at a time (see module docs). The type is
// Send+Sync so one `Arc<SpscRing>` can be shared between the producer
// thread and the audio callback.
unsafe impl Send for SpscRing {}
unsafe impl Sync for SpscRing {}

impl SpscRing {
    /// A ring that holds at least `min_capacity_frames` stereo samples
    /// (rounded up to a power of two).
    pub fn with_capacity(min_capacity_frames: usize) -> Self {
        let capacity = next_pow2(min_capacity_frames.max(1));
        let mut data = Vec::with_capacity(capacity);
        for _ in 0..capacity {
            data.push(UnsafeCell::new(StereoSample::default()));
        }
        SpscRing {
            data: data.into_boxed_slice(),
            mask: capacity - 1,
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    pub fn capacity(&self) -> usize {
        self.mask + 1
    }

    /// Samples currently queued (consumer side; a lower bound if the
    /// producer is pushing concurrently, which is fine -- the caller only
    /// uses this to decide whether to wait, never to index directly).
    pub fn len(&self) -> usize {
        let tail = self.tail.load(Ordering::Acquire);
        let head = self.head.load(Ordering::Acquire);
        tail.wrapping_sub(head)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Free slots (producer side; a lower bound if the consumer is popping
    /// concurrently, which only ever means more room than reported).
    pub fn room(&self) -> usize {
        self.capacity() - self.len()
    }

    /// Producer only. Pushes as many of `samples` as fit; returns the
    /// number actually pushed (the caller decides what to do with a short
    /// write -- the producer thread should have checked `room()` first and
    /// this should not happen in steady state).
    pub fn push_slice(&self, samples: &[StereoSample]) -> usize {
        let mut tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        let room = self.capacity() - tail.wrapping_sub(head);
        let n = samples.len().min(room);
        for s in &samples[..n] {
            let idx = tail & self.mask;
            // SAFETY: this slot is not readable by the consumer until the
            // `tail` store below publishes it (Release), and no other
            // producer call can be in flight (single producer).
            unsafe { *self.data[idx].get() = *s };
            tail = tail.wrapping_add(1);
        }
        self.tail.store(tail, Ordering::Release);
        n
    }

    /// Consumer only (the real-time callback). Pops up to `out.len()`
    /// samples into `out`; returns the number popped. Does not allocate or
    /// lock. Any slots in `out` beyond the returned count are left
    /// untouched -- the caller (the audio callback) fills those with
    /// silence and counts an underrun.
    pub fn pop_into(&self, out: &mut [StereoSample]) -> usize {
        let mut head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        let available = tail.wrapping_sub(head);
        let n = out.len().min(available);
        for slot in out.iter_mut().take(n) {
            let idx = head & self.mask;
            // SAFETY: the `tail` load above (Acquire) synchronizes with the
            // producer's `tail` store (Release), so every slot up to `tail`
            // is visible here; no other consumer call can be in flight.
            *slot = unsafe { *self.data[idx].get() };
            head = head.wrapping_add(1);
        }
        self.head.store(head, Ordering::Release);
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_pow2_rounds_up() {
        assert_eq!(next_pow2(1), 1);
        assert_eq!(next_pow2(2), 2);
        assert_eq!(next_pow2(3), 4);
        assert_eq!(next_pow2(64), 64);
        assert_eq!(next_pow2(65), 128);
    }

    #[test]
    fn starts_empty_with_full_room() {
        let ring = SpscRing::with_capacity(100);
        assert_eq!(ring.capacity(), 128);
        assert_eq!(ring.len(), 0);
        assert_eq!(ring.room(), 128);
        assert!(ring.is_empty());
    }

    #[test]
    fn push_then_pop_round_trips_samples() {
        let ring = SpscRing::with_capacity(8);
        let samples: Vec<StereoSample> = (0..8)
            .map(|i| StereoSample {
                l: i as f32,
                r: -(i as f32),
            })
            .collect();
        let pushed = ring.push_slice(&samples);
        assert_eq!(pushed, 8);
        assert_eq!(ring.len(), 8);
        assert_eq!(ring.room(), 0);

        let mut out = vec![StereoSample::default(); 8];
        let popped = ring.pop_into(&mut out);
        assert_eq!(popped, 8);
        assert_eq!(out, samples);
        assert!(ring.is_empty());
    }

    #[test]
    fn push_stops_at_capacity() {
        let ring = SpscRing::with_capacity(4);
        let samples = vec![StereoSample { l: 1.0, r: 1.0 }; 10];
        let pushed = ring.push_slice(&samples);
        assert_eq!(pushed, 4);
        assert_eq!(ring.room(), 0);
    }

    #[test]
    fn pop_short_when_ring_has_less_than_requested() {
        let ring = SpscRing::with_capacity(8);
        let samples = vec![StereoSample { l: 2.0, r: 3.0 }; 3];
        ring.push_slice(&samples);

        let mut out = vec![StereoSample::default(); 5];
        let popped = ring.pop_into(&mut out);
        assert_eq!(popped, 3);
        // Beyond `popped`, `out` is untouched (still default) -- the
        // caller (audio callback) is responsible for silence-filling and
        // counting the underrun.
        assert_eq!(out[3], StereoSample::default());
        assert_eq!(out[4], StereoSample::default());
    }

    #[test]
    fn wraps_around_correctly() {
        let ring = SpscRing::with_capacity(4);
        for round in 0..5 {
            let samples = vec![
                StereoSample {
                    l: round as f32,
                    r: round as f32,
                };
                3
            ];
            let pushed = ring.push_slice(&samples);
            assert_eq!(pushed, 3);
            let mut out = vec![StereoSample::default(); 3];
            let popped = ring.pop_into(&mut out);
            assert_eq!(popped, 3);
            assert_eq!(out[0].l, round as f32);
        }
    }

    #[test]
    fn concurrent_producer_consumer_preserves_all_samples() {
        use std::sync::Arc;
        use std::thread;

        let ring = Arc::new(SpscRing::with_capacity(64));
        let total_frames = 20_000usize;

        let producer_ring = Arc::clone(&ring);
        let producer = thread::spawn(move || {
            let mut pushed_total = 0usize;
            let mut next = 0u32;
            while pushed_total < total_frames {
                let chunk: Vec<StereoSample> = (0..FRAME_LEN)
                    .map(|_| {
                        let s = StereoSample {
                            l: next as f32,
                            r: next as f32,
                        };
                        next = next.wrapping_add(1);
                        s
                    })
                    .collect();
                let mut off = 0;
                while off < chunk.len() {
                    let n = producer_ring.push_slice(&chunk[off..]);
                    off += n;
                    if n == 0 {
                        thread::yield_now();
                    }
                }
                pushed_total += FRAME_LEN;
            }
        });

        let consumer_ring = Arc::clone(&ring);
        let consumer = thread::spawn(move || {
            let mut received = Vec::with_capacity(total_frames);
            while received.len() < total_frames {
                let mut buf = [StereoSample::default(); 7];
                let n = consumer_ring.pop_into(&mut buf);
                received.extend_from_slice(&buf[..n]);
                if n == 0 {
                    thread::yield_now();
                }
            }
            received
        });

        producer.join().unwrap();
        let received = consumer.join().unwrap();
        assert_eq!(received.len(), total_frames);
        for (i, s) in received.iter().enumerate() {
            assert_eq!(s.l, i as f32, "sample {i} out of order or lost");
        }
    }
}
