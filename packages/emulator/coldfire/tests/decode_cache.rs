//! The pre-decode cache clears what a CPU write overlaps and nothing else: a counter in the page of its own loop leaves the loop cached
//! (the digikit health module's idle loop), a write into an instruction, into the extension words of a 6-byte one, or across a page
//! boundary into the tail of one, makes the next step decode the new bytes.
use coldfire::{Bus, BusError, Cpu};

struct Ram(Vec<u8>);

impl Ram {
    fn new() -> Self {
        Self(vec![0; 0x10000])
    }
    fn range(&self, addr: u32, size: usize, write: bool) -> Result<usize, BusError> {
        let p = addr as usize;
        if p.checked_add(size).is_none_or(|end| end > self.0.len()) {
            return Err(BusError { addr, write });
        }
        Ok(p)
    }
}

impl Bus for Ram {
    fn read8(&mut self, a: u32) -> Result<u8, BusError> {
        Ok(self.0[self.range(a, 1, false)?])
    }
    fn read16(&mut self, a: u32) -> Result<u16, BusError> {
        let p = self.range(a, 2, false)?;
        Ok(u16::from_be_bytes(self.0[p..p + 2].try_into().unwrap()))
    }
    fn read32(&mut self, a: u32) -> Result<u32, BusError> {
        let p = self.range(a, 4, false)?;
        Ok(u32::from_be_bytes(self.0[p..p + 4].try_into().unwrap()))
    }
    fn write8(&mut self, a: u32, v: u8) -> Result<(), BusError> {
        let p = self.range(a, 1, true)?;
        self.0[p] = v;
        Ok(())
    }
    fn write16(&mut self, a: u32, v: u16) -> Result<(), BusError> {
        let p = self.range(a, 2, true)?;
        self.0[p..p + 2].copy_from_slice(&v.to_be_bytes());
        Ok(())
    }
    fn write32(&mut self, a: u32, v: u32) -> Result<(), BusError> {
        let p = self.range(a, 4, true)?;
        self.0[p..p + 4].copy_from_slice(&v.to_be_bytes());
        Ok(())
    }
}

/// `addq.l #1,(counter).l` (6 bytes) at `at`.
fn addq_abs(ram: &mut Ram, at: u32, counter: u32) {
    ram.write16(at, 0x52b9).unwrap();
    ram.write32(at + 2, counter).unwrap();
}

/// `move.l d1,(target).l` at `at`: a CPU write of d1 (4 bytes) to `target`.
fn store_d1(ram: &mut Ram, at: u32, target: u32) {
    ram.write16(at, 0x23c1).unwrap();
    ram.write32(at + 2, target).unwrap();
}

#[test]
fn a_counter_in_the_code_page_leaves_the_loop_cached() {
    let mut cpu = Cpu::new();
    let mut ram = Ram::new();
    addq_abs(&mut ram, 0x3400, 0x3800);
    ram.write16(0x3406, 0x60f8).unwrap(); // bra.s back to 0x3400
    cpu.pc = 0x3400;
    for _ in 0..10 {
        cpu.step(&mut ram).unwrap();
    }
    assert_eq!(ram.read32(0x3800).unwrap(), 5);
    // the counter is in the page of the loop and was written five times: the loop is still decoded
    assert!(cpu.decoded_at(0x3400) && cpu.decoded_at(0x3406));
    assert!(
        !cpu.decode_cached(0x3800, 4),
        "a write to the counter would clear no cached instruction"
    );
    assert!(cpu.decode_cached(0x3402, 2), "a write into the addq would");
}

#[test]
fn a_cpu_write_into_a_cached_instruction_is_seen() {
    let mut cpu = Cpu::new();
    let mut ram = Ram::new();
    ram.write16(0x3400, 0x7001).unwrap(); // moveq #1,d0
    cpu.pc = 0x3400;
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.d[0], 1);
    store_d1(&mut ram, 0x3000, 0x3400);
    cpu.d[1] = 0x7002_0000; // moveq #2,d0 at 0x3400
    cpu.pc = 0x3000;
    cpu.step(&mut ram).unwrap();
    assert!(
        !cpu.decoded_at(0x3400),
        "the overwritten instruction is not cached any more"
    );
    assert!(cpu.decoded_at(0x3000), "the writer in the same page is");
    cpu.pc = 0x3400;
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.d[0], 2);
}

#[test]
fn a_cpu_write_into_the_extension_words_of_an_instruction_is_seen() {
    let mut cpu = Cpu::new();
    let mut ram = Ram::new();
    addq_abs(&mut ram, 0x3400, 0x3800);
    cpu.pc = 0x3400;
    cpu.step(&mut ram).unwrap();
    assert_eq!(ram.read32(0x3800).unwrap(), 1);
    // the low word of the counter address, the last two bytes of the instruction
    store_d1(&mut ram, 0x3000, 0x3404);
    cpu.d[1] = 0x3900_0000;
    cpu.pc = 0x3000;
    cpu.step(&mut ram).unwrap();
    assert!(!cpu.decoded_at(0x3400));
    cpu.pc = 0x3400;
    cpu.step(&mut ram).unwrap();
    assert_eq!(ram.read32(0x3800).unwrap(), 1);
    assert_eq!(ram.read32(0x3900).unwrap(), 1);
}

#[test]
fn a_cpu_write_to_the_next_page_clears_the_tail_of_an_instruction_across_the_boundary() {
    let mut cpu = Cpu::new();
    let mut ram = Ram::new();
    addq_abs(&mut ram, 0x4ffc, 0x3800); // 0x4ffc..0x5002: its address word is on the next page
    cpu.pc = 0x4ffc;
    cpu.step(&mut ram).unwrap();
    assert_eq!(ram.read32(0x3800).unwrap(), 1);
    assert!(cpu.decoded_at(0x4ffc));
    store_d1(&mut ram, 0x3000, 0x5000);
    cpu.d[1] = 0x3900_0000;
    cpu.pc = 0x3000;
    cpu.step(&mut ram).unwrap();
    assert!(!cpu.decoded_at(0x4ffc));
    cpu.pc = 0x4ffc;
    cpu.step(&mut ram).unwrap();
    assert_eq!(ram.read32(0x3900).unwrap(), 1);
    assert_eq!(ram.read32(0x3800).unwrap(), 1);
}

#[test]
fn an_external_write_still_evicts_whole_pages() {
    let mut cpu = Cpu::new();
    let mut ram = Ram::new();
    ram.write16(0x3400, 0x7001).unwrap();
    ram.write16(0x3a00, 0x7003).unwrap();
    for pc in [0x3400, 0x3a00] {
        cpu.pc = pc;
        cpu.step(&mut ram).unwrap();
    }
    ram.write16(0x3400, 0x7002).unwrap();
    cpu.invalidate_external_write(0x3400, 2);
    assert!(!cpu.decoded_at(0x3400));
    assert!(
        !cpu.decoded_at(0x3a00),
        "the page is evicted, not only the written word"
    );
}
