//! Health observations for the event stream: a guest fault, a halted DSP and
//! a stalled main loop. The logic is pure (no runtime access) so it is
//! unit-testable; the controller feeds it one [`Observation`] per chunk.
//!
//! The heartbeat is the number of passes through the resolved main-loop PC
//! (`Status::mainloop_hits`). A stall is the heartbeat standing still for
//! `stall_ms` of emulated time while the CPU runs and the firmware is past
//! boot. Events are emitted once per episode (a stall ends when the
//! heartbeat advances again, which emits `stall.end`).

/// Emulated ColdFire instructions per millisecond (132,000 unless `DIGI_CF_IPS` is set).
pub fn instr_per_ms() -> u64 {
    elektron_native_boot::clock::cf_ips() / 1000
}
/// Default stall threshold in emulated milliseconds.
pub const DEFAULT_STALL_MS: u64 = 1000;

#[derive(Clone, Debug, Default)]
pub struct Observation {
    /// Controller clock: ColdFire instructions since the session began.
    pub elapsed: u64,
    pub hits: u64,
    /// The firmware reached its main panel (the heartbeat is meaningful).
    pub main_ui: bool,
    /// The CPU is in the running state (not STOP, not halted).
    pub cpu_running: bool,
    pub error: Option<String>,
    pub dsp_halted: Option<String>,
    /// Underrun events of the audio sink since the session began, when it was read this chunk.
    pub sink_underruns: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Alert {
    Fault(String),
    Halt(String),
    Stall {
        hits: u64,
        stalled_ms: u64,
    },
    StallEnd {
        hits: u64,
    },
    /// The audio sink ran dry `new` times since the last observation; `total` since the session began.
    Underrun {
        new: u64,
        total: u64,
    },
}

#[derive(Clone, Debug)]
pub struct Health {
    pub stall_ms: u64,
    epoch: u64,
    last_hits: u64,
    last_advance: u64,
    stalled: bool,
    fault_sent: bool,
    halt_sent: bool,
    /// Sink underruns since the runtime began.
    pub underrun_count: u64,
    /// Stall episodes since the runtime began (events can arrive after a reply; this cannot).
    pub stall_count: u64,
}

impl Health {
    pub fn new(stall_ms: u64) -> Self {
        Self {
            stall_ms,
            epoch: u64::MAX,
            last_hits: 0,
            last_advance: 0,
            stalled: false,
            fault_sent: false,
            halt_sent: false,
            underrun_count: 0,
            stall_count: 0,
        }
    }

    pub fn stalled(&self) -> bool {
        self.stalled
    }

