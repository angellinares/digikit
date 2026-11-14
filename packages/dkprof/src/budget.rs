//! The machine budget: how many DSP instructions per block a module may take on a device, and the selection rule that the broker, the packer
//! and the workbench share (docs/plans/profiling.md section 6, docs/plans/sdk.md section 9).
//!
//! The unit is **instructions** (SHARC+ instructions the emulator counts, not hardware cycles) until the device calibration (profiling item
//! P9). A block is `dsp_clock_hz / dsp_blocks_per_second` cycles. The stock firmware with a held note takes `stock_busy` instructions of it.
//! A patched image stays under [`DEFAULT_BUDGET_PCT`] of the block, with instructions projected to cycles by the "high" factor of
//! `devices/cycle-factors.json`; the 15 percent left over are the safety margin until P9. What is left between the stock load and that limit is
//! the **headroom**.
//!
//! No device number is written here: [`DeviceCost`] is filled from `devices/*.toml` (`[board]`, `[adapter.machine] voices`) and
//! `devices/cycle-factors.json` ([`parse_cycle_factors`]).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// A patched image stays under this share of the block, projected with the "high" factor.
pub const DEFAULT_BUDGET_PCT: f64 = 85.0;

/// What the budget needs of one device.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeviceCost {
    pub device: String,
    pub dsp_clock_hz: u64,
    pub dsp_blocks_per_second: u64,
    /// Voices of the machine adapter (`[adapter.machine] voices`).
    pub voices: u64,
    /// Real cycles per emulator instruction, the high bound (`devices/cycle-factors.json`).
    pub dsp_high: f64,
    /// Instructions per block of the stock firmware with one held note.
    pub stock_busy: u64,
}

/// The cycle factors of one device, from `devices/cycle-factors.json`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CycleFactors {
    pub nominal: f64,
    pub high: f64,
    pub stock_busy: u64,
}

/// `devices/cycle-factors.json`: device short name to its DSP factors.
pub fn parse_cycle_factors(json: &str) -> Result<BTreeMap<String, CycleFactors>, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| format!("cycle-factors: {e}"))?;
    let devices = v["devices"]
        .as_object()
        .ok_or("cycle-factors: no `devices` table")?;
    let mut out = BTreeMap::new();
    for (name, d) in devices {
        let dsp = &d["dsp"];
        let num = |k: &str| {
            dsp[k]
                .as_f64()
                .ok_or_else(|| format!("cycle-factors: {name}.dsp.{k} is missing"))
        };
        out.insert(
            name.clone(),
            CycleFactors {
                nominal: num("nominal")?,
                high: num("high")?,
                stock_busy: dsp["stock_busy"]
                    .as_u64()
                    .ok_or_else(|| format!("cycle-factors: {name}.dsp.stock_busy is missing"))?,
            },
        );
    }
    Ok(out)
}

/// What a module may take, in instructions of one block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    /// One voice, a block that is not a note-on: the headroom shared by the device's voices.
    pub voice_block: u64,
    /// The block of a note-on: it may take the whole headroom once.
    pub note_on_block: u64,
    pub headroom: u64,
}

impl DeviceCost {
    /// Cycles of one block (whole cycles).
    pub fn block_cycles(&self) -> u64 {
        self.dsp_clock_hz / self.dsp_blocks_per_second
    }

    /// The stock firmware's load of a block, in percent, projected with the high factor.
    pub fn stock_pct(&self) -> f64 {
        100.0 * self.stock_busy as f64 * self.dsp_high / self.block_cycles() as f64
    }

    /// Instructions between the stock load and `budget_pct` of the block (0 when the stock load is already over it).
    pub fn headroom(&self, budget_pct: f64) -> u64 {
        let h =
            (budget_pct - self.stock_pct()) / 100.0 * self.block_cycles() as f64 / self.dsp_high;
        // truncation toward zero, as `int()` does
        if h > 0.0 { h as u64 } else { 0 }
    }

    /// The budget of one machine module: the headroom shared by the voices, and the whole headroom for a note-on block.
    pub fn budget(&self, budget_pct: f64) -> Budget {
        let headroom = self.headroom(budget_pct);
        Budget {
            voice_block: headroom / self.voices.max(1),
            note_on_block: headroom,
            headroom,
        }
    }

    /// The share of a block that `instructions` take, in percent, projected with the high factor.
    pub fn load_pct(&self, instructions: u64) -> f64 {
        100.0 * instructions as f64 * self.dsp_high / self.block_cycles() as f64
    }
}

/// Instructions of one effect process call: the headroom of the tightest device shared by `fx_max` effects.
pub fn fx_budget(devices: &[DeviceCost], budget_pct: f64, fx_max: u64) -> u64 {
    devices
        .iter()
        .map(|d| d.headroom(budget_pct))
        .min()
        .unwrap_or(0)
        / fx_max.max(1)
}

/// A measurement of a machine module against its budget.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MachineVerdict {
    pub ok: bool,
    pub failures: Vec<String>,
}

/// Compare the worst steady block of one voice and the worst note-on block (both the module's own instructions, instructions per block) with the
/// budget. The note-on block is checked on its own against the whole headroom, never added to the steady blocks of the other voices.
pub fn check_machine(b: &Budget, worst_block: u64, worst_note_on: u64) -> MachineVerdict {
    let mut failures = vec![];
    if worst_block > b.voice_block {
        failures.push(format!(
            "{worst_block} instructions in a block, budget {} per voice",
            b.voice_block
        ));
    }
    if worst_note_on > b.note_on_block {
        failures.push(format!(
            "{worst_note_on} instructions in a note-on block, budget {}",
            b.note_on_block
        ));
    }
    MachineVerdict {
        ok: failures.is_empty(),
        failures,
    }
}

