//! Oracle-compatible PIT/DTIM/INTC timing seam for a native machine loop.
//!
//! This deliberately composes the peripheral banks rather than interpreting
//! their registers itself. `PitBank` owns PIT timing, pending-state, and PIF
//! write-one-to-clear; `IntcBank` owns oracle register passthrough and masking;
//! and `SrTracker` is updated by `PitBank::service` when an interrupt is taken.

use periph::{
    dtim::DtimBank,
    intc::IntcBank,
    machine::{HostWrite, Timers},
    pit::PitBank,
    sr::SrTracker,
};

use crate::{
    MachineState,
    timer_state::{TimerStateError, import_timers},
};

/// Selects the timer delivery contract.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TimerPolicy {
    /// Match the existing trace-oracle PIT and INTC models.
    #[default]
    Oracle,
    /// Reserved for validated hardware timer delivery semantics.
    Device,
}

/// Timer delivery that has not yet been validated for the selected policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimeError {
    /// Device-mode timer interrupt delivery has no validated implementation.
    DeviceInterruptDeliveryUnsupported,
}

/// PIT/DTIM/INTC register facade and guest-clock timing composition.
pub struct Time {
    pit: PitBank,
    dtim: DtimBank,
    intc: IntcBank,
    sr: SrTracker,
    policy: TimerPolicy,
    host_writes: Vec<HostWrite>,
}

impl Time {
    /// Build an oracle-compatible PIT bank with the supplied active channels
    /// and guest instructions-per-second clock.
    pub fn new(policy: TimerPolicy, pit_channels: Vec<usize>, ips: f64) -> Self {
        Self::with_dtims(policy, pit_channels, vec![], ips)
    }

    /// Construct a complete Python `Timers` topology. A checkpoint import
    /// must validate the saved channel order and IPS against these settings.
    pub fn with_dtims(
        policy: TimerPolicy,
        pit_channels: Vec<usize>,
        dtim_channels: Vec<usize>,
        ips: f64,
    ) -> Self {
        Self {
            pit: PitBank::new(pit_channels, ips, false),
            dtim: DtimBank::new(dtim_channels, ips, false),
            // Fresh unknown MMIO is zero-mapped by the Python oracle. Keep
            // the hardware-reset IntcBank default for Device construction.
            intc: match policy {
                TimerPolicy::Oracle => IntcBank::oracle_zeroed(),
                TimerPolicy::Device => IntcBank::new(),
            },
            sr: SrTracker::new(),
            policy,
            host_writes: Vec::new(),
        }
    }

    /// Apply validated Python v1 scheduling state without losing already
    /// loaded PIT/DTIM/INTC register pages. Failed validation changes none of
    /// these banks (`import_timers` validates both sources before mutation).
    pub fn restore_timer_component(&mut self, state: &MachineState) -> Result<(), TimerStateError> {
        let mut lane = Timers {
            pit: std::mem::take(&mut self.pit),
            dtim: std::mem::take(&mut self.dtim),
            intc: std::mem::take(&mut self.intc),
            sr: std::mem::take(&mut self.sr),
        };
        let result = import_timers(state, &mut lane);
        self.pit = lane.pit;
        self.dtim = lane.dtim;
        self.intc = lane.intc;
        self.sr = lane.sr;
        result
    }

    /// True when this seam owns a PIT, DTIM or INTC MMIO address.
    pub fn owns(addr: u32) -> bool {
        PitBank::owns(addr) || DtimBank::owns(addr) || IntcBank::owns(addr)
    }

    fn owns_access(addr: u32, size: u8) -> bool {
        if !matches!(size, 1 | 2 | 4) {
            return false;
        }
        let Some(end) = addr.checked_add(u32::from(size) - 1) else {
            return false;
        };
        // RegFile accesses are slot-local. Do not let an access beginning at
        // the final bytes of an owned slot index beyond its backing array.
        addr & !0x3fff == end & !0x3fff && Self::owns(addr)
    }

    /// Read an oracle PIT or INTC register, if this seam owns the complete,
    /// slot-local access.
    pub fn read(&self, addr: u32, size: u8) -> Option<u32> {
        Self::owns_access(addr, size).then(|| {
            self.pit
                .read(addr, size)
                .or_else(|| self.dtim.read(addr, size))
                .or_else(|| self.intc.read(addr, size))
        })?
    }

    /// Write an oracle PIT or INTC register. PIT handles PCSR PIF clearing.
    pub fn write(&mut self, addr: u32, size: u8, value: u32) -> bool {
        Self::owns_access(addr, size)
            && (self.pit.write(addr, size, value)
                || self.dtim.write(addr, size, value)
                || self.intc.write(addr, size, value))
    }

    /// Load one complete PIT-channel or INTC register page.
    pub fn load_page(&mut self, base: u32, data: &[u8]) -> bool {
        self.pit.load_page(base, data)
            || self.dtim.load_page(base, data)
            || self.intc.load_page(base, data)
    }

    /// Current timer delivery contract.
    pub fn policy(&self) -> TimerPolicy {
        self.policy
    }

    /// Seed the live CPU status register used for IPL delivery checks.
    pub fn seed_sr(&mut self, sr: u16) {
        self.sr.seed(sr);
    }

    /// Return the first PIT/DTIM deadline, arming enabled timers at `done`.
    pub fn deadline(&mut self, done: u64) -> Option<u64> {
        match (self.pit.deadline(done), self.dtim.deadline(done)) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Service PIT at the exact guest instruction boundary `done`.
    ///
    /// `PitBank::service` performs the sole `SrTracker::on_taken` call for
    /// each delivered vector. Device delivery is intentionally unsupported
    /// until its hardware behavior is validated.
    pub fn service(&mut self, done: u64) -> Result<Vec<(u16, u8)>, TimeError> {
        if self.policy == TimerPolicy::Device {
            return Err(TimeError::DeviceInterruptDeliveryUnsupported);
        }
        self.service_with(done, |_, _| true)
    }

    /// Service Oracle PIT at a boundary, retaining pending vectors declined by
    /// `offer` rather than treating them as delivered.
    pub fn service_with(
        &mut self,
        done: u64,
        mut offer: impl FnMut(u16, u8) -> bool,
    ) -> Result<Vec<(u16, u8)>, TimeError> {
        if self.policy == TimerPolicy::Device {
            return Err(TimeError::DeviceInterruptDeliveryUnsupported);
        }
        let mut raised = self
            .pit
            .service_with(done, &self.intc, &mut self.sr, &mut offer);
        let (dtim_raised, writes) = self
            .dtim
            .service_with(done, &self.intc, &mut self.sr, offer);
        raised.extend(dtim_raised);
        self.host_writes.extend(
            writes
                .into_iter()
                .map(|(addr, byte)| HostWrite { addr, byte }),
        );
        Ok(raised)
    }

    /// DTIM REF writes made while servicing the last boundary. The CPU owner
    /// must apply these as host writes, never as guest W1C register writes.
    pub fn take_host_writes(&mut self) -> Vec<HostWrite> {
        std::mem::take(&mut self.host_writes)
    }
}
