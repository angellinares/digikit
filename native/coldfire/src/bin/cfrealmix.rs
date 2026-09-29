//! Real-firmware-mix speed for the ColdFire interpreter (cfbench.rs's
//! straight-line loop is a ceiling, not a real-code number). Loads a real
//! machine state (registers + every mapped RAM/flash page) exported by
//! `scratchpad/export_snap.py` from an `emu.snapshot` capture, and runs
//! `Cpu::step` over it with a stub bus: RAM/flash pages come from the dump
//! (a flat page-pointer table indexed by `addr >> 20`, at the snapshot's
//! own 1 MiB page granularity, `tools/snapread.py`'s `PAGE`); any address
//! outside those pages is peripheral/MMIO space and is answered with a
//! constant 0 on reads and silently accepted on writes (no recorded-MMIO-
//! trace replay --
//! wiring `native/periph`'s trace reader in needs clock/SR synchronization
//! this bench doesn't attempt; see the handback). Interrupts are not
//! delivered (nothing raises one from outside `Cpu::step`).
//!
//! The window stops when the stub can no longer answer an instruction
//! *fetch* (PC lands on a page this dump never mapped -- the one case a
//! constant would silently fabricate an instruction stream instead of
//! admitting the limit), when `Cpu::step` itself reports `Unimplemented`/
//! `Halted`, or at the instruction limit.
//!
//! `cfrealmix DUMP [LIMIT] [--profile]`: LIMIT defaults to 20_000_000.
//! `--profile` runs a second, instrumented pass (bus-call time + a decode-
//! only microbench over the sampled opcode stream) to split time into
//! bus/memory vs. decode vs. the execute remainder; it is not part of the
//! headline instructions/s number, which always comes from the plain pass.

use coldfire::{Bus, BusError, Cpu, Stop, decode};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

const PAGE_SIZE: usize = 0x0010_0000; // matches tools/snapread.py's PAGE
const PAGE_SHIFT: u32 = 20; // log2(PAGE_SIZE)
const PAGE_MASK: u32 = !(PAGE_SIZE as u32 - 1);
/// Perf step 2 (cf-interp-speed.md): `pages[addr >> PAGE_SHIFT]` covers the
/// full 32-bit address space at 1 MiB granularity, so a mapped RAM/flash
/// access is a shift + array index, never a hash. `None` means unmapped/
/// peripheral space, the only case that reaches the MMIO stub below.
const PAGE_TABLE_LEN: usize = 1 << (32 - PAGE_SHIFT);

#[inline]
fn page_index(addr: u32) -> usize {
    (addr >> PAGE_SHIFT) as usize
}

/// bra.b -2 (CFPRM p.81-82): branches to its own address, the firmware's
/// idle spin. Confirmed, not just inferred from pc==pc_before: see
/// `check_spin_opcode` below.
const BRA_SELF: u16 = 0x60fe;

struct SparseBus {
    pages: Vec<Option<Box<[u8; PAGE_SIZE]>>>,
    mmio_reads: u64,
    mmio_writes: u64,
    // --profile only
    profiling: bool,
    bus_ns: u64,
    sample: Vec<(u32, [u16; 3])>,
    sample_cap: usize,
}

impl SparseBus {
    fn new(pages: Vec<Option<Box<[u8; PAGE_SIZE]>>>, profiling: bool) -> SparseBus {
        SparseBus {
            pages,
            mmio_reads: 0,
            mmio_writes: 0,
            profiling,
            bus_ns: 0,
            sample: Vec::new(),
            sample_cap: 2_000_000,
        }
    }

    fn page_present(&self, addr: u32) -> bool {
        self.pages[page_index(addr)].is_some()
    }

    /// `n` bytes at `addr`, only when they fit in one dump page (they always
    /// do here: real accesses never straddle a 1 MiB region boundary).
    fn slice(&self, addr: u32, n: usize) -> Option<&[u8]> {
        let base = addr & PAGE_MASK;
        let off = (addr - base) as usize;
        if off + n > PAGE_SIZE {
            return None;
        }
        self.pages[page_index(addr)]
            .as_deref()
            .map(|p| &p[off..off + n])
    }

    fn slice_mut(&mut self, addr: u32, n: usize) -> Option<&mut [u8]> {
        let base = addr & PAGE_MASK;
        let off = (addr - base) as usize;
        if off + n > PAGE_SIZE {
            return None;
        }
        self.pages[page_index(addr)]
            .as_deref_mut()
            .map(|p| &mut p[off..off + n])
    }
}

