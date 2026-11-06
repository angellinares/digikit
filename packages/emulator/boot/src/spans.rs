//! Instruction spans between two PCs (off by default): a host names an entry PC and an exit
//! PC, for example an interrupt handler's first instruction and its `rte`. For each pass the
//! runtime keeps the instructions from the entry to the exit (the exit instruction itself is
//! not counted) and the instructions from the previous entry. Nothing is recorded until a host
//! adds a span, and a span adds only its two PCs to the observed set.
//!
//! An exit with no open entry (the run started inside the span) is not counted. A second entry
//! before the exit counts as `nested` and keeps the first entry.

use serde_json::{Value, json};

/// Passes kept per span (about 11 minutes of a 1,500 Hz handler).
const KEEP: usize = 1 << 20;

#[derive(Debug, Clone)]
struct Span {
    name: String,
    entry: u32,
    exit: u32,
    open: Option<u64>,
    last_entry: Option<u64>,
    /// Each pass: the instruction count at its entry, and its length.
    passes: Vec<(u64, u32)>,
    gaps: Vec<u32>,
    nested: u64,
    dropped: u64,
}

#[derive(Debug, Clone, Default)]
pub struct Spans {
    spans: Vec<Span>,
}

fn clamp(n: u64) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn push(values: &mut Vec<u32>, dropped: &mut u64, value: u32) {
    if values.len() < KEEP {
        values.push(value);
    } else {
        *dropped += 1;
    }
}

fn stats(values: &[u32], period: u64) -> Value {
    if values.is_empty() {
        return json!({"count": 0});
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let pick = |q: f64| sorted[((sorted.len() - 1) as f64 * q).round() as usize];
    let total: u64 = values.iter().map(|&v| u64::from(v)).sum();
    json!({
        "count": values.len(),
        "min": sorted[0],
        "mean": total as f64 / values.len() as f64,
        "p50": pick(0.5),
        "p99": pick(0.99),
        "max": sorted[sorted.len() - 1],
        "over_period": values.iter().filter(|&&v| u64::from(v) > period).count(),
    })
}

impl Spans {
    /// Add a span. Two spans may share a PC.
    pub fn add(&mut self, name: &str, entry: u32, exit: u32) {
        self.spans.push(Span {
            name: name.to_owned(),
            entry,
            exit,
            open: None,
            last_entry: None,
            passes: Vec::new(),
            gaps: Vec::new(),
            nested: 0,
            dropped: 0,
        });
    }

    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    /// Every PC the runtime must observe.
    pub fn pcs(&self) -> impl Iterator<Item = u32> + '_ {
        self.spans.iter().flat_map(|s| [s.entry, s.exit])
    }

    /// Called before the instruction at `pc` runs, with the instructions executed so far.
    pub fn observe(&mut self, pc: u32, icount: u64) {
        for span in &mut self.spans {
            if pc == span.entry {
                if span.open.is_some() {
                    span.nested += 1;
                    continue;
                }
                if let Some(last) = span.last_entry {
                    push(&mut span.gaps, &mut span.dropped, clamp(icount - last));
                }
                span.last_entry = Some(icount);
                span.open = Some(icount);
            } else if pc == span.exit {
                if let Some(start) = span.open.take() {
                    if span.passes.len() < KEEP {
                        span.passes.push((start, clamp(icount - start)));
                    } else {
                        span.dropped += 1;
                    }
                }
            }
        }
    }

    /// Forget every recorded pass and keep the spans.
    pub fn clear(&mut self) {
        for span in &mut self.spans {
            span.open = None;
            span.last_entry = None;
            span.passes.clear();
            span.gaps.clear();
            span.nested = 0;
            span.dropped = 0;
        }
    }

    /// One object per span. `period` is the instructions in one handler period: a length
    /// or a gap above it counts in `over_period`.
    pub fn report(&self, period: u64) -> Value {
        Value::Array(
            self.spans
                .iter()
                .map(|s| {
                    let lengths: Vec<u32> = s.passes.iter().map(|p| p.1).collect();
                    // The first pass of the greatest length: its index and its entry count.
                    let worst = s
                        .passes
                        .iter()
                        .enumerate()
                        .max_by_key(|(i, p)| (p.1, std::cmp::Reverse(*i)))
                        .map(|(i, p)| json!({"pass": i, "at": p.0}));
                    json!({
                        "name": s.name,
                        "entry": format!("{:#010x}", s.entry),
                        "exit": format!("{:#010x}", s.exit),
                        "period": period,
                        "length": stats(&lengths, period),
                        "worst": worst,
                        "gap": stats(&s.gaps, period),
                        "nested": s.nested,
                        "dropped": s.dropped,
                    })
                })
                .collect(),
        )
    }
}

impl Spans {
    /// Every pass of every span, in entry order per span: (name, entry count, length).
    pub fn passes(&self) -> impl Iterator<Item = (&str, u64, u32)> + '_ {
        self.spans
            .iter()
            .flat_map(|s| s.passes.iter().map(move |p| (s.name.as_str(), p.0, p.1)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_lengths_and_gaps() {
        let mut s = Spans::default();
        s.add("isr", 0x100, 0x200);
        s.observe(0x200, 5); // exit with no entry: ignored
        s.observe(0x100, 10);
        s.observe(0x200, 40);
        s.observe(0x100, 110);
        s.observe(0x100, 120); // nested entry: keeps 110
        s.observe(0x200, 170);
        let r = s.report(50);
        assert_eq!(r[0]["length"]["count"], 2);
        assert_eq!(r[0]["length"]["min"], 30);
        assert_eq!(r[0]["length"]["max"], 60);
        assert_eq!(r[0]["length"]["over_period"], 1);
        assert_eq!(r[0]["gap"]["count"], 1);
        assert_eq!(r[0]["gap"]["max"], 100);
        assert_eq!(r[0]["nested"], 1);
        assert_eq!(r[0]["worst"]["pass"], 1);
        assert_eq!(r[0]["worst"]["at"], 110);
        assert_eq!(
            s.passes().collect::<Vec<_>>(),
            [("isr", 10, 30), ("isr", 110, 60)]
        );
        s.clear();
        assert_eq!(s.report(50)[0]["length"]["count"], 0);
    }

    #[test]
    fn spans_share_a_pc() {
        let mut s = Spans::default();
        s.add("a", 0x100, 0x200);
        s.add("b", 0x100, 0x180);
        s.observe(0x100, 0);
        s.observe(0x180, 8);
        s.observe(0x200, 20);
        let r = s.report(100);
        assert_eq!(r[0]["length"]["max"], 20);
        assert_eq!(r[1]["length"]["max"], 8);
        assert_eq!(s.pcs().count(), 4);
    }
}
