//! The span gate (#307 step 2): compare one scenario's spans with the same scenario's spans in a
//! stock report, and fail when the build adds more instructions than a limit.
//!
//! The gate checks two numbers for each span: the typical pass (`p50`) and the longest pass
//! (`max`). A build passes a span when neither number is more than the span's limit above stock.
//! Instruction counts are not cycles, so the gate uses only the difference from stock.
//!
//! The gate also checks late entries. `gap.max` is the longest time from one entry to the next,
//! so a late entry makes it longer. A build fails a span when its `gap.max` is more than the
//! limit above stock's, or when it has more passes past the period (`length.over_period`) than
//! stock. In the emulator a handler entry drifts by a few dozen instructions around its period,
//! so the gate does not count gaps over the period.
//!
//! The gate also checks the passes over the period at the unit's speed
//! (`length.over_unit_period`: a pass's length times the report's `unit_scale`, above the period).
//! On one unit a heavy pass took about 1.21 timer ticks per emulator instruction, so a build can
//! run late on a unit before it runs late here. A build fails a span when it has more of these
//! passes than stock. Two reports with another `unit_scale` cannot be compared, and neither can
//! a stock report made before the count: run stock again.
//!
//! The gate fails, and does not pass in silence, when the stock report cannot be compared: no
//! scenario of the same name, no span of the same name, other span PCs, another period, another
//! unit scale, another DSP mode, or scenario lengths more than one chunk and one period apart.

use serde_json::{Value, json};

/// The extra instructions a span may add to stock when `--span-max` names no limit for it.
pub const DEFAULT_MAX_EXTRA: u64 = 1_000;

/// Per-span limits in instructions. A span not named here gets [`DEFAULT_MAX_EXTRA`].
#[derive(Debug, Clone, Default)]
pub struct Limits {
    pub per_span: Vec<(String, u64)>,
}

impl Limits {
    pub fn for_span(&self, name: &str) -> u64 {
        self.per_span
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map_or(DEFAULT_MAX_EXTRA, |(_, n)| *n)
    }
}

/// Parse `--span-max NAME=N`.
pub fn parse_limit(spec: &str) -> Result<(String, u64), String> {
    let (name, n) = spec.split_once('=').ok_or("--span-max needs NAME=N")?;
    if name.is_empty() {
        return Err("--span-max needs a NAME".into());
    }
    let n = n
        .parse()
        .map_err(|_| format!("--span-max {name}: N must be a whole number of instructions"))?;
    Ok((name.to_owned(), n))
}

fn fail(error: String) -> Value {
    json!({"ok": false, "error": error, "spans": []})
}

