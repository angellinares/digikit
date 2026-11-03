//! Which generated DSP core runs a firmware's DSP.
//!
//! A build holds one generated core (`SHARC_GEN_DIR`), or two with the `second-core` feature
//! (`SHARC_GEN_DIR_2` is the second one's slot; the names do not say which device a core is for).
//! Each core knows the DSP image it was made for (`dn2-1.11`, `dt2-1.16`), and the core whose image
//! name starts with the firmware's device id runs that firmware's DSP. A device without a core of
//! its own gets the first core, as a single-core build always did (the image's blocks then do not
//! match and the engine interprets).

use std::{
    cell::RefCell,
    rc::Rc,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use elektron_native_boot::{
    dsp_patch::DspPatch,
    sharc_peer::{DspEngine, IdleLoop, Shared, ThreadedHandle, ThreadedPeer, open_engine_patched},
};

/// The generated core a DSP session runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Core {
    First,
    #[cfg(feature = "second-core")]
    Second,
}

impl Core {
    /// The core for a device id (`dn2`, `dt2`): the one made for that device's image.
    pub fn for_device(device: &str) -> Self {
        #[cfg(feature = "second-core")]
        if !sharc_native::gen_image_name().starts_with(device)
            && sharc_native_2::gen_image_name().starts_with(device)
        {
            return Self::Second;
        }
        let _ = device;
        Self::First
    }

    /// The name of the DSP image the core was made for.
    pub fn image_name(self) -> &'static str {
        match self {
            Self::First => sharc_native::gen_image_name(),
            #[cfg(feature = "second-core")]
            Self::Second => sharc_native_2::gen_image_name(),
        }
    }
}

/// Everything `open_engine_patched` needs; it runs on the worker thread.
pub struct EngineInputs {
    pub image: Vec<u8>,
    pub state: Vec<u8>,
    pub clock_base: u64,
    pub patch: Option<DspPatch>,
    pub idle: IdleLoop,
}

/// The DSP worker of `core`, with its DSPI2 peer.
pub fn spawn_dsp(
    core: Core,
    inputs: EngineInputs,
    period: u32,
    link_timing: bool,
    fast_tier: bool,
) -> Result<(ThreadedPeer, ThreadedHandle, Rc<RefCell<Shared>>), String> {
    let EngineInputs {
        image,
        state,
        clock_base,
        patch,
        idle,
    } = inputs;
    match core {
        Core::First => {
            if fast_tier {
                sharc_fast_cl::register();
            }
            ThreadedPeer::spawn_with_timing(
                move || {
                    open_engine_patched(&image, &state, clock_base, patch.as_ref(), idle)
                        .map(elektron_native_boot::sharc_peer::NativeDsp)
                },
                period,
                idle.range,
                link_timing,
            )
        }
        #[cfg(feature = "second-core")]
        Core::Second => {
            if fast_tier {
                sharc_fast_cl_2::register();
            }
            ThreadedPeer::spawn_with_timing(
                move || {
                    elektron_native_boot::sharc_peer::second::open_engine_patched(
                        &image,
                        &state,
                        clock_base,
                        patch.as_ref(),
                        idle,
                    )
                    .map(elektron_native_boot::sharc_peer::second::NativeDsp)
                },
                period,
                idle.range,
                link_timing,
            )
        }
    }
}

/// Initialize a changed DSP before attaching the ColdFire. Exporting here keeps
/// the worker's restore path ordinary: its input has no patch left to apply.
pub fn initialize_cold_dsp(
    core: Core,
    inputs: EngineInputs,
    cancel: &AtomicBool,
) -> Result<EngineInputs, String> {
    let started = Instant::now();
    validate_cold_state(&inputs.image, &inputs.state, inputs.idle)?;
    let mut engine: Box<dyn DspEngine> = match core {
        Core::First => Box::new(elektron_native_boot::sharc_peer::NativeDsp(
            open_engine_patched(&inputs.image, &inputs.state, 0, None, inputs.idle)?,
        )),
        #[cfg(feature = "second-core")]
        Core::Second => Box::new(elektron_native_boot::sharc_peer::second::NativeDsp(
            elektron_native_boot::sharc_peer::second::open_engine_patched(
                &inputs.image,
                &inputs.state,
                0,
                None,
                inputs.idle,
            )?,
        )),
    };
    finish_cold_initialization(inputs, engine.as_mut(), cancel, || {
        started.elapsed() >= Duration::from_secs(120)
    })
}

