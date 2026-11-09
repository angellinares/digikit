// The engine glue of one generated core: `sharc_native` names that core's engine crate. Included by
// sharc_peer.rs once per core (module `engine_dn2`, and `second` with the `second-core` feature), so a
// binary can run either device's DSP; the types they return come from sharc-stats and are shared.

fn ddr_monitor_command(
    engine: &mut sharc_native::Engine,
    command: DdrMonitorCommand,
) -> Result<DdrMonitorStatus, String> {
    use crate::ddr_monitor::{DdrMonitorHit, DdrMonitorKind, DdrMonitorRange};
    use sharc_native::mem::DdrAccessKind;
    command.validate()?;
    match command {
        DdrMonitorCommand::Arm(range) => engine.set_ddr_monitor(Some((range.start, range.end))),
        DdrMonitorCommand::Disarm => engine.set_ddr_monitor(None),
        DdrMonitorCommand::Clear => {
            engine.s.mem.clear_ddr_access();
            Ok(())
        }
        DdrMonitorCommand::Status => Ok(()),
    }
    .map_err(|_| "invalid DDR monitor range".to_string())?;
    // Metadata only: peeking at memory here would latch the diagnostic itself.
    let range = engine
        .s
        .mem
        .ddr_monitor_range()
        .map(|(start, end)| DdrMonitorRange { start, end });
    let first = engine.s.mem.first_ddr_access().map(|hit| DdrMonitorHit {
        address: hit.address,
        width: hit.width,
        kind: match hit.kind {
            DdrAccessKind::CpuRead => DdrMonitorKind::CpuRead,
            DdrAccessKind::CpuWrite => DdrMonitorKind::CpuWrite,
            DdrAccessKind::StorageRead => DdrMonitorKind::StorageRead,
            DdrAccessKind::StorageWrite => DdrMonitorKind::StorageWrite,
        },
    });
    Ok(DdrMonitorStatus {
        active_core_profile: sharc_native::gen_image_name().to_string(),
        dsp_icount: engine.s.icount,
        armed: range.is_some(),
        range,
        first,
    })
}

/// The native engine as a [`DspEngine`].
pub struct NativeDsp(pub sharc_native::Engine);

impl DspEngine for NativeDsp {
    fn spi2_exchange(&mut self, frame: &[u8]) -> Result<Vec<u8>, String> {
        self.0
            .spi2_exchange(frame)
            .map_err(|t| format!("spi2_exchange trap {}", sharc_native::trap_name(t)))
    }
    fn lp0_ready(&self) -> bool {
        self.0.lp0_ready()
    }
    fn lp0_transfer(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.0
            .lp0_transfer(bytes)
            .map_err(|t| format!("lp0_transfer trap {}", sharc_native::trap_name(t)))
    }
    fn step(&mut self, n: u32) -> u32 {
        self.0.step(n)
    }
    fn step_until_in(&mut self, n: u32, range: (u32, u32)) -> Option<u32> {
        Some(self.0.step_until_in(n, range.0, range.1))
    }
    fn step_until_in_aligned(
        &mut self,
        n: u32,
        range: (u32, u32),
        chunk: u32,
        base_icount: u64,
    ) -> Option<u32> {
        Some(
            self.0
                .step_until_in_aligned(n, range.0, range.1, chunk, base_icount),
        )
    }
    fn sport_block(&mut self) -> Result<Option<Vec<u8>>, String> {
        self.0
            .sport_block(None)
            .map_err(|t| format!("sport_block trap {}", sharc_native::trap_name(t)))
    }
    fn pc(&self) -> u32 {
        self.0.s.pc_sw as u32
    }
    fn halt_reason(&self) -> Option<String> {
        self.0.halt.clone()
    }
    fn export(&self) -> Vec<u8> {
        self.0.export()
    }
    fn icount(&self) -> u64 {
        self.0.s.icount
    }
    fn tier_counters(&self) -> Option<sharc_native::TierCounters> {
        Some(self.0.tier_counters())
    }
    fn model_stats(&self) -> Option<sharc_native::ModelStats> {
        Some(self.0.model_stats)
    }
    fn set_region_profile(&mut self, enabled: bool) {
        self.0.set_region_profile(enabled);
    }
    fn region_profile(&self) -> Option<sharc_native::RegionProfile> {
        self.0.region_profile()
    }
    fn apply_dsp_patch(&mut self, patch: &crate::dsp_patch::DspPatch) -> Result<(), String> {
        apply_dsp_patch(&mut self.0, patch)
    }
    fn set_execution_profile(&mut self, enabled: bool) -> bool {
        self.0.set_option(26, enabled as i64) == 0
    }
    fn ddr_monitor(&mut self, command: DdrMonitorCommand) -> Result<DdrMonitorStatus, String> {
        ddr_monitor_command(&mut self.0, command)
    }
    fn peek_word(&self, addr: u32) -> Option<u32> {
        self.0.peek(addr as u64, 4).ok().flatten()
    }
    fn execution_profile(&self) -> Option<ExecutionProfile> {
        Some(ExecutionProfile {
            maps: [
                self.0.profile_text(0)?,
                self.0.profile_text(1)?,
                self.0.profile_text(2)?,
                self.0.profile_text(3)?,
                self.0.profile_text(4)?,
            ],
        })
    }
}

