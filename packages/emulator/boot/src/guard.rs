//! Write-range guards: a regression run fails when stock firmware code writes
//! into memory a patch claims. A guard set is a list of protected ranges plus
//! the PC ranges of the patch's own code, which may write its own regions.

use crate::watch::{FlushHit, Watch, WatchConfig};
use serde::{Deserialize, Serialize};

/// Hits kept in full; later ones are only counted.
pub const GUARD_HIT_LIMIT: usize = 64;

/// A protected address range `[lo, hi)` with a name for reports.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuardRange {
    pub lo: u32,
    pub hi: u32,
    pub label: String,
}

/// A store whose address range overlaps a protected range, made from a PC
/// outside every allowed-writer range.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuardHit {
    pub pc: u32,
    pub addr: u32,
    pub size: u8,
    pub label: String,
    pub icount: u64,
}

/// The first `GUARD_HIT_LIMIT` hits and the total count.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuardReport {
    pub hits: Vec<GuardHit>,
    pub total: u64,
}

fn guard_addr(value: &serde_json::Value, what: &str) -> Result<u32, String> {
    let n = match value {
        serde_json::Value::Number(n) => n.as_u64(),
        serde_json::Value::String(s) => match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))
        {
            Some(hex) => u64::from_str_radix(&hex.replace('_', ""), 16).ok(),
            None => s.parse().ok(),
        },
        _ => None,
    };
    n.filter(|&n| n <= 0x1_0000_0000)
        .map(|n| n.min(u64::from(u32::MAX)) as u32)
        .ok_or_else(|| format!("{what} must be an address (integer or \"0x...\" string)"))
}

/// Parse `{protect: [{lo, hi, label?}], allow_pc: [{lo, hi}]}` (the `guard.set`
/// parameters and the `coupled-run --guard` file). Addresses are integers or
/// `"0x..."` strings; `hi` is exclusive and may be `0x1_0000_0000`
/// (clamped to `u32::MAX`). A missing list is empty.
#[allow(clippy::type_complexity, reason = "a one-off pair of lists")]
pub fn parse_guard_spec(
    spec: &serde_json::Value,
) -> Result<(Vec<GuardRange>, Vec<(u32, u32)>), String> {
    fn list<'a>(spec: &'a serde_json::Value, key: &str) -> Result<&'a [serde_json::Value], String> {
        match spec.get(key) {
            None | Some(serde_json::Value::Null) => Ok(&[]),
            Some(serde_json::Value::Array(a)) => Ok(a),
            Some(_) => Err(format!("{key} must be an array")),
        }
    }
    let mut protect = Vec::new();
    for (i, r) in list(spec, "protect")?.iter().enumerate() {
        let lo = guard_addr(&r["lo"], &format!("protect[{i}].lo"))?;
        let hi = guard_addr(&r["hi"], &format!("protect[{i}].hi"))?;
        let label = r
            .get("label")
            .and_then(|l| l.as_str())
            .map_or_else(|| format!("{lo:#x}..{hi:#x}"), str::to_string);
        protect.push(GuardRange { lo, hi, label });
    }
    let mut allow = Vec::new();
    for (i, r) in list(spec, "allow_pc")?.iter().enumerate() {
        allow.push((
            guard_addr(&r["lo"], &format!("allow_pc[{i}].lo"))?,
            guard_addr(&r["hi"], &format!("allow_pc[{i}].hi"))?,
        ));
    }
    Ok((protect, allow))
}

/// Guard state held by the bus. Empty `protect` means disabled.
#[derive(Debug, Default)]
pub(crate) struct WriteGuards {
    /// Any guard, write-watch or flush check armed: the one flag the store path tests.
    armed: bool,
    watch: Option<Box<Watch>>,
    protect: Vec<GuardRange>,
    allow_pc: Vec<(u32, u32)>,
    report: GuardReport,
    /// Per protected range: whether it has been hit yet.
    seen: Vec<bool>,
    /// First hits of ranges not yet taken by `take_first_hits`.
    firsts: Vec<GuardHit>,
}

impl WriteGuards {
    /// Nothing armed (the default): the store path does nothing more.
    #[inline(always)]
    pub(crate) fn is_empty(&self) -> bool {
        !self.armed
    }

    pub(crate) fn set(&mut self, protect: Vec<GuardRange>, allow_pc: Vec<(u32, u32)>) {
        self.seen = vec![false; protect.len()];
        self.protect = protect;
        self.allow_pc = allow_pc;
        self.armed = !self.protect.is_empty() || self.watch.is_some();
        self.clear();
    }

    /// Arm the write-watch and flush check (`None` disarms them).
    pub(crate) fn set_watch(&mut self, cfg: Option<WatchConfig>) {
        self.watch = cfg.map(|c| Box::new(Watch::new(c)));
        self.armed = !self.protect.is_empty() || self.watch.is_some();
    }

    /// A CPU store of `size` bytes at `addr` with `value`, made from `pc`. Only called
    /// when armed; out of line so the store path stays as small as without it.
    #[cold]
    #[inline(never)]
    pub(crate) fn note_store(&mut self, pc: u32, addr: u32, size: u8, value: u32, icount: u64) {
        if !self.protect.is_empty() {
            self.note(pc, addr, size, icount);
        }
        if let Some(w) = self.watch.as_mut() {
            w.store(pc, addr, size, value, icount);
        }
    }

