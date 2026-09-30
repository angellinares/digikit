//! The frame repeater: a bounded FIFO of SPI2 TX frames that repeats the
//! last frame, one-shot fields cleared, when it runs dry.
//!
//! The ColdFire side runs far slower than the 1,500 SPI2 frames/s the
//! SHARC side needs (`scratchpad/rt-coldfire.md`), and the emulator hands
//! its frames over in bursts (all the frames of one emulation chunk
//! arrive together). So:
//!
//! - `write()` appends to a FIFO, and `take_into()` (once per rendered
//!   SHARC frame, on the producer thread) pops the oldest frame. Every
//!   frame the ColdFire sent is rendered once, in order.
//! - When the FIFO is empty, `take_into()` returns the last frame again
//!   with its one-shot fields cleared, so a repeat never re-fires a trig
//!   or a release. This is the normal case: the ColdFire is 3-10x slower
//!   than real time.
//! - The FIFO is bounded (`DEFAULT_DEPTH`). A write to a full FIFO does not
//!   drop a frame's events: the new frame replaces the newest queued frame
//!   and the replaced frame's one-shot words are OR-ed into it, so no trig
//!   or release bit is lost and the order of the older frames is kept.
//!   The replaced frame's other (state) fields are superseded by the newer
//!   frame's. A trig and a release of the same track merged into one frame
//!   arrive together (the SHARC arms in the next frame and releases in the
//!   one after, `scratchpad/sharc-trig-arm.md` section 2).
//!
//! Frames are kept in wire order (big-endian halfwords, as the ColdFire
//! sends them and `emu/dspi2.py` hands them over); the one-shot offsets
//! below are wire offsets. The byte swap into the SHARC's receive ring
//! happens at the SHARC edge (`sharc_source::LiveSource`), not here.
//!
//! One-shot words (`scratchpad/sharc-trig-arm.md`, "Frame fields"; the
//! captures show 0x26 mirroring 0x22 and 0x28 mirroring 0x24, each set in
//! the one frame of its event and zero otherwise):
//!
//! | offset | field |
//! |---|---|
//! | 0x22 | trig mask, bit t = track t note-on this frame |
//! | 0x24 | release mask, bit t = note-off |
//! | 0x26 | trigs with event flag 0x200 |
//! | 0x28 | release of the 0x200 event |
//! | 0x2a | all-voice reset (0 or 0xffff) |

use std::cell::Cell;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// How the frame most recently handed to the render thread was obtained.
/// `Taken` includes a frame whose one-shot fields were merged while queued.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TakeSource {
    Taken,
    Repeat,
}

static RENDER_DIAGNOSTICS_ENABLED: AtomicBool = AtomicBool::new(false);

thread_local! {
    // `take_into()` and `FrameSource::render_frame()` run consecutively on
    // the producer (or offline render) thread. This side channel preserves
    // the queue provenance without changing the hot FrameSource API.
    static RENDER_TAKE_SOURCE: Cell<Option<TakeSource>> = const { Cell::new(None) };
}

fn set_render_take_source(source: Option<TakeSource>) {
    if RENDER_DIAGNOSTICS_ENABLED.load(Ordering::Relaxed) {
        RENDER_TAKE_SOURCE.with(|slot| slot.set(source));
    }
}

/// Enable queue provenance for an optional rendered-input diagnostic.
/// This is process-wide because queues and sources meet only at the existing
/// `FrameSource` seam; it is never enabled in normal playback.
pub fn enable_render_diagnostics() {
    RENDER_DIAGNOSTICS_ENABLED.store(true, Ordering::Relaxed);
}

/// Consume the provenance from the immediately preceding [`FrameQueue::take_into`].
/// This is intended for an optional render diagnostic only.
pub fn take_source_for_render() -> Option<TakeSource> {
    RENDER_TAKE_SOURCE.with(Cell::take)
}

