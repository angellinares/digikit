//! The `0x8C00_0000` FlexBus-attached coprocessor port's ready line -- a
//! direct port of `emu/dsp.py`'s `Fifo`. See that module's docstring for
//! the hardware reasoning (the firmware's `0x400cf4a8`/`0x400cfd40`
//! transfer primitives, the ready-bit poll at `0x400cf4ec`).
//!
//! Only the ready line is modelled, exactly as the Python oracle models it:
//! a 16-bit read of `STATUS` (`0x8C00_0002`) increments a poll counter and
//! writes back `READY` (bit 0) once `polls > poll_delay` -- with the
//! recorded traces' `poll_delay=0` default (`emu.longrun.build`'s
//! `install_dsp(m, ev, log_path=dsp_log)` never overrides it), `polls` is
//! always `>= 1` immediately after the read hook's own increment, so
//! **every** `STATUS` read returns `READY` unconditionally; the poll
//! counter only ever matters for a nonzero `poll_delay`, a live-machine
//! configuration no trace here exercises. A `STATUS`/`LATCH` write is
//! counted (`words`/`bursts`, for reporting) and otherwise discarded, same
//! as the oracle: nothing reads either byte back.

pub const BASE: u32 = 0x8C000000;
pub const STATUS: u32 = BASE + 0x02;
pub const LATCH: u32 = BASE + 0x0A;
pub const READY: u16 = 0x0001;
pub const STROBE: u16 = 0x0080;

pub struct Fifo {
    pub poll_delay: u64,
    pub polls: u64,
    pub words: u64,
    pub bursts: u64,
}

impl Default for Fifo {
    fn default() -> Self {
        Self::new(0)
    }
}

impl Fifo {
    pub fn new(poll_delay: u64) -> Self {
        Self {
            poll_delay,
            polls: 0,
            words: 0,
            bursts: 0,
        }
    }

    pub fn owns(addr: u32) -> bool {
        (STATUS..STATUS + 2).contains(&addr) || (LATCH..LATCH + 2).contains(&addr)
    }

    /// A guest read of `STATUS` (any other address in this bank's range is
    /// plain RAM, matching the oracle -- nothing else here is modelled).
    /// -> `Some(ready_value)` -- also the value the host echo-write puts
    /// back into `STATUS` (`on_read`'s `uc.mem_write`, `emu/dsp.py`), which
    /// a trace records as an `HWR` from `emu.dsp.Fifo.__init__.<locals>.on_read`.
    pub fn read_status(&mut self, addr: u32) -> Option<u16> {
        if !(STATUS..STATUS + 2).contains(&addr) {
            return None;
        }
        self.polls += 1;
        Some(if self.polls > self.poll_delay {
            READY
        } else {
            0
        })
    }

    /// A guest write anywhere in this bank's range: bookkeeping only, no
    /// guest-visible effect (`emu/dsp.py`'s `on_write`/`on_latch`).
    pub fn write(&mut self, addr: u32) -> bool {
        if (STATUS..STATUS + 2).contains(&addr) {
            self.words += 1;
            self.polls = 0;
            true
        } else if (LATCH..LATCH + 2).contains(&addr) {
            self.bursts += 1;
            true
        } else {
            false
        }
    }
}

impl Fifo {
    pub fn snap_save(&self, w: &mut crate::snap::Writer) {
        w.u64(self.polls);
        w.u64(self.words);
        w.u64(self.bursts);
    }
    pub fn snap_load(&mut self, r: &mut crate::snap::Reader) -> crate::snap::Result<()> {
        self.polls = r.u64()?;
        self.words = r.u64()?;
        self.bursts = r.u64()?;
        Ok(())
    }
}
