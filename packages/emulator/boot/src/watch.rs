//! Two ColdFire instruments, off by default (docs/INSTRUMENTATION.md, "Write-watch and flush check"):
//!
//! * write-watch: every write into configured guest ranges, from CPU stores and from the DMA
//!   engines (eSDHC, DSPI), with the guest instruction count, the PC (or DMA source), address,
//!   size and value; PC ranges may be allowed (the runtime's own code);
//! * flush check: an instruction fetch from a 16-byte line that was written after the caches
//!   were enabled and has not been pushed or invalidated since. The ColdFire V4 caches are not
//!   coherent on hardware; the emulator is, so a missing flush is invisible there.
//!
//! The state lives in `WriteGuards` (guard.rs), which the bus already tests with one flag per
//! store, so both instruments add no branch to the hot store path.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;

/// DDR as the CPU sees it with the caches on (ACR0 in the stock firmware).
const DDR: std::ops::Range<u32> = 0x4000_0000..0x4800_0000;
/// The uncached alias of DDR: `DDR + 0x0800_0000`.
const ALIAS: std::ops::Range<u32> = 0x4800_0000..0x5000_0000;
const ALIAS_OFFSET: u32 = 0x0800_0000;

const CACR_DEC: u32 = 1 << 31;
const CACR_DCINVA: u32 = 1 << 24;
const CACR_IEC: u32 = 1 << 15;
const CACR_SPA: u32 = 1 << 14;
const CACR_IDPI: u32 = 1 << 12;
const CACR_ICINVA: u32 = 1 << 8;
const CACR_IVO: u32 = 1 << 20;
/// 8 KB, 4-way, 16-byte lines: 128 sets.
const SETS: usize = 128;

/// The uncached alias counts as the same RAM.
pub(crate) fn normalise(addr: u32) -> u32 {
    if ALIAS.contains(&addr) {
        addr - ALIAS_OFFSET
    } else {
        addr
    }
}

/// Who wrote: a CPU store (the PC) or a DMA engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Writer {
    Pc(u32),
    Dma(&'static str),
}

impl Writer {
    fn json(self) -> String {
        match self {
            Writer::Pc(pc) => format!("\"pc\":\"{pc:#010x}\""),
            Writer::Dma(src) => format!("\"dma\":\"{src}\""),
        }
    }
}

/// When the write-watch starts reporting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum After {
    Start,
    /// The first CACR write that enables a cache.
    Cacr,
    Icount(u64),
}

#[derive(Clone, Debug)]
pub struct WatchConfig {
    /// `[lo, hi)` in the cached view (an alias address is folded onto DDR).
    pub ranges: Vec<(u32, u32)>,
    /// Stores from these PCs are not reported by the write-watch.
    pub allow_pc: Vec<(u32, u32)>,
    pub after: After,
    pub flush_check: bool,
    /// JSONL events; the summary goes next to it (`.summary.json`).
    pub out: Option<PathBuf>,
    /// Events written to the log; counting continues past it.
    pub limit: u64,
    /// Task stacks `[lo, hi)` (high-water mark instrument, `DIGI_STACK_WATCH`): the lowest address a store reached.
    pub stacks: Vec<(u32, u32)>,
    /// A store into one of these ranges ends a stack epoch (`DIGI_STACK_WATCH_EPOCH`): the low-water mark is kept per epoch.
    pub epochs: Vec<(u32, u32)>,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            ranges: Vec::new(),
            allow_pc: Vec::new(),
            after: After::Start,
            flush_check: false,
            out: None,
            limit: 100_000,
            stacks: Vec::new(),
            epochs: Vec::new(),
        }
    }
}

fn parse_num(text: &str) -> Result<u32, String> {
    let t = text.trim().replace('_', "");
    let t = t
        .strip_prefix("0x")
        .or_else(|| t.strip_prefix("0X"))
        .unwrap_or(&t);
    u32::from_str_radix(t, 16).map_err(|_| format!("bad hex number {text:?}"))
}

/// `LO-HI` (HI exclusive) or `LO+LEN`, hex, comma separated.
pub fn parse_ranges(text: &str) -> Result<Vec<(u32, u32)>, String> {
    let mut out = Vec::new();
    for item in text.split(',').map(str::trim).filter(|i| !i.is_empty()) {
        let (lo, hi) = if let Some((lo, len)) = item.split_once('+') {
            let lo = parse_num(lo)?;
            (
                lo,
                lo.checked_add(parse_num(len)?).ok_or("range overflows")?,
            )
        } else if let Some((lo, hi)) = item.split_once('-') {
            (parse_num(lo)?, parse_num(hi)?)
        } else {
            return Err(format!("range {item:?} is not LO-HI or LO+LEN"));
        };
        if lo >= hi {
            return Err(format!("range {item:?} is empty"));
        }
        out.push((lo, hi));
    }
    Ok(out)
}

