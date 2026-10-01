//! Verified MAIN reset loop. Only complete, non-final iterations are batched.
use crate::common::{MAIN_LOAD, hex, unique};

const SIGNATURE: &str = "4feffff048d700f0207c40312000223c47e284709288e881428442854286428748d000f041e80010538166f44cd700f04fef00104e75";

#[derive(Clone, Copy)]
pub(crate) struct RamClear {
    pub entry: u32,
    pub loop_pc: u32,
    pub start: u32,
    pub end: u32,
    pub code: [u8; 54],
}

impl RamClear {
    pub fn resolve(main: &[u8]) -> Option<Self> {
        let bytes = hex(SIGNATURE);
        let mut mask = vec![false; bytes.len()];
        mask[10..14].fill(true);
        mask[16..20].fill(true);
        let entry = unique(main, &bytes, &mask).ok()?;
        let at = (entry - MAIN_LOAD) as usize;
        let code: [u8; 54] = main.get(at..at + 54)?.try_into().ok()?;
        let start = u32::from_be_bytes(code[10..14].try_into().ok()?);
        let end = u32::from_be_bytes(code[16..20].try_into().ok()?);
        // These are the SDRAM bounds already supported by the board. The
        // arithmetic shift in the prelude requires a positive signed length.
        if entry & 1 != 0
            || start < 0x4000_0000
            || end > 0x4800_0000
            || start >= end
            || start & 15 != 0
            || end & 15 != 0
            || entry + 54 > start
        {
            return None;
        }
        Some(Self {
            entry,
            loop_pc: entry + 32,
            start,
            end,
            code,
        })
    }

    pub fn iterations(self, a0: u32, d1: u32, budget: u32) -> u32 {
        if a0 < self.start || a0 >= self.end || a0 & 15 != 0 || d1 != (self.end - a0) / 16 {
            return 0;
        }
        // Never allocate pages ahead of the original MOVEM. Keep the final
        // decrement/branch on the interpreter, including its exact CCR.
        let page_bytes = 0x10_0000 - (a0 & 0x0f_ffff);
        (budget / 4).min(d1.saturating_sub(1)).min(page_bytes / 16)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resolver_requires_the_complete_loop_and_valid_sdram_bounds() {
        let bytes = hex(SIGNATURE);
        let good = RamClear::resolve(&bytes).unwrap();
        assert_eq!((good.start, good.end), (0x4031_2000, 0x47e2_8470));
        for offset in [0, 24, 32, 43, 53] {
            let mut broken = bytes.clone();
            broken[offset] ^= 1;
            assert!(RamClear::resolve(&broken).is_none());
        }
        let mut duplicate = bytes.clone();
        duplicate.extend_from_slice(&bytes);
        assert!(RamClear::resolve(&duplicate).is_none());
        for start in [0x3fff_fff0u32, 0x4031_2001, 0x47e2_8470] {
            let mut broken = bytes.clone();
            broken[10..14].copy_from_slice(&start.to_be_bytes());
            assert!(RamClear::resolve(&broken).is_none());
        }
    }
    #[test]
    fn batch_stops_at_budget_page_boundary_and_before_final_iteration() {
        let clear = RamClear::resolve(&hex(SIGNATURE)).unwrap();
        let a0 = clear.end - 64;
        assert_eq!(clear.iterations(a0, 4, 100), 3);
        assert_eq!(clear.iterations(a0, 4, 7), 1);
        assert_eq!(clear.iterations(a0, 4, 3), 0);
        assert_eq!(clear.iterations(a0, 3, 100), 0);
        assert_eq!(clear.iterations(a0 + 1, 4, 100), 0);
        let a0 = 0x403f_fff0;
        assert_eq!(clear.iterations(a0, (clear.end - a0) / 16, 100), 1);
        assert_eq!(clear.iterations(clear.end - 16, 1, 100), 0);
    }
}
