use coldfire::{Bus, BusError, Cpu, InterruptPolicy, RunState};

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

#[test]
fn interrupt_frame_live_ipl_and_rte_restore() {
    let mut cpu = Cpu::new();
    let mut ram = Ram::new();
    cpu.ctrl.vbr = 0;
    cpu.pc = 0x3400;
    cpu.a[7] = 0x8000;
    cpu.sr = 0x2004;
    let vector = 208u8;
    ram.write32(4 * u32::from(vector), 0x3500).unwrap();
    ram.write16(0x3500, 0x4e73).unwrap(); // rte
    assert!(
        cpu.take_interrupt(&mut ram, vector, Some(3), InterruptPolicy::Oracle)
            .unwrap()
    );
    assert_eq!(cpu.pc, 0x3500);
    assert_eq!(cpu.sr, 0x2304);
    assert_eq!(cpu.a[7], 0x7ff8);
    assert_eq!(ram.read32(0x7ff8).unwrap(), 0x4340_2004);
    assert_eq!(ram.read32(0x7ffc).unwrap(), 0x3400);
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.pc, 0x3400);
    assert_eq!(cpu.sr, 0x2004);
    assert_eq!(cpu.a[7], 0x8000);
}

#[test]
fn refused_interrupt_does_not_push_frame() {
    let mut cpu = Cpu::new();
    let mut ram = Ram::new();
    cpu.pc = 0x3400;
    cpu.a[7] = 0x8000;
    cpu.sr = 0x2004;
    assert!(
        !cpu.take_interrupt(&mut ram, 208, Some(3), InterruptPolicy::Oracle)
            .unwrap()
    );
    assert_eq!((cpu.pc, cpu.a[7], cpu.sr), (0x3400, 0x8000, 0x2004));
    ram.write32(4 * 208, 0x4800_0000).unwrap();
    assert!(
        !cpu.take_interrupt(&mut ram, 208, Some(3), InterruptPolicy::Oracle)
            .unwrap()
    );
    assert_eq!(cpu.state, RunState::Running);

    ram.write32(4 * 208, 0).unwrap();
    assert!(
        cpu.take_interrupt(&mut ram, 208, Some(3), InterruptPolicy::Device)
            .unwrap()
    );
    assert_eq!(cpu.pc, 0);
    assert_eq!(cpu.a[7], 0x7ff8);
}

#[test]
fn external_dma_write_invalidates_old_decoded_instruction() {
    let mut cpu = Cpu::new();
    let mut ram = Ram::new();
    cpu.pc = 0x3400;
    ram.write16(0x3400, 0x7001).unwrap(); // moveq #1,D0
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.d[0], 1);
    ram.write16(0x3400, 0x7002).unwrap(); // external DMA to executable RAM
    cpu.invalidate_external_write(0x3400, 2);
    cpu.pc = 0x3400;
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.d[0], 2);

    // A large DMA invalidates interior pages too, not just the endpoints.
    for pc in [0x4400, 0x5400] {
        ram.write16(pc, 0x7001).unwrap();
        cpu.pc = pc;
        cpu.step(&mut ram).unwrap();
    }
    ram.write16(0x4400, 0x7007).unwrap();
    cpu.invalidate_external_write(0x3400, 0x2002);
    cpu.pc = 0x4400;
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.d[0], 7);

    // An instruction cached on the previous page reads its immediate across
    // the boundary, including the word at offset +2 on the written page.
    cpu.pc = 0x4ffe;
    ram.write16(0x4ffe, 0x203c).unwrap(); // move.l #imm,D0
    ram.write32(0x5000, 0x1234_5678).unwrap();
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.d[0], 0x1234_5678);
    ram.write16(0x5002, 0xabcd).unwrap();
    cpu.invalidate_external_write(0x5002, 2);
    cpu.pc = 0x4ffe;
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.d[0], 0x1234_abcd);
}

#[test]
fn external_dma_near_full_write_invalidates_decode_cache() {
    for len in [
        Some(u32::MAX as usize),
        usize::try_from(u64::from(u32::MAX) + 1).ok(),
    ]
    .into_iter()
    .flatten()
    {
        let mut cpu = Cpu::new();
        let mut ram = Ram::new();
        ram.write16(0x4400, 0x7001).unwrap(); // moveq #1,D0
        cpu.pc = 0x4400;
        cpu.step(&mut ram).unwrap();
        assert_eq!(cpu.d[0], 1);

        ram.write16(0x4400, 0x7002).unwrap(); // moveq #2,D0
        cpu.invalidate_external_write(0x3000, len);
        cpu.pc = 0x4400;
        cpu.step(&mut ram).unwrap();
        assert_eq!(cpu.d[0], 2);
    }
}