impl WatchConfig {
    /// `DIGI_WRITE_WATCH=LO-HI[,..]` (or `LO+LEN`), `DIGI_WRITE_WATCH_ALLOW_PC=LO-HI[,..]`,
    /// `DIGI_WRITE_WATCH_AFTER=cacr|ICOUNT`, `DIGI_FLUSH_CHECK=1`,
    /// `DIGI_WRITE_WATCH_OUT=PATH` (default `$DIGI_DIAG_DIR/writewatch.jsonl`, else stderr),
    /// `DIGI_WRITE_WATCH_LIMIT=N`, `DIGI_STACK_WATCH=LO-HI[,..]` (task stack ranges: lowest store, new lows and per-epoch
    /// lows in the log and summary), `DIGI_STACK_WATCH_EPOCH=LO-HI[,..]` (a store there ends an epoch).
    /// `Ok(None)` when no instrument is switched on.
    pub fn from_env() -> Result<Option<Self>, String> {
        fn non_empty(v: Result<String, std::env::VarError>) -> Option<String> {
            v.ok().filter(|v| !v.is_empty())
        }
        let mut cfg = WatchConfig::default();
        if let Some(v) = non_empty(std::env::var("DIGI_WRITE_WATCH")) {
            cfg.ranges = parse_ranges(&v).map_err(|e| format!("DIGI_WRITE_WATCH: {e}"))?;
        }
        cfg.flush_check = non_empty(std::env::var("DIGI_FLUSH_CHECK")).is_some_and(|v| v != "0");
        if let Some(v) = non_empty(std::env::var("DIGI_STACK_WATCH")) {
            cfg.stacks = parse_ranges(&v).map_err(|e| format!("DIGI_STACK_WATCH: {e}"))?;
        }
        if let Some(v) = non_empty(std::env::var("DIGI_STACK_WATCH_EPOCH")) {
            cfg.epochs = parse_ranges(&v).map_err(|e| format!("DIGI_STACK_WATCH_EPOCH: {e}"))?;
        }
        if cfg.ranges.is_empty() && !cfg.flush_check && cfg.stacks.is_empty() {
            return Ok(None);
        }
        if let Some(v) = non_empty(std::env::var("DIGI_WRITE_WATCH_ALLOW_PC")) {
            cfg.allow_pc =
                parse_ranges(&v).map_err(|e| format!("DIGI_WRITE_WATCH_ALLOW_PC: {e}"))?;
        }
        if let Some(v) = non_empty(std::env::var("DIGI_WRITE_WATCH_AFTER")) {
            cfg.after = if v == "cacr" {
                After::Cacr
            } else {
                After::Icount(v.parse().map_err(|_| {
                    format!("DIGI_WRITE_WATCH_AFTER: {v:?} is not `cacr` or an instruction count")
                })?)
            };
        }
        if let Some(v) = non_empty(std::env::var("DIGI_WRITE_WATCH_LIMIT")) {
            cfg.limit = v
                .parse()
                .map_err(|_| format!("DIGI_WRITE_WATCH_LIMIT: {v:?} is not a number"))?;
        }
        cfg.out = non_empty(std::env::var("DIGI_WRITE_WATCH_OUT"))
            .map(PathBuf::from)
            .or_else(|| {
                let dir = std::env::var_os("DIGI_DIAG_DIR").filter(|d| !d.is_empty())?;
                std::fs::create_dir_all(&dir).ok()?;
                Some(PathBuf::from(dir).join("writewatch.jsonl"))
            });
        // DIGI_WRITE_WATCH_SEQ=1: one log and summary per emulator of the process (`NAME-0.jsonl`, `NAME-1.jsonl`, ...): a cold boot, a
        // soft-reset boot and the installed runtime would otherwise overwrite each other's summary.
        if non_empty(std::env::var("DIGI_WRITE_WATCH_SEQ")).is_some_and(|v| v != "0")
            && let Some(out) = cfg.out.take()
        {
            static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let stem = out
                .file_stem()
                .map_or(String::new(), |s| s.to_string_lossy().into_owned());
            let ext = out
                .extension()
                .map_or(String::new(), |s| format!(".{}", s.to_string_lossy()));
            cfg.out = Some(out.with_file_name(format!("{stem}-{n}{ext}")));
        }
        Ok(Some(cfg))
    }
}

/// One line that was fetched while its last write was still unflushed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlushHit {
    pub line: u32,
    pub fetch_pc: u32,
    pub fetch_icount: u64,
    pub writer: Writer,
    pub write_icount: u64,
    /// Bytes of the line stored since it was last cleaned.
    pub mask: u16,
}

/// The bits of bytes [lo, hi) of a line; 0 <= lo <= hi <= 16.
fn byte_mask(lo: u32, hi: u32) -> u16 {
    (((1u32 << (hi - lo)) - 1) << lo) as u16
}

#[derive(Clone, Copy)]
struct Line {
    writer: Writer,
    icount: u64,
    /// Bytes stored since the line was last cleaned.
    mask: u16,
    /// A cached (copyback) store: the data cache must push it.
    need_d: bool,
    d_done: bool,
    i_done: bool,
}

impl Line {
    fn clean(&self) -> bool {
        (!self.need_d || self.d_done) && self.i_done
    }
}

struct Flush {
    sets: Vec<HashMap<u32, Line>>,
    /// Ways of each set pushed (data) or invalidated (instruction) since the last store to it.
    dmask: [u8; SETS],
    imask: [u8; SETS],
    reported: HashSet<u32>,
    hits: Vec<FlushHit>,
    /// Fetches on a dirty line that did not overlap the stored bytes.
    same_line_clean: u64,
}

fn set_of(line: u32) -> usize {
    ((line >> 4) as usize) & (SETS - 1)
}

impl Flush {
    fn new() -> Self {
        Self {
            sets: (0..SETS).map(|_| HashMap::new()).collect(),
            dmask: [0; SETS],
            imask: [0; SETS],
            reported: HashSet::new(),
            hits: Vec::new(),
            same_line_clean: 0,
        }
    }