    /// A CPU load (the read-watch). Only called when armed; out of line, as `note_store`.
    #[cold]
    #[inline(never)]
    pub(crate) fn note_load(&mut self, pc: u32, addr: u32, size: u8, icount: u64) {
        if let Some(w) = self.watch.as_mut() {
            w.load(pc, addr, size, icount);
        }
    }

    pub(crate) fn note_dma(&mut self, src: &'static str, addr: u32, len: usize, icount: u64) {
        if let Some(w) = self.watch.as_mut() {
            w.dma(src, addr, len, icount);
        }
    }

    pub(crate) fn note_fetch(&mut self, pc: u32, len: u32, icount: u64) {
        if let Some(w) = self.watch.as_mut() {
            w.fetch(pc, len, icount);
        }
    }

    pub(crate) fn note_cacr(&mut self, cacr: u32, icount: u64) {
        if let Some(w) = self.watch.as_mut() {
            w.cacr_written(cacr, icount);
        }
    }

    pub(crate) fn note_cpushl(&mut self, cache: u8, ax: u32) {
        if let Some(w) = self.watch.as_mut() {
            w.cpushl(cache, ax);
        }
    }

    pub(crate) fn watch_mut(&mut self) -> Option<&mut Watch> {
        self.watch.as_deref_mut()
    }

    pub(crate) fn flush_hits(&self) -> Vec<FlushHit> {
        self.watch
            .as_ref()
            .map_or_else(Vec::new, |w| w.flush_hits())
    }

    /// Forget recorded hits (including which ranges were already hit).
    pub(crate) fn clear(&mut self) {
        self.report = GuardReport::default();
        self.seen.iter_mut().for_each(|s| *s = false);
        self.firsts.clear();
    }

    pub(crate) fn report(&self) -> GuardReport {
        self.report.clone()
    }

    pub(crate) fn total(&self) -> u64 {
        self.report.total
    }

    /// Hits that were the first for their range since the last call.
    pub(crate) fn take_first_hits(&mut self) -> Vec<GuardHit> {
        std::mem::take(&mut self.firsts)
    }

    /// Record a `size`-byte store at `addr` made from `pc`.
    #[cold]
    #[inline(never)]
    pub(crate) fn note(&mut self, pc: u32, addr: u32, size: u8, icount: u64) {
        if self.allow_pc.iter().any(|&(lo, hi)| (lo..hi).contains(&pc)) {
            return;
        }
        let end = u64::from(addr) + u64::from(size);
        for (i, range) in self.protect.iter().enumerate() {
            if u64::from(addr) < u64::from(range.hi) && end > u64::from(range.lo) {
                let hit = GuardHit {
                    pc,
                    addr,
                    size,
                    label: range.label.clone(),
                    icount,
                };
                self.report.total += 1;
                if !self.seen[i] {
                    self.seen[i] = true;
                    self.firsts.push(hit.clone());
                }
                if self.report.hits.len() < GUARD_HIT_LIMIT {
                    self.report.hits.push(hit);
                }
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guards() -> WriteGuards {
        let mut g = WriteGuards::default();
        g.set(
            vec![GuardRange {
                lo: 0x4000_1000,
                hi: 0x4000_1100,
                label: "patch".into(),
            }],
            vec![(0x4000_2000, 0x4000_3000)],
        );
        g
    }

    #[test]
    fn stock_store_into_protected_range_is_recorded() {
        let mut g = guards();
        g.note(0x1000, 0x4000_10fe, 4, 7); // straddles the end
        g.note(0x1000, 0x4000_1100, 4, 8); // just outside
        g.note(0x1000, 0x4000_0ffc, 4, 9); // ends at lo: outside
        g.note(0x1000, 0x4000_0fff, 2, 10); // overlaps lo
        let r = g.report();
        assert_eq!(r.total, 2);
        assert_eq!(r.hits[0].icount, 7);
        assert_eq!(r.hits[0].label, "patch");
        assert_eq!(g.take_first_hits().len(), 1);
        assert!(g.take_first_hits().is_empty());
    }

    #[test]
    fn allowed_pc_is_not_recorded() {
        let mut g = guards();
        g.note(0x4000_2000, 0x4000_1000, 4, 1);
        g.note(0x4000_2ffe, 0x4000_1010, 1, 2);
        assert_eq!(g.total(), 0);
        g.note(0x4000_3000, 0x4000_1000, 4, 3); // hi is exclusive
        assert_eq!(g.total(), 1);
    }

    #[test]
    fn only_the_first_hits_are_kept() {
        let mut g = guards();
        for i in 0..100 {
            g.note(0x1000, 0x4000_1000, 1, i);
        }
        let r = g.report();
        assert_eq!(r.total, 100);
        assert_eq!(r.hits.len(), GUARD_HIT_LIMIT);
        g.clear();
        assert_eq!(g.total(), 0);
    }
}