fn validate_cold_state(image: &[u8], state: &[u8], idle: IdleLoop) -> Result<(), String> {
    // Validate before open_engine_patched concretises unknown memory or applies
    // the patch. A cold.state is canonical reset state, with no executed stores.
    let mut raw =
        sharc_native::Engine::from_image(image).map_err(|e| format!("DSP image error {e}"))?;
    raw.import(state)
        .map_err(|e| format!("invalid raw DSP cold.state ({e}); regenerate ready inputs with `mise run emu:ready -- <device> --force`"))?;
    let s = &raw.s;
    if !s.mem.dirty_bytes().is_empty()
        || s.pending.is_some()
        || !s.loops.items().is_empty()
        || !s.call_stack.items().is_empty()
        || !s.pc_stack.items().is_empty()
        || !s.status_stack.items().is_empty()
        || (idle.range.0..=idle.range.1).contains(&(s.pc_sw as u32))
    {
        return Err("DSP cold.state is already initialized; regenerate ready inputs with `mise run emu:ready -- <device> --force`".into());
    }
    Ok(())
}

fn finish_cold_initialization(
    mut inputs: EngineInputs,
    engine: &mut dyn DspEngine,
    cancel: &AtomicBool,
    timed_out: impl FnMut() -> bool,
) -> Result<EngineInputs, String> {
    if let Some(patch) = inputs.patch.take() {
        engine.apply_dsp_patch(&patch)?;
    }
    run_cold_to_idle(engine, inputs.idle, cancel, 800_000_000, timed_out)?;
    inputs.state = engine.export();
    inputs.clock_base = engine.icount();
    Ok(inputs)
}

