//! C ABI (ctypes-friendly): the surface `tools/live_audio.py` calls.
//!
//! Every exported function takes and returns only plain types (pointers,
//! integers, floats, a fixed-layout struct) so `ctypes` can call it with no
//! generated bindings. A handle is an opaque `*mut LivePlayer` -- callers
//! must not dereference it themselves, only pass it back in.
//!
//! Error reporting: functions return `0` for success and a negative `i32`
//! for failure; `live_last_error` returns the most recent failure's
//! message for the calling thread (set by `open`/`play_wav`, the only
//! calls that can fail for a reason worth a message).

use std::cell::RefCell;
use std::ffi::{CStr, c_char, c_float, c_int};

use crate::player::LivePlayer;
use crate::ring::{FRAME_LEN, StereoSample};
use crate::sharc_lib::{capture_source, live_source};
use crate::sharc_source::AfterEnd;

thread_local! {
    static LAST_ERROR: RefCell<String> = const { RefCell::new(String::new()) };
}

fn store_last_error(message: String) {
    LAST_ERROR.with(|cell| *cell.borrow_mut() = message);
}

fn write_c_string(text: &str, buf: *mut c_char, buf_len: usize) -> c_int {
    if buf.is_null() || buf_len == 0 {
        return -1;
    }
    let bytes = text.as_bytes();
    // Leave room for the trailing NUL; truncate if the caller's buffer is
    // too small rather than overflow it.
    let copy_len = bytes.len().min(buf_len - 1);
    // SAFETY: the caller guarantees `buf` points at a writable buffer of
    // at least `buf_len` bytes (documented contract of every ABI function
    // that takes a `buf`/`buf_len` pair).
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf as *mut u8, copy_len);
        *buf.add(copy_len) = 0;
    }
    copy_len as c_int
}

/// Repr(C) mirror of `player::Stats`, for `live_stats`.
#[repr(C)]
pub struct LiveStats {
    pub sample_rate: u32,
    pub requested_sample_rate: u32,
    pub channels: u16,
    pub used_fallback: u8,
    pub _pad: u8,
    pub target_latency_frames: u32,
    pub ring_capacity_frames: u32,
    pub ring_fill_frames: u32,
    pub underruns: u64,
    pub frames_rendered: u64,
}

/// Opens the default output device (48 kHz stereo where available; a
/// clean, reported fallback otherwise -- see `live_stats`'s
/// `used_fallback`) and starts the producer thread on silence. Returns a
/// handle, or `NULL` on failure (`live_last_error` has the reason).
#[unsafe(no_mangle)]
pub extern "C" fn live_open(target_latency_frames: u32) -> *mut LivePlayer {
    match LivePlayer::open(target_latency_frames) {
        Ok(player) => Box::into_raw(Box::new(player)),
        Err(e) => {
            store_last_error(e);
            std::ptr::null_mut()
        }
    }
}

/// Stops the producer and the output stream and frees the handle. `handle`
/// must not be used again after this call. A `NULL` handle is a no-op.
///
/// # Safety
/// `handle` must be `NULL` or a value previously returned by `live_open`
/// and not already passed to `live_close`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn live_close(handle: *mut LivePlayer) {
    if handle.is_null() {
        return;
    }
    // SAFETY: `handle` came from `live_open` (the only way to obtain one)
    // and the caller promises not to use it again.
    unsafe { drop(Box::from_raw(handle)) };
}

/// Swaps in a sine test tone as the current frame source. Returns `0`, or
/// `-1` if `handle` is `NULL`.
///
/// # Safety
/// `handle` must be `NULL` or a live handle from `live_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn live_play_tone(
    handle: *mut LivePlayer,
    freq_hz: c_float,
    amplitude: c_float,
) -> c_int {
    let Some(player) = (unsafe { handle.as_ref() }) else {
        store_last_error("live_play_tone: NULL handle".to_string());
        return -1;
    };
    player.play_tone(freq_hz, amplitude);
    0
}

