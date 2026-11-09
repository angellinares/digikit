//! Interrupts at arbitrary instruction boundaries (diagnostic, opt-in): `SHARC_IRQ_PROBE=LO-HI[,LO-HI...]:MASK[:EVERY]` (hex software PCs,
//! an IRPTL bit mask, an optional decimal divisor).
//!
//! The coupled model delivers the DSP's peripheral events (SPI2 and SPORT DMA completions) between runs of the core, when it is idle or at
//! a fixed phase, so a nested interrupt never lands inside a short routine; on silicon it can land between any two instructions. The probe
//! latches the IRPTL bits MASK just before the interpreter executes an instruction whose PC is in [LO, HI), once for each visit of that
//! PC (after the interrupt's return to the same PC it moves on), for every EVERY-th visit. The core's own entry rules then decide (IMASK,
//! IRPTEN, NESTM, delayed-branch slots, loop ends), exactly as for the core timer and software interrupts, and the firmware's handler
//! runs. A bit that was not taken stays latched and is taken at the next boundary that allows it.
//!
//! Use `0x400000` (TMZHI, the kernel tick) or `0x80000000` (SFT3I, the kernel's context switch) or both; the code window is the
//! patched code under test (appended code runs in the interpreter, so every boundary is visited). Not part of any proof tier.

pub struct IrqProbe {
    pub windows: Vec<(u32, u32)>,
    pub mask: u32,
    pub every: u64,
    last: i64,
    visits: u64,
    pub latched: u64,
}

impl IrqProbe {
    pub fn from_env() -> Option<IrqProbe> {
        let spec = std::env::var("SHARC_IRQ_PROBE").ok()?;
        let mut it = spec.split(':');
        let hex = |s: &str| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok();
        let windows: Option<Vec<(u32, u32)>> = it
            .next()
            .unwrap_or("")
            .split(',')
            .map(|w| {
                let (lo, hi) = w.split_once('-')?;
                Some((hex(lo)?, hex(hi)?))
            })
            .collect();
        let mask = it.next().and_then(hex);
        let (Some(windows), Some(mask)) = (windows, mask) else {
            eprintln!("SHARC_IRQ_PROBE: want LO-HI[,LO-HI...]:MASK[:EVERY] in hex; probe disabled");
            return None;
        };
        let every = it
            .next()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(1)
            .max(1);
        Some(IrqProbe {
            windows,
            mask,
            every,
            last: -1,
            visits: 0,
            latched: 0,
        })
    }

    /// Whether to latch the probe's bits before the instruction at PC.
    pub fn due(&mut self, pc: u32) -> bool {
        if !self.windows.iter().any(|&(lo, hi)| pc >= lo && pc < hi) || self.last == pc as i64 {
            return false;
        }
        self.last = pc as i64;
        self.visits += 1;
        if self.visits % self.every != 0 {
            return false;
        }
        self.latched += 1;
        true
    }
}
