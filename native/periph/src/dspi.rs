//! DSPI2 (`0xEC03_8000`) + eDMA channels 28/29: the periodic ColdFire<->SHARC
//! frame link -- a direct port of `emu/dspi2.py`'s `Dspi2Link`. See that
//! module's docstring for the hardware reasoning (channel roles, the polled
//! `SR_LINK_IDLE` status bit instead of an interrupt, the defensive
//! `INT_MAJOR` completion vector).
//!
//! Also documents DSPI0/1/3's shared register layout (RM Chapter 40, Table
//! 40-3 p.1180): none of this crate's replayed traces exercise DSPI1 beyond
//! a handful of plain register writes (`scratchpad/p4-mmio-recorder.md`,
//! "Peripherals... the Python emulator does not model"), so DSPI1/DSPI3 stay
//! plain `RegFile` passthrough in `EdmaBank`'s neighbour slot -- see
//! [`DSPI1_BASE`]'s doc comment.
//!
//! ## The peer, and where its bytes come from in a replay
//!
//! [`Peer::exchange`] is exactly `emu.dspi2`'s `peer.exchange(tx) -> rx`
//! contract: the SHARC side is outside this machine (`docs/plan-native-
//! emulator.md`'s P4 task), and every trace this crate replays against was
//! itself recorded with `ZeroPeer` (`tools/mmio_record.py` always passes
//! `emu.dspi2.ZeroPeer()` as `dspi2_peer` -- see its own source and
//! `scratchpad/p4-mmio-recorder.md`'s window table), so [`ZeroPeer`] here
//! reproduces the same silence; a replay checks its RX output against the
//! trace's own `HWR` for `_deliver`, which doubles as independent
//! confirmation that `ZeroPeer` really was what recorded these traces.
//!
//! The TX side has the harder problem: [`Dspi2Link::capture`] needs the raw
//! bytes eDMA channel 29 would have read from `0x8000_1BC0`-ish SRAM
//! staging -- guest DDR this peripheral-only crate has no bus to. A replay
//! does not invent them: it takes the trace's own merged `HRD` record for
//! that exact `_capture` call (`scratchpad/p4-mmio-recorder.md`: "HRD
//! recorded only for peripheral modules... the DMA source data a model
//! replayed in isolation needs as input") and hands its bytes to
//! `capture`'s `source` parameter, so what gets checked is the
//! *transformation* (PUSHR-tag stripping, `SMOD` masking, the TCD
//! write-back), on real recorded input, not a re-derivation of memory this
//! crate cannot see -- see `bin/replay.rs`'s module docs, "TX capture: HRD
//! as the memory substitute". A live machine (a future `native/machine`
//! with a real bus) instead reads those bytes itself and calls `capture`
//! the same way.

use crate::edma::{TcdView, tcd_offset};
use crate::regfile::RegFile;

pub const DSPI2_BASE: u32 = 0xEC038000;
pub const DSPI2_MCR: u32 = DSPI2_BASE;
pub const DSPI2_SR: u32 = DSPI2_BASE + 0x2C;

/// DSPI1's register block (`0xFC03_C000`, RM Table 40-3 p.1180): same
/// layout as DSPI0/2/3 (MCR at +0x00, SR at +0x2C, PUSHR/POPR at
/// +0x34/+0x38), but nothing in this codebase's traces drives it beyond a
/// handful of plain reads/writes at MCR and SR -- `scratchpad/
/// p4-mmio-recorder.md` calls the eDMA TCD14/15 programming seen alongside
/// it "likely a DSPI1 DMA pair", not confirmed. Kept as plain `RegFile`
/// passthrough (matching the Python oracle, which does not model it
/// either): implementing a second, unverified DMA engine here would be
/// speculation this worktree's traces cannot check. [`Dspi1Sr`] documents
/// the one register layout fact (RM p.1188-1189, shared by every DSPI
/// instance) worth a manual-cited unit test even though it is never
/// evaluated by a replay.
pub const DSPI1_BASE: u32 = 0xFC03C000;
pub const DSPI1_MCR: u32 = DSPI1_BASE;
pub const DSPI1_SR: u32 = DSPI1_BASE + 0x2C;

