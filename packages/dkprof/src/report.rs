//! Raw counters named through the address map: per-function and per-module self aggregates.
//!
//! A raw counter is a count at one address (a sampled PC, an exit PC), so a function's number is the count of the addresses inside it and
//! nothing else: callers are not credited with their callees, and nothing here adds inclusive costs. Series (`pc`, `idle_pc`, ...) and CPUs are
//! reported apart.

use crate::profile::{Metric, Profile};
use dkaddrmap::{AddrMap, Cpu, Layer, LoadTable, Resolver};
use dksharc::{Analysis, LoopCount};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fmt::Write;

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Func {
    /// The function name without the offset; an address no layer holds is the hex address.
    pub name: String,
    /// The module (the name the runtime reports) the function belongs to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    /// `our`, `anchor`, `fw`; None when unnamed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layer: Option<Layer>,
    pub self_count: u64,
    /// Of the total of this series and CPU, 0 to 100.
    pub share: f64,
    /// SHARC series only, when the profile has the metric [`BLOCKS_METRIC`]: `self_count` per DSP block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub per_block: Option<f64>,
}

/// The metric that holds the DSP blocks a SHARC profile covers; it turns counts into instructions per block.
pub const BLOCKS_METRIC: &str = "sharc.blocks";

/// A DO loop of a module's DSP code (from `dksharc`) with the counters inside its body. A counter belongs to the innermost loop that holds
/// it, so loops of one nest are never added up: a loop's own number is its own body without the inner loops.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct LoopRow {
    pub module: String,
    /// The function the loop is in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
    pub depth: u32,
    /// The body, as short-word offsets from the start of the module's DSP image (first and last instruction).
    pub start: u64,
    pub end: u64,
    /// How the loop counts (`dksharc`): `lit N`, `reg R0`, `until`.
    pub count: String,
    pub self_count: u64,
    /// Of the total of this series and CPU, 0 to 100.
    pub share: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub per_block: Option<f64>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Owner {
    /// A module id, `runtime` (our code outside modules), `firmware` (anchors and firmware names) or `unnamed`.
    pub owner: String,
    pub self_count: u64,
    pub share: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub per_block: Option<f64>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct SeriesReport {
    pub series: String,
    pub cpu: Cpu,
    /// The sum of all counters of the series on this CPU.
    pub total: u64,
    /// Counters (distinct addresses) behind the totals.
    pub rows: usize,
    pub functions: Vec<Func>,
    pub owners: Vec<Owner>,
    /// SHARC series with module analyses: the loops that hold counters, heaviest first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub loops: Vec<LoopRow>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Report {
    pub run: String,
    pub target: String,
    pub clock: String,
    pub unit: String,
    pub metrics: Vec<Metric>,
    /// DSP blocks of the run (`sharc.blocks`), when the profile says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocks: Option<f64>,
    pub series: Vec<SeriesReport>,
    pub loss: Vec<String>,
    /// Modules named by the maps but missing from the load table: their names are missing from the report.
    pub unloaded: Vec<String>,
}

fn pct(n: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        100.0 * n as f64 / total as f64
    }
}

/// Aggregate the profile. `maps` and `loads` name the addresses; `loads` of the call is added to the load table the run carries. `top` keeps
/// the heaviest `top` functions of each series (the totals still count all).
pub fn build(
    p: &Profile,
    maps: &[AddrMap],
    loads: &LoadTable,
    top: usize,
) -> Result<Report, String> {
    build_with(p, maps, loads, &BTreeMap::new(), top)
}