/// TX byte offset of the trig mask (big-endian u16).
pub const TRIG_MASK_OFFSET: usize = 0x22;
/// TX byte offset of the release mask (big-endian u16).
pub const RELEASE_MASK_OFFSET: usize = 0x24;
/// Every one-shot halfword (see the module docs): cleared on a repeat,
/// OR-merged when a full FIFO merges two frames.
pub const ONE_SHOT_OFFSETS: [usize; 5] = [0x22, 0x24, 0x26, 0x28, 0x2a];

/// The full DSPI2 TX frame size (`emu/dspiframe.py` `FRAME_BYTES`, 0xABC =
/// 2,748 bytes). Frames of any length are accepted.
pub const FRAME_BYTES: usize = 0xABC;

/// Frames the FIFO holds before a write merges (about 43 ms of SHARC
/// frames; the ColdFire side produces tens of frames per second).
pub const DEFAULT_DEPTH: usize = 64;

/// Counters, for the GUI status line and the checks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QueueStats {
    /// Frames written.
    pub pushed: u64,
    /// Written frames that had a trig bit (0x22).
    pub trig_pushed: u64,
    /// Frames taken from the FIFO (each written frame once, unless merged).
    pub taken: u64,
    /// Taken frames that had a trig bit.
    pub trig_taken: u64,
    /// Takes that found the FIFO empty and repeated the last frame.
    pub repeats: u64,
    /// Writes merged into the newest queued frame (FIFO full).
    pub merged: u64,
    /// The deepest the FIFO has been.
    pub max_depth: u64,
    /// Frames queued now.
    pub depth: u64,
}

struct Inner {
    fifo: VecDeque<Vec<u8>>,
    /// The last frame taken, one-shot words cleared: what a repeat sends.
    last: Option<Vec<u8>>,
    stats: QueueStats,
}

pub struct FrameQueue {
    inner: Mutex<Inner>,
    depth: usize,
}

impl FrameQueue {
    pub fn new() -> Self {
        Self::with_depth(DEFAULT_DEPTH)
    }

    /// A queue holding up to DEPTH frames (at least 1).
    pub fn with_depth(depth: usize) -> Self {
        FrameQueue {
            inner: Mutex::new(Inner {
                fifo: VecDeque::with_capacity(depth.max(1)),
                last: None,
                stats: QueueStats::default(),
            }),
            depth: depth.max(1),
        }
    }

    /// Append a TX frame (wire order). Called from the emulator's thread,
    /// never from the real-time callback, so a mutex is fine here.
    pub fn write(&self, frame: &[u8]) {
        let mut g = self.inner.lock().unwrap();
        g.stats.pushed += 1;
        if mask(frame, TRIG_MASK_OFFSET) != 0 {
            g.stats.trig_pushed += 1;
        }
        if g.fifo.len() >= self.depth {
            let tail = g.fifo.back_mut().expect("a full FIFO has a tail");
            let mut merged = frame.to_vec();
            for &at in &ONE_SHOT_OFFSETS {
                if at + 2 <= merged.len() && at + 2 <= tail.len() {
                    merged[at] |= tail[at];
                    merged[at + 1] |= tail[at + 1];
                }
            }
            *tail = merged;
            g.stats.merged += 1;
        } else {
            g.fifo.push_back(frame.to_vec());
        }
        g.stats.depth = g.fifo.len() as u64;
        g.stats.max_depth = g.stats.max_depth.max(g.stats.depth);
    }