pub const TX_CHAN: usize = 29;
pub const RX_CHAN: usize = 28;
pub const TX_VECTOR: u16 = (TX_CHAN + 120) as u16; // 149
pub const RX_VECTOR: u16 = (RX_CHAN + 120) as u16; // 148
/// DSPIx_SR bit 28 (`EOQF`, RM Table 40-7 p.1189: "End of queue flag...
/// set when the TX FIFO entry has the EOQ bit set... and after the last
/// incoming databit is sampled"). `FUN_400cd2bc` polls this bit
/// (`emu/dspiframe.py`'s naming, `SR_LINK_IDLE`) to see whether the
/// previous transfer finished before arming a new one.
pub const SR_LINK_IDLE: u32 = 1 << 28;

pub const FRAME_BYTES: usize = 0xABC; // 2,748 bytes, emu/dspiframe.py
pub const PUSHR_TAG: u16 = 0x8001;

/// The SHARC side of the link: outside this machine (see the module docs).
pub trait Peer {
    /// -> exactly `tx.len()` bytes.
    fn exchange(&mut self, tx: &[u8]) -> Vec<u8>;
}

/// What the unmodeled hardware already implies: an untouched, zero-
/// initialized SRAM RX buffer (`emu.dspi2.ZeroPeer`; every trace this crate
/// replays against was recorded with exactly this peer -- see the module
/// docs).
#[derive(Default)]
pub struct ZeroPeer;

impl Peer for ZeroPeer {
    fn exchange(&mut self, tx: &[u8]) -> Vec<u8> {
        vec![0u8; tx.len()]
    }
}

/// One channel's captured-TX-waiting-for-RX-arm state, and the reverse
/// (`Dspi2Link`'s `_tx_ready`/`_rx_armed`, `emu/dspi2.py`).
#[derive(Default)]
pub struct Dspi2Link {
    pub tx_chan: usize,
    pub rx_chan: usize,
    pub raise_completion: bool,
    tx_ready: Option<Vec<u8>>,
    rx_armed: bool,
    /// Queued completion vectors (`chan -> ()`), delivered from `service`.
    pending_vector: Vec<usize>,
    pub frames: u64,
    pub tx_bytes: u64,
}

/// One host write `deliver` (the RX major loop) performs into guest memory
/// outside this crate's own register file -- what a replay checks against
/// the trace's `HWR` for `_deliver`.
pub struct DeliverWrite {
    pub addr: u32,
    pub data: Vec<u8>,
}

impl Dspi2Link {
    /// A TX may wait for a later RX SERQ. The board preflights the RX
    /// descriptor against these actual captured bytes before exchange.
    pub fn pending_tx_len(&self) -> Option<usize> {
        self.tx_ready.as_ref().map(Vec::len)
    }

    pub fn rx_armed(&self) -> bool {
        self.rx_armed
    }

    /// Deferred source capture follows SERQ; complete an exchange if RX
    /// was armed earlier (including a set-all SERQ selecting both channels).
    pub fn exchange_after_capture(
        &mut self,
        regs: &mut RegFile,
        peer: &mut dyn Peer,
    ) -> Option<DeliverWrite> {
        self.maybe_exchange(regs, peer)
    }

    pub fn new(tx_chan: usize, rx_chan: usize, raise_completion: bool) -> Self {
        Self {
            tx_chan,
            rx_chan,
            raise_completion,
            tx_ready: None,
            rx_armed: false,
            pending_vector: Vec::new(),
            frames: 0,
            tx_bytes: 0,
        }
    }

    fn int_major(regs: &RegFile, chan: usize) -> bool {
        regs.u16_at(tcd_offset(chan) + crate::edma::CSR) & crate::edma::CSR_INT_MAJOR != 0
    }

    /// `_on_serq`: a `EDMA_SERQ` write naming (or `SAER`-ing) `tx_chan`
    /// captures it (see [`Self::capture`] -- the caller supplies the bytes,
    /// from a trace's `HRD` or a live bus); naming `rx_chan` arms it. ->
    /// whether this channel set includes a channel this link owns (so a
    /// caller building a replay knows whether to still expect an `HRD`).
    pub fn touches(&self, value: u8) -> bool {
        if value & 0x80 != 0 {
            return false; // NOP bit
        }
        if value & 0x40 != 0 {
            return true; // SAER: set-all touches every channel
        }
        let ch = (value & 0x3F) as usize;
        ch == self.tx_chan || ch == self.rx_chan
    }