/// [`build`] with the static analyses of module DSP code (`dksharc`, by module id): SHARC counters inside a loop body are added to the loop.
pub fn build_with(
    p: &Profile,
    maps: &[AddrMap],
    loads: &LoadTable,
    analyses: &BTreeMap<String, Analysis>,
    top: usize,
) -> Result<Report, String> {
    let blocks = p
        .metrics
        .iter()
        .find(|m| m.name == BLOCKS_METRIC)
        .and_then(|m| m.value)
        .filter(|b| *b > 0.0);
    let per_block = |cpu: Cpu, n: u64| match (cpu, blocks) {
        (Cpu::Sharc, Some(b)) => Some(n as f64 / b),
        _ => None,
    };
    let mut table = p.run.loads.clone().unwrap_or_default();
    table.modules.extend(loads.modules.iter().cloned());
    let resolver = Resolver::new(maps, &table)?;
    type Key = (String, Cpu);
    #[derive(Default)]
    struct Acc {
        rows: usize,
        total: u64,
        funcs: BTreeMap<String, Func>,
        addrs: Vec<(u64, u64)>,
    }
    let mut acc: BTreeMap<Key, Acc> = BTreeMap::new();
    for r in &p.raw {
        let hit = resolver.lookup(r.cpu, r.addr);
        let a = acc.entry((r.series.clone(), r.cpu)).or_default();
        a.rows += 1;
        a.total += r.count;
        a.addrs.push((r.addr, r.count));
        let (name, module, layer) = match hit {
            Some(h) => (h.name, h.module, Some(h.layer)),
            None => (format!("{:#x}", r.addr), None, None),
        };
        let f = a.funcs.entry(name.clone()).or_insert(Func {
            name,
            module,
            layer,
            self_count: 0,
            share: 0.0,
            per_block: None,
        });
        f.self_count += r.count;
    }
    let mut series = vec![];
    for ((name, cpu), a) in acc {
        let mut owners: BTreeMap<String, u64> = BTreeMap::new();
        let mut functions: Vec<Func> = a.funcs.into_values().collect();
        for f in &mut functions {
            f.share = pct(f.self_count, a.total);
            f.per_block = per_block(cpu, f.self_count);
            let owner = match (&f.module, f.layer) {
                (Some(m), _) => m.clone(),
                (None, Some(Layer::Our)) => "runtime".into(),
                (None, Some(_)) => "firmware".into(),
                (None, None) => "unnamed".into(),
            };
            *owners.entry(owner).or_default() += f.self_count;
        }
        functions.sort_by(|x, y| {
            y.self_count
                .cmp(&x.self_count)
                .then_with(|| x.name.cmp(&y.name))
        });
        functions.truncate(top);
        let mut owners: Vec<Owner> = owners
            .into_iter()
            .map(|(owner, self_count)| Owner {
                owner,
                self_count,
                share: pct(self_count, a.total),
                per_block: per_block(cpu, self_count),
            })
            .collect();
        owners.sort_by(|x, y| {
            y.self_count
                .cmp(&x.self_count)
                .then_with(|| x.owner.cmp(&y.owner))
        });
        series.push(SeriesReport {
            series: name,
            cpu,
            total: a.total,
            rows: a.rows,
            functions,
            owners,
            loops: if cpu == Cpu::Sharc {
                loop_rows(&a.addrs, &table, analyses, a.total, &per_block)
            } else {
                vec![]
            },
        });
    }
    Ok(Report {
        run: p.run.id.clone(),
        target: p.run.target.clone(),
        clock: p.clock.source.clone(),
        unit: p.clock.unit.clone(),
        metrics: p.metrics.clone(),
        blocks,
        series,
        loss: p
            .loss
            .iter()
            .map(|l| match l.dropped {
                Some(n) => format!("{}: {} ({n} dropped)", l.series, l.reason),
                None => format!("{}: {}", l.series, l.reason),
            })
            .collect(),
        unloaded: resolver.unloaded.clone(),
    })
}

/// The loops of the analysed modules that hold counters. A module's DSP image starts at `dsp_base` of its load; an analysis pc `x` sits at
/// `dsp_base + x - base_sw`.
fn loop_rows(
    addrs: &[(u64, u64)],
    table: &LoadTable,
    analyses: &BTreeMap<String, Analysis>,
    total: u64,
    per_block: &dyn Fn(Cpu, u64) -> Option<f64>,
) -> Vec<LoopRow> {
    let mut rows = vec![];
    for (id, a) in analyses {
        let Some(base) = table
            .modules
            .iter()
            .find(|l| &l.id == id)
            .and_then(|l| l.dsp_base)
        else {
            continue;
        };
        let off = |pc: u32| u64::from(pc.wrapping_sub(a.base_sw));
        // innermost first: a counter goes to the deepest loop that holds it
        let mut order: Vec<usize> = (0..a.loops.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(a.loops[i].depth));
        let mut sums = vec![0u64; a.loops.len()];
        for &(addr, count) in addrs {
            let Some(rel) = addr.checked_sub(base) else {
                continue;
            };
            if let Some(&i) = order
                .iter()
                .find(|&&i| (off(a.loops[i].start)..=off(a.loops[i].end)).contains(&rel))
            {
                sums[i] += count;
            }
        }
        for (i, l) in a.loops.iter().enumerate() {
            if sums[i] == 0 {
                continue;
            }
            rows.push(LoopRow {
                module: id.clone(),
                function: l
                    .function
                    .and_then(|f| a.functions.get(f))
                    .map(|f| f.name.clone()),
                depth: l.depth,
                start: off(l.start),
                end: off(l.end),
                count: match &l.count {
                    LoopCount::Lit { count } => format!("lit {count}"),
                    LoopCount::Reg { name, .. } => format!("reg {name}"),
                    LoopCount::Until { .. } => "until".into(),
                },
                self_count: sums[i],
                share: pct(sums[i], total),
                per_block: per_block(Cpu::Sharc, sums[i]),
            });
        }
    }
    rows.sort_by(|x, y| {
        y.self_count
            .cmp(&x.self_count)
            .then_with(|| (&x.module, x.start).cmp(&(&y.module, y.start)))
    });
    rows
}

