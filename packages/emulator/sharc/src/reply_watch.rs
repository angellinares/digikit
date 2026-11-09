//! Reply record in flight (diagnostic, opt-in): `SHARC_REPLY_INFLIGHT=1` (report) or `=fail` (also halt the engine at the first early change).
//!
//! The emulator sends the SPI2 reply record in one instant, when the ColdFire's frame arrives (`Engine::spi2_exchange`). On silicon the DSP's
//! SPI2 transmit DMA reads the record while the ColdFire shifts the frame out, so the core's stores into the record during that time change
//! what the wire carries. This check opens a window at each exchange, for the time the frame needs on the wire (frame bytes x 8 / SCK, in
//! DSP instructions), and reports every change of the record's bytes inside it: the frame number, the 32-bit word index, the PC, and whether
//! the change came before the DMA had read the word (early: the wire carries the new value) or after it (late: this send is unaffected).
//!
//! The check compares the record with a snapshot after each dispatch unit, like `write_watch`: it sees changes of value only (a store of the
//! value already there is not seen), and it knows the exact writer PC only in interpreter-only runs (`SHARC_AOT=0 SHARC_FAST_REGIONS=
//! SHARC_BLOCKTRANS=`); a block or fast-tier unit reports the PC it started at (unit letter `b` or `f`). It runs only while a window is open
//! and costs nothing when `SHARC_REPLY_INFLIGHT` is unset (the engine's per-instruction diagnostics flag stays off).
//!
//! Rate: `SHARC_REPLY_INFLIGHT_SCK` (Hz; default 44000000, the DSPI2 setting CTAR0 `0xFA010000` on the 132 MHz bus clock: bus / 3) and
//! `SHARC_REPLY_INFLIGHT_IPS` (DSP instructions per second; default 1500 x `DIGI_DSP_PERIOD`, 1 GHz). The DMA's FIFO read-ahead is not
//! modelled: a word counts as read when its first halfword would be shifted out, so the early zone ends a little too late (by the read-ahead).
//! `SHARC_REPLY_INFLIGHT_WORDS=I,J` counts only those word indexes; `SHARC_REPLY_INFLIGHT_LOG=N` prints the first N changes one by one (default 24).
//! `SHARC_REPLY_INFLIGHT_SKIP=N` leaves out the first N exchanges (the stock firmware writes a few record words during its boot).
//! Output: a summary on stderr when the engine is dropped, and the same text in `SHARC_REPLY_INFLIGHT_OUT` (default
//! `$DIGI_DIAG_DIR/reply-inflight.txt`).

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

use sharc_rt::mem::Mem;

const DEFAULT_SCK: f64 = 44_000_000.0;
const DEFAULT_IPS: f64 = 1_000_000_000.0;
/// Hits printed one by one before the summary takes over.
const LOG_FIRST: u32 = 24;
/// A progress line (the summary's first line) every this many exchanges, for a daemon that exits without dropping the engine.
const PROGRESS: u64 = 500;

struct Window {
    base: u32,
    len: usize,
    start: u64,
    end: u64,
    frame: u64,
}

#[derive(Default)]
struct Tally {
    count: u64,
    first_frame: u64,
    first_dt: u64,
}

pub struct ReplyWatch {
    fail: bool,
    sck: f64,
    ips: f64,
    frames: u64,
    /// Exchanges at or below this number are not counted (the boot).
    skip: u64,
    win: Option<Window>,
    snap: Vec<u8>,
    cur: Vec<u8>,
    /// The unit awaiting its post-check: (pc before, unit letter).
    pub pending: Option<(u64, char)>,
    /// (pc, word, early, unit) -> count
    hits: BTreeMap<(u32, u32, bool, char), Tally>,
    logged: u32,
    log_max: u32,
    lines: Vec<String>,
    /// Only these word indexes count (empty: all).
    words: Vec<u32>,
    /// The record addresses and the frame length seen.
    bases: BTreeSet<(u32, usize)>,
    /// The transmit DMA's CFG, XCNT and XMOD as the exchange found them.
    dma: BTreeSet<[u32; 3]>,
    out: Option<std::ffi::OsString>,
    /// Set at the first early change in `fail` mode; the engine halts with it.
    pub failure: Option<String>,
}

fn env_f64(name: &str) -> Option<f64> {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| *v > 0.0)
}

impl ReplyWatch {
    pub fn from_env() -> Option<Box<ReplyWatch>> {
        let v = std::env::var("SHARC_REPLY_INFLIGHT").ok()?;
        let fail = match v.trim() {
            "" | "0" => return None,
            "1" => false,
            "2" | "fail" => true,
            other => {
                eprintln!("SHARC_REPLY_INFLIGHT={other:?}: want 1 or fail; check disabled");
                return None;
            }
        };
        let ips = env_f64("SHARC_REPLY_INFLIGHT_IPS")
            .or_else(|| env_f64("DIGI_DSP_PERIOD").map(|p| p * 1500.0))
            .unwrap_or(DEFAULT_IPS);
        Some(Box::new(ReplyWatch {
            fail,
            sck: env_f64("SHARC_REPLY_INFLIGHT_SCK").unwrap_or(DEFAULT_SCK),
            ips,
            frames: 0,
            skip: env_f64("SHARC_REPLY_INFLIGHT_SKIP").map_or(0, |v| v as u64),
            win: None,
            snap: Vec::new(),
            cur: Vec::new(),
            pending: None,
            hits: BTreeMap::new(),
            logged: 0,
            lines: Vec::new(),
            log_max: env_f64("SHARC_REPLY_INFLIGHT_LOG").map_or(LOG_FIRST, |v| v as u32),
            words: std::env::var("SHARC_REPLY_INFLIGHT_WORDS")
                .map(|v| v.split(',').filter_map(|w| w.trim().parse().ok()).collect())
                .unwrap_or_default(),
            bases: BTreeSet::new(),
            dma: BTreeSet::new(),
            out: crate::diag::out_path("SHARC_REPLY_INFLIGHT_OUT", "reply-inflight.txt"),
            failure: None,
        }))
    }