    fn store(&mut self, line: u32, mask: u16, writer: Writer, icount: u64, cached: bool) {
        let s = set_of(line);
        let old_need = self.sets[s]
            .get(&line)
            .is_some_and(|l| l.need_d && !l.d_done);
        let old_mask = self.sets[s].get(&line).map_or(0, |l| l.mask);
        self.sets[s].insert(
            line,
            Line {
                writer,
                icount,
                mask: mask | old_mask,
                need_d: cached || old_need,
                d_done: false,
                i_done: false,
            },
        );
        self.dmask[s] = 0;
        self.imask[s] = 0;
    }

    fn sweep(&mut self) {
        for set in &mut self.sets {
            set.retain(|_, l| !l.clean());
        }
    }

    fn cacr(&mut self, v: u32) {
        if v & CACR_ICINVA != 0 {
            self.sets
                .iter_mut()
                .flat_map(HashMap::values_mut)
                .for_each(|l| l.i_done = true);
            self.imask = [0; SETS];
        }
        if v & CACR_DCINVA != 0 {
            self.sets
                .iter_mut()
                .flat_map(HashMap::values_mut)
                .for_each(|l| l.d_done = true);
            self.dmask = [0; SETS];
        }
        self.sweep();
    }

    fn cpushl(&mut self, cache: u8, ax: u32, cacr: u32) {
        let ivo = cacr & CACR_IVO != 0;
        let d_push = cache & 1 != 0 && !ivo;
        let i_clear = cache & 2 != 0 && (ivo || cacr & CACR_IDPI == 0);
        if cacr & CACR_SPA != 0 {
            let line = normalise(ax) & !15;
            let s = set_of(line);
            if let Some(l) = self.sets[s].get_mut(&line) {
                l.d_done |= d_push;
                l.i_done |= i_clear;
                if l.clean() {
                    self.sets[s].remove(&line);
                }
            }
            return;
        }
        // Set and way: the line is cleaned when all four ways of its set were pushed.
        let s = ((ax >> 4) as usize) & (SETS - 1);
        let way = 1u8 << (ax & 3);
        if d_push {
            self.dmask[s] |= way;
        }
        if i_clear {
            self.imask[s] |= way;
        }
        let (dall, iall) = (self.dmask[s] == 0xf, self.imask[s] == 0xf);
        if dall || iall {
            for l in self.sets[s].values_mut() {
                l.d_done |= dall;
                l.i_done |= iall;
            }
            if dall {
                self.dmask[s] = 0;
            }
            if iall {
                self.imask[s] = 0;
            }
            self.sets[s].retain(|_, l| !l.clean());
        }
    }
}

impl std::fmt::Debug for Watch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watch")
            .field("events", &self.events)
            .finish_non_exhaustive()
    }
}

/// Low-water mark of one task stack. A stack grows down, so the lowest address any store reached is its high-water use
/// (a frame reserved but never written is not seen; the reserve is small next to the stack).
#[derive(Clone, Copy, Debug)]
struct StackMark {
    lo: u32,
    hi: u32,
    /// Lowest store address, the PC that made it and its instruction count.
    low: u32,
    pc: u32,
    ic: u64,
    /// Last low written to the log (a new line is written when the low is 64 bytes deeper).
    logged: u32,
    /// The same for the current epoch.
    e_low: u32,
    e_pc: u32,
    e_ic: u64,
    e_start: u64,
    e_writes: u64,
}

impl StackMark {
    fn new(lo: u32, hi: u32) -> Self {
        Self {
            lo,
            hi,
            low: hi,
            pc: 0,
            ic: 0,
            logged: hi,
            e_low: hi,
            e_pc: 0,
            e_ic: 0,
            e_start: 0,
            e_writes: 0,
        }
    }
}

pub(crate) struct Watch {
    stacks: Vec<StackMark>,
    /// Closed epochs, as JSON objects, for the summary (at most 512).
    epoch_rows: Vec<String>,
    cfg: WatchConfig,
    started: bool,
    cacr: u32,
    sink: Option<BufWriter<File>>,
    events: u64,
    logged: u64,
    per_pc: BTreeMap<u32, u64>,
    per_dma: BTreeMap<&'static str, u64>,
    flush: Option<Flush>,
    summary_done: bool,
    /// Work counters for the summary: CACR writes, flush-check stores and DMA ranges tracked,
    /// instruction fetches checked, CPUSHLs seen.
    seen: [u64; 5],
}

impl Watch {
    pub(crate) fn new(cfg: WatchConfig) -> Self {
        let sink = cfg.out.as_ref().and_then(|p| match File::create(p) {
            Ok(f) => Some(BufWriter::new(f)),
            Err(e) => {
                eprintln!("write-watch: {}: {e}", p.display());
                None
            }
        });
        let cfg_stacks = cfg
            .stacks
            .iter()
            .map(|&(lo, hi)| StackMark::new(lo, hi))
            .collect();
        Self {
            started: cfg.after == After::Start,
            flush: cfg.flush_check.then(Flush::new),
            cfg,
            cacr: 0,
            sink,
            events: 0,
            logged: 0,
            per_pc: BTreeMap::new(),
            per_dma: BTreeMap::new(),
            stacks: cfg_stacks,
            epoch_rows: Vec::new(),
            summary_done: false,
            seen: [0; 5],
        }
    }