fn metric_text(m: &Metric) -> String {
    match (m.value, &m.na_reason) {
        (Some(v), _) if v.fract() == 0.0 && v.abs() < 1e15 => format!("{} {}", v as i64, m.unit),
        (Some(v), _) => format!("{v:.3} {}", m.unit),
        (None, Some(r)) => format!("N/A ({r})"),
        (None, None) => "N/A".into(),
    }
}

fn block_text(per_block: Option<f64>) -> String {
    per_block.map_or(String::new(), |b| format!(" {b:>10.1}/block"))
}

/// The text view of a report.
pub fn text(r: &Report, top: usize) -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "run {} target {} clock {} ({})",
        r.run, r.target, r.clock, r.unit
    );
    if !r.metrics.is_empty() {
        let _ = writeln!(s, "metrics:");
        for m in &r.metrics {
            let _ = writeln!(s, "  {:<34} {}", m.name, metric_text(m));
        }
    }
    for x in &r.series {
        let cpu = format!("{:?}", x.cpu).to_lowercase();
        let _ = writeln!(
            s,
            "{} {cpu} by function (top {top}; {} rows, total {}):",
            x.series, x.rows, x.total
        );
        for f in x.functions.iter().take(top) {
            let _ = writeln!(
                s,
                "  {:>14} {:5.1}%{}  {}",
                f.self_count,
                f.share,
                block_text(f.per_block),
                f.name
            );
        }
        if !x.loops.is_empty() {
            let _ = writeln!(
                s,
                "{} {cpu} by loop (self; an inner loop is not in its outer one):",
                x.series
            );
            for l in x.loops.iter().take(top) {
                let _ = writeln!(
                    s,
                    "  {:>14} {:5.1}%{}  {} {} depth {} +{:#06x}..+{:#06x} count {}",
                    l.self_count,
                    l.share,
                    block_text(l.per_block),
                    l.module,
                    l.function.as_deref().unwrap_or("?"),
                    l.depth,
                    l.start,
                    l.end,
                    l.count
                );
            }
        }
        let _ = writeln!(s, "{} {cpu} by owner:", x.series);
        for o in &x.owners {
            let _ = writeln!(
                s,
                "  {:>14} {:5.1}%{}  {}",
                o.self_count,
                o.share,
                block_text(o.per_block),
                o.owner
            );
        }
    }
    for l in &r.loss {
        let _ = writeln!(s, "loss: {l}");
    }
    if !r.unloaded.is_empty() {
        let _ = writeln!(
            s,
            "not in the load table (unnamed): {}",
            r.unloaded.join(", ")
        );
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{Clock, Raw, Run, SCHEMA};
    use dkaddrmap::{Entry, Key, Load};

    fn entry(
        cpu: Cpu,
        addr: u64,
        size: u64,
        name: &str,
        layer: Layer,
        module: Option<&str>,
    ) -> Entry {
        Entry {
            cpu,
            addr,
            size,
            name: name.into(),
            layer,
            module: module.map(String::from),
        }
    }

    fn maps() -> Vec<AddrMap> {
        vec![
            AddrMap::new(
                "ship",
                Key::default(),
                vec![
                    entry(Cpu::Cf, 0x4000_0000, 0x100, "rt_main", Layer::Our, None),
                    entry(Cpu::Cf, 0x4000_0100, 0x40, "fw_hook", Layer::Anchor, None),
                ],
            ),
            AddrMap::new(
                "module",
                Key::default(),
                vec![
                    entry(Cpu::Cf, 0, 0x80, "init", Layer::Our, Some("vendor/demo")),
                    entry(
                        Cpu::Cf,
                        0x80,
                        0x80,
                        "render",
                        Layer::Our,
                        Some("vendor/demo"),
                    ),
                    entry(
                        Cpu::Sharc,
                        0x10,
                        0x20,
                        "_pk_render",
                        Layer::Our,
                        Some("vendor/demo"),
                    ),
                ],
            ),
        ]
    }

    fn profile(raw: Vec<Raw>) -> Profile {
        Profile {
            schema: SCHEMA.into(),
            run: Run {
                id: "t".into(),
                target: "dn2".into(),
                loads: Some(LoadTable {
                    modules: vec![Load {
                        id: "vendor/demo".into(),
                        base: 0x4700_0000,
                        size: 0x100,
                        dsp_base: Some(0xb8_1000),
                        fini: None,
                    }],
                }),
                ..Run::default()
            },
            clock: Clock {
                source: "emu.cf.insn".into(),
                unit: "insn".into(),
                sampling: "synthetic".into(),
            },
            raw,
            metrics: vec![],
            markers: vec![],
            loss: vec![],
            overhead: vec![],
            spans: Vec::new(),
            span_clock_hz: None,
            span_elapsed_ticks: None,
        }
    }

    fn raw(cpu: Cpu, addr: u64, count: u64, series: &str) -> Raw {
        Raw {
            cpu,
            addr,
            count,
            series: series.into(),
        }
    }

    #[test]
    fn counters_become_self_aggregates_per_function_and_owner() {
        let p = profile(vec![
            raw(Cpu::Cf, 0x4700_0084, 30, "pc"),
            raw(Cpu::Cf, 0x4700_0090, 10, "pc"), // the same function: summed
            raw(Cpu::Cf, 0x4000_0010, 50, "pc"),
            raw(Cpu::Cf, 0x4000_0104, 5, "pc"),
            raw(Cpu::Cf, 0x1234_5678, 5, "pc"), // no layer holds it
        ]);
        let r = build(&p, &maps(), &LoadTable::default(), 10).unwrap();
        assert_eq!(r.series.len(), 1);
        let s = &r.series[0];
        assert_eq!(s.total, 100);
        let names: Vec<(&str, u64)> = s
            .functions
            .iter()
            .map(|f| (f.name.as_str(), f.self_count))
            .collect();
        assert_eq!(
            names,
            [
                ("rt_main", 50),
                ("render", 40),
                ("0x12345678", 5),
                ("fw_hook", 5)
            ]
        );
        let owners: Vec<(&str, u64)> = s
            .owners
            .iter()
            .map(|o| (o.owner.as_str(), o.self_count))
            .collect();
        assert_eq!(
            owners,
            [
                ("runtime", 50),
                ("vendor/demo", 40),
                ("firmware", 5),
                ("unnamed", 5)
            ]
        );
        assert_eq!(s.owners.iter().map(|o| o.self_count).sum::<u64>(), s.total);
    }

    #[test]
    fn series_and_cpus_stay_apart_and_dsp_names_use_dsp_base() {
        let p = profile(vec![
            raw(Cpu::Cf, 0x4700_0084, 7, "pc"),
            raw(Cpu::Cf, 0x4700_0084, 3, "idle_pc"),
            raw(Cpu::Sharc, 0xb8_1018, 9, "pc"),
        ]);
        let r = build(&p, &maps(), &LoadTable::default(), 10).unwrap();
        let keys: Vec<(&str, Cpu, u64)> = r
            .series
            .iter()
            .map(|s| (s.series.as_str(), s.cpu, s.total))
            .collect();
        assert_eq!(
            keys,
            [
                ("idle_pc", Cpu::Cf, 3),
                ("pc", Cpu::Cf, 7),
                ("pc", Cpu::Sharc, 9)
            ]
        );
        assert_eq!(r.series[2].functions[0].name, "_pk_render");
    }

    fn analysis() -> Analysis {
        let function = |name: &str, entry: u32, end: u32| dksharc::Function {
            name: name.into(),
            entry,
            end,
            global: true,
            origin: vec![],
            complete: true,
            blocks: vec![],
            effects: Default::default(),
        };
        let lp = |start, end, depth, parent, count| dksharc::DoLoop {
            do_pc: start - 1,
            start,
            end,
            count,
            parent,
            depth,
            function: Some(0),
        };
        Analysis {
            // the module's DSP image starts at software PC 0x1000 in the analysis
            base_sw: 0x1000,
            functions: vec![function("_pk_render", 0x1010, 0x1030)],
            loops: vec![
                lp(0x1014, 0x101c, 0, None, LoopCount::Lit { count: 8 }),
                lp(
                    0x1016,
                    0x1018,
                    1,
                    Some(0),
                    LoopCount::Reg {
                        ureg: 0,
                        name: "R0".into(),
                    },
                ),
            ],
            ..Analysis::default()
        }
    }

    #[test]
    fn dsp_counters_go_to_the_function_and_to_the_innermost_loop_and_per_block() {
        // dsp_base 0xb8_1000 is where the analysis's 0x1000 sits at run time: the function is at 0xb8_1010
        let mut p = profile(vec![
            raw(Cpu::Sharc, 0xb8_1011, 10, "pc"), // _pk_render, outside any loop
            raw(Cpu::Sharc, 0xb8_1015, 20, "pc"), // the outer loop
            raw(Cpu::Sharc, 0xb8_1017, 60, "pc"), // the inner loop
            raw(Cpu::Sharc, 0xb8_2000, 10, "pc"), // another module's code
        ]);
        p.metrics = vec![Metric::value(BLOCKS_METRIC, 10.0, "blocks")];
        let analyses = BTreeMap::from([("vendor/demo".to_string(), analysis())]);
        let r = build_with(&p, &maps(), &LoadTable::default(), &analyses, 10).unwrap();
        let s = &r.series[0];
        assert_eq!(r.blocks, Some(10.0));
        let render = s.functions.iter().find(|f| f.name == "_pk_render").unwrap();
        assert_eq!(render.self_count, 90);
        assert_eq!(render.per_block, Some(9.0));
        let loops: Vec<(u32, u64, &str)> = s
            .loops
            .iter()
            .map(|l| (l.depth, l.self_count, l.count.as_str()))
            .collect();
        // heaviest first; the outer loop does not include the inner one
        assert_eq!(loops, [(1, 60, "reg R0"), (0, 20, "lit 8")]);
        assert_eq!(s.loops[0].function.as_deref(), Some("_pk_render"));
        assert_eq!((s.loops[1].start, s.loops[1].end), (0x14, 0x1c));
        assert_eq!(s.loops[0].per_block, Some(6.0));
        let t = text(&r, 10);
        assert!(t.contains("by loop"), "{t}");
        assert!(t.contains("/block"), "{t}");
        // without analyses or the blocks metric nothing of that is reported
        p.metrics.clear();
        let r = build(&p, &maps(), &LoadTable::default(), 10).unwrap();
        assert!(r.series[0].loops.is_empty() && r.blocks.is_none());
        assert!(r.series[0].functions[0].per_block.is_none());
    }

    #[test]
    fn top_cuts_the_list_but_not_the_totals() {
        let p = profile(
            (0..5)
                .map(|i| raw(Cpu::Cf, 0x2000_0000 + i, 1, "pc"))
                .collect(),
        );
        let r = build(&p, &maps(), &LoadTable::default(), 2).unwrap();
        assert_eq!(r.series[0].functions.len(), 2);
        assert_eq!(r.series[0].total, 5);
        assert_eq!(r.series[0].rows, 5);
    }

    #[test]
    fn a_module_missing_from_the_load_table_is_reported() {
        let mut p = profile(vec![raw(Cpu::Cf, 0x4700_0084, 1, "pc")]);
        p.run.loads = None;
        let r = build(&p, &maps(), &LoadTable::default(), 10).unwrap();
        assert_eq!(r.unloaded, ["vendor/demo"]);
        assert_eq!(r.series[0].functions[0].name, "0x47000084");
    }

    #[test]
    fn na_metrics_keep_their_reason_and_a_bare_metric_is_refused() {
        let mut p = profile(vec![]);
        p.metrics = vec![
            Metric::value("path.run_fast.instructions", 1234.0, "insn"),
            Metric::na("host.cpu", "ns", "the device has no host CPU"),
        ];
        let text = crate::report::text(&build(&p, &[], &LoadTable::default(), 5).unwrap(), 5);
        assert!(text.contains("1234 insn"), "{text}");
        assert!(text.contains("N/A (the device has no host CPU)"), "{text}");
        let bad = r#"{"schema":"dk.profile/1","run":{"id":"x","target":"dn2"},"clock":{"source":"s","unit":"u","sampling":""},
            "raw":[],"metrics":[{"name":"m","unit":"ns"}]}"#;
        assert!(Profile::parse(bad).unwrap_err().contains("never a zero"));
    }

    #[test]
    fn a_profile_round_trips_and_checks_its_schema() {
        let p = profile(vec![raw(Cpu::Cf, 1, 2, "pc")]);
        assert_eq!(Profile::parse(&p.to_json()).unwrap(), p);
        let other = p.to_json().replace("dk.profile/1", "dk.profile/2");
        assert!(Profile::parse(&other).unwrap_err().contains("schema"));
    }
}
