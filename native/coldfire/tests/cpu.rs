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
    // move.w #0x2700,sr in user mode: privilege violation on the SSP.
    // Requires CACR[EUSP] set (RM p.169 Table 6-3): confirmed against
    // Unicorn that with the reset default (EUSP=0) A7 does not swap at all
    // on an SR[S] change (see no_swap_without_eusp below), which the
    // firmware relies on implicitly by never executing MOVE to/from USP
    // (P3 stage-1 census: that ISA_B form is unused in both images).
    let (mut cpu, mut ram) = machine(&[0x46fc, 0x2700]);
    cpu.ctrl.cacr = 0x20; // EUSP
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
fn no_stack_swap_without_eusp() {
    // Confirmed against Unicorn (CFV4E, CACR reset to 0): dropping to user
    // mode (S: 1 -> 0) with CACR[EUSP]=0 leaves A7 exactly where it was --
    // no swap with OTHER_A7 -- unlike full 68k's unconditional SSP/USP
    // split (RM p.169: "0 USP disabled, core uses a single stack pointer").
    let (mut cpu, mut ram) = machine(&[0x46fc, 0x0700]); // move.w #0x0700,sr
    cpu.other_a7 = 0x6000;
    let a7_before = cpu.a[7];
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.a[7], a7_before);
    assert_eq!(cpu.other_a7, 0x6000);
    assert_eq!(cpu.sr, 0x0700);
}

#[test]
fn fpu_opcode_without_fpu_is_line_f() {
    let (mut cpu, mut ram) = machine(&[0xf200, 0x0422]);
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.pc, 0x4000 + 0x10 * vector::LINE_F as u32);
}

#[test]
fn unimplemented_leaves_the_core_at_the_instruction() {
    // pulse (CFPRM p.134) is not used by either image and has no semantics
    // yet in this skeleton.
    let (mut cpu, mut ram) = machine(&[0x4acc]);
    assert_eq!(cpu.step(&mut ram), Err(Stop::Unimplemented(Form::Pulse)));
    assert_eq!(cpu.pc, 0x1000);
    assert_eq!(cpu.icount, 0);
}

#[test]
fn sats() {
    // sats.l d0 (CFPRM p.138): only saturates when CCR[V] is already set.
    let (mut cpu, mut ram) = machine(&[0x4c80, 0x4c80]);
    cpu.d[0] = 0x1234_5678;
    cpu.sr = 0; // V clear: unchanged
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.d[0], 0x1234_5678);
    assert_eq!(cpu.sr & sr::CCR, 0);
    cpu.sr = sr::V; // V set, Dx positive: saturate to the largest negative
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.d[0], 0x8000_0000);
    assert_eq!(cpu.sr & sr::CCR, sr::N);
}

// -- P3 stage 2: immediates, bits, shifts, muldiv, EMAC ---------------------

#[test]
fn immediate_ops_on_dn() {
    // ori.l #0xf0,d0 ; andi.l #0x3c,d0 ; addi.l #1,d0 ; subi.l #0x3e,d0 ;
    // eori.l #-1,d0 ; cmpi.l #-2,d0 (CFPRM p.132,78,73,143,102,96)
    let (mut cpu, mut ram) = machine(&[
        0x0080, 0x0000, 0x00f0, 0x0280, 0x0000, 0x003c, 0x0680, 0x0000, 0x0001, 0x0480, 0x0000,
        0x003e, 0x0a80, 0xffff, 0xffff, 0x0c00, 0xffff, 0xfffe,
    ]);
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.d[0], 0xf0);
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.d[0], 0x30);
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.d[0], 0x31);
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.d[0], 0xffff_fff3); // 0x31 - 0x3e wraps negative
    assert_eq!(cpu.sr & sr::CCR, sr::N | sr::C | sr::X);
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.d[0], 0x0000_000c); // XOR with all-ones complements
    cpu.step(&mut ram).unwrap(); // cmpi.l #-2,d0: 0xc - 0xfffffffe = 0xe
    // X is unaffected by CMP (still set from the SUBI above); C is set
    // because 0xfffffffe is the larger value bit-for-bit (unsigned borrow).
    assert_eq!(cpu.sr & sr::CCR, sr::C | sr::X);
}

#[test]
fn bit_instructions() {
    // btst d1,d2 (dynamic, .l since dest is Dn) ; bset #3,d3 (static, .b)
    let (mut cpu, mut ram) = machine(&[0x0302, 0x08c3, 0x0003]);
    cpu.d[1] = 3; // bit number
    cpu.d[2] = 0; // bit 3 clear
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.sr & sr::Z, sr::Z); // CFPRM p.91-92: Z set if the bit was 0
    cpu.d[3] = 0;
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.d[3] & 0xff, 0x08); // bit 3 now set
    assert_eq!(cpu.sr & sr::Z, sr::Z); // was clear before the set
}

#[test]
fn movem_store_and_load_ind_and_disp() {
    // movem.l d0-d1,(a0) ; movem.l (0,a0),d2-d3 -- ColdFire's MOVEM addresses
    // only (An) and (d16,An) (CFPRM p.115-116's ea table has no
    // pre-decrement/post-increment, unlike full 68k MOVEM).
    let (mut cpu, mut ram) = machine(&[0x48d0, 0x0003, 0x4ce8, 0x000c, 0x0000]);
    cpu.a[0] = 0x2000;
    cpu.d[0] = 0x1111_1111;
    cpu.d[1] = 0x2222_2222;
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.a[0], 0x2000); // MOVEM never updates An itself
    assert_eq!(ram.read32(0x2000).unwrap(), 0x1111_1111);
    assert_eq!(ram.read32(0x2004).unwrap(), 0x2222_2222);
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.d[2], 0x1111_1111);
    assert_eq!(cpu.d[3], 0x2222_2222);
    assert_eq!(cpu.a[0], 0x2000);
}

