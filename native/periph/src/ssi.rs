//! SSI0/eDMA 48/50: the audio-clocked sample DMA producer chain -- a port of
//! `emu/ssi.py`'s `Ssi0Dma`, the *exact* (non-coalescing) path only. See
//! that module's docstring for the full hardware reasoning (request
//! cadence, the `SSI_CLKIN` assumption, coalescing) -- this file omits
//! `coalesce=True` and its `_run_block` fast path entirely: both are pure
//! performance optimizations over the exact model below (same bytes, same
//! boundaries, `emu/ssi.py`'s own docs: "Same bytes, same order... instead
//! of one per request"), and **none of the four traces this crate replays
//! against ever installs an `Ssi0Dma` at all** (`ssi0_request_hz` is never
//! passed to `tools/mmio_record.py`'s runs -- `scratchpad/
//! p4-mmio-recorder.md`: "SSI0 (`emu.ssi`) is not exercised: none of the
//! snapshots carry an `ssi0_dma` component"). This module is therefore
//! built and unit-tested from the manual and from `emu/ssi.py`'s own
//! recovered facts, but is not wired into `bin/replay.rs`'s active
//! dispatch -- there is no recorded trace to check a coalescing-free port
//! of it against, and wiring it in unexercised would be exactly the
//! unverified-speculation risk `docs/plan-native-emulator.md` warns against
//! (see this crate's `machine.rs` module docs, "SSI0" note).
//!
//! Register addresses (RM Chapter 35, cited in full in `emu/ssi.py`'s own
//! docstring: SSIn_CCR/TMASK/RMASK/FCSR, p.1080-1090) are not re-derived
//! here; `AUDIO_SSI0_REQUEST_HZ` and the eDMA channel/vector assignment are
//! carried over unchanged from that module.

use crate::edma::TcdView;
use crate::regfile::RegFile;

pub const RX_CHAN: usize = 48;
pub const TX_CHAN: usize = 50;
pub const RX_VECTOR: u16 = 168;
pub const TX_VECTOR: u16 = 170;
pub const FORCE_VECTOR: u16 = 191;
pub const INTFRCH1: u32 = 0xFC04C010;
pub const INTFRCH1_SOURCE63: u32 = 0x8000_0000;

/// `emu/ssi.py`'s recovered opt-in default: 2 requests/SSI-audio-frame x
/// 48 kHz (see that module's docstring for the three independent
/// corroborations).
pub const AUDIO_SSI0_REQUEST_HZ: u64 = 96_000;

/// The audio peer: RX samples in, TX samples out -- `emu/ssi.py`'s "SSI0
/// peer hook", the audio-side counterpart of `dspi::Peer`.
pub trait SsiPeer {
    /// -> exactly `nbytes` bytes, this period's RX samples.
    fn rx(&mut self, nbytes: usize) -> Vec<u8>;
    /// This period's captured TX samples.
    fn tx(&mut self, data: &[u8]);
}

/// A request source armed but with no peer: RX destination bytes are left
/// untouched (matches `Ssi0Dma(peer=None)`, the module's unchanged-by-
/// default behaviour) and TX bytes are only counted.
#[derive(Default)]
pub struct NoPeer;
impl SsiPeer for NoPeer {
    fn rx(&mut self, _nbytes: usize) -> Vec<u8> {
        Vec::new()
    }
    fn tx(&mut self, _data: &[u8]) {}
}

/// `RxHandoverPeer` (`emu/ssi.py`): supplies the `0x007FFFFF` marker the
/// generic vector-170 handler scans for at each RX major-loop start, so
/// vector 170 can hand itself over to the real per-block ISR. See that
/// module's class docs for the full mechanism; `at_major_start` mirrors its
/// `_at_major_start` (CITER, read *before* this call's own decrement,
/// still equal to BITER).
pub struct RxHandoverPeer;
impl RxHandoverPeer {
    pub const MARKER: u32 = 0x007F_FFFF;
    pub fn rx_major(&mut self, nbytes: usize, major_start: bool) -> Vec<u8> {
        let mut payload = vec![0u8; nbytes];
        if major_start && nbytes >= 4 {
            payload[0..4].copy_from_slice(&Self::MARKER.to_be_bytes());
        }
        payload
    }
}
impl SsiPeer for RxHandoverPeer {
    fn rx(&mut self, nbytes: usize) -> Vec<u8> {
        // Without TCD access this trait method cannot tell major-loop start
        // on its own; `Ssi0Dma::run_minor` calls `rx_major` directly instead
        // (see its body) -- this impl exists only so `RxHandoverPeer` can
        // also be used generically as a `Box<dyn SsiPeer>`.
        self.rx_major(nbytes, false)
    }
    fn tx(&mut self, _data: &[u8]) {}
}