    fn line_out(&mut self, text: &str) {
        match self.sink.as_mut() {
            Some(w) => {
                let _ = writeln!(w, "{text}");
            }
            None => eprintln!("{text}"),
        }
    }

    fn overlaps(&self, addr: u32, size: u32) -> bool {
        let lo = u64::from(normalise(addr));
        let hi = lo + u64::from(size);
        self.cfg
            .ranges
            .iter()
            .any(|&(rlo, rhi)| lo < u64::from(rhi) && hi > u64::from(rlo))
    }

    fn report(&mut self, text: String) {
        self.events += 1;
        if self.logged < self.cfg.limit {
            self.logged += 1;
            self.line_out(&text);
        }
    }

    /// End the stack epoch: write one row per stack that saw a store in it, and start the next.
    fn close_epoch(&mut self, trigger: u32, icount: u64) {
        for i in 0..self.stacks.len() {
            let m = self.stacks[i];
            if m.e_writes > 0 {
                let row = format!(
                    "{{\"stack\":\"{:#010x}\",\"ic0\":{},\"ic1\":{icount},\"trigger\":\"{trigger:#010x}\",\"low\":\"{:#010x}\",\"depth\":{},\"pc\":\"{:#010x}\",\"low_ic\":{},\"writes\":{}}}",
                    m.lo,
                    m.e_start,
                    m.e_low,
                    m.hi - m.e_low,
                    m.e_pc,
                    m.e_ic,
                    m.e_writes
                );
                self.line_out(&format!("{{\"t\":\"stack_epoch\",\"row\":{row}}}"));
                if self.epoch_rows.len() < 512 {
                    self.epoch_rows.push(row);
                }
            }
            let m = &mut self.stacks[i];
            m.e_low = m.hi;
            m.e_writes = 0;
            m.e_start = icount;
        }
    }

    fn stack_store(&mut self, pc: u32, addr: u32, size: u8, icount: u64) {
        if self
            .cfg
            .epochs
            .iter()
            .any(|&(lo, hi)| addr < hi && addr.saturating_add(u32::from(size)) > lo)
        {
            self.close_epoch(addr, icount);
        }
        let n = normalise(addr);
        for i in 0..self.stacks.len() {
            let m = &mut self.stacks[i];
            if n < m.lo || n >= m.hi {
                continue;
            }
            m.e_writes += 1;
            if n < m.e_low {
                (m.e_low, m.e_pc, m.e_ic) = (n, pc, icount);
            }
            if n < m.low {
                (m.low, m.pc, m.ic) = (n, pc, icount);
                if n + 64 <= m.logged {
                    m.logged = n;
                    let line = format!(
                        "{{\"t\":\"stack\",\"ic\":{icount},\"pc\":\"{pc:#010x}\",\"addr\":\"{n:#010x}\",\"depth\":{}}}",
                        m.hi - n
                    );
                    self.line_out(&line);
                }
            }
        }
    }

    pub(crate) fn store(&mut self, pc: u32, addr: u32, size: u8, value: u32, icount: u64) {
        self.tick(icount);
        if self.started && !self.stacks.is_empty() {
            self.stack_store(pc, addr, size, icount);
        }
        if self.started
            && !self.cfg.ranges.is_empty()
            && self.overlaps(addr, u32::from(size))
            && !self
                .cfg
                .allow_pc
                .iter()
                .any(|&(lo, hi)| (lo..hi).contains(&pc))
        {
            *self.per_pc.entry(pc).or_default() += 1;
            self.report(format!(
                "{{\"t\":\"w\",\"ic\":{icount},\"pc\":\"{pc:#010x}\",\"addr\":\"{addr:#010x}\",\"size\":{size},\"value\":\"{value:#x}\"}}"
            ));
        }
        if self.cacr & CACR_IEC != 0 {
            let n = normalise(addr);
            if let Some(flush) = self.flush.as_mut()
                && DDR.contains(&n)
            {
                let cached = DDR.contains(&addr) && self.cacr & CACR_DEC != 0;
                let end = n.saturating_add(u32::from(size));
                let mut line = n & !15;
                self.seen[1] += 1;
                while line < end {
                    let mask = byte_mask(n.max(line) - line, end.min(line + 16) - line);
                    flush.store(line, mask, Writer::Pc(pc), icount, cached);
                    line += 16;
                }
            }
        }
    }

    pub(crate) fn dma(&mut self, src: &'static str, addr: u32, len: usize, icount: u64) {
        if len == 0 {
            return;
        }
        self.tick(icount);
        let len32 = u32::try_from(len).unwrap_or(u32::MAX);
        if self.started && !self.cfg.ranges.is_empty() && self.overlaps(addr, len32) {
            *self.per_dma.entry(src).or_default() += 1;
            self.report(format!(
                "{{\"t\":\"dma\",\"ic\":{icount},\"src\":\"{src}\",\"addr\":\"{addr:#010x}\",\"size\":{len}}}"
            ));
        }
        if self.cacr & CACR_IEC != 0
            && let Some(flush) = self.flush.as_mut()
        {
            let lo = normalise(addr);
            let hi = lo.saturating_add(len32);
            self.seen[1] += 1;
            let mut line = lo.max(DDR.start) & !15;
            while line < hi.min(DDR.end) {
                let mask = byte_mask(lo.max(line) - line, hi.min(line + 16) - line);
                flush.store(line, mask, Writer::Dma(src), icount, false);
                line += 16;
            }
        }
    }

