//! Exact executed-instruction profile for cycle costing (diagnostic, off by default).
//! `SHARC_CYCMIX=1` with `SHARC_CYCMIX_OUT=path.tsv` (or `DIGI_DIAG_DIR`), and optionally
//! `SHARC_CYCMIX_MEM=file` with lines `pc nbytes branch delayed [i1 [i2]]` (hex software PC, instruction
//! bytes, branch 0 none / 1 unconditional / 2 conditional, delayed 0/1, decimal I register numbers 0..15 used as
//! the DAG base of the data accesses of that instruction). The file is made by tools/sharc_dev/sharc_cpi.py
//! from the program database; without it only the per-PC counts and pairs are written.
//!
//! Besides the counts the profile simulates, online, three structures whose hit rates depend on the order of
//! execution: the 2-way L1 instruction cache for code in L2 (line 64 bytes, PRM Table 8-1: 512 bits), the two 2-way data
//! caches (DM, kind `d`, and PM, kind `p`; PRM p. 8-7) for L2 and external data (line 64 bytes), each at 16, 32 and 64 KB, and the branch target buffer (128 sets x 2 ways,
//! set = low seven bits of the short-word PC, 2-bit counter, LRU). Sizes and indexing are the manual's where it
//! gives them and assumptions where it does not (see tools/sharc_dev/sharc_timing.py).
//!
//! Only interpreter steps are hooked: run with `SHARC_AOT=0 SHARC_FAST_REGIONS= SHARC_BLOCKTRANS=`
//! so that every instruction goes through the interpreter. The idle loop is still skipped
//! analytically (its instructions are not counted).
//!
//! Output lines (tab separated): `P pc count`, `T from to count` (every executed
//! instruction pair, previous PC -> this PC), `M pc slot b0..b7` (data accesses by address
//! bucket, see `bucket`), `S pc n` (instructions whose two accesses fell in the same bucket),
//! `C kind KB accesses misses` (kind i, d for DM or p for PM), `B pc n taken hit_t hit_n mis_pt mis_pn miss_t miss_n` (branch
//! outcomes against the BTB), `U pc raw>>16 n` (accesses that no bucket claims), `L pc slot n` (L2/external accesses whose
//! 64-byte line differs from the previous access of the same instruction slot), `R raw>>20 n`
//! (L2/external accesses by 1 MiB region), `A pc address n` (accesses to a core or system MMR, by
//! exact address, so a cost model can price each peripheral), `I lo hi head` (the idle-skip window).

use std::collections::HashMap;
use std::io::Write;

/// A 2-way LRU set-associative tag store.
struct SetAssoc {
    sets: u32,
    tags: Vec<u32>,
    mru: Vec<u8>,
    accesses: u64,
    misses: u64,
}

impl SetAssoc {
    fn new(bytes: u32, line: u32) -> SetAssoc {
        let sets = bytes / line / 2;
        SetAssoc {
            sets,
            tags: vec![u32::MAX; sets as usize * 2],
            mru: vec![0; sets as usize],
            accesses: 0,
            misses: 0,
        }
    }

    fn access(&mut self, line: u32) {
        self.accesses += 1;
        let set = (line % self.sets) as usize;
        let t = &mut self.tags[set * 2..set * 2 + 2];
        if t[0] == line {
            self.mru[set] = 0;
        } else if t[1] == line {
            self.mru[set] = 1;
        } else {
            self.misses += 1;
            let victim = 1 - self.mru[set] as usize;
            t[victim] = line;
            self.mru[set] = victim as u8;
        }
    }
}

/// Branch target buffer: 128 sets x 2 ways of (tag, 2-bit counter).
struct Btb {
    tag: [u32; 256],
    ctr: [u8; 256],
    mru: [u8; 128],
}

#[derive(Clone, Copy)]
struct Spec {
    nbytes: u8,
    branch: u8,
    delayed: bool,
    nregs: u8,
    regs: [u8; 2],
}

struct Pending {
    pc: u32,
    fall: u32,
    wait: u8,
}

