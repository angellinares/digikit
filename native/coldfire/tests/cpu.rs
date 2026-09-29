//! Shape checks for the interpreter skeleton: state, bus, exception entry and
//! the first handful of instructions. Expected values are worked by hand from
//! the CFPRM instruction pages.

use coldfire::cpu::{sr, vector};
use coldfire::{Bus, BusError, Cpu, Form, Stop};

/// 64 KiB of RAM at 0; anything else is a bus error.
struct Ram(Vec<u8>);

impl Ram {
    fn new() -> Ram {
        Ram(vec![0; 0x10000])
    }
    fn at(&self, a: u32, n: usize) -> Result<usize, BusError> {
        let a = a as usize;
        if a + n <= self.0.len() {
            Ok(a)
        } else {
            Err(BusError {
                addr: a as u32,
                write: false,
            })
        }
    }
    fn words(&mut self, addr: u32, w: &[u16]) {
        for (k, v) in w.iter().enumerate() {
            self.write16(addr + 2 * k as u32, *v).unwrap();
        }
    }
}

impl Bus for Ram {
    fn read8(&mut self, a: u32) -> Result<u8, BusError> {
        Ok(self.0[self.at(a, 1)?])
    }
    fn read16(&mut self, a: u32) -> Result<u16, BusError> {
        let i = self.at(a, 2)?;
        Ok(u16::from_be_bytes([self.0[i], self.0[i + 1]]))
    }
    fn read32(&mut self, a: u32) -> Result<u32, BusError> {
        let i = self.at(a, 4)?;
        Ok(u32::from_be_bytes(self.0[i..i + 4].try_into().unwrap()))
    }
    fn write8(&mut self, a: u32, v: u8) -> Result<(), BusError> {
        let i = self.at(a, 1)?;
        self.0[i] = v;
        Ok(())
    }
    fn write16(&mut self, a: u32, v: u16) -> Result<(), BusError> {
        let i = self.at(a, 2)?;
        self.0[i..i + 2].copy_from_slice(&v.to_be_bytes());
        Ok(())
    }
    fn write32(&mut self, a: u32, v: u32) -> Result<(), BusError> {
        let i = self.at(a, 4)?;
        self.0[i..i + 4].copy_from_slice(&v.to_be_bytes());
        Ok(())
    }
}

fn machine(code: &[u16]) -> (Cpu, Ram) {
    let mut ram = Ram::new();
    ram.write32(0, 0x8000).unwrap(); // reset SSP
    ram.write32(4, 0x1000).unwrap(); // reset PC
    for v in 2..64u32 {
        ram.write32(4 * v, 0x4000 + 0x10 * v).unwrap(); // vector v -> 0x4000 + 16v
    }
    ram.words(0x1000, code);
    let mut cpu = Cpu::new();
    cpu.reset(&mut ram).unwrap();
    (cpu, ram)
}

#[test]
fn reset_loads_ssp_and_pc() {
    let (cpu, _) = machine(&[]);
    assert_eq!(cpu.a[7], 0x8000);
    assert_eq!(cpu.pc, 0x1000);
    assert_eq!(cpu.sr & sr::S, sr::S);
}

#[test]
fn arithmetic_and_flags() {
    // moveq #-1,d0 ; addq.l #1,d0 ; moveq #0x7f,d1 ; add.l d1,d1 ; cmp.l d1,d0
    let (mut cpu, mut ram) = machine(&[0x70ff, 0x5280, 0x727f, 0xd281, 0xb081]);
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.d[0], 0xffff_ffff);
    assert_eq!(cpu.sr & sr::CCR, sr::N);
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.d[0], 0);
    assert_eq!(cpu.sr & sr::CCR, sr::Z | sr::C | sr::X);
    cpu.step(&mut ram).unwrap();
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.d[1], 0xfe);
    assert_eq!(cpu.sr & sr::CCR, 0); // X cleared by ADD without carry
    cpu.step(&mut ram).unwrap();
    // 0 - 0xfe: borrow and negative; CMP leaves X alone
    assert_eq!(cpu.sr & sr::CCR, sr::N | sr::C);
    assert_eq!(cpu.icount, 5);
}