    /// An instruction of `len` bytes is fetched at `pc`.
    pub(crate) fn fetch(&mut self, pc: u32, len: u32, icount: u64) {
        if self.cacr & CACR_IEC == 0 {
            return;
        }
        let Some(flush) = self.flush.as_mut() else {
            return;
        };
        self.seen[2] += 1;
        let lo = normalise(pc);
        let end = lo.saturating_add(len.max(2));
        let mut line = lo & !15;
        let mut dirty = false;
        let mut found = None;
        while line < end {
            if let Some(l) = flush.sets[set_of(line)].get(&line).copied() {
                dirty = true;
                if l.mask & byte_mask(lo.max(line) - line, end.min(line + 16) - line) != 0 {
                    found = Some((line, l));
                    break;
                }
            }
            line += 16;
        }
        let Some((line, l)) = found else {
            if dirty {
                flush.same_line_clean += 1;
            }
            return;
        };
        if flush.reported.insert(line) {
            let hit = FlushHit {
                line,
                fetch_pc: pc,
                fetch_icount: icount,
                writer: l.writer,
                write_icount: l.icount,
                mask: l.mask,
            };
            flush.hits.push(hit.clone());
            // Not a write-watch event: logged, counted in `flush_hits`.
            self.line_out(&format!(
                "{{\"t\":\"flush\",\"ic\":{icount},\"fetch_pc\":\"{pc:#010x}\",\"line\":\"{line:#010x}\",\"writer\":{{{}}},\"write_ic\":{},\"dcache_pending\":{},\"mask\":\"0x{:04x}\"}}",
                hit.writer.json(),
                hit.write_icount,
                l.need_d && !l.d_done,
                l.mask
            ));
        }
    }

    pub(crate) fn cacr_written(&mut self, v: u32, icount: u64) {
        self.cacr = v;
        self.seen[0] += 1;
        if !self.started
            && match self.cfg.after {
                After::Cacr => v & (CACR_IEC | CACR_DEC) != 0,
                After::Icount(n) => icount >= n,
                After::Start => true,
            }
        {
            self.started = true;
        }
        if let Some(flush) = self.flush.as_mut() {
            flush.cacr(v);
        }
    }

    pub(crate) fn cpushl(&mut self, cache: u8, ax: u32) {
        let cacr = self.cacr;
        self.seen[3] += 1;
        if let Some(flush) = self.flush.as_mut() {
            flush.cpushl(cache, ax, cacr);
        }
    }

    /// Set the CACR the instruments assume (a state restored from a snapshot).
    pub(crate) fn set_cacr(&mut self, v: u32) {
        self.cacr = v & !(CACR_ICINVA | CACR_DCINVA);
    }

    /// A snapshot was restored past the trigger: with `After::Cacr` and the caches already on, the watch runs from here.
    pub(crate) fn resume(&mut self) {
        if self.cfg.after == After::Cacr && self.cacr & (CACR_IEC | CACR_DEC) != 0 {
            self.started = true;
        }
    }

    /// The icount trigger of `After::Icount`.
    fn tick(&mut self, icount: u64) {
        if !self.started && matches!(self.cfg.after, After::Icount(n) if icount >= n) {
            self.started = true;
        }
    }

    pub(crate) fn totals(&self) -> (u64, usize) {
        (self.events, self.flush.as_ref().map_or(0, |f| f.hits.len()))
    }

    pub(crate) fn flush_hits(&self) -> Vec<FlushHit> {
        self.flush
            .as_ref()
            .map_or_else(Vec::new, |f| f.hits.clone())
    }

    /// Per-PC counts of reported stores, most frequent first.
    pub(crate) fn per_pc(&self) -> Vec<(u32, u64)> {
        let mut v: Vec<_> = self.per_pc.iter().map(|(&p, &c)| (p, c)).collect();
        v.sort_by_key(|&(p, c)| (std::cmp::Reverse(c), p));
        v
    }

