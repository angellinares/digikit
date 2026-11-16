//! ColdFire PC probe (off by default): a host names PCs, and for each hit the probe records the
//! instruction count, SR, SP, the current-task pointer's value and chosen memory words. It keeps
//! the first `keep` hits per PC and counts every hit per (task, IPL). A probe with `frame` also
//! decodes the exception frame at SP and counts per (vector, stacked IPL).
//!
//! Nothing is recorded until a host sets a probe, and the probe adds only its PCs to the observed
//! set. It reads guest memory only through the caller's `read` and changes no guest state.
//!
//! The frame is the ColdFire exception frame: a long at SP whose high word holds the format
//! and the vector (`(word >> 2) & 0xff` of the high half) and whose low word is the stacked SR.
//! It means something only for a PC that is the first instruction of exception code (a vector
//! target, or a stub that no instruction has changed SP before).

use std::collections::BTreeMap;

use serde_json::{Value, json};

/// Hits kept per PC when the spec gives no `keep`.
pub const DEFAULT_KEEP: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Hit {
    n: u64,
    icount: u64,
    sr: u16,
    sp: u32,
    task: Option<u32>,
    words: Vec<Option<u32>>,
    stack: Vec<Option<u32>>,
    frame: Option<u32>,
}

#[derive(Debug, Clone)]
struct Probe {
    label: String,
    words: Vec<u32>,
    stack: usize,
    frame: bool,
    count: u64,
    hits: Vec<Hit>,
    by_task_ipl: BTreeMap<(Option<u32>, u8), u64>,
    by_frame: BTreeMap<(u8, u8), u64>,
}

#[derive(Debug, Clone, Default)]
pub struct PcProbe {
    task_ptr: Option<u32>,
    keep: usize,
    probes: BTreeMap<u32, Probe>,
}

/// One probed PC of a spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeSpec {
    pub pc: u32,
    pub label: String,
    pub words: Vec<u32>,
    /// Longs read from SP at each kept hit: at a function's first instruction, the return address and the arguments.
    pub stack: usize,
    pub frame: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PcProbeSpec {
    pub task_ptr: Option<u32>,
    pub keep: usize,
    pub probes: Vec<ProbeSpec>,
}

fn addr(value: &Value, what: &str) -> Result<u32, String> {
    let n = match value {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            Some(hex) => u64::from_str_radix(&hex.replace('_', ""), 16).ok(),
            None => s.parse().ok(),
        },
        _ => None,
    };
    n.and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| format!("{what} must be an address (integer or \"0x...\" string)"))
}

/// Parse `{task_ptr?, keep?, probes: [{pc, label?, words?: [addr], stack?: n, frame?}]}` (the `pc.probe.set`
/// parameters). Addresses are integers or `"0x..."` strings.
pub fn parse_spec(spec: &Value) -> Result<PcProbeSpec, String> {
    let task_ptr = match spec.get("task_ptr") {
        None | Some(Value::Null) => None,
        Some(v) => Some(addr(v, "task_ptr")?),
    };
    let keep = match spec.get("keep") {
        None | Some(Value::Null) => DEFAULT_KEEP,
        Some(v) => v
            .as_u64()
            .and_then(|k| usize::try_from(k).ok())
            .ok_or("keep must be a count")?,
    };
    let list = match spec.get("probes") {
        Some(Value::Array(a)) => a,
        _ => return Err("probes must be an array".into()),
    };
    let mut probes = Vec::new();
    for (i, p) in list.iter().enumerate() {
        let pc = addr(&p["pc"], &format!("probes[{i}].pc"))?;
        let label = p
            .get("label")
            .and_then(Value::as_str)
            .map_or_else(|| format!("{pc:#010x}"), str::to_owned);
        let mut words = Vec::new();
        match p.get("words") {
            None | Some(Value::Null) => {}
            Some(Value::Array(a)) => {
                for (j, w) in a.iter().enumerate() {
                    words.push(addr(w, &format!("probes[{i}].words[{j}]"))?);
                }
            }
            Some(_) => return Err(format!("probes[{i}].words must be an array")),
        }
        let frame = p.get("frame").and_then(Value::as_bool).unwrap_or(false);
        let stack = match p.get("stack") {
            None | Some(Value::Null) => 0,
            Some(v) => v
                .as_u64()
                .filter(|&n| n <= 64)
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| format!("probes[{i}].stack must be a count up to 64"))?,
        };
        probes.push(ProbeSpec {
            pc,
            label,
            words,
            stack,
            frame,
        });
    }
    Ok(PcProbeSpec {
        task_ptr,
        keep,
        probes,
    })
}