pub struct Ssi0Dma {
    pub request_hz: u64,
    pub ips: u64,
    pub now: u64,
    /// The next request's deadline, in units of `1/request_hz` instructions
    /// (`Ssi0Dma._q`; `None` while unarmed).
    q: Option<u64>,
    pub enabled: [bool; 64],
    pub int50_asserted: bool,
    pub int50_delivered: bool,
    pub force_asserted: bool,
    pub force_delivered: bool,
    pub requests: u64,
    pub major_loops_rx: u64,
    pub major_loops_tx: u64,
    pub tx_bytes: u64,
    /// Requests rejected by the machine bus because their TCD shape or RAM
    /// range is outside this deliberately bounded diagnostic lane.
    pub rejected_requests: u64,
}

impl Default for Ssi0Dma {
    fn default() -> Self {
        Self::new(1, 1)
    }
}

impl Ssi0Dma {
    pub fn new(request_hz: u64, ips: u64) -> Self {
        Self {
            request_hz,
            ips,
            now: 0,
            q: None,
            enabled: [false; 64],
            int50_asserted: false,
            int50_delivered: false,
            force_asserted: false,
            force_delivered: false,
            requests: 0,
            major_loops_rx: 0,
            major_loops_tx: 0,
            tx_bytes: 0,
            rejected_requests: 0,
        }
    }

    fn first(&self, done: u64) -> u64 {
        done * self.request_hz + self.ips
    }

    /// `align`: start a fresh clock at an explicit legacy-upgrade boundary.
    pub fn align(&mut self, now: u64) {
        self.now = now;
        if self.enabled.iter().any(|&e| e) && self.q.is_none() {
            self.q = Some(self.first(now));
        }
    }

    /// `rescale`: change the time base, keeping the next deadline's device
    /// time.
    pub fn rescale(&mut self, ips: u64) {
        if let Some(q) = self.q
            && ips != self.ips
        {
            let origin = self.now * self.request_hz;
            let diff = origin as i128 - q as i128;
            self.q = Some((origin as i128 - diff * ips as i128 / self.ips as i128) as u64);
        }
        self.ips = ips;
    }

    /// `step`: instructions until the next request is due (no coalescing).
    pub fn step(&mut self, done: u64, remaining: Option<u64>) -> u64 {
        self.now = done;
        if !self.enabled.iter().any(|&e| e) {
            return remaining.unwrap_or(u64::MAX);
        }
        let hz = self.request_hz;
        if self.q.is_none() {
            self.q = Some(self.first(done));
        }
        let end = self.q.unwrap();
        let step = (done * hz).checked_sub(end).map(|_| 0).unwrap_or_else(|| {
            let num = end as i128 - (done * hz) as i128;
            ((num + hz as i128 - 1) / hz as i128) as u64
        });
        let step = step.max(1);
        match remaining {
            Some(r) => step.min(r),
            None => step,
        }
    }

    /// Earliest guest-instruction boundary at which a request can be due.
    /// The caller uses this to keep analytic CPU advances from crossing SSI.
    pub fn deadline(&mut self, done: u64) -> Option<u64> {
        if !self.enabled.iter().any(|&e| e) {
            return None;
        }
        Some(done.saturating_add(self.step(done, None)))
    }

    pub fn due(&mut self, done: u64) -> bool {
        if !self.enabled.iter().any(|&enabled| enabled) {
            return false;
        }
        if self.q.is_none() {
            self.q = Some(self.first(done));
        }
        done.saturating_mul(self.request_hz) >= self.q.unwrap()
    }

    pub fn advance_due(&mut self) {
        if let Some(q) = self.q {
            self.q = Some(q.saturating_add(self.ips));
        }
    }