    /// Flush the log and write `<out>.summary.json` (once).
    pub(crate) fn finish(&mut self) {
        if self.summary_done {
            return;
        }
        self.summary_done = true;
        if let Some(w) = self.sink.as_mut() {
            let _ = w.flush();
        }
        let per_pc: Vec<String> = self
            .per_pc()
            .iter()
            .map(|(p, c)| format!("{{\"pc\":\"{p:#010x}\",\"count\":{c}}}"))
            .collect();
        let per_dma: Vec<String> = self
            .per_dma
            .iter()
            .map(|(s, c)| format!("{{\"src\":\"{s}\",\"count\":{c}}}"))
            .collect();
        let flush: Vec<String> = self
            .flush_hits()
            .iter()
            .map(|h| {
                format!(
                    "{{\"line\":\"{:#010x}\",\"fetch_pc\":\"{:#010x}\",\"writer\":{{{}}},\"mask\":\"0x{:04x}\"}}",
                    h.line,
                    h.fetch_pc,
                    h.writer.json(),
                    h.mask
                )
            })
            .collect();
        let stacks: Vec<String> = self
            .stacks
            .iter()
            .map(|m| {
                format!(
                    "{{\"lo\":\"{:#010x}\",\"hi\":\"{:#010x}\",\"size\":{},\"low\":\"{:#010x}\",\"depth\":{},\"pc\":\"{:#010x}\",\"ic\":{}}}",
                    m.lo,
                    m.hi,
                    m.hi - m.lo,
                    m.low,
                    m.hi - m.low,
                    m.pc,
                    m.ic
                )
            })
            .collect();
        let text = format!(
            "{{\"stacks\":[{}],\"stack_epochs\":[{}],\"events\":{},\"logged\":{},\"cacr_writes\":{},\"flush_stores_tracked\":{},\"flush_fetches_checked\":{},\"flush_same_line_clean\":{},\"cpushl\":{},\"per_pc\":[{}],\"per_dma\":[{}],\"flush_hits\":[{}]}}",
            stacks.join(","),
            self.epoch_rows.join(","),
            self.events,
            self.logged,
            self.seen[0],
            self.seen[1],
            self.seen[2],
            self.flush.as_ref().map_or(0, |f| f.same_line_clean),
            self.seen[3],
            per_pc.join(","),
            per_dma.join(","),
            flush.join(",")
        );
        if let Some(out) = &self.cfg.out {
            let path = out.with_extension("summary.json");
            if let Err(e) = std::fs::write(&path, &text) {
                eprintln!("write-watch: {}: {e}", path.display());
            }
        }
        eprintln!(
            "write-watch: {} events, {} flush hits{}",
            self.events,
            self.flush.as_ref().map_or(0, |f| f.hits.len()),
            self.cfg
                .out
                .as_ref()
                .map_or(String::new(), |p| format!(" ({})", p.display()))
        );
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stock firmware's CACR: both caches on, invalidated, SPA (search by physical address).
    const IEC_DEC_INV: u32 = 0xa50c_e100;
    /// The same without SPA: CPUSHL names a set and a way.
    const SET_WAY: u32 = 0xa50c_a100;

    fn watch(ranges: &[(u32, u32)], flush: bool) -> Watch {
        Watch::new(WatchConfig {
            ranges: ranges.to_vec(),
            flush_check: flush,
            ..WatchConfig::default()
        })
    }

    #[test]
    fn stack_low_water_is_tracked_per_epoch() {
        let mut w = Watch::new(WatchConfig {
            stacks: vec![(0x4096_0000, 0x4098_0000)],
            epochs: vec![(0x4700_0000, 0x4700_0004)],
            ..WatchConfig::default()
        });
        w.store(0x4000_1000, 0x4097_fff0, 4, 0, 1);
        w.store(0x4000_1004, 0x4097_ff00, 4, 0, 2);
        w.store(0x4000_1008, 0x4700_0000, 4, 0, 3); // epoch ends: low 0x100 deep
        w.store(0x4000_100c, 0x4097_ffc0, 4, 0, 4);
        w.store(0x4000_1010, 0x4000_0000, 4, 0, 5); // outside the stack
        assert_eq!(
            (w.stacks[0].low, w.stacks[0].pc),
            (0x4097_ff00, 0x4000_1004)
        );
        assert_eq!(w.stacks[0].e_low, 0x4097_ffc0);
        assert_eq!(w.epoch_rows.len(), 1);
        assert!(
            w.epoch_rows[0].contains("\"depth\":256"),
            "{}",
            w.epoch_rows[0]
        );
    }

    #[test]
    fn ranges_parse() {
        assert_eq!(
            parse_ranges("0x47e8b000-0x47fe0000, 44e4a5b8+400").unwrap(),
            vec![(0x47e8_b000, 0x47fe_0000), (0x44e4_a5b8, 0x44e4_a9b8)]
        );
        assert!(parse_ranges("10-10").is_err());
        assert!(parse_ranges("zz").is_err());
    }

    #[test]
    fn store_into_range_is_counted_per_pc_and_alias_folds() {
        let mut w = watch(&[(0x4700_0000, 0x4700_0100)], false);
        w.store(0x4000_1000, 0x4700_00fe, 4, 1, 1); // straddles the end
        w.store(0x4000_1000, 0x4700_0100, 4, 2, 2); // just outside
        w.store(0x4000_2000, 0x4F00_0010, 1, 3, 3); // alias of 0x47000010
        assert_eq!(w.totals().0, 2);
        assert_eq!(w.per_pc(), vec![(0x4000_1000, 1), (0x4000_2000, 1)]);
    }

    #[test]
    fn allowed_pc_is_not_reported() {
        let mut w = Watch::new(WatchConfig {
            ranges: vec![(0x4700_0000, 0x4700_0100)],
            allow_pc: vec![(0x4700_0000, 0x4700_0100)],
            ..WatchConfig::default()
        });
        w.store(0x4700_0040, 0x4700_0000, 4, 1, 1);
        assert_eq!(w.totals().0, 0);
        w.store(0x4000_0040, 0x4700_0000, 4, 1, 2);
        assert_eq!(w.totals().0, 1);
    }

    #[test]
    fn dma_is_reported_and_after_cacr_waits() {
        let mut w = Watch::new(WatchConfig {
            ranges: vec![(0x4700_0000, 0x4700_1000)],
            after: After::Cacr,
            ..WatchConfig::default()
        });
        w.dma("esdhc-dma59", 0x4700_0200, 512, 1);
        assert_eq!(w.totals().0, 0);
        w.cacr_written(IEC_DEC_INV, 2);
        w.dma("esdhc-dma59", 0x4700_0200, 512, 3);
        w.dma("esdhc-dma59", 0x4600_0000, 512, 4);
        assert_eq!(w.totals().0, 1);
    }

    #[test]
    fn a_restored_state_past_the_cacr_trigger_runs_the_watch() {
        let mut w = Watch::new(WatchConfig {
            ranges: vec![(0x4700_0000, 0x4700_1000)],
            after: After::Cacr,
            ..WatchConfig::default()
        });
        w.resume(); // caches off in the restored state: still waiting
        w.dma("esdhc-dma59", 0x4700_0200, 512, 1);
        assert_eq!(w.totals().0, 0);
        w.set_cacr(IEC_DEC_INV); // what load_state does, then resume
        w.resume();
        w.dma("esdhc-dma59", 0x4700_0200, 512, 2);
        assert_eq!(w.totals().0, 1);
    }

    fn store_then_fetch(w: &mut Watch) {
        w.store(0x4000_0100, 0x4600_0010, 4, 0x4e71_4e71, 10);
    }

    #[test]
    fn fetch_after_store_without_flush_is_reported_once() {
        let mut w = watch(&[], true);
        w.cacr_written(IEC_DEC_INV, 1);
        store_then_fetch(&mut w);
        w.fetch(0x4600_0010, 2, 11);
        w.fetch(0x4600_0012, 2, 12);
        let hits = w.flush_hits();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line, 0x4600_0010);
        assert_eq!(hits[0].fetch_pc, 0x4600_0010);
        assert_eq!(hits[0].writer, Writer::Pc(0x4000_0100));
    }

