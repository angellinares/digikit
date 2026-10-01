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
    /// Earliest boundary worth servicing after a pass with no pending IRQ.
    /// Keep the banks' floating-point clock representation exactly.
    service_not_before: Option<f64>,
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
            service_not_before: None,
        }
    }

    /// Apply validated Python v1 scheduling state without losing already
    /// loaded PIT/DTIM/INTC register pages. Failed validation changes none of
    /// these banks (`import_timers` validates both sources before mutation).
    pub fn restore_timer_component(&mut self, state: &MachineState) -> Result<(), TimerStateError> {
        self.service_not_before = None;
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
        self.service_not_before = None;
        Self::owns_access(addr, size)
            && (self.pit.write(addr, size, value)
                || self.dtim.write(addr, size, value)
                || self.intc.write(addr, size, value))
    }

    /// Load one complete PIT-channel or INTC register page.
    pub fn load_page(&mut self, base: u32, data: &[u8]) -> bool {
        self.service_not_before = None;
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

    /// Pending IRQs must still be offered at every interpreted boundary.
    pub fn has_pending_interrupts(&self) -> bool {
        (0..4).any(|channel| self.pit.pending(channel) || self.dtim.pending(channel))
    }

    /// Return the first PIT/DTIM deadline, arming enabled timers at `done`.
    pub fn deadline(&mut self, done: u64) -> Option<u64> {
        self.service_not_before = None;
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
        if self
            .service_not_before
            .is_some_and(|next| (done as f64) < next)
        {
            return Ok(Vec::new());
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
        // A refused IRQ must be retried at every instruction: an SR change
        // can unmask it even before the next timer tick. MMIO writes, page
        // loads, checkpoint restores and deadline arming invalidate this
        // cache. With no pending IRQ, only the earliest tick can do work.
        self.service_not_before = if (0..4).any(|i| self.pit.pending(i) || self.dtim.pending(i)) {
            None
        } else {
            (0..4)
                .flat_map(|i| [self.pit.next_deadline(i), self.dtim.next_deadline(i)])
                .flatten()
                .try_fold(f64::INFINITY, |next, deadline| {
                    (!deadline.is_nan()).then(|| next.min(deadline))
                })
        };
        Ok(raised)
    }

    /// DTIM REF writes made while servicing the last boundary. The CPU owner
    /// must apply these as host writes, never as guest W1C register writes.
    pub fn take_host_writes(&mut self) -> Vec<HostWrite> {
        std::mem::take(&mut self.host_writes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use periph::{dtim, intc, pit};

    fn level(time: &mut Time, vector: u16, value: u32) {
        let base = intc::BASES[usize::from(vector / 64 - 1)];
        time.write(base + 0x40 + u32::from(vector % 64), 1, value);
    }

    #[test]
    fn non_finite_clock_does_not_cache_nan_deadlines() {
        let mut time = Time::new(TimerPolicy::Oracle, vec![0], f64::NAN);
        time.write(pit::BASES[0], 2, 0x000b);
        level(&mut time, pit::VECTORS[0], 1);
        time.seed_sr(0x2000);
        assert_eq!(time.service(0), Ok(vec![]));
        assert_eq!(time.service_not_before, None);
        // Preserve the existing bank's NaN comparison behavior rather than
        // treating an invalid deadline as an infinite idle interval.
        assert_eq!(time.service(1), Ok(vec![(pit::VECTORS[0], 1)]));
    }

    #[test]
    fn cached_service_matches_every_boundary_with_writes_and_refused_irqs() {
        for start in [0, (1_u64 << 53) + 1] {
            let make = || {
                let mut time = Time::with_dtims(
                    TimerPolicy::Oracle,
                    vec![3, 2, 0],
                    vec![3, 1],
                    pit::F_BUS / 3.0,
                );
                for i in [3, 2, 0] {
                    time.write(pit::BASES[i], 2, 0x000b);
                    time.write(pit::BASES[i] + 2, 2, 21 + i as u32);
                    level(&mut time, pit::VECTORS[i], 1);
                }
                for i in [3, 1] {
                    time.write(dtim::BASES[i], 2, 0x001b);
                    time.write(dtim::BASES[i] + 4, 4, 30 + i as u32);
                    level(&mut time, dtim::VECTORS[i], 2);
                }
                time
            };
            let mut fast = make();
            let mut reference = make();
            for offset in 0..1000 {
                let done = start + offset;
                for time in [&mut fast, &mut reference] {
                    time.seed_sr(if offset % 37 < 9 { 0x2700 } else { 0x2000 });
                    match offset {
                        100 => {
                            time.write(pit::BASES[0], 2, 0);
                        }
                        130 => {
                            time.write(pit::BASES[0], 2, 0x000b);
                        }
                        160 => {
                            time.write(dtim::BASES[3], 2, 0);
                        }
                        201 => {
                            time.write(dtim::BASES[3], 2, 0x001b);
                        }
                        239 => {
                            time.write(intc::BASES[0] + 8, 4, u32::MAX);
                        }
                        243 => {
                            time.write(intc::BASES[0] + 8, 4, 0);
                        }
                        300 => {
                            time.write(pit::BASES[2] + 1, 1, 0x0f);
                        }
                        401 => {
                            let mut page = vec![0; 0x4000];
                            page[0..2].copy_from_slice(&0x001b_u16.to_be_bytes());
                            page[4..8].copy_from_slice(&7_u32.to_be_bytes());
                            assert!(time.load_page(dtim::BASES[3], &page));
                        }
                        510 => {
                            time.deadline(done);
                        }
                        _ => {}
                    }
                }
                reference.service_not_before = None;
                let offer = |_, _| offset % 11 != 0;
                assert_eq!(
                    fast.service_with(done, offer),
                    reference.service_with(done, offer)
                );
                assert_eq!(fast.take_host_writes(), reference.take_host_writes());
                for i in 0..4 {
                    assert_eq!(fast.pit.pending(i), reference.pit.pending(i));
                    assert_eq!(fast.pit.next_deadline(i), reference.pit.next_deadline(i));
                    assert_eq!(fast.pit.fired(i), reference.pit.fired(i));
                    assert_eq!(fast.pit.missed(i), reference.pit.missed(i));
                    assert_eq!(fast.pit.cleared(i), reference.pit.cleared(i));
                    assert_eq!(fast.dtim.pending(i), reference.dtim.pending(i));
                    assert_eq!(fast.dtim.next_deadline(i), reference.dtim.next_deadline(i));
                    assert_eq!(fast.dtim.fired(i), reference.dtim.fired(i));
                    assert_eq!(fast.dtim.missed(i), reference.dtim.missed(i));
                    assert_eq!(fast.dtim.cleared(i), reference.dtim.cleared(i));
                }
            }
        }
    }
}