/// Gate `scenario` (one entry of a report's `scenarios`, with `spans`) against `stock` (a whole
/// earlier report). `dsp` is this run's DSP mode (`"zero"` or `"sharc"`).
pub fn check(scenario: &Value, dsp: &str, stock: &Value, limits: &Limits) -> Value {
    let name = scenario["name"].as_str().unwrap_or("?");
    if stock["dsp"].as_str() != Some(dsp) {
        return fail(format!(
            "the stock report ran with --dsp {}, this run with --dsp {dsp}",
            stock["dsp"].as_str().unwrap_or("?")
        ));
    }
    let Some(base) = stock["scenarios"]
        .as_array()
        .and_then(|all| all.iter().find(|s| s["name"].as_str() == Some(name)))
    else {
        return fail(format!("the stock report has no scenario {name}"));
    };
    let Some(spans) = scenario["spans"].as_array().filter(|s| !s.is_empty()) else {
        return fail(format!("scenario {name} has no spans to gate"));
    };
    // A run stops at the first chunk boundary past its length, and `--call` work is not counted.
    // So two builds can end the same scenario up to one chunk apart, plus a few instructions.
    let period = spans[0]["period"].as_u64().unwrap_or(0);
    let slack = u64::from(crate::STEP_CHUNK) + period;
    let length = |s: &Value| s["guest_instructions"].as_u64();
    match (length(base), length(scenario)) {
        (Some(a), Some(b)) if a.abs_diff(b) <= slack => {}
        (a, b) => {
            return fail(format!(
                "scenario {name} ran {a:?} instructions in the stock report and {b:?} here, \
                 more than one chunk and one period ({slack}) apart"
            ));
        }
    }
    let mut rows = Vec::new();
    for span in spans {
        let span_name = span["name"].as_str().unwrap_or("?");
        let Some(stock_span) = base["spans"]
            .as_array()
            .and_then(|all| all.iter().find(|s| s["name"].as_str() == Some(span_name)))
        else {
            return fail(format!(
                "the stock report has no span {span_name} in {name}"
            ));
        };
        for key in ["entry", "exit", "period", "unit_scale"] {
            if span[key] != stock_span[key] {
                return fail(format!(
                    "span {span_name}: {key} is {} in the stock report and {} here",
                    stock_span[key], span[key]
                ));
            }
        }
        let num = |s: &Value, k: &str| s["length"][k].as_u64();
        let (Some(p50), Some(max), Some(stock_p50), Some(stock_max)) = (
            num(span, "p50"),
            num(span, "max"),
            num(stock_span, "p50"),
            num(stock_span, "max"),
        ) else {
            return fail(format!("span {span_name} has no passes in {name}"));
        };
        let limit = limits.for_span(span_name);
        let extra = |a: u64, b: u64| a as i64 - b as i64;
        let (extra_p50, extra_max) = (extra(p50, stock_p50), extra(max, stock_max));
        let overrun = |s: &Value| s["length"]["over_period"].as_u64().unwrap_or(0);
        let (overrun, stock_overrun) = (overrun(span), overrun(stock_span));
        // An empty `gap` (one pass or none) has no `max`: there is no entry to be late.
        let gap_max = |s: &Value| s["gap"]["max"].as_u64().unwrap_or(0);
        let (gap_max, stock_gap_max) = (gap_max(span), gap_max(stock_span));
        let extra_gap_max = extra(gap_max, stock_gap_max);
        let unit_overrun = |s: &Value| s["length"]["over_unit_period"].as_u64().unwrap_or(0);
        let (unit_overrun, stock_unit_overrun) = (unit_overrun(span), unit_overrun(stock_span));
        rows.push(json!({
            "name": span_name, "limit": limit,
            "stock_p50": stock_p50, "p50": p50, "extra_p50": extra_p50,
            "stock_max": stock_max, "max": max, "extra_max": extra_max,
            "stock_overrun": stock_overrun, "overrun": overrun,
            "stock_gap_max": stock_gap_max, "gap_max": gap_max, "extra_gap_max": extra_gap_max,
            "stock_unit_overrun": stock_unit_overrun, "unit_overrun": unit_overrun,
            "ok": extra_p50 <= limit as i64 && extra_max <= limit as i64
                && extra_gap_max <= limit as i64 && overrun <= stock_overrun
                && unit_overrun <= stock_unit_overrun,
        }));
    }
    let ok = rows.iter().all(|r| r["ok"] == true);
    json!({"ok": ok, "error": Value::Null, "spans": rows})
}