    #[test]
    fn cpushl_of_all_ways_cleans_the_line() {
        let mut w = watch(&[], true);
        w.cacr_written(SET_WAY, 1);
        store_then_fetch(&mut w);
        // CPUSHL bc over the four ways of the line's set.
        for way in 0..4 {
            w.cpushl(3, 0x4600_0010 + way);
        }
        w.fetch(0x4600_0010, 2, 11);
        assert!(w.flush_hits().is_empty());
    }

    #[test]
    fn cpushl_of_one_way_is_not_enough_and_dc_alone_is_not_enough() {
        let mut w = watch(&[], true);
        w.cacr_written(SET_WAY, 1);
        store_then_fetch(&mut w);
        w.cpushl(3, 0x4600_0010);
        for way in 0..4 {
            w.cpushl(1, 0x4600_0020 + way); // data only, another set
        }
        w.fetch(0x4600_0010, 2, 11);
        assert_eq!(w.flush_hits().len(), 1);
        let mut w = watch(&[], true);
        w.cacr_written(SET_WAY, 1);
        store_then_fetch(&mut w);
        for way in 0..4 {
            w.cpushl(1, 0x4600_0010 + way); // pushed, but the I-cache still holds the old line
        }
        w.fetch(0x4600_0010, 2, 11);
        assert_eq!(w.flush_hits().len(), 1);
    }

    #[test]
    fn physical_address_cpushl_and_cacr_invalidate_clean() {
        let mut w = watch(&[], true);
        w.cacr_written(IEC_DEC_INV | CACR_SPA, 1);
        store_then_fetch(&mut w);
        w.cpushl(3, 0x4600_0014);
        w.fetch(0x4600_0010, 2, 11);
        assert!(w.flush_hits().is_empty());
        store_then_fetch(&mut w);
        w.cacr_written(IEC_DEC_INV, 12); // ICINVA | DCINVA
        w.fetch(0x4600_0010, 2, 13);
        assert!(w.flush_hits().is_empty());
    }

    #[test]
    fn caches_off_stores_do_not_count_and_alias_needs_no_push() {
        let mut w = watch(&[], true);
        store_then_fetch(&mut w); // CACR not written yet
        w.cacr_written(IEC_DEC_INV, 11);
        w.fetch(0x4600_0010, 2, 12);
        assert!(w.flush_hits().is_empty());
        // An uncached-alias store needs only the instruction side.
        w.store(0x4000_0100, 0x4e00_0030 + ALIAS_OFFSET, 4, 0, 13);
        for way in 0..4 {
            w.cpushl(2, 0x4e00_0030 + way);
        }
        w.fetch(0x4e00_0030, 2, 14);
        assert!(w.flush_hits().is_empty());
    }

    #[test]
    fn dma_into_code_is_a_flush_hit() {
        let mut w = watch(&[], true);
        w.cacr_written(IEC_DEC_INV, 1);
        w.dma("esdhc-dma59", 0x4500_0000, 64, 5);
        w.fetch(0x4500_0020, 2, 6);
        let hits = w.flush_hits();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].writer, Writer::Dma("esdhc-dma59"));
    }

    #[test]
    fn fetch_of_a_stored_byte_is_a_hit() {
        let mut w = watch(&[], true);
        w.cacr_written(IEC_DEC_INV, 1);
        store_then_fetch(&mut w);
        w.fetch(0x4600_0010, 2, 11);
        assert_eq!(w.flush_hits().len(), 1);
        assert_eq!(w.flush_hits()[0].mask, 0x000f);
    }

    #[test]
    fn extension_word_on_a_stored_byte_is_a_hit() {
        let mut w = watch(&[], true);
        w.cacr_written(IEC_DEC_INV, 1);
        w.store(0x4000_0100, 0x4600_0012, 2, 0, 10);
        w.fetch(0x4600_0010, 6, 11);
        assert_eq!(w.flush_hits().len(), 1);
    }

    #[test]
    fn same_line_without_overlap_is_clean_and_counted() {
        let mut w = watch(&[], true);
        w.cacr_written(IEC_DEC_INV, 1);
        w.store(0x4000_0100, 0x4600_001c, 4, 0, 10);
        w.fetch(0x4600_0010, 6, 11);
        assert!(w.flush_hits().is_empty());
        assert_eq!(w.flush.as_ref().unwrap().same_line_clean, 1);
    }

    #[test]
    fn instruction_crossing_into_a_stored_line_is_a_hit() {
        let mut w = watch(&[], true);
        w.cacr_written(IEC_DEC_INV, 1);
        store_then_fetch(&mut w);
        w.fetch(0x4600_000e, 4, 11);
        assert_eq!(w.flush_hits().len(), 1);
        assert_eq!(w.flush_hits()[0].line, 0x4600_0010);
    }

    #[test]
    fn a_clean_fetch_does_not_use_up_the_report() {
        let mut w = watch(&[], true);
        w.cacr_written(IEC_DEC_INV, 1);
        w.store(0x4000_0100, 0x4600_001c, 4, 0, 10);
        w.fetch(0x4600_0010, 2, 11);
        assert!(w.flush_hits().is_empty());
        w.fetch(0x4600_001c, 2, 12);
        assert_eq!(w.flush_hits().len(), 1);
    }

    #[test]
    fn dma_marks_only_its_bytes() {
        let mut w = watch(&[], true);
        w.cacr_written(IEC_DEC_INV, 1);
        w.dma("x", 0x4500_0008, 4, 5);
        w.fetch(0x4500_0000, 2, 6);
        assert!(w.flush_hits().is_empty());
        w.fetch(0x4500_0008, 2, 7);
        assert_eq!(w.flush_hits().len(), 1);
    }
}

