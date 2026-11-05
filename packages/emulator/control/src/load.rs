//! Live CPU meters: how busy the ColdFire and the SHARC+ are, in the emulator's model and as an estimate for the
//! real chip. Only counters the engines keep anyway are read, once per `status` poll; nothing runs on the hot path.
//!
//! The factors are `devices/cycle-factors.json` (one copy, shared with `tools/patchkit/pk_dsp_budget.py`):
//! derived from the public manuals' cycle tables and our own instruction counts, so the "real" values are
//! estimates, not measurements.
//!
//! - ColdFire, model: busy / total instructions, busy = interpreted (not the idle fast-forward, not the RAM clear).
//!   Real: busy instructions per emulated second x busy CPI / 264 MHz. Emulated time is total / 132 M.
//! - DSP, model: busy instructions per block / 666,667 cycles. Real: the same times the device's cycle factor,
//!   nominal and high.
//!
//! The DSP also keeps `peak`: the largest unsmoothed `real` of any window since the last reset.
//!
//! A window is at least [`WINDOW_S`] of emulated time (a poll closes it once it is long enough); each closed
//! window is blended into the shown value with weight [`SMOOTHING`].

use serde_json::{Value, json};

const FACTORS_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../devices/cycle-factors.json"
));

/// Minimum emulated time of one window, seconds.
pub const WINDOW_S: f64 = 1.0;
/// Weight of the newest window in the shown value.
pub const SMOOTHING: f64 = 0.5;

/// Cycle factors of one device.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Factors {
    pub cf_core_hz: f64,
    pub cf_model_hz: f64,
    pub cf_cpi_nominal: f64,
    pub cf_cpi_high: f64,
    pub dsp_block_rate: f64,
    pub dsp_clock_hz: f64,
    pub dsp_nominal: f64,
    pub dsp_high: f64,
}

impl Factors {
    /// The factors of a device short name (`dn2`, `dt2`) from the embedded table.
    pub fn for_device(short: &str) -> Option<Self> {
        Self::from_json(FACTORS_JSON, short)
    }

    pub fn from_json(source: &str, short: &str) -> Option<Self> {
        let root: Value = serde_json::from_str(source).ok()?;
        let device = root.get("devices")?.get(short)?;
        let number = |value: &Value, key: &str| value.get(key)?.as_f64();
        Some(Self {
            cf_core_hz: number(&root, "cf_core_hz")?,
            cf_model_hz: number(&root, "cf_model_hz")?,
            cf_cpi_nominal: number(device.get("cf_busy_cpi")?, "nominal")?,
            cf_cpi_high: number(device.get("cf_busy_cpi")?, "high")?,
            dsp_block_rate: number(&root, "dsp_block_rate")?,
            dsp_clock_hz: number(&root, "dsp_clock_hz")?,
            dsp_nominal: number(device.get("dsp")?, "nominal")?,
            dsp_high: number(device.get("dsp")?, "high")?,
        })
    }
}

/// Cumulative counters at one status poll.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Counters {
    /// ColdFire instructions interpreted (busy).
    pub cf_busy: u64,
    /// Every ColdFire instruction of the model: interpreted, idle fast-forwarded, RAM clear.
    pub cf_total: u64,
    /// DSP: audio seconds and instructions executed / idle-skipped (`status.audio.dsp_tiers`), when a DSP runs.
    pub dsp: Option<DspCounters>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DspCounters {
    pub audio_s: f64,
    pub instructions: u64,
    pub idle_skip: u64,
}

/// One closed window, in percent of the chip. `None`: no DSP in the window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Load {
    pub cf_model: f64,
    pub cf_real: f64,
    pub cf_real_high: f64,
    pub dsp: Option<DspLoad>,
    pub window_s: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DspLoad {
    pub busy_per_block: f64,
    pub model: f64,
    pub real: f64,
    pub real_high: f64,
    /// The largest `real` of any single window since the meter was reset (unsmoothed).
    pub peak: f64,
}