    /// `service`: run every due request (one, without coalescing) and
    /// deliver whatever that makes ready. Returns nothing directly -- poll
    /// [`Self::int50_asserted`]/[`Self::force_asserted`] and call
    /// [`Self::deliver_vector170`]/[`Self::deliver_vector170_allowed`]
    /// analogues the same way `bin/replay.rs` drives `PitBank`/`DtimBank`
    /// through `IntcBank`/`SrTracker` (kept separate here since this module
    /// has no active replay caller to standardize the signature against --
    /// see the module docs).
    /// Not called by anything in this crate yet (see the module docs: no
    /// trace exercises SSI0). The shape a live or future replay caller
    /// would drive: fetch `rx_major_start(regs)`/`peer.rx(...)` and
    /// `peer.tx(...)` bytes itself (real DDR access this crate cannot do,
    /// see [`Self::run_minor`]'s doc comment), the same split
    /// `dspi::Dspi2Link::capture`/`arm_rx` already use.
    pub fn service(&mut self, regs: &mut RegFile, done: u64, peer: &mut dyn SsiPeer) {
        self.now = done;
        let due = done * self.request_hz;
        if let Some(q) = self.q
            && due >= q
        {
            if self.enabled[RX_CHAN] {
                let nbytes = TcdView::new(regs, RX_CHAN).nbytes() as usize;
                let provided = peer.rx(nbytes);
                self.run_minor(regs, RX_CHAN, Some(&provided));
            }
            if self.enabled[TX_CHAN] {
                // TX source bytes are outside this crate's reach (see
                // `run_minor`'s doc comment); address/CITER bookkeeping and
                // completion flagging still run, `peer.tx` does not -- a
                // live caller with a real bus calls it separately with the
                // bytes it read itself.
                self.run_minor(regs, TX_CHAN, None);
            }
            self.requests += 1;
            self.q = Some(q + self.ips);
        }
    }

    /// `_run_minor`: one DMA period on `channel`.
    ///
    /// `provided`, for the RX channel, is this period's samples -- already
    /// fetched by the caller from `peer.rx(nbytes)` (this method cannot do
    /// that itself: `nbytes` depends on the TCD, read here). For the TX
    /// channel this method cannot read the source bytes either (arbitrary
    /// guest RAM outside this crate's register file, same limitation as
    /// `dspi::Dspi2Link::capture` -- see its module docs); it returns
    /// `captured: None` there and only performs the address/CITER
    /// bookkeeping and completion flagging, which do not depend on the
    /// bytes' content. -> `None` if the channel is disabled or CITER is 0
    /// (Python's early-return no-ops).
    pub fn run_minor(
        &mut self,
        regs: &mut RegFile,
        channel: usize,
        provided: Option<&[u8]>,
    ) -> Option<RunMinorResult> {
        if !self.enabled[channel] {
            return None;
        }
        let mut tcd = TcdView::new(regs, channel);
        let citer = tcd.citer();
        if citer == 0 {
            return None;
        }
        let attr = tcd.attr();
        let source_size = 1usize << (attr & 0x7);
        let dest_size = 1usize << ((attr >> 8) & 0x7);
        if source_size != 4 || dest_size != 4 {
            return None;
        }
        let nbytes = tcd.nbytes() as usize;
        if nbytes == 0 || nbytes > 4096 || !nbytes.is_multiple_of(source_size) {
            return None;
        }
        let dest0 = tcd.daddr();
        let source0 = tcd.saddr();
        if let Some(p) = provided {
            if p.len() != nbytes {
                return None;
            }
        }
        let elements = nbytes / source_size;
        let new_source = advance(tcd.saddr(), tcd.soff(), elements);
        let new_dest = advance(tcd.daddr(), tcd.doff(), elements);
        tcd.set_saddr(new_source);
        tcd.set_daddr(new_dest);
        let citer = citer - 1;
        tcd.set_citer(citer);
        let mut major = false;
        if citer == 0 {
            // Major loop complete: reload (or scatter/gather -- unexercised,
            // see `dspi::reload`'s same caveat).
            let biter = tcd.biter();
            let slast = tcd.slast();
            tcd.set_saddr(((new_source as i64 + slast) & 0xFFFF_FFFF) as u32);
            let csr = tcd.csr();
            if csr & crate::edma::CSR_E_SG == 0 {
                let dlast = tcd.dlast();
                tcd.set_daddr(((new_dest as i64 + dlast) & 0xFFFF_FFFF) as u32);
                tcd.set_citer(biter);
            }
            if channel == RX_CHAN {
                self.major_loops_rx += 1;
            } else {
                self.major_loops_tx += 1;
            }
            if channel == TX_CHAN && csr & crate::edma::CSR_INT_MAJOR != 0 {
                self.int50_asserted = true;
                self.int50_delivered = false;
            }
            if csr & crate::edma::CSR_D_REQ != 0 {
                self.enabled[channel] = false;
                if !self.enabled.iter().any(|&enabled| enabled) {
                    self.q = None;
                }
            }
            major = true;
        }
        let _ = dest_size;
        Some(RunMinorResult {
            major_complete: major,
            source_addr: source0,
            dest_addr: dest0,
        })
    }