/// A tiny guest program on the real core and bus: store a NOP pair into RAM, jump to it.
#[cfg(test)]
mod guest_tests {
    use super::*;
    use crate::common::LoggingBus;
    use coldfire::Cpu;
    use emmc_card::Card;
    use machine::{Board, CompletionPolicy};

    const CODE: u32 = 0x4000_0100;
    const TARGET: u32 = 0x4000_0200;

    fn words(out: &mut Vec<u8>, w: &[u32]) {
        for &x in w {
            if x > 0xffff {
                out.extend_from_slice(&x.to_be_bytes());
            } else {
                out.extend_from_slice(&(x as u16).to_be_bytes());
            }
        }
    }

    /// `caches` writes CACR first, `cpushl` flushes the stored line before the jump,
    /// `alias` stores through the uncached alias.
    fn run(caches: bool, cpushl: bool, flush_check: bool) -> (Vec<FlushHit>, u64) {
        let mut code = Vec::new();
        if caches {
            // move.l #0xa50ce100,d0 ; movec d0,CACR
            words(&mut code, &[0x203c]);
            words(&mut code, &[0xa50c_e100]);
            words(&mut code, &[0x4e7b, 0x0002]);
        }
        // movea.l #TARGET,a0 ; move.l #0x4e714e71,(a0)
        words(&mut code, &[0x207c]);
        words(&mut code, &[TARGET]);
        words(&mut code, &[0x20bc]);
        words(&mut code, &[0x4e71_4e71]);
        if cpushl {
            words(&mut code, &[0xf4e8]); // cpushl bc,(a0)
        }
        words(&mut code, &[0x4ed0]); // jmp (a0)
        let mut board = Board::new(
            Card::default(),
            Default::default(),
            CompletionPolicy::Oracle,
        );
        board.map_zeroed_ram_page(0x4000_0000).unwrap();
        for (i, b) in code.iter().enumerate() {
            board
                .write_guest(CODE + i as u32, 1, u32::from(*b))
                .unwrap();
        }
        let mut bus = LoggingBus::<false>::new(board, false, false, 0, CODE);
        bus.guards.set_watch(Some(WatchConfig {
            ranges: vec![(TARGET, TARGET + 16)],
            flush_check,
            ..WatchConfig::default()
        }));
        let mut cpu = Cpu::new();
        cpu.pc = CODE;
        // The program's instructions, then the first NOP at the target.
        let steps = 2 + if caches { 2 } else { 0 } + usize::from(cpushl) + 1 + 1;
        for _ in 0..steps {
            bus.current_pc = cpu.pc;
            bus.current_icount = cpu.icount;
            if cpu.step(&mut bus).is_err() {
                break;
            }
        }
        let events = bus.guards.watch_mut().unwrap().totals().0;
        (bus.guards.flush_hits(), events)
    }

    #[test]
    fn store_then_fetch_without_cpushl_is_reported_with_both_pcs() {
        let (hits, events) = run(true, false, true);
        assert_eq!(events, 1, "the write-watch saw the store into the range");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line, TARGET);
        assert_eq!(hits[0].fetch_pc, TARGET);
        assert!(matches!(hits[0].writer, Writer::Pc(pc) if (CODE..TARGET).contains(&pc)));
    }

    #[test]
    fn cpushl_before_the_jump_is_clean() {
        let (hits, events) = run(true, true, true);
        assert_eq!(events, 1);
        assert!(hits.is_empty());
    }

    #[test]
    fn caches_off_is_clean() {
        let (hits, events) = run(false, false, true);
        assert_eq!(events, 1);
        assert!(hits.is_empty());
    }

    #[test]
    fn both_off_by_default() {
        let mut bus = LoggingBus::<false>::new(
            Board::new(
                Card::default(),
                Default::default(),
                CompletionPolicy::Oracle,
            ),
            false,
            false,
            0,
            CODE,
        );
        assert!(bus.guards.is_empty());
        assert!(bus.guards.watch_mut().is_none());
    }
}