/// The selection rule is one integer function for the runtime's boot planner, the DIGIKIT menu, `patchkit fit` and this crate (`dkrt_fit::check_selection`):
/// `max(machine costs) * voices + sum(effect costs) <= headroom`, in instructions per block, with the stock baseline and the safety margin already out of the headroom.
pub use dkrt_fit::{DspCheck, check_selection, dsp_own};

impl DeviceCost {
    /// `check_selection` for this device: its voices and its headroom at `budget_pct`.
    pub fn check_selection(&self, budget_pct: f64, machines: &[u32], fx: &[u32]) -> DspCheck {
        let clamp = |n: u64| u32::try_from(n).unwrap_or(u32::MAX);
        check_selection(
            clamp(self.voices),
            clamp(self.headroom(budget_pct)),
            machines,
            fx,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dn2() -> DeviceCost {
        DeviceCost {
            device: "dn2".into(),
            dsp_clock_hz: 1_000_000_000,
            dsp_blocks_per_second: 1500,
            voices: 16,
            dsp_high: 1.326075,
            stock_busy: 334_529,
        }
    }

    fn dt2() -> DeviceCost {
        DeviceCost {
            device: "dt2".into(),
            dsp_clock_hz: 1_000_000_000,
            dsp_blocks_per_second: 1500,
            voices: 16,
            dsp_high: 1.406521,
            stock_busy: 208_640,
        }
    }

    #[test]
    fn the_headroom_is_what_the_stock_load_leaves_under_the_limit() {
        let d = dn2();
        assert_eq!(d.block_cycles(), 666_666);
        let h = d.headroom(DEFAULT_BUDGET_PCT);
        // stock + headroom projected with the high factor is the limit (to rounding of the truncation)
        let pct = d.load_pct(d.stock_busy + h);
        assert!((pct - DEFAULT_BUDGET_PCT).abs() < 0.001, "{pct}");
        assert!(d.load_pct(d.stock_busy + h + 2) > DEFAULT_BUDGET_PCT);
        // a device whose stock load is over the limit has no headroom, not a negative one
        assert_eq!(d.headroom(10.0), 0);
        // the lighter stock firmware leaves more
        assert!(dt2().headroom(DEFAULT_BUDGET_PCT) > h);
    }

    #[test]
    fn the_voices_share_the_headroom_and_a_note_on_may_take_all_of_it() {
        let d = dn2();
        let b = d.budget(DEFAULT_BUDGET_PCT);
        assert_eq!(b.voice_block, b.headroom / 16);
        assert_eq!(b.note_on_block, b.headroom);
    }

    #[test]
    fn the_effect_budget_is_the_tightest_headroom_shared_by_the_chain() {
        let both = [dn2(), dt2()];
        let tight = dn2()
            .headroom(DEFAULT_BUDGET_PCT)
            .min(dt2().headroom(DEFAULT_BUDGET_PCT));
        assert_eq!(fx_budget(&both, DEFAULT_BUDGET_PCT, 4), tight / 4);
        assert_eq!(fx_budget(&[], DEFAULT_BUDGET_PCT, 4), 0);
    }

    #[test]
    fn a_machine_is_checked_per_voice_and_the_note_on_apart() {
        let b = dn2().budget(DEFAULT_BUDGET_PCT);
        assert!(check_machine(&b, b.voice_block, b.note_on_block).ok);
        let v = check_machine(&b, b.voice_block + 1, b.note_on_block + 1);
        assert!(!v.ok);
        assert_eq!(v.failures.len(), 2);
        assert!(v.failures[0].contains("per voice"), "{:?}", v.failures);
        assert!(v.failures[1].contains("note-on"), "{:?}", v.failures);
        // a note-on above the per-voice budget but within the headroom is fine
        assert!(check_machine(&b, 1, b.voice_block * 4).ok);
    }

    #[test]
    fn the_selection_rule_takes_the_largest_machine_for_every_voice_plus_the_effects() {
        let d = dn2();
        let h = d.headroom(DEFAULT_BUDGET_PCT) as u32;
        let each = h / 16;
        let ok = d.check_selection(DEFAULT_BUDGET_PCT, &[each / 2, each], &[]);
        assert_eq!(ok.own, each * 16);
        assert!(ok.fits);
        assert!(d.load_pct(d.stock_busy + u64::from(ok.own)) <= DEFAULT_BUDGET_PCT + 0.001);
        // one instruction per voice more, or an effect on top, does not fit
        assert!(!d.check_selection(DEFAULT_BUDGET_PCT, &[each + 1], &[]).fits);
        let with_fx = d.check_selection(DEFAULT_BUDGET_PCT, &[each], &[h - each * 16 + 1]);
        assert!(!with_fx.fits);
        assert!(d.load_pct(d.stock_busy + u64::from(with_fx.own)) > DEFAULT_BUDGET_PCT);
        // no module: only the stock baseline
        let none = d.check_selection(DEFAULT_BUDGET_PCT, &[], &[]);
        assert!(none.fits && none.own == 0);
    }

    #[test]
    fn the_cycle_factors_file_is_read_by_device() {
        let f = parse_cycle_factors(
            r#"{"devices": {"dn2": {"dsp": {"nominal": 1.1, "high": 1.3, "stock_busy": 100}}}}"#,
        )
        .unwrap();
        assert_eq!(f["dn2"].stock_busy, 100);
        assert!(parse_cycle_factors(r#"{"devices": {"x": {"dsp": {}}}}"#).is_err());
    }
}