/// A native engine that the host keeps a handle to, so it can export the
/// canonical state (coupled snapshots) while the peer owns the engine.
#[derive(Clone)]
pub struct SharedDsp(pub Rc<RefCell<sharc_native::Engine>>);

impl SharedDsp {
    pub fn new(e: sharc_native::Engine) -> Self {
        Self(Rc::new(RefCell::new(e)))
    }
    /// Canonical state blob of the engine right now.
    pub fn export(&self) -> Vec<u8> {
        self.0.borrow().export()
    }
}

impl DspEngine for SharedDsp {
    fn spi2_exchange(&mut self, frame: &[u8]) -> Result<Vec<u8>, String> {
        self.0
            .borrow_mut()
            .spi2_exchange(frame)
            .map_err(|t| format!("spi2_exchange trap {}", sharc_native::trap_name(t)))
    }
    fn lp0_ready(&self) -> bool {
        self.0.borrow().lp0_ready()
    }
    fn lp0_transfer(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.0
            .borrow_mut()
            .lp0_transfer(bytes)
            .map_err(|t| format!("lp0_transfer trap {}", sharc_native::trap_name(t)))
    }
    fn step(&mut self, n: u32) -> u32 {
        self.0.borrow_mut().step(n)
    }
    fn step_until_in(&mut self, n: u32, range: (u32, u32)) -> Option<u32> {
        Some(self.0.borrow_mut().step_until_in(n, range.0, range.1))
    }
    fn step_until_in_aligned(
        &mut self,
        n: u32,
        range: (u32, u32),
        chunk: u32,
        base_icount: u64,
    ) -> Option<u32> {
        Some(
            self.0
                .borrow_mut()
                .step_until_in_aligned(n, range.0, range.1, chunk, base_icount),
        )
    }
    fn sport_block(&mut self) -> Result<Option<Vec<u8>>, String> {
        self.0
            .borrow_mut()
            .sport_block(None)
            .map_err(|t| format!("sport_block trap {}", sharc_native::trap_name(t)))
    }
    fn pc(&self) -> u32 {
        self.0.borrow().s.pc_sw as u32
    }
    fn halt_reason(&self) -> Option<String> {
        self.0.borrow().halt.clone()
    }
    fn export(&self) -> Vec<u8> {
        self.0.borrow().export()
    }
    fn icount(&self) -> u64 {
        self.0.borrow().s.icount
    }
    fn tier_counters(&self) -> Option<sharc_native::TierCounters> {
        Some(self.0.borrow().tier_counters())
    }
    fn model_stats(&self) -> Option<sharc_native::ModelStats> {
        Some(self.0.borrow().model_stats)
    }
    fn set_region_profile(&mut self, enabled: bool) {
        self.0.borrow_mut().set_region_profile(enabled);
    }
    fn region_profile(&self) -> Option<sharc_native::RegionProfile> {
        self.0.borrow().region_profile()
    }
    fn set_execution_profile(&mut self, enabled: bool) -> bool {
        self.0.borrow_mut().set_option(26, enabled as i64) == 0
    }
    fn ddr_monitor(&mut self, command: DdrMonitorCommand) -> Result<DdrMonitorStatus, String> {
        ddr_monitor_command(&mut self.0.borrow_mut(), command)
    }
    fn peek_word(&self, addr: u32) -> Option<u32> {
        self.0.borrow().peek(addr as u64, 4).ok().flatten()
    }
    fn execution_profile(&self) -> Option<ExecutionProfile> {
        let e = self.0.borrow();
        Some(ExecutionProfile {
            maps: [
                e.profile_text(0)?,
                e.profile_text(1)?,
                e.profile_text(2)?,
                e.profile_text(3)?,
                e.profile_text(4)?,
            ],
        })
    }
}