pub struct CycMix {
    out: std::ffi::OsString,
    counts: HashMap<u32, u64>,
    pairs: HashMap<(u32, u32), u64>,
    last: u32,
    spec: HashMap<u32, Spec>,
    icache: Vec<(u32, SetAssoc)>,
    dcache: Vec<(u32, SetAssoc)>,
    pcache: Vec<(u32, SetAssoc)>,
    btb: Btb,
    branches: HashMap<u32, [u64; 8]>,
    pending: Option<Pending>,
    last_iline: u32,
    mem: HashMap<u32, [[u64; 8]; 2]>,
    same: HashMap<u32, u64>,
    unknown: HashMap<(u32, u32), u64>,
    /// Per PC and slot: how often the cache line differs from the previous access of that slot.
    lines: HashMap<(u32, usize), (u32, u64)>,
    /// Accesses to L2 and external memory by `raw >> 20`.
    far: HashMap<u32, u64>,
    /// Accesses to a core or system MMR (bucket 7), by PC and exact address.
    mmr: HashMap<(u32, u32), u64>,
    pub idle: (u32, u32, u32),
}

/// 0 other, 1..=4 L1 block 0..3, 5 L2 SRAM, 6 external, 7 core/system MMR.
/// The ranges are the datasheet's address maps (byte, normal-word and short-word spaces; see
/// `sharc-rt` addressing.rs); an I register holds an address in the space of its instruction.
pub fn bucket(a: u32) -> usize {
    match a {
        0x240000..0x270000 | 0x90000..0x9c000 | 0x120000..0x138000 => 1,
        0x2c0000..0x2f0000 | 0xb0000..0xbc000 | 0x160000..0x178000 => 2,
        0x300000..0x320000 | 0xc0000..0xc8000 | 0x180000..0x190000 => 3,
        0x380000..0x3a0000 | 0xe0000..0xe8000 | 0x1c0000..0x1d0000 => 4,
        0x20000000..0x20118000 | 0x8000000..0x8046000 | 0xb80000..0xc0c000 => 5,
        0x10000000..0x18000000 | 0x4000000..0x8000000 | 0x60000000..=0xffff_ffff => 6,
        0..0x80000 | 0x30000000..0x31100000 => 7,
        _ => 0,
    }
}

impl CycMix {
    pub fn from_env() -> Option<Box<CycMix>> {
        std::env::var_os("SHARC_CYCMIX")?;
        let out = crate::diag::out_path("SHARC_CYCMIX_OUT", "cycmix.tsv")?;
        let mut spec: HashMap<u32, Spec> = HashMap::new();
        if let Some(p) = std::env::var_os("SHARC_CYCMIX_MEM") {
            let text = std::fs::read_to_string(&p).unwrap_or_default();
            for line in text.lines() {
                let f: Vec<&str> = line.split_whitespace().collect();
                if f.len() < 4 {
                    continue;
                }
                let Ok(pc) = u32::from_str_radix(f[0].trim_start_matches("0x"), 16) else {
                    continue;
                };
                let regs: Vec<u8> = f[4..]
                    .iter()
                    .filter_map(|x| x.parse().ok())
                    .filter(|r| *r < 16)
                    .collect();
                let mut r2 = [0u8; 2];
                for (k, r) in regs.iter().take(2).enumerate() {
                    r2[k] = *r;
                }
                spec.insert(
                    pc,
                    Spec {
                        nbytes: f[1].parse().unwrap_or(2),
                        branch: f[2].parse().unwrap_or(0),
                        delayed: f[3] == "1",
                        nregs: regs.len().min(2) as u8,
                        regs: r2,
                    },
                );
            }
        }
        Some(Box::new(CycMix {
            out,
            counts: HashMap::new(),
            pairs: HashMap::new(),
            last: u32::MAX,
            spec,
            icache: [16u32, 32, 64]
                .iter()
                .map(|k| (*k, SetAssoc::new(k * 1024, 64)))
                .collect(),
            dcache: [16u32, 32, 64]
                .iter()
                .map(|k| (*k, SetAssoc::new(k * 1024, 64)))
                .collect(),
            pcache: [16u32, 32, 64]
                .iter()
                .map(|k| (*k, SetAssoc::new(k * 1024, 64)))
                .collect(),
            btb: Btb {
                tag: [u32::MAX; 256],
                ctr: [0; 256],
                mru: [0; 128],
            },
            branches: HashMap::new(),
            pending: None,
            last_iline: u32::MAX,
            mem: HashMap::new(),
            same: HashMap::new(),
            unknown: HashMap::new(),
            lines: HashMap::new(),
            far: HashMap::new(),
            mmr: HashMap::new(),
            idle: (0, 0, 0),
        }))
    }