/// Every access goes through here so `--profile` can time "bus dispatch +
/// memory access" as one bucket (the two are not separable without editing
/// `cpu.rs`, out of this bench's scope -- see the module doc).
macro_rules! timed {
    ($self:expr, $body:expr) => {{
        if $self.profiling {
            let t0 = Instant::now();
            let r = $body;
            $self.bus_ns += t0.elapsed().as_nanos() as u64;
            r
        } else {
            $body
        }
    }};
}

impl Bus for SparseBus {
    fn read8(&mut self, a: u32) -> Result<u8, BusError> {
        Ok(timed!(self, {
            match self.slice(a, 1) {
                Some(s) => s[0],
                None => {
                    self.mmio_reads += 1;
                    0
                }
            }
        }))
    }
    fn read16(&mut self, a: u32) -> Result<u16, BusError> {
        Ok(timed!(self, {
            match self.slice(a, 2) {
                Some(s) => u16::from_be_bytes([s[0], s[1]]),
                None => {
                    self.mmio_reads += 1;
                    0
                }
            }
        }))
    }
    fn read32(&mut self, a: u32) -> Result<u32, BusError> {
        Ok(timed!(self, {
            match self.slice(a, 4) {
                Some(s) => u32::from_be_bytes(s.try_into().unwrap()),
                None => {
                    self.mmio_reads += 1;
                    0
                }
            }
        }))
    }
    fn write8(&mut self, a: u32, v: u8) -> Result<(), BusError> {
        timed!(self, {
            match self.slice_mut(a, 1) {
                Some(s) => s[0] = v,
                None => self.mmio_writes += 1,
            }
        });
        Ok(())
    }
    fn write16(&mut self, a: u32, v: u16) -> Result<(), BusError> {
        timed!(self, {
            match self.slice_mut(a, 2) {
                Some(s) => s.copy_from_slice(&v.to_be_bytes()),
                None => self.mmio_writes += 1,
            }
        });
        Ok(())
    }
    fn write32(&mut self, a: u32, v: u32) -> Result<(), BusError> {
        timed!(self, {
            match self.slice_mut(a, 4) {
                Some(s) => s.copy_from_slice(&v.to_be_bytes()),
                None => self.mmio_writes += 1,
            }
        });
        Ok(())
    }
}

fn load_dump(path: &str) -> (Cpu, Vec<Option<Box<[u8; PAGE_SIZE]>>>) {
    let raw = std::fs::read(path).expect("read dump");
    assert_eq!(&raw[0..8], b"CFDUMP1\0", "bad magic in {path}");
    let mut off = 8usize;
    let u32le = |o: usize| u32::from_le_bytes(raw[o..o + 4].try_into().unwrap());
    let mut cpu = Cpu::new();
    for i in 0..8 {
        cpu.d[i] = u32le(off);
        off += 4;
    }
    for i in 0..8 {
        cpu.a[i] = u32le(off);
        off += 4;
    }
    cpu.pc = u32le(off);
    off += 4;
    cpu.sr = u32le(off) as u16;
    off += 4;
    let npages = u32le(off);
    off += 4;
    let mut pages: Vec<Option<Box<[u8; PAGE_SIZE]>>> = vec![None; PAGE_TABLE_LEN];
    for _ in 0..npages {
        let base = u32le(off);
        off += 4;
        let mut page = Box::new([0u8; PAGE_SIZE]);
        page.copy_from_slice(&raw[off..off + PAGE_SIZE]);
        off += PAGE_SIZE;
        assert_eq!(base & !PAGE_MASK, 0, "dump page base not page-aligned");
        pages[page_index(base)] = Some(page);
    }
    (cpu, pages)
}

struct RunResult {
    icount_start: u64,
    icount_end: u64,
    idle: u64,
    elapsed: Duration,
    stop_reason: &'static str,
    mmio_reads: u64,
    mmio_writes: u64,
    bus_ns: u64,
    sample: Vec<(u32, [u16; 3])>,
    /// Correctness gate for perf changes to this crate: a hash of the full
    /// final state (every register incl. EMAC/ctrl, plus every mapped
    /// page's bytes, in address order). Must be byte-identical to the
    /// pre-change baseline.
    state_hash: u64,
}

