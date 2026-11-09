//! Counters and profiles the DSP engine reports, apart from the engine so that two instances of the
//! engine (one per generated core) return the same types.

/// What the run-time model gate decided (diagnostic counters).
#[derive(Clone, Copy, Debug)]
pub struct ModelStats {
    /// Block calls made with a model on.
    pub gated: u64,
    /// Dispatch attempts at a block the generator did not class model-safe.
    pub unsafe_block: u64,
    /// Attempts refused because an interrupt source was latched and enabled
    /// (it is taken, or deferred by a delay slot or loop, by the interpreter).
    pub irq_deferred: u64,
    /// Attempts refused because the core timer was within one tick of expiry
    /// or a bank or PC-stack change was still in flight.
    pub timer_gated: u64,
    /// Block exits through TRAP_BLOCK_MODEL (a peripheral store).
    pub mmr_exits: u64,
    /// Attempts refused because the loaded code differs from the code the
    /// blocks were generated from.
    pub code_mismatch: u64,
    /// `verify_code` runs (a write to a code-watched page since the last
    /// check), the wall time they took, and the interpreted instructions at
    /// or above the end of the generated image's code (appended module code).
    pub code_verifies: u64,
    pub code_verify_ns: u64,
    pub interp_appended: u64,
    /// Interrupts the engine actually entered (`_enter_interrupt` succeeded).
    pub irq_taken: u64,
    /// `irq_taken` by source, indexed by the IRPTL/IMASK bit number
    /// (`mask.trailing_zeros()`; the entry takes one source bit at a time).
    pub irq_by_vector: [u64; 64],
}

impl Default for ModelStats {
    fn default() -> Self {
        ModelStats {
            gated: 0,
            unsafe_block: 0,
            irq_deferred: 0,
            timer_gated: 0,
            mmr_exits: 0,
            code_mismatch: 0,
            code_verifies: 0,
            code_verify_ns: 0,
            interp_appended: 0,
            irq_taken: 0,
            irq_by_vector: [0; 64],
        }
    }
}

/// Cumulative instruction counts by execution tier (cheap to read: the
/// engine keeps them anyway). `block_instructions` includes the fast tier's.
#[derive(Clone, Debug, Default)]
pub struct TierCounters {
    pub icount: u64,
    /// Instructions replayed analytically by the idle-loop skip.
    pub idle_skipped: u64,
    pub idle_skips: u64,
    /// Instructions run one at a time by the interpreter.
    pub interpreted: u64,
    pub block_entries: u64,
    pub block_instructions: u64,
    pub block_traps: u64,
    pub traps: u64,
    /// Fast-tier counters (all zero without a fast tier).
    pub fast_present: bool,
    pub fast_insns: u64,
    pub fast_runs: u64,
    pub fast_calls: u64,
    pub fast_built: u64,
    pub fast_refused: u64,
    pub fast_armed: u64,
    pub fast_compile_ns: u64,
    pub fast_build_ns: u64,
    pub fast_declined: [u64; 5],
    /// Code-identity checks (see `ModelStats`): `Mem::code_gen`, the checks
    /// that ran, their wall time, the dispatches they refused, and the
    /// interpreted instructions in appended code.
    pub code_gen: u64,
    pub code_verifies: u64,
    pub code_verify_ns: u64,
    pub code_mismatch: u64,
    pub interp_appended: u64,
    /// `SHARC_TIER_TIME=1`: host ticks (see `tick_hz`) spent in each tier, in
    /// order: interpreter, generated blocks, fast tier, idle skip, before the
    /// first tier of a step call. The time between two tier calls (dispatch,
    /// models) counts for the tier that ran before it. All zero when off.
    pub tt_on: bool,
    pub tt_ticks: [u64; 5],
    pub tt_hz: u64,
}

/// Cumulative cost of dispatching one entry PC in a run-time tier.
///
/// `instructions` is the `icount` delta across the invocation. A fast-tier
/// call is recorded even when it returns `None` or makes no progress.
#[derive(Clone, Copy, Debug, Default)]
pub struct RegionCost {
    pub calls: u64,
    pub zero_progress_calls: u64,
    pub instructions: u64,
    pub wall_ns: u64,
}

/// Cumulative costs for the two dispatch tiers at one entry PC.
#[derive(Clone, Copy, Debug, Default)]
pub struct RegionEntryProfile {
    pub fast: RegionCost,
    pub aot: RegionCost,
}

/// Sparse pages over the SHARC+'s 24-bit short-word PC space.
#[derive(Clone, Debug)]
pub struct RegionProfile {
    pages: Vec<Option<Box<[RegionEntryProfile; 256]>>>,
}

impl Default for RegionProfile {
    fn default() -> Self {
        Self {
            pages: vec![None; 1 << 16],
        }
    }
}

impl RegionProfile {
    pub fn record(&mut self, pc: u32, fast: bool, instructions: u64, wall_ns: u64) {
        let pc = (pc & 0x00ff_ffff) as usize;
        let page = &mut self.pages[pc >> 8];
        let entry = &mut page.get_or_insert_with(|| Box::new([RegionEntryProfile::default(); 256]))
            [pc & 0xff];
        let cost = if fast {
            &mut entry.fast
        } else {
            &mut entry.aot
        };
        cost.calls = cost
            .calls
            .checked_add(1)
            .expect("region profile calls overflow");
        if instructions == 0 {
            cost.zero_progress_calls = cost
                .zero_progress_calls
                .checked_add(1)
                .expect("region profile zero-progress calls overflow");
        }
        cost.instructions = cost
            .instructions
            .checked_add(instructions)
            .expect("region profile instructions overflow");
        cost.wall_ns = cost
            .wall_ns
            .checked_add(wall_ns)
            .expect("region profile wall time overflow");
    }

    /// Entries with at least one measured dispatch, in short-word PC order.
    pub fn entries(&self) -> impl Iterator<Item = (u32, &RegionEntryProfile)> {
        self.pages
            .iter()
            .enumerate()
            .flat_map(|(page_index, page)| {
                page.iter().flat_map(move |entries| {
                    entries
                        .iter()
                        .enumerate()
                        .filter_map(move |(offset, entry)| {
                            (entry.fast.calls != 0 || entry.aot.calls != 0)
                                .then_some((((page_index << 8) | offset) as u32, entry))
                        })
                })
            })
    }
}