    fn read(mem: &Mem, base: u32, len: usize, into: &mut Vec<u8>) {
        into.clear();
        match mem.bytes_in_page(base, len) {
            Some(b) => into.extend_from_slice(b),
            None => into.extend((0..len as u32).map(|k| mem.byte(base.wrapping_add(k)))),
        }
    }

    /// The exchange at instruction count ICOUNT starts sending the LEN-byte record at BASE.
    pub fn arm(&mut self, icount: u64, base: u32, len: usize, dma: [u32; 3], mem: &Mem) {
        self.frames += 1;
        self.bases.insert((base, len));
        if self.frames % PROGRESS == 0 {
            eprintln!("{}", self.headline());
        }
        self.dma.insert(dma);
        let span = (len as f64 * 8.0 / self.sck * self.ips) as u64;
        Self::read(mem, base, len, &mut self.snap);
        self.win = Some(Window {
            base,
            len,
            start: icount,
            end: icount + span,
            frame: self.frames,
        });
    }

    /// Close the pending unit: compare the record with the snapshot.
    pub fn check(&mut self, icount: u64, mem: &Mem) {
        let Some((pc, unit)) = self.pending.take() else {
            return;
        };
        let Some(w) = &self.win else {
            return;
        };
        if icount > w.end {
            self.win = None;
            return;
        }
        let (base, len, start, frame) = (w.base, w.len, w.start, w.frame);
        Self::read(mem, base, len, &mut self.cur);
        if self.cur == self.snap {
            return;
        }
        if frame <= self.skip {
            std::mem::swap(&mut self.snap, &mut self.cur);
            return;
        }
        let dt = icount.saturating_sub(start);
        for (i, (a, b)) in self.snap.chunks(4).zip(self.cur.chunks(4)).enumerate() {
            if a == b {
                continue;
            }
            let word = i as u32;
            if !self.words.is_empty() && !self.words.contains(&word) {
                continue;
            }
            let read_at = (word as f64 * 32.0 / self.sck * self.ips) as u64;
            let early = dt < read_at;
            let t = self.hits.entry((pc as u32, word, early, unit)).or_default();
            if t.count == 0 {
                t.first_frame = frame;
                t.first_dt = dt;
            }
            t.count += 1;
            if self.logged < self.log_max {
                self.logged += 1;
                let le = |v: &[u8]| u32::from_le_bytes([v[0], v[1], v[2], v[3]]);
                self.lines.push(format!(
                    "  frame {frame} word {word} {:#x} -> {:#x} {} pc {pc:#x} unit {unit} at +{dt} (word read at +{read_at})\n",
                    le(a),
                    le(b),
                    if early { "EARLY" } else { "late" }
                ));
            }
            if early && self.fail && self.failure.is_none() {
                self.failure = Some(format!(
                    "reply-inflight: frame {frame} word {word} written at pc {pc:#x} {dt} instructions after the record began to be sent (read at +{read_at})"
                ));
            }
        }
        std::mem::swap(&mut self.snap, &mut self.cur);
    }

    /// The first line of the summary.
    fn headline(&self) -> String {
        let count = |early: bool| -> u64 {
            self.hits
                .iter()
                .filter(|(k, _)| k.2 == early)
                .map(|(_, t)| t.count)
                .sum()
        };
        let coarse: u64 = self
            .hits
            .iter()
            .filter(|(k, _)| k.3 != 'i')
            .map(|(_, t)| t.count)
            .sum();
        format!(
            "reply-inflight: {} frames armed; sck {} Hz, {} instr/s; early {}, late {} word changes ({coarse} from block or fast-tier units: PC is the unit entry)",
            self.frames,
            self.sck,
            self.ips,
            count(true),
            count(false)
        )
    }

    pub fn summary(&self) -> String {
        let span = self.win.as_ref().map_or(0, |w| w.end - w.start);
        let mut s = format!(
            "{}{}\n",
            self.headline(),
            if span > 0 {
                format!("; window {span} instructions")
            } else {
                String::new()
            }
        );
        for [cfg, xcnt, xmod] in &self.dma {
            s += &format!("  transmit DMA cfg {cfg:#x} xcnt {xcnt:#x} xmod {xmod}\n");
        }
        s += &self.lines.concat();
        for (base, len) in &self.bases {
            s += &format!("  record {base:#x}, {len} bytes\n");
        }
        for ((pc, word, early, unit), t) in &self.hits {
            s += &format!(
                "  {} word {word} pc {pc:#x} unit {unit}: {} (first: frame {}, +{})\n",
                if *early { "early" } else { "late " },
                t.count,
                t.first_frame,
                t.first_dt
            );
        }
        s
    }
}

impl Drop for ReplyWatch {
    fn drop(&mut self) {
        let s = self.summary();
        eprint!("{s}");
        if let Some(path) = &self.out
            && let Ok(mut f) = std::fs::File::create(path)
        {
            let _ = f.write_all(s.as_bytes());
        }
    }
}