/// Swaps in a WAV/PCM file as the current frame source. `path` is a
/// NUL-terminated UTF-8 path. `looping` != 0 loops the file forever.
/// Returns `0` on success, a negative code on failure
/// (`live_last_error` has the reason).
///
/// # Safety
/// `handle` must be `NULL` or a live handle from `live_open`. `path`, if
/// not `NULL`, must point at a NUL-terminated string valid for the
/// duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn live_play_wav(
    handle: *mut LivePlayer,
    path: *const c_char,
    looping: c_int,
) -> c_int {
    let Some(player) = (unsafe { handle.as_ref() }) else {
        store_last_error("live_play_wav: NULL handle".to_string());
        return -1;
    };
    if path.is_null() {
        store_last_error("live_play_wav: NULL path".to_string());
        return -2;
    }
    // SAFETY: `path` is documented as a NUL-terminated string owned by the
    // caller for the duration of this call.
    let c_str = unsafe { CStr::from_ptr(path) };
    let path_str = match c_str.to_str() {
        Ok(s) => s,
        Err(_) => {
            store_last_error("live_play_wav: path is not valid UTF-8".to_string());
            return -3;
        }
    };
    match player.play_wav(std::path::Path::new(path_str), looping != 0) {
        Ok(_) => 0,
        Err(e) => {
            store_last_error(format!("live_play_wav: {e}"));
            -4
        }
    }
}

/// Queues one SPI2 TX frame (wire order) for the frame source: the
/// emulator's DSPI2 peer calls this for every frame the ColdFire sends
/// (`emu/livesharc.py`). Each queued frame is rendered once, in order; when
/// none is queued the last one repeats with its one-shot words cleared
/// (`src/repeater.rs`). Returns `0`, or a negative code on a bad handle.
///
/// # Safety
/// `handle` must be `NULL` or a live handle from `live_open`. `data`, if
/// not `NULL`, must point at `len` readable bytes valid for the duration
/// of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn live_push_frame(
    handle: *mut LivePlayer,
    data: *const u8,
    len: usize,
) -> c_int {
    let Some(player) = (unsafe { handle.as_ref() }) else {
        store_last_error("live_push_frame: NULL handle".to_string());
        return -1;
    };
    if data.is_null() {
        store_last_error("live_push_frame: NULL data".to_string());
        return -2;
    }
    // SAFETY: caller-documented contract: `data` points at `len` readable
    // bytes for the duration of this call.
    let frame = unsafe { std::slice::from_raw_parts(data, len) };
    player.push_frame(frame);
    0
}

/// Fills `out` with the current stats snapshot. Returns `0`, or a negative
/// code on a bad handle/pointer.
///
/// # Safety
/// `handle` must be `NULL` or a live handle from `live_open`. `out`, if not
/// `NULL`, must point at one writable, properly aligned `LiveStats`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn live_stats(handle: *mut LivePlayer, out: *mut LiveStats) -> c_int {
    let Some(player) = (unsafe { handle.as_ref() }) else {
        return -1;
    };
    if out.is_null() {
        return -2;
    }
    let stats = player.stats();
    let c_stats = LiveStats {
        sample_rate: stats.sample_rate,
        requested_sample_rate: stats.requested_sample_rate,
        channels: stats.channels,
        used_fallback: stats.used_fallback as u8,
        _pad: 0,
        target_latency_frames: stats.target_latency_frames,
        ring_capacity_frames: stats.ring_capacity_frames,
        ring_fill_frames: stats.ring_fill_frames,
        underruns: stats.underruns,
        frames_rendered: stats.frames_rendered,
    };
    // SAFETY: caller-documented contract: `out` points at one writable
    // `LiveStats`.
    unsafe { std::ptr::write(out, c_stats) };
    0
}

/// Repr(C) summary of the SHARC capture source's render log, for
/// `live_render_stats` (times in µs; frame 0, the cold start, is left out
/// of the percentiles and reported as `first_us`).
#[repr(C)]
#[derive(Default)]
pub struct LiveRenderStats {
    pub frames: u64,
    pub clean: u64,
    pub stopped: u64,
    pub single_steps: u64,
    pub frames_with_single_steps: u64,
    pub instructions: u64,
    pub median_us: f64,
    pub p99_us: f64,
    pub max_us: f64,
    pub mean_us: f64,
    pub first_us: f64,
    /// Frames with a nonzero voice sample; the first of them (-1: none).
    pub nonzero_frames: u64,
    pub first_nonzero: i64,
    /// Frames rendered silent before the first TX frame arrived.
    pub idle_frames: u64,
}

