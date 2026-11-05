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

/// Loads the reference-compatible default runtime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn digi_load(ptr: *const u8, len: usize) -> i32 {
    unsafe { load_with_policy(ptr, len, ExecutionPolicy::Reference) }
}

/// Loads firmware and restores a `DT2SNP01` ColdFire snapshot (taken at ready
/// with the SSI diagnostic lane on, as the coupled ready snapshots are), with
/// no DSP attached. For measuring the ColdFire cost of the audio path alone.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn digi_load_snapshot(
    syx: *const u8,
    syx_len: usize,
    snap: *const u8,
    snap_len: usize,
) -> i32 {
    if syx.is_null()
        || syx_len == 0
        || syx_len > ALLOCATION_LIMIT
        || snap.is_null()
        || snap_len == 0
    {
        return failure("invalid input");
    }
    let (syx, snap) = unsafe {
        (
            slice::from_raw_parts(syx, syx_len),
            slice::from_raw_parts(snap, snap_len),
        )
    };
    let loaded = Emulator::new(syx, None).and_then(|mut emulator| {
        emulator.enable_ssi_diagnostic(96_000)?;
        emulator.load_state(snap)?;
        Ok(emulator)
    });
    match loaded {
        Ok(mut emulator) => {
            let snapshot = emulator.snapshot();
            EMULATOR.with(|slot| *slot.borrow_mut() = Some(emulator));
            result(serde_json::json!({"snapshot": snapshot}));
            0
        }
        Err(error) => failure(error),
    }
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
    use crate::sharc_peer::{
        DEFAULT_PERIOD, RemoteDsp, SharcPeer, Shared, SharedDsp, open_dn2_engine,
    };
    use std::rc::Rc;

    /// Header of the `.dsp` file `sharc_live` writes beside a coupled
    /// snapshot: magic, EMUCLK tick (u64 LE), DSP instruction count (u64 LE),
    /// then the canonical DSP state. A bare state blob is also accepted.
    const DSP_MAGIC: &[u8; 8] = b"DT2DSP01";
    const DEFAULT_CLOCK_BASE: u64 = 573_627_620;

    thread_local! {
        static ENGINE: RefCell<Option<SharedDsp>> = const { RefCell::new(None) };
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
        let dsp_handle = SharedDsp::new(engine);
        ENGINE.with(|slot| *slot.borrow_mut() = Some(SharedDsp(dsp_handle.0.clone())));
        let (peer, shared) = SharcPeer::new(dsp_handle, DEFAULT_PERIOD);
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

    // -- Two instances: the ColdFire in one worker, the DSP in another ------
    //
    // `digi_dsp_*` is the DSP instance: it holds the engine and answers each
    // DSPI2 frame the ColdFire instance sends (`digi_dsp_exchange`), its PCM
    // leaving through `digi_pcm_take` as in the one-instance core.
    // `digi_load_coupled_remote` is the ColdFire instance: its DSPI2 peer is
    // the page (imports `env.host_dspi_exchange` / `host_dspi_link`, which
    // block on a SharedArrayBuffer until the DSP worker has answered).

    thread_local! {
        static DSP_PEER: RefCell<Option<RemoteDsp<SharedDsp>>> = const { RefCell::new(None) };
    }

    /// Opens the DSP side: packed image, DSP state (`.dsp` file or bare state).
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn digi_dsp_open(
        image: *const u8,
        image_len: usize,
        dsp: *const u8,
        dsp_len: usize,
    ) -> i32 {
        let opened = (|| {
            let (image, dsp) = (bytes(image, image_len)?, bytes(dsp, dsp_len)?);
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
            let handle = SharedDsp::new(engine);
            ENGINE.with(|slot| *slot.borrow_mut() = Some(SharedDsp(handle.0.clone())));
            let (peer, shared) = RemoteDsp::new(handle, DEFAULT_PERIOD);
            shared.borrow_mut().dsp_instructions = dsp_instructions;
            DSP_PEER.with(|slot| *slot.borrow_mut() = Some(peer));
            SHARED.with(|slot| *slot.borrow_mut() = Some(shared));
            PCM.with(|pcm| pcm.borrow_mut().clear());
            TAKEN_FRAMES.with(|n| *n.borrow_mut() = 0);
            Ok::<_, String>(serde_json::json!({"dsp": true}))
        })();
        match opened {
            Ok(value) => {
                result(value);
                0
            }
            Err(error) => failure(error),
        }
    }

    /// First half of a DSPI2 frame: `tx_len` bytes in, the reply (the same
    /// number of bytes) written to `rx`. The period is rendered by
    /// `digi_dsp_render`, which the host calls after it has handed the reply
    /// to the ColdFire instance.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn digi_dsp_reply(tx: *const u8, tx_len: usize, rx: *mut u8) -> i32 {
        let Ok(tx) = bytes(tx, tx_len) else { return -1 };
        DSP_PEER.with(|slot| match slot.borrow_mut().as_mut() {
            Some(peer) => {
                let reply = peer.reply(tx);
                unsafe {
                    std::ptr::copy_nonoverlapping(reply.as_ptr(), rx, reply.len().min(tx_len))
                };
                0
            }
            None => -1,
        })
    }

    /// Second half: render the period of the last `digi_dsp_reply` (a no-op
    /// when it is already rendered).
    #[unsafe(no_mangle)]
    pub extern "C" fn digi_dsp_render() -> i32 {
        DSP_PEER.with(|slot| match slot.borrow_mut().as_mut() {
            Some(peer) => {
                peer.render();
                0
            }
            None => -1,
        })
    }

    /// One whole DSPI2 frame (reply, then render).
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn digi_dsp_exchange(tx: *const u8, tx_len: usize, rx: *mut u8) -> i32 {
        let code = unsafe { digi_dsp_reply(tx, tx_len, rx) };
        if code != 0 { code } else { digi_dsp_render() }
    }

    /// Link port 0 bytes the ColdFire sent since the previous frame.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn digi_dsp_link(ptr: *const u8, len: usize) -> i32 {
        let Ok(b) = bytes(ptr, len) else { return -1 };
        DSP_PEER.with(|slot| match slot.borrow_mut().as_mut() {
            Some(peer) => {
                peer.link_bytes(b);
                0
            }
            None => -1,
        })
    }

    #[link(wasm_import_module = "env")]
    #[cfg(target_arch = "wasm32")]
    unsafe extern "C" {
        fn host_dspi_exchange(tx: *const u8, len: usize, rx: *mut u8);
        fn host_dspi_link(ptr: *const u8, len: usize);
    }

    /// The ColdFire instance's DSPI2 peer: the DSP lives in another worker.
    #[cfg(target_arch = "wasm32")]
    struct HostPeer;

    #[cfg(target_arch = "wasm32")]
    impl periph::dspi::Peer for HostPeer {
        fn exchange(&mut self, tx: &[u8]) -> Vec<u8> {
            let mut rx = vec![0u8; tx.len()];
            unsafe { host_dspi_exchange(tx.as_ptr(), tx.len(), rx.as_mut_ptr()) };
            rx
        }
        fn link_bytes(&mut self, b: &[u8]) {
            unsafe { host_dspi_link(b.as_ptr(), b.len()) };
        }
    }

    /// The ColdFire instance of a two-worker session: firmware and optionally
    /// a ColdFire snapshot taken at ready; the DSP is in the other worker.
    #[cfg(target_arch = "wasm32")]
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn digi_load_coupled_remote(
        syx: *const u8,
        syx_len: usize,
        snapshot: *const u8,
        snapshot_len: usize,
    ) -> i32 {
        let loaded = (|| {
            let mut emulator = Emulator::new(bytes(syx, syx_len)?, None)?;
            emulator.enable_ssi_diagnostic(96_000)?;
            let snapshot = bytes(snapshot, snapshot_len)?;
            if !snapshot.is_empty() {
                emulator.load_state(snapshot)?;
            }
            emulator.set_dspi2_peer(Box::new(HostPeer));
            let first = emulator.snapshot();
            EMULATOR.with(|slot| *slot.borrow_mut() = Some(emulator));
            Ok::<_, String>(serde_json::json!({"snapshot": first}))
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

    /// Turns the DSP fast tier on with run-time region selection (`threshold`
    /// visits; regions yielding fewer than `min_insns` per call are dropped:
    /// 1 and 0 arm every entry seen and never drop one) and, with `follow`,
    /// the instruction after each unlowerable instruction as an entry. The
    /// kernels are WebAssembly modules the page compiles (`env.host_wasm_*`).
    #[cfg(feature = "fast-wasm")]
    #[unsafe(no_mangle)]
    pub extern "C" fn digi_sharc_fast(threshold: u32, min_insns: u32, follow: u32) -> i32 {
        use sharc_native::fast::FastEngine;
        sharc_fast_wasm::host::register();
        ENGINE.with(|slot| match slot.borrow().as_ref() {
            Some(dsp) => {
                let fe = FastEngine::new(sharc_native::fast::default_backend(), &[])
                    .with_auto(threshold, min_insns as u64, false)
                    .with_follow(follow != 0);
                dsp.0.borrow_mut().set_fast(Some(Box::new(fe)));
                result(serde_json::json!({"fast": true}));
                0
            }
            None => failure("no coupled emulator loaded"),
        })
    }

    /// The desktop configuration of the fast tier: REGIONS and ROOTS are
    /// arrays of little-endian u32 entry PCs (`n` entries each; the static
    /// regions and the block-translator cluster roots of
    /// `tools/emulator/fast_regions.json`), and a `threshold` other than `u32::MAX`
    /// adds run-time selection as `digi_sharc_fast` does.
    #[cfg(feature = "fast-wasm")]
    #[unsafe(no_mangle)]
    pub extern "C" fn digi_sharc_fast_regions(
        regions: *const u32,
        n_regions: usize,
        roots: *const u32,
        n_roots: usize,
        threshold: u32,
        min_insns: u32,
        follow: u32,
    ) -> i32 {
        use sharc_native::fast::FastEngine;
        sharc_fast_wasm::host::register();
        // SAFETY: the page passes two buffers it allocated with `digi_alloc`.
        let (regions, roots) = unsafe {
            (
                std::slice::from_raw_parts(regions, n_regions),
                std::slice::from_raw_parts(roots, n_roots),
            )
        };
        ENGINE.with(|slot| match slot.borrow().as_ref() {
            Some(dsp) => {
                let mut fe = FastEngine::new(sharc_native::fast::default_backend(), regions)
                    .with_clusters(roots)
                    .with_follow(follow != 0);
                if threshold != u32::MAX {
                    fe = fe.with_auto(threshold, min_insns as u64, false);
                }
                dsp.0.borrow_mut().set_fast(Some(Box::new(fe)));
                result(serde_json::json!({"fast": true, "regions": n_regions, "roots": n_roots}));
                0
            }
            None => failure("no coupled emulator loaded"),
        })
    }

    /// `helper_exec` for the kernels' `env.helper` import: the page links the
    /// import to this export (a wasm-to-wasm call). REC is a record pointer of
    /// this core's memory.
    #[cfg(feature = "fast-wasm")]
    #[unsafe(no_mangle)]
    pub extern "C" fn digi_helper_exec(st: *mut u8, rec: u32) -> u32 {
        sharc_native::fast::helper::helper_exec(st, rec as u64)
    }

    /// JSON: the fast tier's report (regions armed, built, refused, calls) and
    /// the engine's tier counters.
    #[cfg(feature = "fast-wasm")]
    #[unsafe(no_mangle)]
    pub extern "C" fn digi_sharc_fast_report() -> i32 {
        ENGINE.with(|slot| match slot.borrow().as_ref() {
            Some(dsp) => {
                let e = dsp.0.borrow();
                let t = e.tier_counters();
                result(serde_json::json!({
                    "report": e.fast.as_ref().map(|f| f.report()),
                    "icount": t.icount, "interpreted": t.interpreted, "block_instructions": t.block_instructions,
                    "fast_insns": t.fast_insns, "fast_calls": t.fast_calls, "idle_skipped": t.idle_skipped,
                }));
                0
            }
            None => failure("no coupled emulator loaded"),
        })
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

    /// The number of AOT blocks in the linked generated core (0: the generic core).
    #[unsafe(no_mangle)]
    pub extern "C" fn digi_sharc_aot_blocks() -> u32 {
        sharc_native::aot_block_count() as u32
    }
}