/// Window arithmetic: counters at the window start and now -> one window's loads (None: window too short, or a
/// counter went backwards, which means the runtime was replaced).
pub fn window(f: &Factors, start: &Counters, now: &Counters) -> Option<Load> {
    let total = now.cf_total.checked_sub(start.cf_total)?;
    let busy = now.cf_busy.checked_sub(start.cf_busy)?;
    let window_s = total as f64 / f.cf_model_hz;
    if total == 0 || window_s < WINDOW_S || busy > total {
        return None;
    }
    let per_s = busy as f64 / window_s;
    let dsp = match (start.dsp, now.dsp) {
        (Some(a), Some(b)) => {
            let blocks = (b.audio_s - a.audio_s) * f.dsp_block_rate;
            let executed = b.instructions.checked_sub(a.instructions)?;
            let idle = b.idle_skip.checked_sub(a.idle_skip)?;
            if blocks > 0.0 {
                let per_block = executed.saturating_sub(idle) as f64 / blocks;
                let cycles = f.dsp_clock_hz / f.dsp_block_rate;
                let pct = |factor: f64| 100.0 * per_block * factor / cycles;
                Some(DspLoad {
                    busy_per_block: per_block,
                    model: pct(1.0),
                    real: pct(f.dsp_nominal),
                    real_high: pct(f.dsp_high),
                    peak: pct(f.dsp_nominal),
                })
            } else {
                None
            }
        }
        _ => None,
    };
    Some(Load {
        cf_model: 100.0 * busy as f64 / total as f64,
        cf_real: 100.0 * per_s * f.cf_cpi_nominal / f.cf_core_hz,
        cf_real_high: 100.0 * per_s * f.cf_cpi_high / f.cf_core_hz,
        dsp,
        window_s,
    })
}

fn blend(old: f64, new: f64) -> f64 {
    SMOOTHING * new + (1.0 - SMOOTHING) * old
}

fn smooth(old: Load, new: Load) -> Load {
    Load {
        cf_model: blend(old.cf_model, new.cf_model),
        cf_real: blend(old.cf_real, new.cf_real),
        cf_real_high: blend(old.cf_real_high, new.cf_real_high),
        dsp: match (old.dsp, new.dsp) {
            (Some(a), Some(b)) => Some(DspLoad {
                busy_per_block: blend(a.busy_per_block, b.busy_per_block),
                model: blend(a.model, b.model),
                real: blend(a.real, b.real),
                real_high: blend(a.real_high, b.real_high),
                peak: a.peak.max(b.peak),
            }),
            (_, b) => b,
        },
        window_s: new.window_s,
    }
}

/// The meter: owns the window start and the smoothed value.
#[derive(Default)]
pub struct Meter {
    start: Option<Counters>,
    shown: Option<Load>,
}

impl Meter {
    /// Forget everything (a new runtime: its counters restart).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// One status poll. Closes the window when it is long enough; the shown value (None until the first window).
    pub fn poll(&mut self, f: &Factors, now: Counters) -> Option<Load> {
        match self.start {
            None => self.start = Some(now),
            Some(start) => {
                if now.cf_total < start.cf_total {
                    // counters went backwards: restored or replaced runtime
                    self.start = Some(now);
                    self.shown = None;
                } else if let Some(load) = window(f, &start, &now) {
                    self.shown = Some(self.shown.map_or(load, |old| smooth(old, load)));
                    self.start = Some(now);
                }
            }
        }
        self.shown
    }
}