    /// The next frame into OUT (cleared and refilled, so a caller reusing
    /// one buffer does not allocate once it has grown): the oldest queued
    /// frame, else the last frame with its one-shot words cleared. False
    /// (OUT empty) when nothing was ever written.
    pub fn take_into(&self, out: &mut Vec<u8>) -> bool {
        let mut g = self.inner.lock().unwrap();
        out.clear();
        if RENDER_DIAGNOSTICS_ENABLED.load(Ordering::Relaxed) {
            set_render_take_source(None);
        }
        if let Some(mut frame) = g.fifo.pop_front() {
            out.extend_from_slice(&frame);
            g.stats.taken += 1;
            if mask(&frame, TRIG_MASK_OFFSET) != 0 {
                g.stats.trig_taken += 1;
            }
            clear_one_shot_fields(&mut frame);
            g.last = Some(frame);
            g.stats.depth = g.fifo.len() as u64;
            set_render_take_source(Some(TakeSource::Taken));
            return true;
        }
        match g.last.as_ref() {
            Some(last) => {
                out.extend_from_slice(last);
                g.stats.repeats += 1;
                set_render_take_source(Some(TakeSource::Repeat));
                true
            }
            None => false,
        }
    }

    /// `take_into` into a new buffer; None when nothing was ever written.
    pub fn take(&self) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        self.take_into(&mut out).then_some(out)
    }

    /// True once at least one frame has been written.
    pub fn has_frame(&self) -> bool {
        let g = self.inner.lock().unwrap();
        !g.fifo.is_empty() || g.last.is_some()
    }

    pub fn stats(&self) -> QueueStats {
        self.inner.lock().unwrap().stats
    }
}

impl Default for FrameQueue {
    fn default() -> Self {
        Self::new()
    }
}

/// The big-endian halfword at AT (0 past the end).
pub fn mask(frame: &[u8], at: usize) -> u16 {
    if at + 2 <= frame.len() {
        u16::from_be_bytes([frame[at], frame[at + 1]])
    } else {
        0
    }
}