#[test]
fn shifts() {
    // asr.l #1,d0 ; lsl.l d1,d0 (CFPRM p.79-80,109-110)
    let (mut cpu, mut ram) = machine(&[0xe280, 0xe3a8]);
    cpu.d[0] = 0x8000_0001; // negative, lsb set
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.d[0], 0xc000_0000); // sign-extended
    assert_eq!(cpu.sr & sr::CCR, sr::N | sr::C | sr::X); // bit shifted out was 1
    cpu.d[1] = 4;
    cpu.step(&mut ram).unwrap();
    // 0xc000_0000 << 4 shifts both set bits (31,30) out of a 32-bit word.
    assert_eq!(cpu.d[0], 0);
    assert_eq!(cpu.sr & sr::Z, sr::Z);
}

#[test]
fn multiply_and_divide() {
    // muls.l d1,d0 ; divu.l d2,d3 (CFPRM p.120-121,99-100)
    let (mut cpu, mut ram) = machine(&[0x4c01, 0x0800, 0x4c42, 0x3003]);
    cpu.d[0] = 0xffff_fffe; // -2
    cpu.d[1] = 5;
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.d[0], 0xffff_fff6); // -10
    assert_eq!(cpu.sr & sr::CCR, sr::N); // V,C always cleared by MULS
    cpu.d[2] = 0;
    cpu.d[3] = 7;
    cpu.step(&mut ram).unwrap(); // the exception is taken, not surfaced as Err
    assert_eq!(cpu.pc, 0x4000 + 0x10 * vector::DIVIDE_BY_ZERO as u32);
}

#[test]
fn movec_vbr_and_rgpiobar() {
    // movec d0,vbr ; movec d1,rgpiobar (CFPRM p.249-250; RM p.90 Table 3-1)
    let (mut cpu, mut ram) = machine(&[0x4e7b, 0x0801, 0x4e7b, 0x1009]);
    cpu.d[0] = 0x4000_0000;
    cpu.d[1] = 0xff00_0000;
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.ctrl.vbr, 0x4000_0000);
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.ctrl.rgpiobar, 0xff00_0000);
}

#[test]
fn rte_restores_sr_and_pc_and_the_exact_stack_pointer() {
    // A misaligned-by-1 A7 at the time of an exception is recorded in the
    // frame's FORMAT field and must come back exactly on RTE (CFPRM
    // p.286-287 Table 11-2).
    let (mut cpu, mut ram) = machine(&[0x4e73]); // rte
    ram.write32(0x7ff8, 5u32 << 28 | (vector::ILLEGAL as u32) << 18 | 0x2704)
        .unwrap();
    ram.write32(0x7ffc, 0x1234).unwrap();
    cpu.a[7] = 0x7ff8;
    cpu.sr = sr::S;
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.pc, 0x1234);
    assert_eq!(cpu.sr, 0x2704);
    assert_eq!(cpu.a[7], 0x8001); // 0x7ff8 + 8 + (format 5 - 4)
}

#[test]
fn ext_swap_neg_not() {
    // extb.l d0 ; swap.w d1 ; neg.l d2 ; not.l d3 (CFPRM p.103,146,126,129)
    let (mut cpu, mut ram) = machine(&[0x49c0, 0x4841, 0x4482, 0x4683]);
    cpu.d[0] = 0xffff_ff80; // low byte 0x80
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.d[0], 0xffff_ff80); // sign-extends to the same negative
    cpu.d[1] = 0x0001_0002;
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.d[1], 0x0002_0001);
    cpu.d[2] = 1;
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.d[2], 0xffff_ffff);
    assert_eq!(cpu.sr & sr::CCR, sr::N | sr::C | sr::X);
    cpu.d[3] = 0x0000_00ff;
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.d[3], 0xffff_ff00);
}

#[test]
fn emac_mac_signed_integer_and_movclr() {
    // mac.l d1,d0,acc0 ; movclr acc0,d2 (CFPRM p.170-171,174-175;
    // RM p.156-158 pseudocode)
    let (mut cpu, mut ram) = machine(&[0xa001, 0x0800, 0xa1c2]);
    cpu.emac.macsr = 0; // signed integer, no saturation
    cpu.d[0] = 3;
    cpu.d[1] = 4;
    cpu.emac.acc[0] = 10;
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.emac.acc[0], 22); // 10 + 3*4
    assert_eq!(cpu.emac.macsr & 0x0e, 0); // N=Z=V=0
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.d[2], 22);
    assert_eq!(cpu.emac.acc[0], 0); // MOVCLR clears the accumulator
}

#[test]
fn emac_fractional_mode_and_macsr_move() {
    // move.l d3,macsr (#0x20: F/I=1) ; mac.l d0,d0,acc0 (RM p.158-159)
    let (mut cpu, mut ram) = machine(&[0xa903, 0xa000, 0x0800]);
    cpu.d[3] = 0x20;
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.emac.macsr & 0x20, 0x20);
    cpu.d[0] = 0x4000_0000; // 0.5 in Q31
    cpu.step(&mut ram).unwrap();
    assert_eq!(cpu.emac.acc[0], 0x2000_0000); // 0.5 * 0.5 = 0.25
}