impl Load {
    /// The `status.load` object (docs/CONTROL.md).
    pub fn to_json(&self) -> Value {
        let round = |x: f64| (x * 100.0).round() / 100.0;
        let dsp = self.dsp.map_or(Value::Null, |d| {
            json!({
                "busy_per_block": d.busy_per_block.round(),
                "model_pct": round(d.model),
                "real_pct": round(d.real),
                "real_high_pct": round(d.real_high),
                "peak_pct": round(d.peak),
            })
        });
        json!({
            "window_s": round(self.window_s),
            "cf": {
                "model_pct": round(self.cf_model),
                "real_pct": round(self.cf_real),
                "real_high_pct": round(self.cf_real_high),
            },
            "dsp": dsp,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dn2() -> Factors {
        Factors::for_device("dn2").unwrap()
    }

    fn at(cf_busy: u64, cf_total: u64, dsp: Option<(f64, u64, u64)>) -> Counters {
        Counters {
            cf_busy,
            cf_total,
            dsp: dsp.map(|(audio_s, instructions, idle_skip)| DspCounters {
                audio_s,
                instructions,
                idle_skip,
            }),
        }
    }

    #[test]
    fn factors_come_from_the_shared_table() {
        let f = dn2();
        assert_eq!(f.dsp_nominal, 1.138261);
        assert_eq!(f.dsp_high, 1.326075);
        assert_eq!(f.cf_cpi_nominal, 1.39);
        assert_eq!(Factors::for_device("dt2").unwrap().dsp_high, 1.406521);
        assert!(Factors::for_device("zz").is_none());
    }

    #[test]
    fn window_arithmetic() {
        let f = dn2();
        // 2 s of model time = 264 M instructions; 82 M busy -> 41 M busy/s
        let start = at(0, 0, Some((10.0, 0, 0)));
        let now = at(
            82_000_000,
            264_000_000,
            Some((12.0, 1_000_000_000, 331_000_000)),
        );
        let l = window(&f, &start, &now).unwrap();
        assert!((l.window_s - 2.0).abs() < 1e-12);
        assert!((l.cf_model - 100.0 * 82.0 / 264.0).abs() < 1e-9);
        assert!((l.cf_real - 100.0 * 41e6 * 1.39 / 264e6).abs() < 1e-9);
        // 669 M busy over 3000 blocks = 223,000 per block
        let d = l.dsp.unwrap();
        assert!((d.busy_per_block - 223_000.0).abs() < 1e-6);
        assert!((d.model - 100.0 * 223_000.0 / 666_666.6666666666).abs() < 1e-9);
        assert!((d.real / d.model - 1.138261).abs() < 1e-12);
        assert!((d.real_high / d.model - 1.326075).abs() < 1e-12);
    }

    #[test]
    fn short_window_backwards_counters_and_no_dsp() {
        let f = dn2();
        let start = at(0, 0, None);
        assert!(
            window(&f, &start, &at(10, 100_000_000, None)).is_none(),
            "0.76 s"
        );
        assert!(window(&f, &at(5, 200_000_000, None), &at(5, 100_000_000, None)).is_none());
        let l = window(&f, &start, &at(66_000_000, 132_000_000, None)).unwrap();
        assert!(l.dsp.is_none());
        assert!((l.cf_model - 50.0).abs() < 1e-9);
        // DSP counters absent at one end: no DSP load, CF still shown
        let l = window(&f, &start, &at(1, 132_000_000, Some((1.0, 5, 0)))).unwrap();
        assert!(l.dsp.is_none());
    }

    #[test]
    fn meter_smooths_and_resets() {
        let f = dn2();
        let mut m = Meter::default();
        assert!(m.poll(&f, at(0, 0, None)).is_none());
        assert!(
            m.poll(&f, at(10, 66_000_000, None)).is_none(),
            "window still open"
        );
        let first = m.poll(&f, at(66_000_000, 132_000_000, None)).unwrap();
        assert!((first.cf_model - 50.0).abs() < 1e-9);
        // next window at 100 % busy: blended halfway
        let second = m
            .poll(&f, at(66_000_000 + 132_000_000, 264_000_000, None))
            .unwrap();
        assert!((second.cf_model - 75.0).abs() < 1e-9);
        // counters restart: value cleared until the next window closes
        assert!(m.poll(&f, at(0, 1_000, None)).is_none());
    }

    #[test]
    fn dsp_peak_keeps_a_spike_and_resets() {
        let f = dn2();
        let mut m = Meter::default();
        // windows of 1 s; busy instructions per block: 100k, 400k, 100k busy instructions per block
        let blocks = f.dsp_block_rate as u64;
        m.poll(&f, at(0, 0, Some((0.0, 0, 0))));
        let mut instr = 0u64;
        let mut peak_in = Vec::new();
        for (i, per_block) in [100_000u64, 400_000, 100_000].into_iter().enumerate() {
            instr += per_block * blocks;
            let s = i as u64 + 1;
            let counters = at(1, 132_000_000 * s, Some((s as f64, instr, 0)));
            let shown = m.poll(&f, counters).unwrap().dsp.unwrap();
            peak_in.push((shown.real, shown.peak));
        }
        let spike = 100.0 * 400_000.0 * f.dsp_nominal / (f.dsp_clock_hz / f.dsp_block_rate);
        assert!((peak_in[1].1 - spike).abs() < 1e-9, "the spike window");
        assert!(
            (peak_in[2].1 - spike).abs() < 1e-9,
            "kept after quiet windows"
        );
        assert!(peak_in[2].0 < spike, "while the shown value falls");
        m.reset();
        assert!(m.poll(&f, at(0, 0, None)).is_none());
    }
}