/// Remove two sources of unknown ("taint") state from a freshly imported DN2
/// state so that code reading them stays on the fast tiers. Both are
/// modelling choices.
///
/// 1. Special registers (MRF, MRB, MSF, MSB, BFFWRP, BFF_HI, BFF_LO) that are
///    only partly known in the snapshot become fully known: known bits are
///    kept, unknown bits are 0. The structural MODE1STK, PCSTK and the stack
///    mirrors are not touched.
/// 2. Unmapped low-alias space: the absent bytes of [0x28000004, 0x28000100)
///    read as known zero instead of unknown. This is a modelling choice for
///    memory the snapshot does not hold; the owner decided it reads zero.
fn concretise_import(e: &mut sharc_native::Engine) {
    use sharc_native::rt::{Spec, MR, MR_MASK, V};
    for i in 0..e.s.special.len() {
        let present = e.s.special_present[i];
        let new = match e.s.special[i] {
            Spec::V(v) if present && v.is_c() => continue,
            Spec::V(v) => Spec::V(V::c(v.b as _)),
            Spec::M(m) if m.known() => continue,
            Spec::M(m) => Spec::M(MR::new(MR_MASK, m.bits)),
        };
        e.s.special[i] = new;
        e.s.special_present[i] = true;
    }
    for a in 0x2800_0004u64..0x2800_0100 {
        if matches!(e.peek(a, 1), Ok(None)) {
            e.poke(a, &[0], 1);
        }
    }
}

/// Build the DN2 continuation engine from a packed image blob
/// (`tools/sharc_dev/sharc_pack_image.py`) and a canonical state blob, with the option
/// set the audio runs use. `clock_base` seeds the diagnostic instruction
/// clock (EMUCLK).
pub fn open_dn2_engine(
    image: &[u8],
    state: &[u8],
    clock_base: u64,
) -> Result<sharc_native::Engine, String> {
    open_dn2_engine_patched(image, state, clock_base, None)
}