/// One line per span for the console.
pub fn summary(gate: &Value) -> Vec<String> {
    if let Some(e) = gate["error"].as_str() {
        return vec![format!("  span gate FAILED: {e}")];
    }
    gate["spans"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            format!(
                "  span {:<10} p50 {:>+7} max {:>+7} gap {:>+7} (limit +{}) overrun {}/{} unit {}/{} {}",
                r["name"].as_str().unwrap_or("?"),
                r["extra_p50"].as_i64().unwrap_or(0),
                r["extra_max"].as_i64().unwrap_or(0),
                r["extra_gap_max"].as_i64().unwrap_or(0),
                r["limit"],
                r["overrun"],
                r["stock_overrun"],
                r["unit_overrun"],
                r["stock_unit_overrun"],
                if r["ok"] == true { "ok" } else { "FAILED" }
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(name: &str, p50: u64, max: u64) -> Value {
        json!({"name": name, "entry": "0x00000100", "exit": "0x00000200", "period": 88000,
               "length": {"count": 10, "p50": p50, "max": max}})
    }

    fn scenario(spans: Vec<Value>) -> Value {
        json!({"name": "idle", "guest_instructions": 264000000u64, "spans": spans})
    }

    fn stock() -> Value {
        json!({"dsp": "zero", "scenarios": [scenario(vec![span("isr", 50000, 54000)])]})
    }

    #[test]
    fn stock_against_itself_passes_with_no_extra() {
        let g = check(
            &scenario(vec![span("isr", 50000, 54000)]),
            "zero",
            &stock(),
            &Limits::default(),
        );
        assert_eq!(g["ok"], true);
        assert_eq!(g["spans"][0]["extra_p50"], 0);
        assert_eq!(g["spans"][0]["extra_max"], 0);
        assert_eq!(g["spans"][0]["limit"], DEFAULT_MAX_EXTRA);
    }

    #[test]
    fn the_limit_holds_for_p50_and_for_max() {
        let at_limit = scenario(vec![span("isr", 51000, 55000)]);
        assert_eq!(
            check(&at_limit, "zero", &stock(), &Limits::default())["ok"],
            true
        );
        let worst_only = scenario(vec![span("isr", 50000, 55001)]);
        assert_eq!(
            check(&worst_only, "zero", &stock(), &Limits::default())["ok"],
            false
        );
        let typical_only = scenario(vec![span("isr", 51001, 54000)]);
        assert_eq!(
            check(&typical_only, "zero", &stock(), &Limits::default())["ok"],
            false
        );
        let named = Limits {
            per_span: vec![("isr".into(), 2000)],
        };
        assert_eq!(check(&worst_only, "zero", &stock(), &named)["ok"], true);
    }

    #[test]
    fn a_report_that_cannot_be_compared_fails() {
        let s = scenario(vec![span("isr", 50000, 54000)]);
        let l = Limits::default();
        assert!(
            check(&s, "sharc", &stock(), &l)["error"]
                .as_str()
                .unwrap()
                .contains("--dsp")
        );
        let mut other = stock();
        other["scenarios"][0]["name"] = json!("kb-chord16");
        assert!(
            check(&s, "zero", &other, &l)["error"]
                .as_str()
                .unwrap()
                .contains("no scenario")
        );
        let mut close = stock();
        close["scenarios"][0]["guest_instructions"] = json!(264_000_000u64 + 250_000 + 88_000);
        assert_eq!(check(&s, "zero", &close, &l)["ok"], true);
        let mut longer = stock();
        longer["scenarios"][0]["guest_instructions"] = json!(264_000_000u64 + 250_000 + 88_001);
        assert!(
            check(&s, "zero", &longer, &l)["error"]
                .as_str()
                .unwrap()
                .contains("instructions")
        );
        let evala = scenario(vec![span("evala", 8000, 9000)]);
        assert!(
            check(&evala, "zero", &stock(), &l)["error"]
                .as_str()
                .unwrap()
                .contains("no span")
        );
        let mut moved = span("isr", 50000, 54000);
        moved["exit"] = json!("0x00000300");
        let moved = scenario(vec![moved]);
        assert!(
            check(&moved, "zero", &stock(), &l)["error"]
                .as_str()
                .unwrap()
                .contains("exit")
        );
        assert_eq!(check(&scenario(vec![]), "zero", &stock(), &l)["ok"], false);
    }

    #[test]
    fn an_overrun_or_a_late_entry_fails() {
        let l = Limits::default();
        let mut overran = span("isr", 50000, 54000);
        overran["length"]["over_period"] = json!(1);
        let g = check(&scenario(vec![overran]), "zero", &stock(), &l);
        assert_eq!(g["ok"], false);
        assert_eq!(g["spans"][0]["overrun"], 1);
        let mut drift = stock();
        drift["scenarios"][0]["spans"][0]["gap"] = json!({"count": 9, "max": 88056});
        let mut late = span("isr", 50000, 54000);
        late["gap"] = json!({"count": 9, "max": 89057});
        let g = check(&scenario(vec![late.clone()]), "zero", &drift, &l);
        assert_eq!(g["ok"], false);
        assert_eq!(g["spans"][0]["extra_gap_max"], 1001);
        late["gap"]["max"] = json!(89056);
        assert_eq!(check(&scenario(vec![late]), "zero", &drift, &l)["ok"], true);
    }

    #[test]
    fn an_overrun_at_the_unit_speed_fails() {
        let l = Limits::default();
        // Stock itself has one pass over the period at the unit's speed (the first step after PLAY).
        let mut base = stock();
        base["scenarios"][0]["spans"][0]["length"]["over_unit_period"] = json!(1);
        base["scenarios"][0]["spans"][0]["unit_scale"] = json!(1.21);
        let mut build = span("isr", 50000, 54000);
        build["unit_scale"] = json!(1.21);
        build["length"]["over_unit_period"] = json!(1);
        assert_eq!(
            check(&scenario(vec![build.clone()]), "zero", &base, &l)["ok"],
            true
        );
        build["length"]["over_unit_period"] = json!(2);
        let g = check(&scenario(vec![build.clone()]), "zero", &base, &l);
        assert_eq!(g["ok"], false);
        assert_eq!(g["spans"][0]["unit_overrun"], 2);
        assert_eq!(g["spans"][0]["stock_unit_overrun"], 1);
        // Another scale, or a stock report from before the count, cannot be compared.
        build["unit_scale"] = json!(1.0);
        let g = check(&scenario(vec![build]), "zero", &base, &l);
        assert!(g["error"].as_str().unwrap().contains("unit_scale"));
    }

    #[test]
    fn the_summary_signs_the_extra() {
        let s = scenario(vec![span("isr", 49000, 56000)]);
        let lines = summary(&check(&s, "zero", &stock(), &Limits::default()));
        assert!(lines[0].contains("p50   -1000") && lines[0].contains("max   +2000"));
        assert!(lines[0].ends_with("FAILED"));
    }

    #[test]
    fn limits_parse() {
        assert_eq!(parse_limit("isr=1500").unwrap(), ("isr".into(), 1500));
        assert!(parse_limit("isr").is_err());
        assert!(parse_limit("=5").is_err());
        assert!(parse_limit("isr=-1").is_err());
    }
}
