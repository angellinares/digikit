//! Oracle-compatible PIT/DTIM/INTC timing seam for a native machine loop.
//!
//! This deliberately composes the peripheral banks rather than interpreting
//! their registers itself. `PitBank` owns PIT timing, pending-state, and PIF
//! write-one-to-clear; `IntcBank` owns oracle register passthrough and masking;
//! and `SrTracker` is updated by `PitBank::service` when an interrupt is taken.

use periph::{
    dtim::{DeviceReferenceError, DtimBank},
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
    /// Bounded explicit Device PIT events; automatic timer delivery is unsupported.
    Device,
}

/// Timer delivery that has not yet been validated for the selected policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimeError {
    /// Device-mode timer interrupt delivery has no validated implementation.
    DeviceInterruptDeliveryUnsupported,
}

/// Invalid use of the explicit Device PIT event/request component.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DevicePitError {
    /// Explicit Device PIT APIs require Device timer policy.
    DevicePolicyRequired,
    /// PIT physical channels are numbered 0 through 3.
    InvalidChannel { channel: usize },
}

/// Invalid use of the bounded Device DTIM reference-event component.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeviceDtimError {
    DevicePolicyRequired,
    InvalidChannel { channel: usize },
    UnsupportedMode { channel: usize },
    CounterOutOfRange {
        channel: usize,
        counter: u32,
        reference: u32,
    },
}

impl From<DeviceReferenceError> for DeviceDtimError {
    fn from(error: DeviceReferenceError) -> Self {
        match error {
            DeviceReferenceError::InvalidChannel { channel } => Self::InvalidChannel { channel },
            DeviceReferenceError::UnsupportedMode { channel } => Self::UnsupportedMode { channel },
            DeviceReferenceError::CounterOutOfRange { channel, counter, reference } => {
                Self::CounterOutOfRange { channel, counter, reference }
            }
        }
    }
}