    /// `_on_serq`.
    pub fn on_serq(&mut self, value: u8) {
        if value & 0x80 != 0 {
            return;
        }
        let channels: &[usize] = if value & 0x40 != 0 {
            &[RX_CHAN, TX_CHAN]
        } else {
            &[(value & 0x3F) as usize]
        };
        for &ch in channels {
            if ch == RX_CHAN || ch == TX_CHAN {
                self.enabled[ch] = true;
            }
        }
        if self.enabled.iter().any(|&e| e) && self.q.is_none() {
            self.q = Some(self.first(self.now));
        }
    }

    /// `_on_cint`.
    pub fn on_cint(&mut self, value: u8) {
        if value & 0x80 != 0 {
            return;
        }
        if value & 0x40 != 0 || (value & 0x3F) as usize == TX_CHAN {
            self.int50_asserted = false;
            self.int50_delivered = false;
        }
    }

    /// `CERQ` disables a named channel, or both SSI channels for CAER.
    pub fn on_cerq(&mut self, value: u8) {
        if value & 0x80 != 0 {
            return;
        }
        if value & 0x40 != 0 {
            self.enabled[RX_CHAN] = false;
            self.enabled[TX_CHAN] = false;
        } else if matches!((value & 0x3f) as usize, RX_CHAN | TX_CHAN) {
            self.enabled[(value & 0x3f) as usize] = false;
        }
        if !self.enabled.iter().any(|&enabled| enabled) {
            self.q = None;
        }
    }

    /// `_deliver_vector170`'s IPL check is the caller's job (`interrupt_
    /// level`/`SrTracker`, as PIT/DTIM already do) -- this only says
    /// whether one is owed. Call [`Self::mark_vector170_delivered`] once the
    /// caller actually raises it.
    pub fn vector170_owed(&self) -> bool {
        self.int50_asserted && !self.int50_delivered
    }
    pub fn mark_vector170_delivered(&mut self) {
        self.int50_delivered = true;
    }

    /// `_on_intfrch1`: observe a write into INTC1's INTFRCH register (owned
    /// by `intc::IntcBank`'s register file elsewhere -- this bank only
    /// watches bit 31 to track the software-forced vector 191 source,
    /// exactly as `emu.ssi.Ssi0Dma._on_intfrch1` does; it never stores the
    /// register itself).
    pub fn on_intfrch1(&mut self, current_value: u32) {
        let asserted = current_value & INTFRCH1_SOURCE63 != 0;
        if asserted && !self.force_asserted {
            self.force_delivered = false;
        }
        self.force_asserted = asserted;
        if !asserted {
            self.force_delivered = false;
        }
    }
    pub fn vector191_owed(&self) -> bool {
        self.force_asserted && !self.force_delivered
    }
    pub fn mark_vector191_delivered(&mut self) {
        self.force_delivered = true;
    }
}

/// [`Ssi0Dma::run_minor`]'s result: whether this request completed a major
/// loop (owed a channel-50 interrupt check), and the destination address
/// this period's RX write (if any) targeted before advancing.
pub struct RunMinorResult {
    pub major_complete: bool,
    pub source_addr: u32,
    pub dest_addr: u32,
}

fn advance(addr: u32, off: i64, count: usize) -> u32 {
    ((addr as i64 + off * count as i64) & 0xFFFF_FFFF) as u32
}

impl Ssi0Dma {
    pub fn snap_save(&self, w: &mut crate::snap::Writer) {
        w.u64(self.request_hz);
        w.u64(self.ips);
        w.u64(self.now);
        w.opt_u64(self.q);
        for e in &self.enabled {
            w.bool(*e);
        }
        w.bool(self.int50_asserted);
        w.bool(self.int50_delivered);
        w.bool(self.force_asserted);
        w.bool(self.force_delivered);
        w.u64(self.requests);
        w.u64(self.major_loops_rx);
        w.u64(self.major_loops_tx);
        w.u64(self.tx_bytes);
        w.u64(self.rejected_requests);
    }
    pub fn snap_load(&mut self, r: &mut crate::snap::Reader) -> crate::snap::Result<()> {
        let hz = r.u64()?;
        let ips = r.u64()?;
        if hz != self.request_hz || ips != self.ips {
            return Err("snapshot SSI clock differs from the enabled diagnostic".into());
        }
        self.now = r.u64()?;
        self.q = r.opt_u64()?;
        for e in &mut self.enabled {
            *e = r.bool()?;
        }
        self.int50_asserted = r.bool()?;
        self.int50_delivered = r.bool()?;
        self.force_asserted = r.bool()?;
        self.force_delivered = r.bool()?;
        self.requests = r.u64()?;
        self.major_loops_rx = r.u64()?;
        self.major_loops_tx = r.u64()?;
        self.tx_bytes = r.u64()?;
        self.rejected_requests = r.u64()?;
        Ok(())
    }
}