fn state_hash(cpu: &Cpu, pages: &[Option<Box<[u8; PAGE_SIZE]>>]) -> u64 {
    let mut h = DefaultHasher::new();
    cpu.d.hash(&mut h);
    cpu.a.hash(&mut h);
    cpu.other_a7.hash(&mut h);
    cpu.pc.hash(&mut h);
    cpu.sr.hash(&mut h);
    cpu.icount.hash(&mut h);
    cpu.ctrl.vbr.hash(&mut h);
    cpu.ctrl.cacr.hash(&mut h);
    cpu.ctrl.asid.hash(&mut h);
    cpu.ctrl.acr.hash(&mut h);
    cpu.ctrl.mmubar.hash(&mut h);
    cpu.ctrl.rgpiobar.hash(&mut h);
    cpu.ctrl.rambar.hash(&mut h);
    cpu.emac.macsr.hash(&mut h);
    cpu.emac.acc.hash(&mut h);
    cpu.emac.accext01.hash(&mut h);
    cpu.emac.accext23.hash(&mut h);
    cpu.emac.mask.hash(&mut h);
    // Index order is address order (unlike the old HashMap's), so this is
    // already deterministic without a separate sort. Hash the recovered
    // base address, not the bare index, so this matches the pre-step-2
    // hash bit for bit (the actual gate: the state is unchanged, not just
    // "a" hash of it).
    for (idx, page) in pages.iter().enumerate() {
        if let Some(p) = page {
            let base = (idx as u32) << PAGE_SHIFT;
            base.hash(&mut h);
            p.hash(&mut h);
        }
    }
    h.finish()
}

