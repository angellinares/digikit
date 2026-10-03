//! Minimal raw ABI for the browser host. JSON output remains valid until the
//! next ABI call that replaces it.

use std::{cell::RefCell, slice};

use crate::{Emulator, ExecutionPolicy};

// Large enough for a coupled ColdFire snapshot (about 62 MB for DN2).
const ALLOCATION_LIMIT: usize = 192 * 1024 * 1024;

thread_local! {
    static EMULATOR: RefCell<Option<Emulator>> = const { RefCell::new(None) };
    static RESULT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

fn result(value: serde_json::Value) {
    RESULT.with(|output| *output.borrow_mut() = serde_json::to_vec(&value).expect("JSON"));
}

fn failure(error: impl ToString) -> i32 {
    result(serde_json::json!({"error": error.to_string()}));
    -1
}

#[unsafe(no_mangle)]
pub extern "C" fn digi_alloc(len: usize) -> *mut u8 {
    if len > ALLOCATION_LIMIT {
        return std::ptr::null_mut();
    }
    Box::into_raw(vec![0u8; len].into_boxed_slice()) as *mut u8
}

/// The caller must pass exactly the pointer and length returned by `digi_alloc`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn digi_dealloc(ptr: *mut u8, len: usize) {
    if !ptr.is_null() && len <= ALLOCATION_LIMIT {
        unsafe {
            drop(Box::from_raw(slice::from_raw_parts_mut(ptr, len)));
        }
    }
}

unsafe fn load_with_policy(ptr: *const u8, len: usize, policy: ExecutionPolicy) -> i32 {
    if ptr.is_null() || len == 0 || len > ALLOCATION_LIMIT {
        return failure("invalid SYX input");
    }
    let syx = unsafe { slice::from_raw_parts(ptr, len) };
    match Emulator::new_with_policy(syx, None, policy) {
        Ok(mut emulator) => {
            let snapshot = emulator.snapshot();
            EMULATOR.with(|slot| *slot.borrow_mut() = Some(emulator));
            result(serde_json::json!({"snapshot": snapshot}));
            0
        }
        Err(error) => failure(error),
    }
}

/// Declares extra executable ranges for the runaway check, as UTF-8 text
/// ("0x4670c000-0x4670ef48, ..."; empty clears them). Applies to the loaded
/// emulator only: call it again after every load or restart.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn digi_exec_ranges(ptr: *const u8, len: usize) -> i32 {
    let text = if len == 0 {
        ""
    } else if ptr.is_null() || len > 4096 {
        return failure("invalid executable-range text");
    } else {
        match std::str::from_utf8(unsafe { slice::from_raw_parts(ptr, len) }) {
            Ok(text) => text,
            Err(_) => return failure("executable-range text is not UTF-8"),
        }
    };
    let ranges = match crate::runtime::parse_exec_ranges(text) {
        Ok(ranges) => ranges,
        Err(error) => return failure(error),
    };
    EMULATOR.with(|slot| match slot.borrow_mut().as_mut() {
        Some(emulator) => {
            let declared = ranges.len();
            emulator.set_exec_ranges(ranges);
            result(serde_json::json!({"exec_ranges": declared}));
            0
        }
        None => failure("no emulator is loaded"),
    })
}

/// Loads the reference-compatible default runtime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn digi_load(ptr: *const u8, len: usize) -> i32 {
    unsafe { load_with_policy(ptr, len, ExecutionPolicy::Reference) }
}

/// Loads the opt-in SoftfloatAbiV1 prototype; callers must not assume exact
/// callee scratch-register, stack-scratch, CCR, or timing equivalence.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn digi_load_softfloat_abi_v1(ptr: *const u8, len: usize) -> i32 {
    unsafe { load_with_policy(ptr, len, ExecutionPolicy::SoftfloatAbiV1) }
}

