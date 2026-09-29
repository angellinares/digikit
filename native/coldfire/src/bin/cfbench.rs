//! Interpreter speed floor: a straight-line hot loop, no decode caching, no
//! JIT (P3 stage 2 plan item 4). `cfbench [ITERATIONS]` (default 200M).
//!
//! The loop is `subq.l #1,d0 ; bne.b loop` (CFPRM p.144,81-82): two
//! instructions, one taken branch, repeated ITERATIONS times, which is
//! about as friendly to this interpreter as ColdFire code gets (no memory
//! operands beyond fetch, no EMAC, no exception). Real firmware will be
//! slower than this number, not faster.

use coldfire::{Bus, BusError, Cpu};
use std::time::Instant;

struct Ram(Vec<u8>);

impl Bus for Ram {
    fn read8(&mut self, a: u32) -> Result<u8, BusError> {
        Ok(self.0[a as usize])
    }
    fn read16(&mut self, a: u32) -> Result<u16, BusError> {
        let i = a as usize;
        Ok(u16::from_be_bytes([self.0[i], self.0[i + 1]]))
    }
    fn read32(&mut self, a: u32) -> Result<u32, BusError> {
        let i = a as usize;
        Ok(u32::from_be_bytes(self.0[i..i + 4].try_into().unwrap()))
    }
    fn write8(&mut self, a: u32, v: u8) -> Result<(), BusError> {
        self.0[a as usize] = v;
        Ok(())
    }
    fn write16(&mut self, a: u32, v: u16) -> Result<(), BusError> {
        let i = a as usize;
        self.0[i..i + 2].copy_from_slice(&v.to_be_bytes());
        Ok(())
    }
    fn write32(&mut self, a: u32, v: u32) -> Result<(), BusError> {
        let i = a as usize;
        self.0[i..i + 4].copy_from_slice(&v.to_be_bytes());
        Ok(())
    }
}

fn main() {
    let iters: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(200_000_000);

    let mut ram = Ram(vec![0; 0x10000]);
    ram.write32(0, 0x8000).unwrap();
    ram.write32(4, 0x1000).unwrap();
    // 0x1000: subq.l #1,d0 (0x5380) ; bne.b -4 (0x66fc)
    ram.write16(0x1000, 0x5380).unwrap();
    ram.write16(0x1002, 0x66fc).unwrap();

    let mut cpu = Cpu::new();
    cpu.reset(&mut ram).unwrap();
    cpu.d[0] = iters as u32;

    let start = Instant::now();
    // Each loop trip is 2 instructions (SUBQ, BNE); stop once D0 reaches 0
    // and the branch falls through (icount is the ground truth either way).
    while cpu.d[0] != 0 {
        cpu.step(&mut ram).unwrap();
        cpu.step(&mut ram).unwrap();
    }
    let elapsed = start.elapsed();
    let n = cpu.icount;
    let ns_per = elapsed.as_secs_f64() * 1e9 / n as f64;
    println!(
        "{} instructions in {:.3}s: {:.2} ns/instruction, {:.1}M instructions/s",
        n,
        elapsed.as_secs_f64(),
        ns_per,
        1000.0 / ns_per
    );
}