    /// Whether an `EDMA_SERQ` write (already checked for size==1, no NOP
    /// bit, via [`Self::touches`]) arms `tx_chan` -- the caller needs this
    /// to know whether an `HRD` for [`Self::capture`] is expected next.
    pub fn arms_tx(&self, value: u8) -> bool {
        value & 0x40 != 0 || (value & 0x3F) as usize == self.tx_chan
    }
    pub fn arms_rx(&self, value: u8) -> bool {
        value & 0x40 != 0 || (value & 0x3F) as usize == self.rx_chan
    }

    /// Run `tx_chan`'s whole major loop now, given the raw source bytes a
    /// trace's `HRD` (or a live bus) supplied -- see the module docs. ->
    /// the logical TX frame (PUSHR tags stripped, exactly what
    /// `peer.exchange` receives in Python). A live bus calls
    /// [`Self::exchange_after_capture`] if `rx_chan` was already armed.
    pub fn capture(&mut self, regs: &mut RegFile, source: &[u8]) -> Vec<u8> {
        let out = Self::run_capture(regs, self.tx_chan, source);
        self.tx_ready = Some(out.clone());
        out
    }

    fn run_capture(regs: &mut RegFile, chan: usize, source: &[u8]) -> Vec<u8> {
        let mut tcd = TcdView::new(regs, chan);
        let citer = tcd.citer() as usize;
        if citer == 0 {
            return Vec::new();
        }
        let attr = tcd.attr();
        let elem = 1usize << (attr & 0x7);
        let nbytes = tcd.nbytes() as usize;
        assert!(
            nbytes != 0 && nbytes.is_multiple_of(elem),
            "Dspi2Link: NBYTES not a multiple of the source element size"
        );
        let want = citer * nbytes;
        assert!(
            source.len() >= want,
            "Dspi2Link::capture: source has {} bytes, TCD wants {}",
            source.len(),
            want
        );
        let mut out =
            Vec::with_capacity(citer * (nbytes / elem) * if elem == 4 { 2 } else { elem });
        for group in source[..want].chunks(elem) {
            if elem == 4 {
                // The upper 16 bits are the driver's own PUSHR CONT/PCS/EOQ
                // tag, consumed by the (unmodelled) DSPI2 shift register and
                // never shifted onto the wire; only TXDATA (the low 16
                // bits) reaches the SHARC (`emu/dspi2.py`'s `_capture`).
                out.extend_from_slice(&group[2..4]);
            } else {
                out.extend_from_slice(group);
            }
        }
        // Address bookkeeping only (the actual bytes came from the trace,
        // already reflecting whatever SMOD wraparound Python applied) --
        // advance SADDR by the same rule so a later `RD`/`HWR` of it
        // matches, and reload CITER<-BITER (or scatter/gather).
        let soff = tcd.soff();
        let mask = tcd.smod_mask();
        let mut src = tcd.saddr();
        let base = if mask != 0 { src & !mask } else { 0 };
        for _ in 0..citer * (nbytes / elem) {
            src = ((src as i64 + soff) & 0xFFFF_FFFF) as u32;
            if mask != 0 {
                src = base | (src & mask);
            }
        }
        src = ((src as i64 + tcd.slast()) & 0xFFFF_FFFF) as u32;
        if mask != 0 {
            src = base | (src & mask);
        }
        tcd.set_saddr(src);
        reload(&mut tcd);
        out
    }

    /// `_on_serq`'s RX-arm half: marks `rx_chan` armed and, if `tx_chan`'s
    /// bytes are already captured, runs the exchange. -> the RX major
    /// loop's host write (guest memory outside this crate's slot, checked
    /// against the trace's `HWR`), if the exchange ran.
    pub fn arm_rx(&mut self, regs: &mut RegFile, peer: &mut dyn Peer) -> Option<DeliverWrite> {
        self.rx_armed = true;
        self.maybe_exchange(regs, peer)
    }

    fn maybe_exchange(&mut self, regs: &mut RegFile, peer: &mut dyn Peer) -> Option<DeliverWrite> {
        if !self.rx_armed {
            return None;
        }
        let tx = self.tx_ready.take()?;
        let rx = peer.exchange(&tx);
        assert_eq!(
            rx.len(),
            tx.len(),
            "Dspi2Link peer returned {} bytes for a {}-byte frame",
            rx.len(),
            tx.len()
        );
        let write = Self::run_deliver(regs, self.rx_chan, &rx);
        self.rx_armed = false;
        self.frames += 1;
        self.tx_bytes += tx.len() as u64;
        if Self::int_major(regs, self.tx_chan) {
            self.pending_vector.push(self.tx_chan);
        }
        if Self::int_major(regs, self.rx_chan) {
            self.pending_vector.push(self.rx_chan);
        }
        Some(write)
    }