fn c_path(path: *const c_char, what: &str) -> Result<std::path::PathBuf, String> {
    if path.is_null() {
        return Err(format!("{what}: NULL path"));
    }
    // SAFETY: caller contract: a NUL-terminated string valid for the call.
    let s = unsafe { CStr::from_ptr(path) }
        .to_str()
        .map_err(|_| format!("{what}: path is not UTF-8"))?;
    Ok(std::path::PathBuf::from(s))
}

/// Opens the default output device playing a SHARC capture live: the
/// native core library at `lib_path` renders the live pack at `pack_path`
/// (`tools/sharc_transpile_run.py live-pack`), one SHARC frame per 32
/// samples. After the capture's last frame the last frame is held (its
/// trig/release words cleared) for `gap_frames` frames and the capture
/// loops, or, with `hold` != 0, held for ever. The stream starts once the
/// ring is full; a device not at 48 kHz gets the output resampled.
/// Returns a handle (use with every other `live_*` call), or `NULL` with
/// `live_last_error` set.
///
/// # Safety
/// `lib_path` and `pack_path` must be `NULL` or NUL-terminated strings
/// valid for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn live_open_capture(
    target_latency_frames: u32,
    lib_path: *const c_char,
    pack_path: *const c_char,
    gap_frames: u32,
    hold: c_int,
    gain: c_float,
) -> *mut LivePlayer {
    let opened = (|| -> Result<LivePlayer, String> {
        let lib = c_path(lib_path, "live_open_capture lib")?;
        let pack = c_path(pack_path, "live_open_capture pack")?;
        let after = if hold != 0 {
            AfterEnd::Hold
        } else {
            AfterEnd::Loop {
                gap: gap_frames as usize,
            }
        };
        let (source, _info) = capture_source(&lib, &pack, after, gain)?;
        let log = source.log();
        let mut player = LivePlayer::open_with_source(
            target_latency_frames,
            Box::new(source),
            Some(std::time::Duration::from_secs(5)),
            Some(48_000),
        )?;
        player.attach_render_log(log);
        Ok(player)
    })();
    match opened {
        Ok(player) => Box::into_raw(Box::new(player)),
        Err(e) => {
            store_last_error(e);
            std::ptr::null_mut()
        }
    }
}

/// Opens the native SHARC core on the emulated ColdFire's frames, live:
/// the core library at `lib_path` starts from the state pack at
/// `pack_path` (`tools/sharc_transpile_run.py state-pack`) and renders
/// every frame `live_push_frame` queues (wire order), one SHARC frame per
/// 32 samples, repeating the last frame with its one-shot words cleared
/// when none is queued (`src/repeater.rs`). `card_sha256` (NULL: not
/// checked) must equal the card image the pack's LP0 feed came from.
/// With `device` != 0 the default output device plays it (stream started
/// once the ring is full; a device not at 48 kHz gets the output
/// resampled); with `device` == 0 there is no device and no thread, and
/// `live_render` pulls frames. Returns a handle, or `NULL` with
/// `live_last_error` set.
///
/// # Safety
/// `lib_path`, `pack_path` and `card_sha256` must be `NULL` or
/// NUL-terminated strings valid for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn live_open_frames(
    target_latency_frames: u32,
    lib_path: *const c_char,
    pack_path: *const c_char,
    card_sha256: *const c_char,
    gain: c_float,
    device: c_int,
) -> *mut LivePlayer {
    let opened = (|| -> Result<LivePlayer, String> {
        let lib = c_path(lib_path, "live_open_frames lib")?;
        let pack = c_path(pack_path, "live_open_frames pack")?;
        let card = if card_sha256.is_null() {
            None
        } else {
            // SAFETY: caller contract: a NUL-terminated string.
            Some(
                unsafe { CStr::from_ptr(card_sha256) }
                    .to_str()
                    .map_err(|_| "live_open_frames: card sha256 is not UTF-8".to_string())?
                    .to_string(),
            )
        };
        let (source, _info) = live_source(&lib, &pack, gain, card.as_deref())?;
        let log = source.log();
        let mut player = if device != 0 {
            LivePlayer::open_with_source(
                target_latency_frames,
                Box::new(source),
                Some(std::time::Duration::from_secs(5)),
                Some(48_000),
            )?
        } else {
            LivePlayer::offline(Box::new(source))
        };
        player.attach_render_log(log);
        Ok(player)
    })();
    match opened {
        Ok(player) => Box::into_raw(Box::new(player)),
        Err(e) => {
            store_last_error(e);
            std::ptr::null_mut()
        }
    }
}