fn run(mut cpu: Cpu, mut bus: SparseBus, limit: u64, profiling: bool) -> RunResult {
    let icount_start = cpu.icount;
    let mut idle = 0u64;
    let stop_reason;
    let start = Instant::now();
    loop {
        if !bus.page_present(cpu.pc) {
            stop_reason = "fetch outside known memory (stub cannot answer)";
            break;
        }
        if profiling && bus.sample.len() < bus.sample_cap {
            // Redundant fetch, profile run only: same 1-3 words cpu.step()
            // is about to fetch itself, kept for the decode microbench.
            let w0 = bus.read16(cpu.pc).unwrap_or(0);
            let w1 = bus.read16(cpu.pc.wrapping_add(2)).unwrap_or(0);
            let w2 = bus.read16(cpu.pc.wrapping_add(4)).unwrap_or(0);
            bus.sample.push((cpu.pc, [w0, w1, w2]));
        }
        let pc_before = cpu.pc;
        match cpu.step(&mut bus) {
            Ok(()) => {
                if cpu.pc == pc_before {
                    idle += 1;
                }
            }
            Err(Stop::Unimplemented(f)) => {
                stop_reason = "unimplemented form (leaked outside P3's used-form census)";
                eprintln!("stopped on unimplemented form {f:?} at pc=0x{pc_before:08x}");
                break;
            }
            Err(Stop::Halted) => {
                stop_reason = "halted (HALT or fault-on-fault)";
                break;
            }
        }
        if cpu.icount - icount_start >= limit {
            stop_reason = "instruction limit";
            break;
        }
    }
    let elapsed = start.elapsed();
    // coldfire's perf step 3 defers N/Z/V (cpu.pending_nzv); resolve it
    // before hashing so the gate sees the true final architectural state,
    // not whatever's left in `sr` from before the last flags-setting
    // instruction. After `elapsed` is captured, so it isn't timed.
    cpu.resolve_nzv();
    let state_hash = state_hash(&cpu, &bus.pages);
    RunResult {
        icount_start,
        icount_end: cpu.icount,
        idle,
        elapsed,
        stop_reason,
        mmio_reads: bus.mmio_reads,
        mmio_writes: bus.mmio_writes,
        bus_ns: bus.bus_ns,
        sample: bus.sample,
        state_hash,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dump = args.get(1).cloned().unwrap_or_else(|| {
        eprintln!("usage: cfrealmix DUMP [LIMIT] [--profile]");
        std::process::exit(2);
    });
    let profile = args.iter().any(|a| a == "--profile");
    let limit: u64 = args
        .iter()
        .skip(2)
        .find(|a| !a.starts_with("--"))
        .and_then(|s| s.parse().ok())
        .unwrap_or(20_000_000);

    let (cpu, pages) = load_dump(&dump);
    let mapped = pages.iter().filter(|p| p.is_some()).count();
    println!(
        "loaded {mapped} pages, pc=0x{:08x}, sr=0x{:04x}, limit={}",
        cpu.pc, cpu.sr, limit
    );

    // Headline pass: no instrumentation.
    let bus = SparseBus::new(pages.clone(), false);
    let r = run(cpu.clone(), bus, limit, false);
    let n = r.icount_end - r.icount_start;
    let secs = r.elapsed.as_secs_f64();
    let ips = n as f64 / secs;
    let idle_pct = 100.0 * r.idle as f64 / n as f64;
    let useful_ips = ips * (1.0 - r.idle as f64 / n as f64);
    println!("\n=== headline (no instrumentation) ===");
    println!(
        "{n} instructions in {secs:.3}s: {:.1}M instr/s ({:.1}M useful/s excl. idle spin)",
        ips / 1e6,
        useful_ips / 1e6
    );
    println!("idle spin: {} / {n} instructions ({idle_pct:.1}%)", r.idle);
    println!("stop reason: {}", r.stop_reason);
    println!(
        "mmio: {} reads answered by constant, {} writes accepted silently",
        r.mmio_reads, r.mmio_writes
    );
    println!(
        "verdict vs 62M useful instr/s: {}",
        if useful_ips >= 62e6 {
            "MEETS the real-time floor"
        } else {
            "BELOW the real-time floor"
        }
    );
    println!("final state hash: {:#018x}", r.state_hash);

    if !profile {
        return;
    }

    // Profiling pass: same dump, same limit, instrumented bus + opcode
    // sampling for the decode microbench. A separate run, not reused for
    // the headline number above (Instant::now() around every bus call
    // measurably slows the loop).
    println!("\n=== profile pass (separate, instrumented run) ===");
    let bus = SparseBus::new(pages, true);
    let pr = run(cpu, bus, limit, true);
    let pn = pr.icount_end - pr.icount_start;
    let p_secs = pr.elapsed.as_secs_f64();
    println!(
        "{pn} instructions in {p_secs:.3}s: {:.1}M instr/s (instrumented, slower than headline by design)",
        pn as f64 / p_secs / 1e6
    );

    // Decode-only microbench over the sampled (pc, words), batch-timed so
    // Instant::now() itself isn't part of the measured cost.
    let sample = pr.sample;
    let t0 = Instant::now();
    let mut sink: u64 = 0; // keeps decode() from being optimized away
    for &(pc, words) in &sample {
        if let Some(insn) = decode(pc, words) {
            sink = sink.wrapping_add(insn.len as u64);
        }
    }
    let decode_elapsed = t0.elapsed();
    std::hint::black_box(sink);
    let decode_ns_per_instr = decode_elapsed.as_nanos() as f64 / sample.len().max(1) as f64;

    let total_ns = pr.elapsed.as_nanos() as f64;
    let bus_share = pr.bus_ns as f64 / total_ns;
    let decode_ns_total = decode_ns_per_instr * pn as f64;
    let decode_share = decode_ns_total / total_ns;
    let execute_share = (1.0 - bus_share - decode_share).max(0.0);

    println!(
        "decode-only microbench: {} sampled opcodes, {:.2} ns/decode",
        sample.len(),
        decode_ns_per_instr
    );
    println!(
        "time split of the instrumented run: bus/memory access {:.1}%, decode (est., isolated microbench) {:.1}%, execute + dispatch remainder {:.1}%",
        bus_share * 100.0,
        decode_share * 100.0,
        execute_share * 100.0
    );
    println!(
        "caveat: bus/memory access is one bucket (dispatch and raw memory access happen in the \
         same SparseBus methods; cpu.rs is out of this bench's ownership so they can't be split \
         further); decode share is extrapolated from an isolated microbench over sampled opcodes, \
         not measured in-line; execute is the remainder, so it also carries any instrumentation \
         overhead not attributed to the other two buckets."
    );

    // Idle-spin opcode confirmation: among consecutive sampled fetches at
    // the same PC (the pc==pc_before signature `run()` counted as idle),
    // how many are really the 0x60FE bra.b-self encoding, from the words
    // already fetched into `sample` (no extra bus access needed).
    let (bra_self, same_pc) = sample
        .windows(2)
        .filter(|w| w[0].0 == w[1].0)
        .fold((0u32, 0u32), |(bs, n), w| {
            (bs + (w[0].1[0] == BRA_SELF) as u32, n + 1)
        });
    if same_pc > 0 {
        println!(
            "idle-spin opcode check: {bra_self}/{same_pc} sampled self-branches are exactly \
             0x60FE (bra.b self)"
        );
    }
}