    /// Forget everything when the runtime is another one (`epoch` changed).
    pub fn observe(&mut self, epoch: u64, obs: &Observation) -> Vec<Alert> {
        if epoch != self.epoch {
            *self = Self::new(self.stall_ms);
            self.epoch = epoch;
            self.last_advance = obs.elapsed;
            self.last_hits = obs.hits;
        }
        let mut alerts = Vec::new();
        if let Some(error) = &obs.error
            && !self.fault_sent
        {
            self.fault_sent = true;
            alerts.push(Alert::Fault(error.clone()));
        }
        if let Some(halt) = &obs.dsp_halted
            && !self.halt_sent
        {
            self.halt_sent = true;
            alerts.push(Alert::Halt(halt.clone()));
        }
        if let Some(total) = obs.sink_underruns {
            if total > self.underrun_count {
                alerts.push(Alert::Underrun {
                    new: total - self.underrun_count,
                    total,
                });
            }
            self.underrun_count = total;
        }
        // A restore can move the clock back: re-anchor.
        if obs.elapsed < self.last_advance || obs.hits < self.last_hits {
            self.last_advance = obs.elapsed;
            self.last_hits = obs.hits;
        }
        if obs.hits != self.last_hits {
            self.last_hits = obs.hits;
            self.last_advance = obs.elapsed;
            if self.stalled {
                self.stalled = false;
                alerts.push(Alert::StallEnd { hits: obs.hits });
            }
        } else if !obs.main_ui || !obs.cpu_running || obs.error.is_some() {
            // Not expected to beat: keep the anchor current.
            self.last_advance = obs.elapsed;
        } else if self.stall_ms > 0 && !self.stalled {
            let ms = (obs.elapsed - self.last_advance) / instr_per_ms();
            if ms >= self.stall_ms {
                self.stalled = true;
                self.stall_count += 1;
                alerts.push(Alert::Stall {
                    hits: obs.hits,
                    stalled_ms: ms,
                });
            }
        }
        alerts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(elapsed_ms: u64, hits: u64) -> Observation {
        Observation {
            elapsed: elapsed_ms * instr_per_ms(),
            hits,
            main_ui: true,
            cpu_running: true,
            ..Default::default()
        }
    }

    #[test]
    fn a_beating_heart_is_quiet() {
        let mut h = Health::new(100);
        for ms in 0..1000 {
            assert!(h.observe(1, &obs(ms * 10, ms)).is_empty());
        }
    }

    #[test]
    fn a_stall_fires_once_and_ends_when_the_heart_beats() {
        let mut h = Health::new(100);
        assert!(h.observe(1, &obs(0, 5)).is_empty());
        assert!(h.observe(1, &obs(99, 5)).is_empty());
        let a = h.observe(1, &obs(100, 5));
        assert_eq!(
            a,
            vec![Alert::Stall {
                hits: 5,
                stalled_ms: 100
            }]
        );
        assert!(h.stalled());
        assert_eq!(h.stall_count, 1);
        assert!(h.observe(1, &obs(500, 5)).is_empty(), "once per episode");
        assert_eq!(
            h.observe(1, &obs(501, 6)),
            vec![Alert::StallEnd { hits: 6 }]
        );
        assert!(!h.stalled());
        // and a second episode fires again
        assert!(h.observe(1, &obs(550, 6)).is_empty());
        assert_eq!(h.observe(1, &obs(601, 6)).len(), 1);
    }

    #[test]
    fn no_stall_before_boot_in_stop_or_when_disabled() {
        let mut h = Health::new(100);
        let mut o = obs(0, 0);
        o.main_ui = false;
        assert!(h.observe(1, &o).is_empty());
        o.elapsed = 10_000 * instr_per_ms();
        assert!(h.observe(1, &o).is_empty(), "booting");
        // reaching the main panel starts the clock then
        o.main_ui = true;
        assert!(h.observe(1, &o).is_empty());
        o.cpu_running = false;
        o.elapsed = 20_000 * instr_per_ms();
        assert!(h.observe(1, &o).is_empty(), "STOP waits for an interrupt");
        let mut off = Health::new(0);
        assert!(off.observe(1, &obs(0, 1)).is_empty());
        assert!(off.observe(1, &obs(100_000, 1)).is_empty());
    }

    #[test]
    fn fault_and_halt_fire_once_and_a_new_runtime_rearms() {
        let mut h = Health::new(100);
        let mut o = obs(0, 1);
        o.error = Some("bus error at 0x1".into());
        o.dsp_halted = Some("bad opcode".into());
        let a = h.observe(1, &o);
        assert_eq!(
            a,
            vec![
                Alert::Fault("bus error at 0x1".into()),
                Alert::Halt("bad opcode".into())
            ]
        );
        assert!(h.observe(1, &o).is_empty());
        assert_eq!(h.observe(2, &o).len(), 2, "new epoch");
    }

    #[test]
    fn underruns_are_reported_by_increase_and_a_new_runtime_rearms() {
        let mut h = Health::new(100);
        let mut o = obs(0, 1);
        o.sink_underruns = Some(0);
        assert!(h.observe(1, &o).is_empty());
        o.sink_underruns = None; // not read this chunk
        assert!(h.observe(1, &o).is_empty());
        o.sink_underruns = Some(3);
        assert_eq!(h.observe(1, &o), vec![Alert::Underrun { new: 3, total: 3 }]);
        assert!(h.observe(1, &o).is_empty(), "no increase, no event");
        o.sink_underruns = Some(4);
        assert_eq!(h.observe(1, &o), vec![Alert::Underrun { new: 1, total: 4 }]);
        // a new runtime starts counting from zero again
        o.sink_underruns = Some(1);
        assert_eq!(h.observe(2, &o), vec![Alert::Underrun { new: 1, total: 1 }]);
    }

    #[test]
    fn a_clock_moving_back_reanchors() {
        let mut h = Health::new(100);
        assert!(h.observe(1, &obs(5000, 9)).is_empty());
        assert!(h.observe(1, &obs(10, 3)).is_empty());
        assert!(h.observe(1, &obs(50, 3)).is_empty());
        assert_eq!(h.observe(1, &obs(110, 3)).len(), 1);
    }
}
