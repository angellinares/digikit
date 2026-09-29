//! The timers+INTC lane as one unit: routes guest reads/writes to whichever
//! bank owns the address, and drives `service()` at a guest-clock boundary
//! in the order `emu.longrun.Timers` does (PIT sources before DMA-timer
//! sources -- `build_timers` constructs `Pits` first).
//!
//! This is the interface the plan's P4 stage 2b (DMA/SSI/DSPI) is meant to
//! follow: a bank exposes `read`/`write` (guest MMIO), `service(clock, ...)`
//! (advance to an instruction-count boundary, returning what it raised), and
//! `deadline`/`step` (for a live driver that does not have a trace telling
//! it when to call `service`). A trace replay only ever needs `service`; a
//! future whole-machine loop needs `deadline`/`step` to know how far it may
//! run before the next call is due.

use crate::dtim::DtimBank;
use crate::intc::IntcBank;
use crate::pit::PitBank;
use crate::sr::SrTracker;

/// One timer/DTIM vector raised at a guest-clock instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Raised {
    pub vector: u16,
    pub level: u8,
}

/// One host write this lane performed into guest-visible memory (DTIM's
/// DTER REF bit; PIT never writes guest memory, see `pit.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostWrite {
    pub addr: u32,
    pub byte: u8,
}

#[derive(Default)]
pub struct Timers {
    pub pit: PitBank,
    pub dtim: DtimBank,
    pub intc: IntcBank,
    pub sr: SrTracker,
}

impl Timers {
    pub fn new(pit_channels: Vec<usize>, dtim_channels: Vec<usize>, ips: f64) -> Self {
        Self {
            pit: PitBank::new(pit_channels, ips, false),
            dtim: DtimBank::new(dtim_channels, ips, false),
            intc: IntcBank::default(),
            sr: SrTracker::default(),
        }
    }

    pub fn owns(addr: u32) -> bool {
        PitBank::owns(addr) || DtimBank::owns(addr) || IntcBank::owns(addr)
    }

    /// Route a guest read. None if `addr` is outside this lane entirely.
    pub fn read(&self, addr: u32, size: u8) -> Option<u32> {
        self.pit
            .read(addr, size)
            .or_else(|| self.dtim.read(addr, size))
            .or_else(|| self.intc.read(addr, size))
    }

    /// Route a guest write. -> whether some bank in this lane owned `addr`.
    pub fn write(&mut self, addr: u32, size: u8, value: u32) -> bool {
        if self.pit.write(addr, size, value) {
            return true;
        }
        if self.dtim.write(addr, size, value) {
            return true;
        }
        self.intc.write(addr, size, value)
    }

    pub fn set_ips(&mut self, ips: f64) {
        self.pit.set_ips(ips);
        self.dtim.set_ips(ips);
    }

    /// `Timers.rescale`: change the time base, keeping each deadline's
    /// device time.
    pub fn rescale(&mut self, now: f64, new_ips: f64) {
        self.pit.rescale(now, new_ips);
        self.dtim.rescale(now, new_ips);
    }

    /// `tools/mmio_record.py`'s `release_intro_timers`: at the intro's exit
    /// hook, the PIT bank's intro-only `(3,)` channel set expands to the
    /// full `(3, 2, 0)` (DTIM's stays `(3,)`), and both banks release their
    /// `held` flag. A trace's `MARK {"event": "intro handover"}` record is
    /// this instant; it carries no channel list of its own (Python mutates
    /// `channels` in place), so a replay has to know this rule rather than
    /// read it off the record.
    pub fn release_intro(&mut self) {
        if self.pit.channels() == [3] {
            self.pit.set_channels(vec![3, 2, 0]);
        }
        self.pit.release();
        self.dtim.release();
    }

    /// Advance to guest instruction count `done`. PIT sources first, then
    /// DTIM (construction order in `emu.longrun.build_timers`). -> raised
    /// vectors in delivery order, and any host writes performed.
    ///
    /// Also arms any channel that is enabled but not yet scheduled, at this
    /// same `done` -- in `emu.longrun.spin`, `Pits.step`/`Dtims.step` (via
    /// `deadline`) run immediately after `service` at the identical clock,
    /// to size the *next* chunk, and `deadline`'s arming
    /// (`if next[ch] is None: next[ch] = done + period`) is a side effect of
    /// that call, not of `service`. A channel newly turned on (or a
    /// freshly-constructed bank, as at a `restored: false` window's start)
    /// is therefore armed at the SAME clock its first `service` call finds
    /// it off, and can be due -- and deliver -- on the very next boundary.
    /// A replay that only ever calls `service` arms one boundary late,
    /// which reproduced as spurious missing/unexpected DTIM/PIT vectors
    /// throughout a freshly-constructed window (`dn2-boot400M`) even though
    /// `RD`/`WR`/`HWR` and periodic `STATE` checkpoints all matched exactly
    /// -- the register-level model was right; only the arming instant was
    /// off. This call folds `step`'s arming in right after `service`, at the
    /// same `done`, so a replay does not need its own separate `step` call.
    pub fn service(&mut self, done: u64) -> (Vec<Raised>, Vec<HostWrite>) {
        let mut raised = Vec::new();
        let mut writes = Vec::new();
        for (v, l) in self.pit.service(done, &self.intc, &mut self.sr) {
            raised.push(Raised {
                vector: v,
                level: l,
            });
        }
        let (dv, dw) = self.dtim.service(done, &self.intc, &mut self.sr);
        for (v, l) in dv {
            raised.push(Raised {
                vector: v,
                level: l,
            });
        }
        for (a, b) in dw {
            writes.push(HostWrite { addr: a, byte: b });
        }
        self.pit.deadline(done);
        self.dtim.deadline(done);
        (raised, writes)
    }

    /// -> the earliest of the PIT and DTIM banks' deadlines (`Timers.step`
    /// takes the minimum across sources).
    pub fn step(&mut self, done: u64, remaining: Option<u64>) -> u64 {
        self.pit
            .step(done, remaining)
            .min(self.dtim.step(done, remaining))
    }
}