fn run_cold_to_idle(
    engine: &mut dyn DspEngine,
    idle: IdleLoop,
    cancel: &AtomicBool,
    limit: u64,
    mut timed_out: impl FnMut() -> bool,
) -> Result<(), String> {
    let mut done = 0u64;
    let mut rested = 0;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        if timed_out() {
            return Err("DSP initialization exceeded 120 seconds before idle".into());
        }
        if let Some(halt) = engine.halt_reason() {
            return Err(format!("DSP halted during initialization: {halt}"));
        }
        if done >= limit {
            return Err(format!(
                "DSP initialization did not reach idle after {done} instructions"
            ));
        }
        let n = engine.step((limit - done).min(250_000) as u32);
        done += u64::from(n);
        if let Some(halt) = engine.halt_reason() {
            return Err(format!("DSP halted during initialization: {halt}"));
        }
        if n == 0 {
            return Err("DSP initialization made no progress".into());
        }
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        if timed_out() {
            return Err("DSP initialization exceeded 120 seconds before idle".into());
        }
        if (idle.range.0..=idle.range.1).contains(&engine.pc()) {
            rested += 1;
            if rested == 8 {
                return Ok(());
            }
        } else {
            rested = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    struct Fake {
        pcs: VecDeque<u32>,
        pc: u32,
        steps: u64,
        halt: bool,
        stalled: bool,
        patches: usize,
        boot_value: u8,
    }
    impl Fake {
        fn new(pcs: impl IntoIterator<Item = u32>) -> Self {
            Self {
                pcs: pcs.into_iter().collect(),
                pc: 0,
                steps: 0,
                halt: false,
                stalled: false,
                patches: 0,
                boot_value: 0,
            }
        }
    }
    impl DspEngine for Fake {
        fn spi2_exchange(&mut self, _: &[u8]) -> Result<Vec<u8>, String> {
            unreachable!()
        }
        fn sport_block(&mut self) -> Result<Option<Vec<u8>>, String> {
            unreachable!()
        }
        fn step(&mut self, n: u32) -> u32 {
            if self.stalled {
                return 0;
            }
            self.pc = self.pcs.pop_front().unwrap_or(self.pc);
            self.steps += u64::from(n);
            self.boot_value = 42;
            n
        }
        fn pc(&self) -> u32 {
            self.pc
        }
        fn halt_reason(&self) -> Option<String> {
            self.halt.then(|| "test halt".into())
        }
        fn apply_dsp_patch(&mut self, _: &DspPatch) -> Result<(), String> {
            self.patches += 1;
            self.boot_value = 0;
            Ok(())
        }
        fn export(&self) -> Vec<u8> {
            vec![self.boot_value]
        }
        fn icount(&self) -> u64 {
            self.steps
        }
    }
    const IDLE: IdleLoop = IdleLoop {
        range: (10, 20),
        head: 10,
    };

    #[test]
    fn raw_state_validation_rejects_initialized_or_malformed_inputs() {
        let mut raw = sharc_native::Engine::from_image(&[]).unwrap();
        raw.s.pc_sw = 42;
        validate_cold_state(&[], &raw.export(), IDLE).unwrap();
        raw.s.pc_sw = 10;
        assert!(
            validate_cold_state(&[], &raw.export(), IDLE)
                .unwrap_err()
                .contains("already initialized")
        );
        raw.s.pc_sw = 42;
        raw.poke_raw(0x2000_0000, &[1]);
        assert!(
            validate_cold_state(&[], &raw.export(), IDLE)
                .unwrap_err()
                .contains("already initialized")
        );
        assert!(
            validate_cold_state(&[], b"DT2DSP01", IDLE)
                .unwrap_err()
                .contains("regenerate ready inputs")
        );
    }

    #[test]
    fn idle_observations_must_be_consecutive() {
        let mut e = Fake::new([10, 10, 0, 10, 10, 10, 10, 10, 10, 10, 20]);
        run_cold_to_idle(&mut e, IDLE, &AtomicBool::new(false), 3_000_000, || false).unwrap();
        assert_eq!(e.steps, 11 * 250_000);
    }

    #[test]
    fn failed_initialization_never_succeeds_at_idle() {
        let cancel = AtomicBool::new(false);
        let mut e = Fake::new([10]);
        e.halt = true;
        assert!(
            run_cold_to_idle(&mut e, IDLE, &cancel, 3_000_000, || false)
                .unwrap_err()
                .contains("halt")
        );
        e.halt = false;
        e.stalled = true;
        assert!(
            run_cold_to_idle(&mut e, IDLE, &cancel, 3_000_000, || false)
                .unwrap_err()
                .contains("no progress")
        );
        e.stalled = false;
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(
            run_cold_to_idle(&mut e, IDLE, &cancel, 3_000_000, || false).unwrap_err(),
            "cancelled"
        );
        cancel.store(false, Ordering::Relaxed);
        assert!(
            run_cold_to_idle(&mut e, IDLE, &cancel, 3_000_000, || true)
                .unwrap_err()
                .contains("120 seconds")
        );
        assert!(
            run_cold_to_idle(&mut e, IDLE, &cancel, 250_000, || false)
                .unwrap_err()
                .contains("did not reach idle")
        );
    }

    #[test]
    fn patch_is_consumed_before_boot_and_initialized_data_survives_attachment() {
        let mut e = Fake::new([10]);
        let inputs = EngineInputs {
            image: vec![],
            state: vec![0],
            clock_base: 0,
            patch: Some(DspPatch::default()),
            idle: IDLE,
        };
        let ready =
            finish_cold_initialization(inputs, &mut e, &AtomicBool::new(false), || false).unwrap();
        assert_eq!(e.patches, 1);
        assert_eq!(ready.state, vec![42]);
        assert_eq!(ready.clock_base, e.steps);
        assert!(
            ready.patch.is_none(),
            "worker must only import initialized state"
        );
    }
}