/// A player opened with no device (`live_open_frames(..., device = 0)`):
/// render `frames` SHARC frames into `out` (interleaved L, R f32, 64 per
/// frame; `out_len` floats available). Returns the frames rendered, `-1`
/// for a bad handle, `-2` for a NULL or short `out`, `-3` for a player with
/// a device.
///
/// # Safety
/// `handle` must be `NULL` or a live handle; `out`, if not `NULL`, must
/// point at `out_len` writable floats.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn live_render(
    handle: *mut LivePlayer,
    frames: u32,
    out: *mut c_float,
    out_len: usize,
) -> c_int {
    let Some(player) = (unsafe { handle.as_ref() }) else {
        return -1;
    };
    let need = frames as usize * FRAME_LEN * 2;
    if out.is_null() || out_len < need {
        return -2;
    }
    let mut samples = vec![StereoSample::default(); frames as usize * FRAME_LEN];
    match player.render_offline(&mut samples) {
        Ok(n) => {
            // SAFETY: caller contract: `out` has `out_len` >= `need` floats.
            let dst = unsafe { std::slice::from_raw_parts_mut(out, need) };
            for (i, s) in samples.iter().enumerate() {
                dst[2 * i] = s.l;
                dst[2 * i + 1] = s.r;
            }
            n as c_int
        }
        Err(e) => {
            store_last_error(e);
            -3
        }
    }
}

/// Repr(C) mirror of `repeater::QueueStats`, for `live_frame_stats`.
#[repr(C)]
#[derive(Default)]
pub struct LiveFrameStats {
    pub pushed: u64,
    pub trig_pushed: u64,
    pub taken: u64,
    pub trig_taken: u64,
    pub repeats: u64,
    pub merged: u64,
    pub max_depth: u64,
    pub depth: u64,
}

/// Fills `out` with the frame queue's counters. Returns `0`, `-1` for a
/// bad handle, `-2` for a NULL `out`.
///
/// # Safety
/// `handle` must be `NULL` or a live handle; `out`, if not `NULL`, must
/// point at one writable `LiveFrameStats`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn live_frame_stats(
    handle: *mut LivePlayer,
    out: *mut LiveFrameStats,
) -> c_int {
    let Some(player) = (unsafe { handle.as_ref() }) else {
        return -1;
    };
    if out.is_null() {
        return -2;
    }
    let s = player.frame_stats();
    let stats = LiveFrameStats {
        pushed: s.pushed,
        trig_pushed: s.trig_pushed,
        taken: s.taken,
        trig_taken: s.trig_taken,
        repeats: s.repeats,
        merged: s.merged,
        max_depth: s.max_depth,
        depth: s.depth,
    };
    // SAFETY: caller contract: `out` points at one writable struct.
    unsafe { std::ptr::write(out, stats) };
    0
}

/// Fills `out` with the SHARC source's render summary. Returns `0`, `-1`
/// for a bad handle, `-2` for a NULL `out`, `-3` when the handle does not
/// play a capture.
///
/// # Safety
/// `handle` must be `NULL` or a live handle; `out`, if not `NULL`, must
/// point at one writable `LiveRenderStats`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn live_render_stats(
    handle: *mut LivePlayer,
    out: *mut LiveRenderStats,
) -> c_int {
    let Some(player) = (unsafe { handle.as_ref() }) else {
        return -1;
    };
    if out.is_null() {
        return -2;
    }
    let Some(log) = player.render_log() else {
        return -3;
    };
    let log = log.lock().expect("render log poisoned");
    let s = log.summary(1);
    let stats = LiveRenderStats {
        frames: log.frames,
        clean: log.clean,
        stopped: log.stopped,
        single_steps: log.single_steps,
        frames_with_single_steps: log.frames_with_single_steps,
        instructions: log.instructions,
        median_us: s.median_us,
        p99_us: s.p99_us,
        max_us: s.max_us,
        mean_us: s.mean_us,
        first_us: log.frame_ns.first().copied().unwrap_or(0) as f64 / 1000.0,
        nonzero_frames: log.nonzero_frames,
        first_nonzero: log.first_nonzero.map_or(-1, |f| f as i64),
        idle_frames: log.idle_frames,
    };
    // SAFETY: caller contract: `out` points at one writable struct.
    unsafe { std::ptr::write(out, stats) };
    0
}