    /// Resolve a branch against the BTB: `taken` is whether the flow left the fall-through path.
    fn resolve(&mut self, pc: u32, conditional: bool, taken: bool) {
        let set = (pc % 128) as usize;
        let way = if self.btb.tag[set * 2] == pc {
            Some(0)
        } else if self.btb.tag[set * 2 + 1] == pc {
            Some(1)
        } else {
            None
        };
        let r = self.branches.entry(pc).or_default();
        r[0] += 1;
        r[1] += taken as u64;
        match way {
            Some(w) => {
                let i = set * 2 + w;
                let pred = self.btb.ctr[i] >= 2;
                // r[2] hit and predicted taken, taken; r[3] hit, not taken, not taken;
                // r[4] predicted taken, not taken; r[5] predicted not taken, taken.
                if !conditional {
                    r[2] += 1;
                } else {
                    match (pred, taken) {
                        (true, true) => r[2] += 1,
                        (false, false) => r[3] += 1,
                        (true, false) => r[4] += 1,
                        (false, true) => r[5] += 1,
                    }
                }
                self.btb.ctr[i] = if taken {
                    (self.btb.ctr[i] + 1).min(3)
                } else {
                    self.btb.ctr[i].saturating_sub(1)
                };
                self.btb.mru[set] = w as u8;
            }
            None => {
                if taken {
                    r[6] += 1;
                } else {
                    r[7] += 1;
                }
                let victim = 1 - self.btb.mru[set] as usize;
                let i = set * 2 + victim;
                self.btb.tag[i] = pc;
                self.btb.ctr[i] = if taken { 2 } else { 1 };
                self.btb.mru[set] = victim as u8;
            }
        }
    }

    /// Just before the instruction at PC executes.
    pub fn hit(&mut self, s: &crate::rt::St, pc: u32) {
        *self.counts.entry(pc).or_default() += 1;
        *self.pairs.entry((self.last, pc)).or_default() += 1;
        self.last = pc;
        if let Some(p) = &mut self.pending {
            if p.wait == 0 {
                let (bpc, fall) = (p.pc, p.fall);
                self.pending = None;
                let conditional = self.spec.get(&bpc).is_some_and(|x| x.branch == 2);
                self.resolve(bpc, conditional, pc != fall);
            } else {
                p.wait -= 1;
            }
        }
        let Some(spec) = self.spec.get(&pc).copied() else {
            return;
        };
        if (0xb80000..0xc0c000).contains(&pc) {
            let first = pc * 2 / 64;
            let last = (pc * 2 + spec.nbytes as u32 - 1) / 64;
            if first != self.last_iline {
                for (_, c) in &mut self.icache {
                    c.access(first);
                }
            }
            if last != first {
                for (_, c) in &mut self.icache {
                    c.access(last);
                }
            }
            self.last_iline = last;
        } else {
            self.last_iline = u32::MAX;
        }
        if spec.branch != 0 && self.pending.is_none() {
            let nxt = pc + spec.nbytes as u32 / 2;
            if spec.delayed {
                let w1 = self.spec.get(&nxt).map_or(2, |x| x.nbytes as u32);
                let n2 = nxt + w1 / 2;
                let w2 = self.spec.get(&n2).map_or(2, |x| x.nbytes as u32);
                self.pending = Some(Pending {
                    pc,
                    fall: n2 + w2 / 2,
                    wait: 2,
                });
            } else {
                self.pending = Some(Pending {
                    pc,
                    fall: nxt,
                    wait: 0,
                });
            }
        }
        if spec.nregs > 0 {
            let slots = self.mem.entry(pc).or_default();
            let mut seen = [usize::MAX; 2];
            for k in 0..spec.nregs as usize {
                let a = s.r[16 + spec.regs[k] as usize].b;
                let b = bucket(a);
                slots[k][b] += 1;
                seen[k] = b;
                if b == 0 {
                    *self.unknown.entry((pc, a >> 16)).or_default() += 1;
                }
                if b == 7 {
                    *self.mmr.entry((pc, a)).or_default() += 1;
                }
                if b >= 5 && b <= 6 {
                    *self.far.entry(a >> 20).or_default() += 1;
                    // 64-byte lines: 32 short words, 16 normal words, or 64 bytes.
                    let line = if (0xb80000..0xc0c000).contains(&a) {
                        a >> 5
                    } else if a < 0x60000000 {
                        a >> 4
                    } else {
                        a >> 6
                    };
                    let e = self.lines.entry((pc, k)).or_insert((u32::MAX, 0));
                    if e.0 != line {
                        e.0 = line;
                        e.1 += 1;
                    }
                    // DM accesses use I0..I7, PM accesses I8..I15: one data cache each.
                    let caches = if spec.regs[k] < 8 {
                        &mut self.dcache
                    } else {
                        &mut self.pcache
                    };
                    for (_, c) in caches {
                        c.access(line);
                    }
                }
            }
            if seen[1] != usize::MAX && seen[0] == seen[1] && (1..=4).contains(&seen[0]) {
                *self.same.entry(pc).or_default() += 1;
            }
        }
    }
}