/// INTC1 source 63: forced by the guest too, but delivered by the SSI model.
const SSI_VECTOR191: u16 = 191;

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
    /// `(earliest deadline, level)` while IRQs are pending but every one is
    /// declined by its INTC level or the CPU IPL: `service_with` then changes
    /// nothing until a deadline, an IPL below `level`, or any register write
    /// or load (which clears this). Host-only cache, not checkpointed.
    blocked: Option<(f64, u8)>,
    /// Deliver guest-forced INTC sources (INTFRCH/INTFRCL). On by default
    /// since 2026-10-13 (finding 19, sequencer step ISR). Native
    /// `DIGI_INTC_FORCE=0` turns it off at construction for A/B; on wasm32 it
    /// is always on. `restore_timer_component` does not touch it.
    force_irq: bool,
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
            dtim: match policy {
                TimerPolicy::Oracle => DtimBank::new(dtim_channels, ips, false),
                TimerPolicy::Device => DtimBank::new_device(dtim_channels, ips, false),
            },
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
            blocked: None,
            force_irq: Self::default_force_irq(),
        }
    }

    fn default_force_irq() -> bool {
        #[cfg(target_arch = "wasm32")]
        {
            true
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            std::env::var_os("DIGI_INTC_FORCE").is_none_or(|v| v != "0")
        }
    }

    /// Software-forced INTC delivery (see `force_irq`).
    pub fn set_force_irq(&mut self, on: bool) {
        self.force_irq = on;
        self.service_not_before = None;
        self.blocked = None;
    }

    fn forced_candidate(&self) -> Option<(u16, u8)> {
        if self.force_irq {
            self.intc.forced_candidate(SSI_VECTOR191)
        } else {
            None
        }
    }

    /// Set the bank clocks without moving any deadline. Used before a snapshot load, whose
    /// saved deadlines are in the base-rate time base.
    pub fn set_ips(&mut self, ips: f64) {
        self.pit.set_ips(ips);
        self.dtim.set_ips(ips);
    }

    /// Change the instruction rate at tick `now`, keeping each armed deadline's emulated
    /// time (remaining instructions scale by `new/old`).
    pub fn rescale(&mut self, now: f64, new_ips: f64) {
        self.pit.rescale(now, new_ips);
        self.dtim.rescale(now, new_ips);
        self.service_not_before = None;
        self.blocked = None;
    }

    /// Apply validated Python v1 scheduling state without losing already
    /// loaded PIT/DTIM/INTC register pages. Failed validation changes none of
    /// these banks (`import_timers` validates both sources before mutation).
    pub fn restore_timer_component(&mut self, state: &MachineState) -> Result<(), TimerStateError> {
        self.service_not_before = None;
        self.blocked = None;
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

    /// Opt-in machine snapshot of the PIT/DTIM/INTC/SR state and the
    /// scheduling cache. Channel sets and clock are construction settings.
    pub fn snap_save(&self, w: &mut periph::snap::Writer) {
        w.tag("TIME");
        self.pit.snap_save(w);
        self.dtim.snap_save(w);
        self.intc.snap_save(w);
        self.sr.snap_save(w);
        w.u64(self.host_writes.len() as u64);
        for hw in &self.host_writes {
            w.u32(hw.addr);
            w.u8(hw.byte);
        }
        w.opt_f64(self.service_not_before);
    }

    pub fn snap_load(&mut self, r: &mut periph::snap::Reader) -> periph::snap::Result<()> {
        r.tag("TIME")?;
        self.pit.snap_load(r)?;
        self.dtim.snap_load(r)?;
        self.intc.snap_load(r)?;
        self.sr.snap_load(r)?;
        let n = r.len(1 << 20)?;
        self.host_writes.clear();
        for _ in 0..n {
            let addr = r.u32()?;
            let byte = r.u8()?;
            self.host_writes.push(HostWrite { addr, byte });
        }
        self.service_not_before = r.opt_f64()?;
        Ok(())
    }

    /// True when this seam owns a PIT, DTIM or INTC MMIO address.
    #[inline]
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

    /// Read an owned, slot-local register. Device SIMR/CIMR byte reads return
    /// zero; unsupported accesses overlapping those commands reject.
    pub fn read(&self, addr: u32, size: u8) -> Option<u32> {
        if self.policy == TimerPolicy::Device && IntcBank::hw_touches_simr_cimr(addr, size) {
            return (size == 1).then_some(0);
        }
        Self::owns_access(addr, size).then(|| {
            self.pit
                .read(addr, size)
                .or_else(|| self.dtim.read(addr, size))
                .or_else(|| self.intc.read(addr, size))
        })?
    }

    /// Read the shared INTC mask/ICR eligibility of eSDHC (INTC2 source 31).
    /// CPU IPL is checked separately by the isolated machine delivery path.
    pub fn esdhc_interrupt_level(&self) -> Option<u8> {
        self.intc.level_for_vector(223)
    }

    /// Eligibility of INTC2 source 0 (eDMA channels 56..63).
    pub fn storage_dma_interrupt_level(&self) -> Option<u8> {
        self.intc.level_for_vector(192)
    }

    /// The explicit storage/force seam does not model controller level masks
    /// or automatic IACK mask updates. CLMASK 0 and 15 disable level masking.
    pub fn storage_forced_config_supported(&self) -> bool {
        self.intc.iconfig() & (1 << 5) == 0 && matches!(self.intc.clmask(), 0 | 15)
    }

    /// Storage lines plus every software-forced source, using the shared bank.
    /// Independent of the Oracle force-delivery knob; no timer or SSI service.
    pub fn storage_with_forced_candidate(&self, esdhc: bool, dma: bool) -> Option<(u16, u8)> {
        let low = (u32::from(esdhc) << 31) | u32::from(dma);
        self.intc.hw_candidate([(0, 0), (0, 0), (low, 0)])
    }

    /// Select once across supplied Device PIT/DTIM reference events, storage,
    /// eDMA SSI50 and all software-force sources. Requires Device timer policy and
    /// supported DTIM reference modes. No clock advancement, SR mutation or
    /// acknowledgement occurs. No automatic SSI service, DSPI or host input.
    pub fn device_event_candidate(
        &self,
        esdhc: bool,
        dma: bool,
        ssi50: bool,
    ) -> Result<Option<(u16, u8)>, DeviceDtimError> {
        let dtim = self.device_dtim_reference_requests()?;
        // SSI50 is INTC1 source 42/vector 170, not software-force source 63.
        let mut asserted = [
            (0, 0),
            (0, u32::from(ssi50) << 10),
            ((u32::from(esdhc) << 31) | u32::from(dma), 0),
        ];
        // DTIM0..3 vectors 96..99 are INTC0 high sources 32..35.
        for vector in dtim {
            asserted[0].1 |= 1 << (vector - 96);
        }
        // PIT0..3 vectors 205..208 are INTC2 low sources 13..16.
        // hw_candidate ORs INTFRC with these masks, deduplicating aliases.
        for vector in self.pit.device_pending_vectors() {
            asserted[2].0 |= 1 << (vector - 192);
        }
        Ok(self.intc.hw_candidate(asserted))
    }

    /// Supply a Device PIT timeout event without clock advancement or delivery.
    /// PIF latches independently of EN/PIE. Invalid policy/channel rejects before
    /// register or cache mutation. The supplied status participates only when
    /// Machine::step_device_events is selected; storage-only routes exclude it.
    pub fn device_pit_timeout(&mut self, channel: usize) -> Result<(), DevicePitError> {
        if self.policy != TimerPolicy::Device {
            return Err(DevicePitError::DevicePolicyRequired);
        }
        if !self.pit.device_latch_timeout(channel) {
            return Err(DevicePitError::InvalidChannel { channel });
        }
        self.service_not_before = None;
        self.blocked = None;
        Ok(())
    }

    /// Raw register-backed Device PIT requests, independent of INTC/CPU masks.
    /// No acknowledgement, timing, SR mutation or delivery occurs.
    pub fn device_pit_requests(&self) -> Result<Vec<u16>, DevicePitError> {
        if self.policy != TimerPolicy::Device {
            return Err(DevicePitError::DevicePolicyRequired);
        }
        Ok(self.pit.device_pending_vectors())
    }

    /// Supply a Device DTIM reference event, without timing or delivery.
    /// Policy/channel/unsupported-mode errors precede register/cache mutation.
    pub fn device_dtim_reference(&mut self, channel: usize) -> Result<(), DeviceDtimError> {
        if self.policy != TimerPolicy::Device {
            return Err(DeviceDtimError::DevicePolicyRequired);
        }
        self.dtim.device_latch_reference(channel)?;
        self.service_not_before = None;
        self.blocked = None;
        Ok(())
    }

    /// Supply completed post-prescaler ticks to an ordinary Device DTIM.
    /// Updates existing DTCN/REF registers only; no delivery, SR changes, host
    /// writes, clock conversion or automatic scheduling. Errors precede mutation.
    pub fn device_dtim_advance_ticks(
        &mut self,
        channel: usize,
        ticks: u64,
    ) -> Result<(), DeviceDtimError> {
        if self.policy != TimerPolicy::Device {
            return Err(DeviceDtimError::DevicePolicyRequired);
        }
        self.dtim.device_advance_ticks(channel, ticks)?;
        self.service_not_before = None;
        self.blocked = None;
        Ok(())
    }

    /// Raw Device DTIM reference-only CPU requests; capture delivery, DMA
    /// execution and timing remain outside this component. The supplied requests
    /// participate in Machine::step_device_events, not storage-only routes.
    pub fn device_dtim_reference_requests(&self) -> Result<Vec<u16>, DeviceDtimError> {
        if self.policy != TimerPolicy::Device {
            return Err(DeviceDtimError::DevicePolicyRequired);
        }
        Ok(self.dtim.device_reference_vectors()?)
    }

    /// Write a PIT or INTC register. Device PIT supports aligned 16-bit
    /// PCSR/PMR writes only, with PIF acknowledgement; Oracle stays passthrough.
    /// Device DTIM accepts only its bounded reference-mode configuration and W1C.
    /// Device SIMR/CIMR byte commands support all three banks and sources;
    /// reserved-bit, wrong-width and overlapping accesses reject before mutation.
    /// Other INTC accesses retain existing passthrough behavior; this does not
    /// add controller level masking or a whole-machine arbiter.
    pub fn write(&mut self, addr: u32, size: u8, value: u32) -> bool {
        if self.policy == TimerPolicy::Device && DtimBank::owns(addr) {
            if !self.dtim.device_write(addr, size, value) {
                return false;
            }
            self.service_not_before = None;
            self.blocked = None;
            return true;
        }
        if self.policy == TimerPolicy::Device && PitBank::owns(addr) {
            if !self.pit.device_write(addr, size, value) {
                return false;
            }
            self.service_not_before = None;
            self.blocked = None;
            return true;
        }
        if self.policy == TimerPolicy::Device && IntcBank::hw_touches_simr_cimr(addr, size) {
            if size != 1 || !self.intc.hw_apply_simr_cimr(addr, value as u8) {
                return false;
            }
            self.service_not_before = None;
            self.blocked = None;
            return true;
        }
        self.service_not_before = None;
        self.blocked = None;
        Self::owns_access(addr, size)
            && (self.pit.write(addr, size, value)
                || self.dtim.write(addr, size, value)
                || self.intc.write(addr, size, value))
    }

    /// Load one complete PIT-channel or INTC register page.
    pub fn load_page(&mut self, base: u32, data: &[u8]) -> bool {
        self.service_not_before = None;
        self.blocked = None;
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
            || self.forced_candidate().is_some()
    }

    /// A software-forced INTC source the CPU could take at its current IPL.
    /// Idle fast-forward must not cross it: service_with would deliver it at
    /// the end of the skipped interval.
    pub fn forced_deliverable(&self) -> bool {
        self.forced_candidate()
            .is_some_and(|(_, level)| self.sr.ipl() < level)
    }

    /// Same boundary cache as service_with, including queued host writes.
    /// Owners may avoid transferring the timer facade when it has no work.
    pub fn can_skip_service(&self, done: u64) -> bool {
        self.policy == TimerPolicy::Oracle && self.host_writes.is_empty() && self.idle_at(done)
    }

    /// For a host that skips `can_skip_service` at every instruction: while the
    /// guest tick `done` stays below `limit` and the CPU's IPL (SR bits 10-8)
    /// is at least `level`, servicing changes nothing. `None` when the timers
    /// need an offer at every boundary (a pending IRQ the CPU could take,
    /// unknown deadlines, queued host writes, or the Device policy).
    pub fn idle_window(&self) -> Option<(u64, u8)> {
        if self.policy != TimerPolicy::Oracle || !self.host_writes.is_empty() {
            return None;
        }
        let (next, level) = match (self.service_not_before, self.blocked) {
            (Some(next), _) => (next, 0),
            (None, Some((next, level))) => (next, level),
            (None, None) => return None,
        };
        // `(done as f64) < next` for integer `done` below 2^53.
        if next == f64::INFINITY {
            return Some((u64::MAX, level)); // no timer is armed
        }
        (next >= 0.0 && next < 9.0e15).then(|| (next.ceil() as u64, level))
    }

    /// `service_with(done, _)` is known to change nothing.
    #[inline]
    fn idle_at(&self, done: u64) -> bool {
        let donef = done as f64;
        self.service_not_before.is_some_and(|next| donef < next)
            || self
                .blocked
                .is_some_and(|(next, level)| donef < next && self.sr.ipl() >= level)
    }

    /// Return the first PIT/DTIM deadline, arming enabled timers at `done`.
    pub fn deadline(&mut self, done: u64) -> Option<u64> {
        self.service_not_before = None;
        self.blocked = None;
        match (self.pit.deadline(done), self.dtim.deadline(done)) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Advance both Oracle timer banks and collect raw pending vectors, without
    /// delivery, INTC/CPU masking, SR changes or consumption. PIT vectors precede
    /// DTIM vectors in each bank's configured order; this is not arbitration.
    /// DTIM REF host writes remain queued for the CPU owner. This preserves the
    /// Oracle scheduling model, not validated Device PIF/REF semantics.
    pub fn collect_oracle_timer_requests(&mut self, done: u64) -> Result<Vec<u16>, TimeError> {
        if self.policy == TimerPolicy::Device {
            return Err(TimeError::DeviceInterruptDeliveryUnsupported);
        }
        self.service_not_before = None;
        self.blocked = None;
        self.pit.advance_pending(done);
        let writes = self.dtim.advance_pending(done);
        self.host_writes.extend(
            writes
                .into_iter()
                .map(|(addr, byte)| HostWrite { addr, byte }),
        );
        let mut requests = self.pit.pending_vectors();
        requests.extend(self.dtim.pending_vectors());
        Ok(requests)
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
        if self.idle_at(done) {
            return Ok(Vec::new());
        }
        let mut raised = self
            .pit
            .service_with(done, &self.intc, &mut self.sr, &mut offer);
        let (dtim_raised, writes) =
            self.dtim
                .service_with(done, &self.intc, &mut self.sr, &mut offer);
        raised.extend(dtim_raised);
        // Software-forced INTC sources (guest writes to INTFRCH/INTFRCL, e.g.
        // the sequencer step countdown): level-triggered until the guest
        // clears the bit, so offered only above the live IPL.
        let forced = self.forced_candidate();
        if let Some((vector, level)) = forced
            && self.sr.ipl() < level
            && offer(vector, level)
        {
            self.sr.on_taken(Some(level), 0);
            raised.push((vector, level));
        }
        self.host_writes.extend(
            writes
                .into_iter()
                .map(|(addr, byte)| HostWrite { addr, byte }),
        );
        // A refused IRQ must be retried at every instruction: an SR change
        // can unmask it even before the next timer tick. MMIO writes, page
        // loads, checkpoint restores and deadline arming invalidate this
        // cache. With no pending IRQ, only the earliest tick can do work.
        let any_pending =
            (0..4).any(|i| self.pit.pending(i) || self.dtim.pending(i)) || forced.is_some();
        self.blocked = if any_pending && !self.pit.is_held() && !self.dtim.is_held() {
            let level = self
                .pit
                .max_pending_level(&self.intc)
                .max(self.dtim.max_pending_level(&self.intc))
                .max(forced.map_or(0, |(_, level)| level));
            // Pending channels that were just declined by IPL stay declined
            // while IPL >= every pending level; one declined by the offer
            // callback (IPL below its level) must be retried at each boundary.
            (self.sr.ipl() >= level)
                .then(|| {
                    (0..4)
                        .flat_map(|i| [self.pit.next_deadline(i), self.dtim.next_deadline(i)])
                        .flatten()
                        .try_fold(f64::INFINITY, |next, deadline| {
                            (!deadline.is_nan()).then(|| next.min(deadline))
                        })
                })
                .flatten()
                .map(|next| (next, level))
        } else {
            None
        };
        self.service_not_before = if any_pending {
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
    fn rejected_device_mask_access_preserves_service_caches() {
        let mut time = Time::new(TimerPolicy::Device, vec![], pit::F_BUS);
        time.service_not_before = Some(123.0);
        time.blocked = Some((456.0, 3));
        for base in intc::BASES {
            for (offset, size, value) in [(0x1c, 1, 0x80), (0x1d, 2, 0), (0x1b, 4, 0)] {
                assert!(!time.write(base + offset, size, value));
                assert_eq!(time.service_not_before, Some(123.0));
                assert_eq!(time.blocked, Some((456.0, 3)));
            }
        }
    }

    #[test]
    fn cached_wait_preserves_undrained_dtim_reference_writes() {
        let mut time = Time::with_dtims(TimerPolicy::Oracle, vec![], vec![0], dtim::F_BUS / 3.0);
        time.write(dtim::BASES[0], 2, 0x001b);
        time.write(dtim::BASES[0] + 4, 4, 30);
        level(&mut time, dtim::VECTORS[0], 1);
        time.seed_sr(0x2000);
        assert_eq!(time.service(0), Ok(vec![]));
        let due = time.deadline(0).unwrap();
        assert_eq!(time.service(due), Ok(vec![(dtim::VECTORS[0], 1)]));
        assert!(!time.can_skip_service(due));
        let writes = time.take_host_writes();
        assert_eq!(writes.len(), 1);
        assert_eq!((writes[0].addr, writes[0].byte), (dtim::BASES[0] + 3, 2));
        assert!(time.can_skip_service(due));
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
            let mut blocked_idle = 0;
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
                reference.blocked = None;
                let offer = |_, _| offset % 11 != 0;
                // The host-batching window must agree with the reference: if
                // it says "idle" here, servicing may change nothing.
                let ipl: u8 = if offset % 37 < 9 { 7 } else { 0 };
                let window_idle = fast
                    .idle_window()
                    .is_some_and(|(limit, level)| done < limit && ipl >= level);
                let actual = if fast.can_skip_service(done) {
                    Ok(Vec::new())
                } else {
                    fast.service_with(done, offer)
                };
                let expected = reference.service_with(done, offer);
                if window_idle {
                    assert_eq!(expected, Ok(Vec::new()), "offset {offset}");
                    blocked_idle += usize::from(fast.blocked.is_some());
                }
                assert_eq!(actual, expected);
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
            // Beyond 2^53 the integer deadline window is deliberately unavailable.
            if start == 0 {
                assert!(
                    blocked_idle > 0,
                    "the pending-IRQ window was never exercised"
                );
            }
        }
    }
}