    fn run_deliver(regs: &mut RegFile, chan: usize, data: &[u8]) -> DeliverWrite {
        let mut tcd = TcdView::new(regs, chan);
        let citer = tcd.citer() as usize;
        let attr = tcd.attr();
        let elem = 1usize << ((attr >> 8) & 0x7);
        let nbytes = tcd.nbytes() as usize;
        if citer == 0 {
            return DeliverWrite {
                addr: tcd.daddr(),
                data: Vec::new(),
            };
        }
        assert!(
            nbytes != 0 && nbytes.is_multiple_of(elem),
            "Dspi2Link: NBYTES not a multiple of the dest element size"
        );
        let expect = citer * (nbytes / elem) * elem;
        assert_eq!(
            data.len(),
            expect,
            "Dspi2Link: peer frame is {} bytes, TCD{} expects {}",
            data.len(),
            chan,
            expect
        );
        let dst0 = tcd.daddr();
        let doff = tcd.doff();
        let mask = tcd.dmod_mask();
        let base = if mask != 0 { dst0 & !mask } else { 0 };
        let mut dst = dst0;
        for _ in 0..citer * (nbytes / elem) {
            dst = ((dst as i64 + doff) & 0xFFFF_FFFF) as u32;
            if mask != 0 {
                dst = base | (dst & mask);
            }
        }
        dst = ((dst as i64 + tcd.dlast()) & 0xFFFF_FFFF) as u32;
        if mask != 0 {
            dst = base | (dst & mask);
        }
        tcd.set_daddr(dst);
        reload(&mut tcd);
        DeliverWrite {
            addr: dst0,
            data: data.to_vec(),
        }
    }

    /// `_on_cint`: an `EDMA_CINT` write for `tx_chan`/`rx_chan` (or
    /// `CAIR`/all) drops any queued completion for that channel.
    pub fn on_cint(&mut self, value: u8) {
        if value & 0x40 != 0 {
            self.pending_vector.clear();
            return;
        }
        let ch = (value & 0x3F) as usize;
        self.pending_vector.retain(|&c| c != ch);
    }

    /// `service`: deliver any queued completion(s) the caller's IPL check
    /// (already done by the caller, exactly like `PitBank`/`DtimBank`'s
    /// `intc`/`sr` collaboration) allows. -> the vectors to raise, in
    /// `(tx_chan, rx_chan)` order (Python's own iteration order).
    pub fn take_pending(&mut self) -> Vec<(usize, u16)> {
        if !self.raise_completion {
            self.pending_vector.clear();
            return Vec::new();
        }
        let mut out = Vec::new();
        for (chan, vector) in [(self.tx_chan, TX_VECTOR), (self.rx_chan, RX_VECTOR)] {
            if self.pending_vector.contains(&chan) {
                out.push((chan, vector));
            }
        }
        out
    }
    pub fn clear_pending(&mut self, chan: usize) {
        self.pending_vector.retain(|&c| c != chan);
    }
}

/// TCDn_CSR's major-loop-complete reload (`_reload`, shared by `capture`
/// and `run_deliver`): scatter-gather if `E_SG`, else `CITER <- BITER`.
fn reload(tcd: &mut TcdView) {
    let csr = tcd.csr();
    if csr & crate::edma::CSR_E_SG != 0 {
        // A live machine would fetch the 32-byte descriptor at DLAST_SGA
        // and overwrite this TCD with it; no trace this crate replays ever
        // sets E_SG on channels 28/29 (`FUN_400cd2bc` never does -- see the
        // module docs), so this path is defensive only, matching Python's
        // `_reload` raising if the pointer is misaligned rather than
        // silently doing nothing.
        assert_eq!(
            tcd.dlast_raw() & 0x1F,
            0,
            "Dspi2Link: scatter/gather pointer is not 32-byte aligned"
        );
    } else {
        let biter = tcd.biter();
        tcd.set_citer(biter);
    }
}
