//! Reconstructing the guest's live status register for replay-time IPL
//! (interrupt priority level) checks.
//!
//! `PitBank`/`DtimBank::service` need the CPU's current IPL to decide
//! whether a pending, unmasked source may actually be delivered right now
//! (`emu.pit.deliver_pending`: `if ((sr >> 8) & 7) >= level: continue`). A
//! peripheral-only crate has no CPU of its own to ask, so this tracks SR the
//! way `emu.harness.Machine.raise_vector` changes it:
//!
//! * on a taken interrupt with a level, SR gets the supervisor bit set and
//!   its IPL field replaced by that level;
//! * on a taken exception with no level (a `trap #N`; ColdFire's `trap` does
//!   not touch the interrupt mask), only the supervisor bit changes;
//! * `rte` restores the exact SR that was pushed to the exception frame.
//!
//! Both formulas are exact copies of `raise_vector`'s two paths
//! (`emu/harness.py`, the non-srtrap branch and the srtrap trampoline's
//! `andi.l`/`ori.l` pair produce the same live SR either way).
//!
//! The one gap: a guest `move.w #x,sr` is not a memory access, so it is
//! invisible to the MMIO recorder and this tracker cannot see it. The trace
//! closes that gap almost entirely on its own: every taken `IRQ` record
//! carries `frame_sr`, the interrupted SR the frame held, which is exactly
//! "what SR was, right before this delivery" -- ground truth -- and every
//! `RTE` record carries the exact value execution resumes with. This tracker
//! resyncs to both whenever they are available (`frame_sr` is zero only when
//! a `srtrap` trampoline fills it in later than the recorder can see it), so
//! drift from an untracked direct SR write can only matter in the narrow gap
//! between one resync point and the next -- and if it ever changed a real
//! delivery decision, the following resync would show it as a mismatch, not
//! silently pass.
pub struct SrTracker {
    /// Bit 13 (0x2000): supervisor. Bits 8-10: IPL (0-7).
    current: u16,
}

pub const S_BIT: u16 = 0x2000;
pub const IPL_MASK: u16 = 0x0700;

impl Default for SrTracker {
    fn default() -> Self {
        // No CPU register snapshot is available at a replay window's start
        // (checkpoints keep peripheral/model state, not raw CPU regs); the
        // real value simply is not knowable from the trace until the first
        // taken interrupt or RTE resyncs it. Supervisor, IPL 0, matches every
        // window's usual resting state (RTOS tasks run unmasked between
        // ticks) and is quickly overwritten by the first resync either way.
        Self { current: S_BIT }
    }
}

impl SrTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn ipl(&self) -> u8 {
        ((self.current & IPL_MASK) >> 8) as u8
    }

    pub fn seed(&mut self, sr: u16) {
        self.current = sr;
    }

    /// Apply a taken interrupt/exception, mirroring `raise_vector`.
    /// `frame_sr`, when nonzero, is the trace's own ground truth for what SR
    /// was immediately before this delivery, and resyncs the tracker.
    pub fn on_taken(&mut self, level: Option<u8>, frame_sr: u16) {
        let before = if frame_sr != 0 {
            frame_sr
        } else {
            self.current
        };
        let mut live = before | S_BIT;
        if let Some(l) = level {
            live = (live & !IPL_MASK) | (((l & 7) as u16) << 8);
        }
        self.current = live;
    }

    /// Apply an `rte`: the trace gives the exact restored SR directly.
    pub fn on_rte(&mut self, sr: u16) {
        self.current = sr;
    }
}