#[test]
fn memory_moves_and_calls() {
    // lea (0x2000).w,a0 ; move.l #0x11223344,(a0)+ ; move.b -(a0),d2
    // bsr.b +2 (to the rts) ; nop ; rts
    let (mut cpu, mut ram) = machine(&[
        0x41f8, 0x2000, 0x20fc, 0x1122, 0x3344, 0x1420, 0x6102, 0x4e71, 0x4e75,
    ]);
    for _ in 0..3 {
        cpu.step(&mut ram).unwrap();
    }
    assert_eq!(ram.read32(0x2000).unwrap(), 0x1122_3344);
    assert_eq!(cpu.a[0], 0x2003);
    assert_eq!(cpu.d[2] & 0xff, 0x44);
    cpu.step(&mut ram).unwrap(); // bsr
    assert_eq!(cpu.pc, 0x1010);
    assert_eq!(ram.read32(cpu.a[7]).unwrap(), 0x100e);
    cpu.step(&mut ram).unwrap(); // rts
    assert_eq!(cpu.pc, 0x100e);
    assert_eq!(cpu.a[7], 0x8000);
}

#[test]
fn link_unlk() {
    // link.w a6,#-8 ; unlk a6
    let (mut cpu, mut ram) = machine(&[0x4e56, 0xfff8, 0x4e5e]);
    cpu.a[6] = 0x1234;
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.a[6], 0x7ffc);
    assert_eq!(cpu.a[7], 0x7ff4);
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.a[6], 0x1234);
    assert_eq!(cpu.a[7], 0x8000);
}

#[test]
fn illegal_takes_vector_4_with_a_format_4_frame() {
    let (mut cpu, mut ram) = machine(&[0x4afc]);
    cpu.sr = sr::S | 0x0700 | sr::Z;
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.pc, 0x4000 + 0x10 * vector::ILLEGAL as u32);
    assert_eq!(cpu.a[7], 0x8000 - 8);
    let fv = ram.read32(0x8000 - 8).unwrap();
    assert_eq!(fv >> 28, 4); // A7 was longword aligned
    assert_eq!((fv >> 18) & 0xff, vector::ILLEGAL as u32);
    assert_eq!(fv & 0xffff, (sr::S | 0x0700 | sr::Z) as u32);
    assert_eq!(ram.read32(0x8000 - 4).unwrap(), 0x1000); // PC of the fault
}

#[test]
fn user_mode_privilege_and_stack_swap() {
    // move.w #0x2700,sr in user mode: privilege violation on the SSP
    let (mut cpu, mut ram) = machine(&[0x46fc, 0x2700]);
    cpu.other_a7 = 0x6000; // USP while in supervisor mode
    cpu.sr = sr::S;
    // drop to user mode the architectural way: A7 becomes the USP
    let (ssp, usp) = (cpu.a[7], cpu.other_a7);
    cpu.a[7] = usp;
    cpu.other_a7 = ssp;
    cpu.sr = 0;
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.pc, 0x4000 + 0x10 * vector::PRIVILEGE as u32);
    assert_eq!(cpu.a[7], 0x8000 - 8); // frame on the SSP
    assert_eq!(cpu.other_a7, 0x6000);
}

#[test]
fn fpu_opcode_without_fpu_is_line_f() {
    let (mut cpu, mut ram) = machine(&[0xf200, 0x0422]);
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.pc, 0x4000 + 0x10 * vector::LINE_F as u32);
}

#[test]
fn unimplemented_leaves_the_core_at_the_instruction() {
    // sats.l d0 has no semantics yet in this skeleton
    let (mut cpu, mut ram) = machine(&[0x4c80]);
    assert_eq!(cpu.step(&mut ram), Err(Stop::Unimplemented(Form::Sats)));
    assert_eq!(cpu.pc, 0x1000);
    assert_eq!(cpu.icount, 0);
}