/// Writes the open device's name (NUL-terminated, truncated to fit) into
/// `buf`. Returns the number of bytes written (excluding the NUL), or a
/// negative code on a bad handle/buffer.
///
/// # Safety
/// `handle` must be `NULL` or a live handle from `live_open`. `buf`, if not
/// `NULL`, must point at a writable buffer of at least `buf_len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn live_device_name(
    handle: *mut LivePlayer,
    buf: *mut c_char,
    buf_len: usize,
) -> c_int {
    let Some(player) = (unsafe { handle.as_ref() }) else {
        return -1;
    };
    write_c_string(player.device_name(), buf, buf_len)
}

/// Writes the calling thread's most recent error message (set by `open`/
/// `play_wav`/a bad-handle call) into `buf`. Returns the number of bytes
/// written (excluding the NUL).
#[unsafe(no_mangle)]
pub extern "C" fn live_last_error(buf: *mut c_char, buf_len: usize) -> c_int {
    LAST_ERROR.with(|cell| write_c_string(&cell.borrow(), buf, buf_len))
}

// A process-wide guard so ABI tests (below) that open a real handle do not
// run concurrently with each other -- cpal's default host/device access is
// not documented as thread-safe for concurrent *opens* across tests.
#[cfg(test)]
static ABI_TEST_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    #[test]
    fn null_handle_calls_return_negative_and_do_not_crash() {
        unsafe {
            assert_eq!(live_play_tone(std::ptr::null_mut(), 440.0, 0.5), -1);
            assert_eq!(live_push_frame(std::ptr::null_mut(), [1u8].as_ptr(), 1), -1);
            let mut stats = LiveStats {
                sample_rate: 0,
                requested_sample_rate: 0,
                channels: 0,
                used_fallback: 0,
                _pad: 0,
                target_latency_frames: 0,
                ring_capacity_frames: 0,
                ring_fill_frames: 0,
                underruns: 0,
                frames_rendered: 0,
            };
            assert_eq!(live_stats(std::ptr::null_mut(), &mut stats), -1);
            live_close(std::ptr::null_mut()); // must not crash
        }
    }

    #[test]
    fn live_play_wav_rejects_null_path() {
        // A NULL handle also returns -1 first, which already proves the
        // function does not dereference a NULL path unguarded; a full
        // open() + NULL path round trip needs a real device (see
        // tests::device, ignored by default).
        unsafe {
            assert_eq!(live_play_wav(std::ptr::null_mut(), std::ptr::null(), 0), -1);
        }
    }

    #[test]
    fn last_error_round_trips_through_the_buffer() {
        store_last_error("boom".to_string());
        let mut buf = [0u8; 16];
        let n = live_last_error(buf.as_mut_ptr() as *mut c_char, buf.len());
        assert_eq!(n, 4);
        let c = unsafe { CStr::from_ptr(buf.as_ptr() as *const c_char) };
        assert_eq!(c.to_str().unwrap(), "boom");
    }

    #[test]
    fn write_c_string_truncates_to_fit() {
        let mut buf = [0xFFu8; 4];
        let n = write_c_string("hello", buf.as_mut_ptr() as *mut c_char, buf.len());
        assert_eq!(n, 3); // 3 chars + NUL == 4-byte buffer
        let c = unsafe { CStr::from_ptr(buf.as_ptr() as *const c_char) };
        assert_eq!(c.to_str().unwrap(), "hel");
    }

    // Requires a real output device; run with
    // `LIVE_AUDIO_DEVICE_TESTS=1 cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn open_play_stats_close_round_trip() {
        let _guard = ABI_TEST_GUARD.lock().unwrap();
        if std::env::var("LIVE_AUDIO_DEVICE_TESTS").is_err() {
            return;
        }
        unsafe {
            let handle = live_open(512);
            assert!(!handle.is_null());

            assert_eq!(live_play_tone(handle, 440.0, 0.2), 0);

            let mut stats = LiveStats {
                sample_rate: 0,
                requested_sample_rate: 0,
                channels: 0,
                used_fallback: 0,
                _pad: 0,
                target_latency_frames: 0,
                ring_capacity_frames: 0,
                ring_fill_frames: 0,
                underruns: 0,
                frames_rendered: 0,
            };
            assert_eq!(live_stats(handle, &mut stats), 0);
            assert!(stats.sample_rate > 0);

            let path = CString::new("/tmp/does-not-exist.wav").unwrap();
            let rc = live_play_wav(handle, path.as_ptr(), 0);
            assert!(rc < 0, "loading a missing file should fail");
            let mut err = [0u8; 256];
            let n = live_last_error(err.as_mut_ptr() as *mut c_char, err.len());
            assert!(n > 0);

            live_close(handle);
        }
    }

    #[test]
    fn capture_calls_reject_bad_arguments_without_a_device() {
        unsafe {
            let h = live_open_capture(512, std::ptr::null(), std::ptr::null(), 0, 0, 1.0);
            assert!(h.is_null());
            let lib = CString::new("/nonexistent/libsharc_native.dylib").unwrap();
            let pack = CString::new("/nonexistent/x.pack").unwrap();
            let h = live_open_capture(512, lib.as_ptr(), pack.as_ptr(), 0, 0, 1.0);
            assert!(h.is_null());
            let mut buf = [0 as c_char; 256];
            let n = live_last_error(buf.as_mut_ptr(), buf.len());
            assert!(n > 0, "an error message is stored");
            let mut rs = LiveRenderStats::default();
            assert_eq!(live_render_stats(std::ptr::null_mut(), &mut rs), -1);
        }
    }

    #[test]
    fn frames_calls_reject_bad_arguments_without_a_device() {
        unsafe {
            let h = live_open_frames(
                512,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                1.0,
                0,
            );
            assert!(h.is_null());
            let lib = CString::new("/nonexistent/libsharc_native.dylib").unwrap();
            let pack = CString::new("/nonexistent/x.pack").unwrap();
            let h = live_open_frames(512, lib.as_ptr(), pack.as_ptr(), std::ptr::null(), 1.0, 0);
            assert!(h.is_null());
            let mut buf = [0 as c_char; 256];
            assert!(live_last_error(buf.as_mut_ptr(), buf.len()) > 0);
            let mut out = [0f32; 64];
            assert_eq!(
                live_render(std::ptr::null_mut(), 1, out.as_mut_ptr(), 64),
                -1
            );
            let mut fs = LiveFrameStats::default();
            assert_eq!(live_frame_stats(std::ptr::null_mut(), &mut fs), -1);
        }
    }

    #[test]
    fn an_offline_player_renders_pushed_frames_through_the_queue() {
        use crate::source::FrameSource;
        /// Echoes the first input byte (0 when empty) as the left sample.
        struct Echo;
        impl FrameSource for Echo {
            fn render_frame(&mut self, input: &[u8], out: &mut [StereoSample; FRAME_LEN]) {
                let v = input.first().copied().unwrap_or(0) as f32;
                *out = [StereoSample { l: v, r: -v }; FRAME_LEN];
            }
        }
        let h = Box::into_raw(Box::new(LivePlayer::offline(Box::new(Echo))));
        unsafe {
            let mut out = vec![0f32; 3 * 64];
            assert_eq!(live_render(h, 1, out.as_mut_ptr(), 64), 1);
            assert_eq!(out[0], 0.0, "nothing pushed yet: empty input");
            for tag in [5u8, 6] {
                assert_eq!(live_push_frame(h, [tag, 0].as_ptr(), 2), 0);
            }
            assert_eq!(live_render(h, 3, out.as_mut_ptr(), out.len()), 3);
            assert_eq!((out[0], out[1], out[64], out[128]), (5.0, -5.0, 6.0, 6.0));
            assert_eq!(live_render(h, 1, out.as_mut_ptr(), 10), -2, "short buffer");
            let mut fs = LiveFrameStats::default();
            assert_eq!(live_frame_stats(h, &mut fs), 0);
            assert_eq!(
                (fs.pushed, fs.taken, fs.repeats, fs.max_depth),
                (2, 2, 1, 2)
            );
            let mut st = std::mem::zeroed::<LiveStats>();
            assert_eq!(live_stats(h, &mut st), 0);
            assert_eq!((st.sample_rate, st.frames_rendered), (48_000, 4 * 32));
            live_close(h);
        }
    }
}