#[unsafe(no_mangle)]
pub extern "C" fn digi_step(budget: u32) -> i32 {
    EMULATOR.with(|slot| match slot.borrow_mut().as_mut() {
        Some(emulator) => {
            result(serde_json::json!({"snapshot": emulator.step_chunk(budget)}));
            0
        }
        None => failure("no emulator loaded"),
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn digi_button(code: u8, down: u8) -> i32 {
    EMULATOR.with(|slot| match slot.borrow_mut().as_mut() {
        Some(emulator) => match emulator.button(code, down != 0) {
            Ok(()) => {
                result(serde_json::json!({"ok": true}));
                0
            }
            Err(error) => failure(error),
        },
        None => failure("no emulator loaded"),
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn digi_turn(encoder: u8, delta: i32) -> i32 {
    EMULATOR.with(|slot| match slot.borrow_mut().as_mut() {
        Some(emulator) => match emulator.turn(encoder, delta) {
            Ok(()) => {
                result(serde_json::json!({"ok": true}));
                0
            }
            Err(error) => failure(error),
        },
        None => failure("no emulator loaded"),
    })
}

/// Read-only report; does not consume a pending display frame.
#[unsafe(no_mangle)]
pub extern "C" fn digi_diagnostics() -> i32 {
    EMULATOR.with(|slot| match slot.borrow().as_ref() {
        Some(emulator) => {
            result(serde_json::json!(emulator.diagnostics()));
            0
        }
        None => failure("no emulator loaded"),
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn digi_stop() {
    EMULATOR.with(|slot| *slot.borrow_mut() = None);
    #[cfg(feature = "sharc")]
    coupled::clear();
    result(serde_json::json!({"ok": true}));
}

#[unsafe(no_mangle)]
pub extern "C" fn digi_result_ptr() -> *const u8 {
    RESULT.with(|output| output.borrow().as_ptr())
}

#[unsafe(no_mangle)]
pub extern "C" fn digi_result_len() -> usize {
    RESULT.with(|output| output.borrow().len())
}

/// Opt-in coupled ColdFire + SHARC+ audio (cargo feature `sharc`). Everything
/// runs synchronously inside the one module: the DSP renders each 32-sample
/// period inside the ColdFire DSPI2 frame write, exactly like the native
/// `SharcPeer`, so a `digi_step` call returns with the PCM of the frames it
/// covered ready for `digi_pcm_take`.
#[cfg(feature = "sharc")]
mod coupled {
    use super::*;
    use crate::sharc_peer::{DEFAULT_PERIOD, SharcPeer, Shared, SharedDsp, open_dn2_engine};
    use std::rc::Rc;

    /// Header of the `.dsp` file `sharc_live` writes beside a coupled
    /// snapshot: magic, EMUCLK tick (u64 LE), DSP instruction count (u64 LE),
    /// then the canonical DSP state. A bare state blob is also accepted.
    const DSP_MAGIC: &[u8; 8] = b"DT2DSP01";
    const DEFAULT_CLOCK_BASE: u64 = 573_627_620;

    thread_local! {
        static SHARED: RefCell<Option<Rc<RefCell<Shared>>>> = const { RefCell::new(None) };
        static PCM: RefCell<Vec<f32>> = const { RefCell::new(Vec::new()) };
        static TAKEN_FRAMES: RefCell<u64> = const { RefCell::new(0) };
    }

    pub(super) fn clear() {
        SHARED.with(|slot| *slot.borrow_mut() = None);
        PCM.with(|pcm| *pcm.borrow_mut() = Vec::new());
    }

    fn bytes<'a>(ptr: *const u8, len: usize) -> Result<&'a [u8], String> {
        if len > ALLOCATION_LIMIT || (ptr.is_null() && len != 0) {
            return Err("invalid input buffer".into());
        }
        Ok(if len == 0 {
            &[]
        } else {
            unsafe { slice::from_raw_parts(ptr, len) }
        })
    }

    fn load(
        syx: &[u8],
        image: &[u8],
        dsp: &[u8],
        snapshot: &[u8],
    ) -> Result<serde_json::Value, String> {
        let (base, dsp_instructions, state) = if dsp.starts_with(DSP_MAGIC) && dsp.len() >= 24 {
            (
                u64::from_le_bytes(dsp[8..16].try_into().unwrap()),
                u64::from_le_bytes(dsp[16..24].try_into().unwrap()),
                &dsp[24..],
            )
        } else {
            (DEFAULT_CLOCK_BASE, 0, dsp)
        };
        let engine = open_dn2_engine(image, state, base)?;
        let (peer, shared) = SharcPeer::new(SharedDsp::new(engine), DEFAULT_PERIOD);
        shared.borrow_mut().dsp_instructions = dsp_instructions;
        let mut emulator = Emulator::new(syx, None)?;
        // The coupled snapshots are taken with this lane on.
        emulator.enable_ssi_diagnostic(96_000)?;
        if !snapshot.is_empty() {
            emulator.load_state(snapshot)?;
        }
        emulator.set_dspi2_peer(peer.boxed());
        let first = emulator.snapshot();
        EMULATOR.with(|slot| *slot.borrow_mut() = Some(emulator));
        SHARED.with(|slot| *slot.borrow_mut() = Some(shared));
        PCM.with(|pcm| pcm.borrow_mut().clear());
        TAKEN_FRAMES.with(|n| *n.borrow_mut() = 0);
        Ok(serde_json::json!({"snapshot": first}))
    }

    /// Loads firmware (SYX), the packed DSP image, the DSP state (`.dsp`
    /// file or bare canonical state) and optionally a `DT2SNP01` ColdFire
    /// snapshot taken at ready (length 0: boot from reset with the DSP
    /// attached from the first frame). Reference-compatible ColdFire policy.
    #[unsafe(no_mangle)]
    #[allow(clippy::too_many_arguments)]
    pub unsafe extern "C" fn digi_load_coupled(
        syx: *const u8,
        syx_len: usize,
        image: *const u8,
        image_len: usize,
        dsp: *const u8,
        dsp_len: usize,
        snapshot: *const u8,
        snapshot_len: usize,
    ) -> i32 {
        let loaded = (|| {
            if syx.is_null() || syx_len == 0 {
                return Err("invalid SYX input".to_string());
            }
            load(
                bytes(syx, syx_len)?,
                bytes(image, image_len)?,
                bytes(dsp, dsp_len)?,
                bytes(snapshot, snapshot_len)?,
            )
        })();
        match loaded {
            Ok(value) => {
                result(value);
                0
            }
            Err(error) => failure(error),
        }
    }

    /// Moves the PCM produced since the last call (interleaved L/R f32 at
    /// 48 kHz, 64 values per DSP block) to the output buffer; returns the
    /// value count (0 when none or not coupled). Raw words and per-frame
    /// records are dropped, so a long run does not grow memory. The buffer at
    /// `digi_pcm_ptr` stays valid until the next call.
    #[unsafe(no_mangle)]
    pub extern "C" fn digi_pcm_take() -> usize {
        SHARED.with(|slot| {
            let slot = slot.borrow();
            let Some(shared) = slot.as_ref() else {
                return 0;
            };
            let mut sh = shared.borrow_mut();
            TAKEN_FRAMES.with(|n| *n.borrow_mut() += sh.frames.len() as u64);
            sh.raw.clear();
            sh.frames.clear();
            PCM.with(|pcm| {
                let mut pcm = pcm.borrow_mut();
                *pcm = std::mem::take(&mut sh.pcm);
                pcm.len()
            })
        })
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn digi_pcm_ptr() -> *const u8 {
        PCM.with(|pcm| pcm.borrow().as_ptr() as *const u8)
    }

    /// JSON: frames (DSPI2 frames since load), dsp_instructions, halted,
    /// nonzero_replies, missing_blocks.
    #[unsafe(no_mangle)]
    pub extern "C" fn digi_sharc_stats() -> i32 {
        SHARED.with(|slot| match slot.borrow().as_ref() {
            Some(shared) => {
                let sh = shared.borrow();
                let frames = TAKEN_FRAMES.with(|n| *n.borrow()) + sh.frames.len() as u64;
                result(serde_json::json!({
                    "frames": frames,
                    "dsp_instructions": sh.dsp_instructions,
                    "halted": sh.halted,
                    "nonzero_replies": sh.nonzero_replies,
                    "missing_blocks": sh.missing_blocks,
                }));
                0
            }
            None => failure("no coupled emulator loaded"),
        })
    }
}