fn hex(v: u32) -> String {
    format!("{v:#010x}")
}

impl PcProbe {
    /// Replace the probe with `spec`; an empty `probes` disarms it.
    pub fn set(&mut self, spec: &PcProbeSpec) {
        self.task_ptr = spec.task_ptr;
        self.keep = spec.keep;
        self.probes = spec
            .probes
            .iter()
            .map(|p| {
                (
                    p.pc,
                    Probe {
                        label: p.label.clone(),
                        words: p.words.clone(),
                        stack: p.stack,
                        frame: p.frame,
                        count: 0,
                        hits: Vec::new(),
                        by_task_ipl: BTreeMap::new(),
                        by_frame: BTreeMap::new(),
                    },
                )
            })
            .collect();
    }

    pub fn is_empty(&self) -> bool {
        self.probes.is_empty()
    }

    /// Every PC the runtime must observe.
    pub fn pcs(&self) -> impl Iterator<Item = u32> + '_ {
        self.probes.keys().copied()
    }

    /// Forget the hits and keep the probe.
    pub fn clear(&mut self) {
        for p in self.probes.values_mut() {
            p.count = 0;
            p.hits.clear();
            p.by_task_ipl.clear();
            p.by_frame.clear();
        }
    }

    /// Called before the instruction at `pc` runs. `read` returns a guest long, None when unmapped.
    pub fn observe(
        &mut self,
        pc: u32,
        icount: u64,
        sr: u16,
        sp: u32,
        read: &mut dyn FnMut(u32) -> Option<u32>,
    ) {
        let Some(p) = self.probes.get_mut(&pc) else {
            return;
        };
        p.count += 1;
        let ipl = ((sr >> 8) & 7) as u8;
        let task = self.task_ptr.and_then(&mut *read);
        *p.by_task_ipl.entry((task, ipl)).or_insert(0) += 1;
        let frame = if p.frame { read(sp) } else { None };
        if let Some(f) = frame {
            let vector = ((f >> 18) & 0xff) as u8;
            let stacked_ipl = (((f & 0xffff) >> 8) & 7) as u8;
            *p.by_frame.entry((vector, stacked_ipl)).or_insert(0) += 1;
        }
        if p.hits.len() < self.keep {
            let words = p.words.iter().map(|&a| read(a)).collect();
            let stack = (0..p.stack)
                .map(|i| read(sp.wrapping_add(4 * i as u32)))
                .collect();
            p.hits.push(Hit {
                n: p.count,
                icount,
                sr,
                sp,
                task,
                words,
                stack,
                frame,
            });
        }
    }

    /// `{task_ptr, keep, probes: [{pc, label, count, by_task_ipl, by_frame?, hits}]}`.
    pub fn report(&self) -> Value {
        let opt = |v: Option<u32>| v.map_or(Value::Null, |v| json!(hex(v)));
        let probes: Vec<Value> = self
            .probes
            .iter()
            .map(|(&pc, p)| {
                let by_task_ipl: Vec<Value> = p
                    .by_task_ipl
                    .iter()
                    .map(|(&(task, ipl), &n)| json!({"task": opt(task), "ipl": ipl, "count": n}))
                    .collect();
                let hits: Vec<Value> = p
                    .hits
                    .iter()
                    .map(|h| {
                        let mut v = json!({
                            "n": h.n,
                            "icount": h.icount,
                            "sr": format!("{:#06x}", h.sr),
                            "ipl": (h.sr >> 8) & 7,
                            "sp": hex(h.sp),
                            "task": opt(h.task),
                            "words": h.words.iter().map(|&w| opt(w)).collect::<Vec<_>>(),
                            "stack": h.stack.iter().map(|&w| opt(w)).collect::<Vec<_>>(),
                        });
                        if let Some(f) = h.frame {
                            v["frame"] = json!({
                                "long": hex(f),
                                "vector": (f >> 18) & 0xff,
                                "stacked_sr": format!("{:#06x}", f & 0xffff),
                                "stacked_ipl": ((f & 0xffff) >> 8) & 7,
                            });
                        }
                        v
                    })
                    .collect();
                let mut v = json!({
                    "pc": hex(pc),
                    "label": p.label,
                    "count": p.count,
                    "by_task_ipl": by_task_ipl,
                    "hits": hits,
                });
                if p.frame {
                    v["by_frame"] = json!(
                        p.by_frame
                            .iter()
                            .map(|(&(vector, ipl), &n)| json!({"vector": vector, "stacked_ipl": ipl, "count": n}))
                            .collect::<Vec<_>>()
                    );
                }
                v
            })
            .collect();
        json!({"task_ptr": self.task_ptr.map(hex), "keep": self.keep, "probes": probes})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coldfire::{Bus, BusError, Cpu, InterruptPolicy};

    struct Ram(Vec<u8>);

    impl Ram {
        fn range(&self, addr: u32, size: usize, write: bool) -> Result<usize, BusError> {
            let p = addr as usize;
            if p.checked_add(size).is_none_or(|end| end > self.0.len()) {
                return Err(BusError { addr, write });
            }
            Ok(p)
        }
    }

    impl Bus for Ram {
        fn read8(&mut self, a: u32) -> Result<u8, BusError> {
            Ok(self.0[self.range(a, 1, false)?])
        }
        fn read16(&mut self, a: u32) -> Result<u16, BusError> {
            let p = self.range(a, 2, false)?;
            Ok(u16::from_be_bytes(self.0[p..p + 2].try_into().unwrap()))
        }
        fn read32(&mut self, a: u32) -> Result<u32, BusError> {
            let p = self.range(a, 4, false)?;
            Ok(u32::from_be_bytes(self.0[p..p + 4].try_into().unwrap()))
        }
        fn write8(&mut self, a: u32, v: u8) -> Result<(), BusError> {
            let p = self.range(a, 1, true)?;
            self.0[p] = v;
            Ok(())
        }
        fn write16(&mut self, a: u32, v: u16) -> Result<(), BusError> {
            let p = self.range(a, 2, true)?;
            self.0[p..p + 2].copy_from_slice(&v.to_be_bytes());
            Ok(())
        }
        fn write32(&mut self, a: u32, v: u32) -> Result<(), BusError> {
            let p = self.range(a, 4, true)?;
            self.0[p..p + 4].copy_from_slice(&v.to_be_bytes());
            Ok(())
        }
    }

    fn spec(task_ptr: u32, keep: usize, probes: &[(u32, &[u32], bool)]) -> PcProbeSpec {
        PcProbeSpec {
            task_ptr: Some(task_ptr),
            keep,
            probes: probes
                .iter()
                .map(|&(pc, words, frame)| ProbeSpec {
                    pc,
                    label: format!("p{pc:x}"),
                    words: words.to_vec(),
                    stack: if frame { 2 } else { 0 },
                    frame,
                })
                .collect(),
        }
    }

    /// Runs a program through the real core: `moveq #1,d0` at 0x3400 (probed, task word A),
    /// then an interrupt at IPL 3 into a handler at 0x3500 (`nop`; `rte`, probed with a frame),
    /// back to `nop` at 0x3402 (probed, task word B), and a second pass of the handler at IPL 5.
    #[test]
    fn counts_and_first_hit_fields_through_the_core() {
        let task_ptr = 0x9000;
        let keep = 1;
        let mut cpu = Cpu::new();
        let mut ram = Ram(vec![0; 0x10000]);
        cpu.ctrl.vbr = 0;
        cpu.pc = 0x3400;
        cpu.a[7] = 0x8000;
        cpu.sr = 0x2004;
        ram.write32(task_ptr, 0xaaaa_0001).unwrap();
        ram.write32(0x9100, 0xdead_beef).unwrap();
        ram.write16(0x3400, 0x7001).unwrap(); // moveq #1,d0
        ram.write16(0x3402, 0x4e71).unwrap(); // nop
        ram.write16(0x3404, 0x4e71).unwrap(); // nop
        ram.write32(4 * 208, 0x3500).unwrap();
        ram.write16(0x3500, 0x4e71).unwrap(); // nop
        ram.write16(0x3502, 0x4e73).unwrap(); // rte

        let mut probe = PcProbe::default();
        probe.set(&spec(
            task_ptr,
            keep,
            &[
                (0x3400, &[0x9100], false),
                (0x3402, &[], false),
                (0x3500, &[0x9100, 0xfffff0], true),
            ],
        ));
        assert_eq!(probe.pcs().collect::<Vec<_>>(), [0x3400, 0x3402, 0x3500]);

        let step = |cpu: &mut Cpu, ram: &mut Ram, probe: &mut PcProbe| {
            let (pc, ic, sr, sp) = (cpu.pc, cpu.icount, cpu.sr, cpu.a[7]);
            let mut read = |a: u32| ram.read32(a).ok();
            probe.observe(pc, ic, sr, sp, &mut read);
            cpu.step(ram).unwrap();
        };
        step(&mut cpu, &mut ram, &mut probe); // 0x3400
        assert!(
            cpu.take_interrupt(&mut ram, 208, Some(3), InterruptPolicy::Oracle)
                .unwrap()
        );
        step(&mut cpu, &mut ram, &mut probe); // 0x3500 at IPL 3
        step(&mut cpu, &mut ram, &mut probe); // 0x3502 rte (not probed)
        ram.write32(task_ptr, 0xbbbb_0002).unwrap();
        step(&mut cpu, &mut ram, &mut probe); // 0x3402
        cpu.sr = 0x2004;
        assert!(
            cpu.take_interrupt(&mut ram, 208, Some(5), InterruptPolicy::Oracle)
                .unwrap()
        );
        step(&mut cpu, &mut ram, &mut probe); // 0x3500 at IPL 5
        ram.write32(task_ptr, 0xcccc_0003).unwrap();
        cpu.pc = 0x3402;
        cpu.sr = 0x2004;
        step(&mut cpu, &mut ram, &mut probe); // second hit of 0x3402: counted, not kept

        let r = probe.report();
        let by_pc = |pc: &str| {
            r["probes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["pc"] == pc)
                .unwrap()
                .clone()
        };

        let a = by_pc("0x00003400");
        assert_eq!(a["count"], 1);
        assert_eq!(a["hits"][0]["icount"], 0);
        assert_eq!(a["hits"][0]["sr"], "0x2004");
        assert_eq!(a["hits"][0]["sp"], "0x00008000");
        assert_eq!(a["hits"][0]["task"], "0xaaaa0001");
        assert_eq!(a["hits"][0]["words"][0], "0xdeadbeef");

        let b = by_pc("0x00003402");
        assert_eq!(b["count"], 2);
        assert_eq!(b["hits"].as_array().unwrap().len(), 1); // keep is 1, the second was only counted
        assert_eq!(b["hits"][0]["task"], "0xbbbb0002");
        assert_eq!(b["by_task_ipl"].as_array().unwrap().len(), 2);

        let h = by_pc("0x00003500");
        assert_eq!(h["count"], 2);
        let first = &h["hits"][0];
        assert_eq!(first["ipl"], 3);
        assert_eq!(first["sp"], "0x00007ff8");
        assert_eq!(first["words"][0], "0xdeadbeef");
        assert_eq!(first["words"][1], Value::Null); // unmapped
        assert_eq!(first["stack"][0], "0x43402000");
        assert_eq!(first["stack"][1], "0x00003402");
        assert_eq!(first["frame"]["vector"], 208);
        assert_eq!(first["frame"]["stacked_sr"], "0x2000");
        assert_eq!(first["frame"]["stacked_ipl"], 0);
        let frames = h["by_frame"].as_array().unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0]["count"], 2);
        let ipls: Vec<_> = h["by_task_ipl"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["ipl"].as_u64().unwrap())
            .collect();
        assert_eq!(ipls, [3, 5]);
    }

    #[test]
    fn keeps_the_first_hits_and_counts_per_task_and_ipl() {
        let mut probe = PcProbe::default();
        probe.set(&spec(0x10, 2, &[(0x100, &[], false)]));
        let hits = [
            (1u32, 0x2000u16),
            (1, 0x2300),
            (2, 0x2000),
            (2, 0x2000),
            (2, 0x2700),
        ];
        for (n, (t, sr)) in hits.into_iter().enumerate() {
            probe.observe(0x100, n as u64, sr, 0, &mut |a| (a == 0x10).then_some(t));
        }
        probe.observe(0x104, 9, 0, 0, &mut |_| None); // not probed
        let p = &probe.report()["probes"][0];
        assert_eq!(p["count"], 5);
        assert_eq!(p["hits"].as_array().unwrap().len(), 2);
        assert_eq!(p["hits"][1]["n"], 2);
        let rows: Vec<(String, u64, u64)> = p["by_task_ipl"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                (
                    e["task"].to_string(),
                    e["ipl"].as_u64().unwrap(),
                    e["count"].as_u64().unwrap(),
                )
            })
            .collect();
        assert_eq!(rows.len(), 4);
        assert!(rows.contains(&("\"0x00000002\"".into(), 0, 2)));
        assert!(rows.contains(&("\"0x00000002\"".into(), 7, 1)));
        probe.clear();
        assert_eq!(probe.report()["probes"][0]["count"], 0);
        assert!(!probe.is_empty());
    }

    #[test]
    fn empty_probe_observes_nothing_and_adds_no_pcs() {
        let mut probe = PcProbe::default();
        assert!(probe.is_empty());
        assert_eq!(probe.pcs().count(), 0);
        let mut reads = 0;
        probe.observe(0x100, 0, 0, 0, &mut |_| {
            reads += 1;
            Some(0)
        });
        assert_eq!(reads, 0);
        assert_eq!(probe.report()["probes"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn parses_the_spec() {
        let s = parse_spec(&json!({
            "task_ptr": "0x47db3f64",
            "probes": [{"pc": "0x40000410", "label": "sw", "words": [16, "0x20"], "stack": 3, "frame": true}, {"pc": 4}]
        }))
        .unwrap();
        assert_eq!(s.task_ptr, Some(0x47db3f64));
        assert_eq!(s.keep, DEFAULT_KEEP);
        assert_eq!(s.probes[0].words, [16, 0x20]);
        assert!(s.probes[0].frame);
        assert_eq!(s.probes[0].stack, 3);
        assert_eq!(s.probes[1].label, "0x00000004");
        assert!(parse_spec(&json!({})).is_err());
        assert!(parse_spec(&json!({"probes": [{"pc": "zz"}]})).is_err());
        assert!(parse_spec(&json!({"probes": [{"pc": 4, "stack": 65}]})).is_err());
        assert!(parse_spec(&json!({"probes": [{"pc": 0x1_0000_0000u64}]})).is_err());
    }
}