/// Overlay a DSP patch onto an engine's memory (`crate::dsp_patch`). Every
/// `was` guard is checked before any byte of that op is written.
pub fn apply_dsp_patch(
    e: &mut sharc_native::Engine,
    patch: &crate::dsp_patch::DspPatch,
) -> Result<(), String> {
    use crate::dsp_patch::PatchOp;
    fn read(e: &sharc_native::Engine, addr: u32, n: usize) -> Result<Vec<u8>, String> {
        (0..n)
            .map(|k| match e.peek(u64::from(addr) + k as u64, 1) {
                Ok(Some(b)) => Ok(b as u8),
                _ => Err(format!(
                    "DSP patch: {:#x} is unknown memory",
                    addr + k as u32
                )),
            })
            .collect()
    }
    fn write(e: &mut sharc_native::Engine, addr: u32, bytes: &[u8]) -> Result<(), String> {
        // New code or data in L2 that nothing loaded: `poke` on an absent
        // address would store it at the short-word alias, where neither the
        // fetch nor a plain data read looks (spike 0.7).
        if (0x2000_0000..0x2010_0000).contains(&addr)
            && !matches!(e.peek(u64::from(addr), 1), Ok(Some(_)))
        {
            e.poke_raw(addr, bytes);
            return Ok(());
        }
        if e.poke(u64::from(addr), bytes, 1) as usize != bytes.len() {
            return Err(format!("DSP patch: write at {addr:#x} refused"));
        }
        Ok(())
    }
    for op in &patch.ops {
        match op {
            PatchOp::Pm { pc, words, was } => {
                let n = words.len() * 2;
                let low = 0x2800_0000u32.wrapping_add(pc.wrapping_mul(2));
                let alias = 0x2000_0000u32.wrapping_add(pc.wrapping_sub(0xb8_0000).wrapping_mul(2));
                let addr = if (0xb8_0000..0xc0_0000).contains(pc)
                    && !matches!(e.peek(u64::from(low), 1), Ok(Some(_)))
                {
                    alias
                } else {
                    low
                };
                // A short word is little endian in memory; the first
                // instruction word is the most significant (written first).
                if let Some(old) = was {
                    let have = read(e, addr, n)?;
                    let want: Vec<u8> = old.iter().flat_map(|w| w.to_le_bytes()).collect();
                    if have != want {
                        return Err(format!(
                            "DSP patch: PM {pc:#x} holds {have:02x?}, patch expects {want:02x?}"
                        ));
                    }
                }
                let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
                write(e, addr, &bytes)?;
            }
            PatchOp::Bytes { addr, bytes, was } => {
                if let Some(old) = was {
                    let have = read(e, *addr, old.len())?;
                    if &have != old {
                        return Err(format!(
                            "DSP patch: {addr:#x} holds {have:02x?}, patch expects {old:02x?}"
                        ));
                    }
                }
                write(e, *addr, bytes)?;
            }
        }
    }
    e.invalidate_runtime_decode();
    Ok(())
}

/// `open_dn2_engine` with an optional DSP patch overlaid on the imported
/// state, so a snapshot's DSP runs the patched code and data. Generated AOT
/// blocks re-check their code range (per range, with a region-entry table in
/// the generated core) and the fast tier re-verifies its code words, so both
/// stay on for the code a patch does not touch.
pub fn open_dn2_engine_patched(
    image: &[u8],
    state: &[u8],
    clock_base: u64,
    patch: Option<&crate::dsp_patch::DspPatch>,
) -> Result<sharc_native::Engine, String> {
    open_engine_patched(image, state, clock_base, patch, DN2_IDLE)
}

/// `open_dn2_engine_patched` for any firmware: `idle` is the RTOS idle loop
/// the exact idle skip (options 23-25) is armed on.
pub fn open_engine_patched(
    image: &[u8],
    state: &[u8],
    clock_base: u64,
    patch: Option<&crate::dsp_patch::DspPatch>,
    idle: IdleLoop,
) -> Result<sharc_native::Engine, String> {
    let mut e = sharc_native::Engine::from_image(image).map_err(|c| format!("image error {c}"))?;
    e.import(state)
        .map_err(|c| format!("state import error {c}"))?;
    // 4 runtime decode, 5 instruction clock, 6 clock base, 9 software IRQs,
    // 10 explicit memory model off, 12 assume NW32,
    // 21 core timer, 22 peripheral model (19/20 come from the state).
    let mut options = vec![
        (4, 1),
        (5, 1),
        (6, clock_base as i64),
        (9, 1),
        (10, 0),
        (12, 1),
        (21, 1),
        (22, 1),
    ];
    // 23-25 idle-loop skip (exact: see docs/findings/07-emulator.md);
    // DSP_IDLE_SKIP=0 turns it off.
    if std::env::var("DSP_IDLE_SKIP").as_deref() != Ok("0") {
        options.extend([
            (23, idle.head as i64),
            (24, idle.range.0 as i64),
            (25, idle.range.1 as i64),
        ]);
    }
    for (k, v) in options {
        if e.set_option(k, v) != 0 {
            return Err(format!("set_option {k} rejected"));
        }
    }
    // After the options: the explicit memory model (10) decides whether an
    // absent byte reads as unknown.
    concretise_import(&mut e);
    if let Some(patch) = patch {
        apply_dsp_patch(&mut e, patch)?;
    }
    Ok(e)
}