/// Zero every one-shot word FRAME is long enough to hold.
pub fn clear_one_shot_fields(frame: &mut [u8]) {
    for &at in &ONE_SHOT_OFFSETS {
        if at + 2 <= frame.len() {
            frame[at] = 0;
            frame[at + 1] = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(tag: u8, trig: u16, release: u16) -> Vec<u8> {
        let mut f = vec![0u8; FRAME_BYTES];
        f[0] = tag;
        f[TRIG_MASK_OFFSET..TRIG_MASK_OFFSET + 2].copy_from_slice(&trig.to_be_bytes());
        f[RELEASE_MASK_OFFSET..RELEASE_MASK_OFFSET + 2].copy_from_slice(&release.to_be_bytes());
        f
    }

    fn trig_of(f: &[u8]) -> u16 {
        mask(f, TRIG_MASK_OFFSET)
    }

    fn release_of(f: &[u8]) -> u16 {
        mask(f, RELEASE_MASK_OFFSET)
    }

    #[test]
    fn empty_queue_has_no_frame() {
        let q = FrameQueue::new();
        assert!(!q.has_frame());
        assert!(q.take().is_none());
        let mut out = vec![1, 2, 3];
        assert!(!q.take_into(&mut out));
        assert!(out.is_empty());
    }

    #[test]
    fn a_burst_is_rendered_in_order_one_frame_each() {
        let q = FrameQueue::new();
        for tag in 1..=5 {
            q.write(&frame(tag, 0, 0));
        }
        let tags: Vec<u8> = (0..5).map(|_| q.take().unwrap()[0]).collect();
        assert_eq!(tags, vec![1, 2, 3, 4, 5]);
        let s = q.stats();
        assert_eq!((s.pushed, s.taken, s.repeats, s.merged), (5, 5, 0, 0));
        assert_eq!(s.max_depth, 5);
        assert_eq!(s.depth, 0);
    }

    #[test]
    fn a_trig_inside_a_burst_is_not_lost() {
        // The old latest-only mailbox kept frame 3 of this burst only.
        let q = FrameQueue::new();
        q.write(&frame(1, 0, 0));
        q.write(&frame(2, 0b1, 0));
        q.write(&frame(3, 0, 0));
        let trigs: Vec<u16> = (0..3).map(|_| trig_of(&q.take().unwrap())).collect();
        assert_eq!(trigs, vec![0, 1, 0]);
        assert_eq!(q.stats().trig_taken, 1);
    }

    #[test]
    fn an_empty_queue_repeats_the_last_frame_with_one_shots_cleared() {
        let q = FrameQueue::new();
        let mut f = frame(7, 0b101, 0b010);
        for &at in &ONE_SHOT_OFFSETS[2..] {
            f[at] = 0xff;
            f[at + 1] = 0xff;
        }
        f[0x94] = 0x42; // machine type: state, kept
        q.write(&f);
        let first = q.take().unwrap();
        assert_eq!(first, f);
        for _ in 0..3 {
            let again = q.take().unwrap();
            for &at in &ONE_SHOT_OFFSETS {
                assert_eq!(mask(&again, at), 0, "one-shot {at:#x} repeated");
            }
            assert_eq!(again[0], 7);
            assert_eq!(again[0x94], 0x42, "state fields survive a repeat");
        }
        assert_eq!(q.stats().repeats, 3);
    }

    #[test]
    fn a_new_frame_after_repeats_carries_its_own_trig() {
        let q = FrameQueue::new();
        q.write(&frame(1, 0b001, 0));
        q.take().unwrap();
        q.take().unwrap(); // repeat
        q.write(&frame(2, 0b010, 0));
        let f = q.take().unwrap();
        assert_eq!((f[0], trig_of(&f)), (2, 0b010));
    }

    #[test]
    fn a_full_queue_merges_into_the_newest_frame_and_keeps_every_event() {
        let q = FrameQueue::with_depth(2);
        q.write(&frame(1, 0b0001, 0));
        q.write(&frame(2, 0b0010, 0b1000));
        q.write(&frame(3, 0b0100, 0)); // merged into frame 2
        q.write(&frame(4, 0, 0b0001)); // merged into (3 + 2)
        let a = q.take().unwrap();
        let b = q.take().unwrap();
        assert_eq!((a[0], trig_of(&a), release_of(&a)), (1, 0b0001, 0));
        assert_eq!(b[0], 4, "the newest frame's state wins");
        assert_eq!(trig_of(&b), 0b0110, "the merged trigs are all kept");
        assert_eq!(release_of(&b), 0b1001);
        let s = q.stats();
        assert_eq!((s.pushed, s.taken, s.merged, s.max_depth), (4, 2, 2, 2));
        assert_eq!(s.trig_pushed, 3);
        // then the repeat of the merged frame, one-shots cleared
        let c = q.take().unwrap();
        assert_eq!((c[0], trig_of(&c), release_of(&c)), (4, 0, 0));
    }

    #[test]
    fn take_into_reuses_the_callers_buffer() {
        let q = FrameQueue::new();
        q.write(&frame(1, 0, 0));
        let mut out = Vec::with_capacity(FRAME_BYTES);
        let cap = out.capacity();
        assert!(q.take_into(&mut out));
        assert!(q.take_into(&mut out));
        assert_eq!(out.len(), FRAME_BYTES);
        assert_eq!(out.capacity(), cap);
    }

    #[test]
    fn diagnostic_provenance_distinguishes_a_take_from_a_repeat() {
        enable_render_diagnostics();
        let q = FrameQueue::new();
        q.write(&frame(1, 0, 0));
        let mut out = Vec::new();
        assert!(q.take_into(&mut out));
        assert_eq!(take_source_for_render(), Some(TakeSource::Taken));
        assert!(q.take_into(&mut out));
        assert_eq!(take_source_for_render(), Some(TakeSource::Repeat));
    }

    #[test]
    fn accepts_short_frames_without_panicking() {
        let q = FrameQueue::with_depth(1);
        q.write(&[1, 2, 3]);
        q.write(&[4, 5]); // merges; too short for any one-shot word
        assert_eq!(q.take().unwrap(), vec![4, 5]);
        assert_eq!(q.take().unwrap(), vec![4, 5]);
    }
}