impl Drop for CycMix {
    fn drop(&mut self) {
        let Ok(f) = std::fs::File::create(&self.out) else {
            return;
        };
        let mut w = std::io::BufWriter::new(f);
        let mut v: Vec<_> = self.counts.iter().collect();
        v.sort();
        for (pc, n) in v {
            let _ = writeln!(w, "P\t{pc:#x}\t{n}");
        }
        let mut v: Vec<_> = self.pairs.iter().collect();
        v.sort();
        for ((a, b), n) in v {
            let _ = writeln!(w, "T\t{a:#x}\t{b:#x}\t{n}");
        }
        let mut v: Vec<_> = self.mem.iter().collect();
        v.sort_by_key(|x| *x.0);
        for (pc, slots) in v {
            for (k, s) in slots.iter().enumerate() {
                if s.iter().any(|x| *x != 0) {
                    let c: Vec<String> = s.iter().map(|x| x.to_string()).collect();
                    let _ = writeln!(w, "M\t{pc:#x}\t{k}\t{}", c.join("\t"));
                }
            }
        }
        let mut v: Vec<_> = self.same.iter().collect();
        v.sort();
        for (pc, n) in v {
            let _ = writeln!(w, "S\t{pc:#x}\t{n}");
        }
        let mut v: Vec<_> = self.unknown.iter().collect();
        v.sort();
        for ((pc, hi), n) in v {
            let _ = writeln!(w, "U\t{pc:#x}\t{hi:#x}\t{n}");
        }
        let mut v: Vec<_> = self.lines.iter().collect();
        v.sort();
        for ((pc, k), (_, n)) in v {
            let _ = writeln!(w, "L\t{pc:#x}\t{k}\t{n}");
        }
        let mut v: Vec<_> = self.far.iter().collect();
        v.sort();
        for (hi, n) in v {
            let _ = writeln!(w, "R\t{hi:#x}\t{n}");
        }
        let mut v: Vec<_> = self.mmr.iter().collect();
        v.sort();
        for ((pc, a), n) in v {
            let _ = writeln!(w, "A\t{pc:#x}\t{a:#x}\t{n}");
        }
        for (kind, set) in [
            ("i", &self.icache),
            ("d", &self.dcache),
            ("p", &self.pcache),
        ] {
            for (kb, c) in set.iter() {
                let _ = writeln!(w, "C\t{kind}\t{kb}\t{}\t{}", c.accesses, c.misses);
            }
        }
        let mut v: Vec<_> = self.branches.iter().collect();
        v.sort();
        for (pc, r) in v {
            let c: Vec<String> = r.iter().map(|x| x.to_string()).collect();
            let _ = writeln!(w, "B\t{pc:#x}\t{}", c.join("\t"));
        }
        let _ = writeln!(
            w,
            "I\t{:#x}\t{:#x}\t{:#x}",
            self.idle.0, self.idle.1, self.idle.2
        );
        let _ = w.flush();
    }
}
