//! `patchkit budget`: the DSP budget of each device, in instructions per block (the rule is `dkprof::budget`).
//!
//! The numbers come from the device files (`[board]`, `[adapter.machine] voices`) and `devices/cycle-factors.json`, both embedded at build time.
//! `module:check` (tools/patchkit/pk_conform.py) reads this output, so the packer, the broker and the checker use one computation.

use crate::devdata;
use crate::error::*;
use dkprof::budget::{self, DEFAULT_BUDGET_PCT, DeviceCost};
use serde_json::{Value, json};

const CYCLE_FACTORS: &str = include_str!("../../../devices/cycle-factors.json");

/// The cost record of every device that has the data (a device file without `[board]` or cycle factors is left out).
pub fn devices() -> Result<Vec<DeviceCost>> {
    let factors = budget::parse_cycle_factors(CYCLE_FACTORS)?;
    let mut out = vec![];
    for d in devdata::all() {
        let (Some(clock), Some(blocks), Some(voices), Some(f)) = (
            d.dsp_clock_hz,
            d.dsp_blocks_per_second,
            d.machine_voices,
            factors.get(&d.short),
        ) else {
            continue;
        };
        out.push(DeviceCost {
            device: d.short.clone(),
            dsp_clock_hz: clock,
            dsp_blocks_per_second: blocks,
            voices,
            dsp_high: f.high,
            stock_busy: f.stock_busy,
        });
    }
    Ok(out)
}

/// The headroom of the device `short` in instructions per block at the default margin (`DEFAULT_BUDGET_PCT`), from its clock, block rate and voices and the cycle
/// factors of the same name; None when `devices/cycle-factors.json` has no entry for it. `patchkit gen` writes it into the device profile (`DSP_HEADROOM`), so the
/// broker on the device compares integers and does no float arithmetic.
pub fn headroom(short: &str, clock: u64, blocks: u64, voices: u64) -> Option<u64> {
    let f = *budget::parse_cycle_factors(CYCLE_FACTORS)
        .ok()?
        .get(short)?;
    Some(
        DeviceCost {
            device: short.to_string(),
            dsp_clock_hz: clock,
            dsp_blocks_per_second: blocks,
            voices,
            dsp_high: f.high,
            stock_busy: f.stock_busy,
        }
        .headroom(DEFAULT_BUDGET_PCT),
    )
}

fn flag_values<'a>(rest: &'a [String], name: &str) -> Vec<&'a str> {
    rest.windows(2)
        .filter(|w| w[0] == name)
        .map(|w| w[1].as_str())
        .collect()
}

/// The JSON answer: `{unit, budget_pct, devices: {SHORT: {...}}, fx_budget?}`.
pub fn budget_json(only: &[&str], fx_max: Option<u64>, pct: f64) -> Result<Value> {
    let mut all = devices()?;
    for o in only {
        if !all.iter().any(|d| d.device == *o) {
            return Err(err!(
                "budget: no cost data for device {o:?} (known: {})",
                all.iter()
                    .map(|d| d.device.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    // the effect budget is the tightest headroom of every device, whatever the selection printed
    let fx = fx_max.map(|n| budget::fx_budget(&all, pct, n));
    if !only.is_empty() {
        all.retain(|d| only.contains(&d.device.as_str()));
    }
    let mut devs = serde_json::Map::new();
    for d in &all {
        let b = d.budget(pct);
        devs.insert(
            d.device.clone(),
            json!({
                "block_cycles": d.block_cycles(), "voices": d.voices, "stock_busy": d.stock_busy, "dsp_high": d.dsp_high,
                "stock_pct": d.stock_pct(), "headroom": b.headroom, "voice_block": b.voice_block, "note_on_block": b.note_on_block,
            }),
        );
    }
    let mut v = json!({"unit": "instructions", "budget_pct": pct, "devices": devs});
    if let Some(fx) = fx {
        v["fx_budget"] = json!(fx);
        v["fx_max"] = json!(fx_max);
    }
    Ok(v)
}

pub fn run(rest: &[String]) -> Result<()> {
    let only = flag_values(rest, "--device");
    let fx_max = flag_values(rest, "--fx-max")
        .first()
        .map(|s| s.parse::<u64>().map_err(|_| err!("budget: --fx-max N")))
        .transpose()?;
    let pct = match flag_values(rest, "--budget-pct").first() {
        Some(s) => s
            .parse::<f64>()
            .map_err(|_| err!("budget: --budget-pct P"))?,
        None => DEFAULT_BUDGET_PCT,
    };
    println!("{}", budget_json(&only, fx_max, pct)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_device_with_a_board_has_a_budget_made_of_its_own_files() {
        let v = budget_json(&[], Some(4), DEFAULT_BUDGET_PCT).unwrap();
        let devs = v["devices"].as_object().unwrap();
        assert!(devs.contains_key("dt2") && devs.contains_key("dn2"));
        for d in devs.values() {
            let h = d["headroom"].as_u64().unwrap();
            assert!(h > 0);
            assert_eq!(
                d["voice_block"].as_u64().unwrap(),
                h / d["voices"].as_u64().unwrap()
            );
            assert_eq!(d["note_on_block"].as_u64().unwrap(), h);
        }
        let tight = devs
            .values()
            .map(|d| d["headroom"].as_u64().unwrap())
            .min()
            .unwrap();
        assert_eq!(v["fx_budget"].as_u64().unwrap(), tight / 4);
        // one device asked for: only it is printed, the effect budget still counts all of them
        let one = budget_json(&["dn2"], Some(4), DEFAULT_BUDGET_PCT).unwrap();
        assert_eq!(one["devices"].as_object().unwrap().len(), 1);
        assert_eq!(one["fx_budget"], v["fx_budget"]);
        assert!(budget_json(&["nope"], None, DEFAULT_BUDGET_PCT).is_err());
    }
}
