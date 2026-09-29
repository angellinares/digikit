//! Processor state, the bus interface, exception entry and a first handful
//! of instruction semantics (enough to fix the shape of the interpreter; the
//! rest lands with the per-instruction lockstep against Unicorn).
//!
//! References: CFPRM (ColdFire Family Programmer's Reference Manual, Rev. 2)
//! and the MCF5441x Reference Manual (RM); page numbers are PDF pages.

use crate::decode::{Ea, Insn, Operand, Size, decode};
use crate::decode_gen::Form;

/// A bus access that failed (unmapped, or refused by a peripheral model).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BusError {
    pub addr: u32,
    pub write: bool,
}

/// Memory and peripherals as the core sees them: big-endian, byte addressed.
/// Misaligned accesses are allowed (RM p.111-112 Table 3-12 splits them).
pub trait Bus {
    fn read8(&mut self, addr: u32) -> Result<u8, BusError>;
    fn read16(&mut self, addr: u32) -> Result<u16, BusError>;
    fn read32(&mut self, addr: u32) -> Result<u32, BusError>;
    fn write8(&mut self, addr: u32, v: u8) -> Result<(), BusError>;
    fn write16(&mut self, addr: u32, v: u16) -> Result<(), BusError>;
    fn write32(&mut self, addr: u32, v: u32) -> Result<(), BusError>;
    /// Instruction fetch; a separate hook so a bus can count or cache fetches.
    fn fetch16(&mut self, addr: u32) -> Result<u16, BusError> {
        self.read16(addr)
    }
}

/// Status register bits (CFPRM p.25 Table 1-7).
pub mod sr {
    pub const C: u16 = 0x0001;
    pub const V: u16 = 0x0002;
    pub const Z: u16 = 0x0004;
    pub const N: u16 = 0x0008;
    pub const X: u16 = 0x0010;
    pub const IPL: u16 = 0x0700;
    pub const M: u16 = 0x1000;
    pub const S: u16 = 0x2000;
    pub const T: u16 = 0x8000;
    pub const CCR: u16 = 0x001f;
}

/// Exception vectors used here (CFPRM p.284-285 Table 11-1).
pub mod vector {
    pub const ACCESS_ERROR: u8 = 2;
    pub const ADDRESS_ERROR: u8 = 3;
    pub const ILLEGAL: u8 = 4;
    pub const DIVIDE_BY_ZERO: u8 = 5;
    pub const PRIVILEGE: u8 = 8;
    pub const LINE_A: u8 = 10;
    pub const LINE_F: u8 = 11;
    pub const FORMAT_ERROR: u8 = 14;
    pub const TRAP0: u8 = 32;
}

/// EMAC registers (CFPRM p.20-23; RM chapter 5). Accumulators and their
/// extension bytes are kept as the architected registers: ACCext01 holds
/// ACC0 upper/lower extension bytes in [31:16] and ACC1's in [15:0].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Emac {
    pub macsr: u32,
    pub acc: [u32; 4],
    pub accext01: u32,
    pub accext23: u32,
    /// Reset value 0xFFFF_FFFF (RM p.90 Table 3-1); only [15:0] is used.
    pub mask: u32,
}

/// FPU registers (CFPRM p.16-18). The MCF5441x has no FPU (RM, CPU
/// configuration word FPU=0, RM p.106-107), so `Cpu::fpu` is None and FPU opcodes take the
/// line-F exception; the state is here for other V4e parts.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Fpu {
    pub fp: [f64; 8],
    pub fpcr: u32,
    pub fpsr: u32,
    pub fpiar: u32,
}

/// Supervisor control registers written with MOVEC (RM p.90 Table 3-1).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ctrl {
    pub vbr: u32,
    pub cacr: u32,
    pub asid: u32,
    pub acr: [u32; 8],
    pub mmubar: u32,
    pub rgpiobar: u32,
    pub rambar: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunState {
    Running,
    /// STOP: waits for an interrupt above the SR mask.
    Stopped,
    /// HALT, or a fault while taking an exception (fault-on-fault).
    Halted,
}

/// Why `step` did not complete an instruction normally.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// The form has no semantics yet (this skeleton).
    Unimplemented(Form),
    /// The processor halted (HALT or fault-on-fault).
    Halted,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cpu {
    pub d: [u32; 8],
    /// A0-A7; a[7] is the active stack pointer.
    pub a: [u32; 8],
    /// The inactive stack pointer: USP while SR[S]=1, SSP while SR[S]=0
    /// (CFPRM p.286 11.1.1).
    pub other_a7: u32,
    pub pc: u32,
    pub sr: u16,
    pub ctrl: Ctrl,
    pub emac: Emac,
    pub fpu: Option<Fpu>,
    pub state: RunState,
    /// Instructions completed (including ones that took an exception).
    pub icount: u64,
}

/// A fault raised while executing: the vector and the PC to stack
/// (0 = the faulting instruction's own PC, filled in by `step`).
struct Exc {
    vector: u8,
    pc: u32,
}

impl From<BusError> for Exc {
    fn from(_: BusError) -> Exc {
        Exc {
            vector: vector::ACCESS_ERROR,
            pc: 0,
        }
    }
}

impl Default for Cpu {
    fn default() -> Cpu {
        Cpu::new()
    }
}

impl Cpu {
    /// Register state before the reset exception loads SSP and PC.
    pub fn new() -> Cpu {
        Cpu {
            d: [0; 8],
            a: [0; 8],
            other_a7: 0,
            pc: 0,
            sr: sr::S | sr::IPL,
            ctrl: Ctrl::default(),
            emac: Emac {
                mask: 0xffff_ffff,
                ..Emac::default()
            },
            fpu: None,
            state: RunState::Running,
            icount: 0,
        }
    }

    /// The reset exception: SSP from vector 0 and PC from vector 1 at VBR=0
    /// (CFPRM p.284 Table 11-1).
    pub fn reset(&mut self, bus: &mut impl Bus) -> Result<(), BusError> {
        *self = Cpu {
            fpu: self.fpu.take(),
            ..Cpu::new()
        };
        self.a[7] = bus.read32(0)?;
        self.pc = bus.read32(4)?;
        Ok(())
    }

    fn set_sr(&mut self, v: u16) {
        // switching between supervisor and user swaps A7 and OTHER_A7
        if (self.sr ^ v) & sr::S != 0 {
            core::mem::swap(&mut self.a[7], &mut self.other_a7);
        }
        self.sr = v;
    }

    /// Exception entry (CFPRM p.286-287 11.1.2): a two-longword frame on the
    /// supervisor stack, format 4-7 recording A7[1:0], then PC from VBR.
    fn exception(&mut self, bus: &mut impl Bus, vec: u8, pc: u32) -> Result<(), Stop> {
        let old = self.sr;
        self.set_sr((old | sr::S) & !(sr::T | sr::M));
        let sp = self.a[7];
        let format = 4 + (sp & 3);
        let sp = (sp & !3).wrapping_sub(8);
        self.a[7] = sp;
        let fv = format << 28 | (vec as u32) << 18 | old as u32;
        let r = bus
            .write32(sp, fv)
            .and_then(|_| bus.write32(sp.wrapping_add(4), pc))
            .and_then(|_| bus.read32(self.ctrl.vbr.wrapping_add(4 * vec as u32)));
        match r {
            Ok(handler) => {
                self.pc = handler;
                Ok(())
            }
            Err(_) => {
                self.state = RunState::Halted;
                Err(Stop::Halted)
            }
        }
    }

    /// Fetch and decode at PC. A word that cannot be fetched is replaced by 0
    /// and faults only if the instruction turns out to need it.
    fn fetch(&mut self, bus: &mut impl Bus) -> Result<Option<Insn>, BusError> {
        let w0 = bus.fetch16(self.pc)?;
        let w1 = bus.fetch16(self.pc.wrapping_add(2));
        let w2 = bus.fetch16(self.pc.wrapping_add(4));
        let words = [w0, *w1.as_ref().unwrap_or(&0), *w2.as_ref().unwrap_or(&0)];
        let insn = decode(self.pc, words);
        if let Some(i) = insn {
            if i.len >= 4 {
                w1?;
            }
            if i.len >= 6 {
                w2?;
            }
        }
        Ok(insn)
    }

    /// Execute one instruction.
    pub fn step(&mut self, bus: &mut impl Bus) -> Result<(), Stop> {
        if self.state == RunState::Halted {
            return Err(Stop::Halted);
        }
        let pc = self.pc;
        let insn = match self.fetch(bus) {
            Ok(Some(i)) => i,
            Ok(None) => {
                // line A (0xAxxx) and line F (0xFxxx) have their own vectors
                let w0 = bus.fetch16(pc).unwrap_or(0);
                let vec = match w0 >> 12 {
                    0xa => vector::LINE_A,
                    0xf => vector::LINE_F,
                    _ => vector::ILLEGAL,
                };
                self.icount += 1;
                return self.exception(bus, vec, pc);
            }
            Err(_) => {
                self.icount += 1;
                return self.exception(bus, vector::ACCESS_ERROR, pc);
            }
        };
        if insn.privileged() && self.sr & sr::S == 0 {
            self.icount += 1;
            return self.exception(bus, vector::PRIVILEGE, pc);
        }
        if insn.unit() == "fpu" && self.fpu.is_none() {
            self.icount += 1;
            return self.exception(bus, vector::LINE_F, pc);
        }
        self.pc = pc.wrapping_add(insn.len as u32);
        let r = self.execute(bus, &insn, pc);
        self.icount += 1;
        match r {
            Ok(()) => Ok(()),
            Err(Ok(e)) => self.exception(bus, e.vector, if e.pc == 0 { pc } else { e.pc }),
            Err(Err(stop)) => {
                if let Stop::Unimplemented(_) = stop {
                    // nothing was executed: leave the core at the instruction
                    self.pc = pc;
                    self.icount -= 1;
                }
                Err(stop)
            }
        }
    }

    // -- operand access ------------------------------------------------------

    fn index_value(&self, xn: u8, scale: u8) -> u32 {
        let x = if xn < 8 {
            self.d[xn as usize]
        } else {
            self.a[(xn - 8) as usize]
        };
        x.wrapping_shl(scale as u32)
    }

    /// The address of a memory EA, applying (An)+/-(An) updates.
    fn ea_addr(&mut self, ea: &Ea, size: Size) -> Result<u32, Exc> {
        let n = match size {
            Size::B => 1,
            Size::W => 2,
            _ => 4,
        };
        Ok(match *ea {
            Ea::Ind(r) => self.a[r as usize],
            Ea::Post(r) => {
                let v = self.a[r as usize];
                self.a[r as usize] = v.wrapping_add(n);
                v
            }
            Ea::Pre(r) => {
                let v = self.a[r as usize].wrapping_sub(n);
                self.a[r as usize] = v;
                v
            }
            Ea::Disp(r, d) => self.a[r as usize].wrapping_add(d as i32 as u32),
            Ea::Idx {
                an,
                xn,
                wl,
                scale,
                d8,
            } => {
                if !wl {
                    return Err(Exc {
                        vector: vector::ADDRESS_ERROR,
                        pc: 0,
                    });
                }
                self.a[an as usize]
                    .wrapping_add(self.index_value(xn, scale))
                    .wrapping_add(d8 as i32 as u32)
            }
            Ea::AbsW(v) => v as i32 as u32,
            Ea::AbsL(v) => v,
            Ea::PcDisp { base, d16 } => base.wrapping_add(d16 as i32 as u32),
            Ea::PcIdx {
                base,
                xn,
                wl,
                scale,
                d8,
            } => {
                if !wl {
                    return Err(Exc {
                        vector: vector::ADDRESS_ERROR,
                        pc: 0,
                    });
                }
                base.wrapping_add(self.index_value(xn, scale))
                    .wrapping_add(d8 as i32 as u32)
            }
            Ea::Dn(_) | Ea::An(_) | Ea::Imm(_) => unreachable!("not a memory EA"),
        })
    }

    fn read(&mut self, bus: &mut impl Bus, ea: &Ea, size: Size) -> Result<u32, Exc> {
        match *ea {
            Ea::Dn(r) => Ok(self.d[r as usize]),
            Ea::An(r) => Ok(self.a[r as usize]),
            Ea::Imm(v) => Ok(v),
            _ => {
                let addr = self.ea_addr(ea, size)?;
                Ok(match size {
                    Size::B => bus.read8(addr)? as u32,
                    Size::W => bus.read16(addr)? as u32,
                    _ => bus.read32(addr)?,
                })
            }
        }
    }

    /// Write `v` (size bits) to an EA whose address is already computed
    /// (`addr` for memory), merging into Dn for byte and word sizes.
    fn write_at(
        &mut self,
        bus: &mut impl Bus,
        ea: &Ea,
        addr: u32,
        size: Size,
        v: u32,
    ) -> Result<(), Exc> {
        match *ea {
            Ea::Dn(r) => {
                let d = &mut self.d[r as usize];
                *d = match size {
                    Size::B => (*d & !0xff) | (v & 0xff),
                    Size::W => (*d & !0xffff) | (v & 0xffff),
                    _ => v,
                };
            }
            Ea::An(r) => self.a[r as usize] = v,
            _ => match size {
                Size::B => bus.write8(addr, v as u8)?,
                Size::W => bus.write16(addr, v as u16)?,
                _ => bus.write32(addr, v)?,
            },
        }
        Ok(())
    }

    fn write(&mut self, bus: &mut impl Bus, ea: &Ea, size: Size, v: u32) -> Result<(), Exc> {
        let addr = match ea {
            Ea::Dn(_) | Ea::An(_) => 0,
            _ => self.ea_addr(ea, size)?,
        };
        self.write_at(bus, ea, addr, size, v)
    }

    fn push32(&mut self, bus: &mut impl Bus, v: u32) -> Result<(), Exc> {
        self.a[7] = self.a[7].wrapping_sub(4);
        bus.write32(self.a[7], v)?;
        Ok(())
    }

    fn pop32(&mut self, bus: &mut impl Bus) -> Result<u32, Exc> {
        let v = bus.read32(self.a[7])?;
        self.a[7] = self.a[7].wrapping_add(4);
        Ok(v)
    }

    // -- condition codes (CFPRM chapter 4 condition code tables) -------------

    fn set_nz(&mut self, v: u32, size: Size) {
        let (m, s) = mask_sign(size);
        let mut ccr = self.sr & !(sr::N | sr::Z | sr::V | sr::C);
        if v & m == 0 {
            ccr |= sr::Z;
        }
        if v & s != 0 {
            ccr |= sr::N;
        }
        self.sr = ccr;
    }

    /// Flags of dst + src = r (ADD, ADDQ): X and C are the carry.
    fn flags_add(&mut self, src: u32, dst: u32, r: u32, size: Size) {
        let (m, s) = mask_sign(size);
        let (src, dst, r) = (src & m, dst & m, r & m);
        let mut ccr = self.sr & !sr::CCR;
        if r == 0 {
            ccr |= sr::Z;
        }
        if r & s != 0 {
            ccr |= sr::N;
        }
        if (src ^ r) & (dst ^ r) & s != 0 {
            ccr |= sr::V;
        }
        if (src & dst | !r & (src | dst)) & s != 0 {
            ccr |= sr::C | sr::X;
        }
        self.sr = ccr;
    }

    /// Flags of dst - src = r. `cmp` leaves X unchanged (CMP, CMPA).
    fn flags_sub(&mut self, src: u32, dst: u32, r: u32, size: Size, cmp: bool) {
        let (m, s) = mask_sign(size);
        let (src, dst, r) = (src & m, dst & m, r & m);
        let keep = if cmp { sr::X } else { 0 };
        let mut ccr = self.sr & (!sr::CCR | keep);
        if r == 0 {
            ccr |= sr::Z;
        }
        if r & s != 0 {
            ccr |= sr::N;
        }
        if (src ^ dst) & (r ^ dst) & s != 0 {
            ccr |= sr::V;
        }
        if (src & !dst | r & !dst | src & r) & s != 0 {
            ccr |= if cmp { sr::C } else { sr::C | sr::X };
        }
        self.sr = ccr;
    }

    /// Bcc/Scc condition (CFPRM p.82 Bcc condition table).
    fn cond(&self, c: u8) -> bool {
        let f = |b: u16| self.sr & b != 0;
        let (n, z, v, cy) = (f(sr::N), f(sr::Z), f(sr::V), f(sr::C));
        match c {
            0 => true,
            1 => false,
            2 => !cy && !z,
            3 => cy || z,
            4 => !cy,
            5 => cy,
            6 => !z,
            7 => z,
            8 => !v,
            9 => v,
            10 => !n,
            11 => n,
            12 => n == v,
            13 => n != v,
            14 => !z && n == v,
            _ => z || n != v,
        }
    }

    // -- the handful ---------------------------------------------------------

    /// Err(Ok(exception)) takes an exception; Err(Err(stop)) stops the core.
    fn execute(&mut self, bus: &mut impl Bus, i: &Insn, pc: u32) -> Result<(), Result<Exc, Stop>> {
        let ops = i.operands();
        let ea = |k: usize| match ops[k] {
            Operand::Ea(e) => e,
            _ => unreachable!("operand {} of {} is not an EA", k, i.id()),
        };
        let imm = |k: usize| match ops[k] {
            Operand::Imm(v) => v,
            _ => unreachable!("operand {} of {} is not an immediate", k, i.id()),
        };
        let size = i.size;
        match i.form {
            Form::Nop => {}
            Form::Moveq => {
                let v = imm(0);
                self.write(bus, &ea(1), Size::L, v).map_err(Ok)?;
                self.set_nz(v, Size::L);
            }
            Form::Move | Form::Mov3q => {
                let v = if i.form == Form::Move {
                    self.read(bus, &ea(0), size).map_err(Ok)?
                } else {
                    imm(0)
                };
                let sz = if i.form == Form::Move { size } else { Size::L };
                self.write(bus, &ea(1), sz, v).map_err(Ok)?;
                self.set_nz(v, sz);
            }
            Form::Movea => {
                let v = self.read(bus, &ea(0), size).map_err(Ok)?;
                let v = if size == Size::W {
                    v as u16 as i16 as i32 as u32
                } else {
                    v
                };
                self.write(bus, &ea(1), Size::L, v).map_err(Ok)?;
            }
            Form::Lea => {
                let addr = self.ea_addr(&ea(0), Size::L).map_err(Ok)?;
                self.write(bus, &ea(1), Size::L, addr).map_err(Ok)?;
            }
            Form::Pea => {
                let addr = self.ea_addr(&ea(0), Size::L).map_err(Ok)?;
                self.push32(bus, addr).map_err(Ok)?;
            }
            Form::Clr => {
                self.write(bus, &ea(0), size, 0).map_err(Ok)?;
                self.set_nz(0, size);
            }
            Form::Tst => {
                let v = self.read(bus, &ea(0), size).map_err(Ok)?;
                self.set_nz(v, size);
            }
            Form::Addq | Form::Subq => {
                let q = imm(0);
                let dst_ea = ea(1);
                let add = i.form == Form::Addq;
                if let Ea::An(r) = dst_ea {
                    // address register destination: no flags (CFPRM p.74)
                    let a = &mut self.a[r as usize];
                    *a = if add {
                        a.wrapping_add(q)
                    } else {
                        a.wrapping_sub(q)
                    };
                } else {
                    let addr = match dst_ea {
                        Ea::Dn(_) => 0,
                        _ => self.ea_addr(&dst_ea, Size::L).map_err(Ok)?,
                    };
                    let d = match dst_ea {
                        Ea::Dn(r) => self.d[r as usize],
                        _ => bus.read32(addr).map_err(|e| Ok(e.into()))?,
                    };
                    let r = if add {
                        d.wrapping_add(q)
                    } else {
                        d.wrapping_sub(q)
                    };
                    self.write_at(bus, &dst_ea, addr, Size::L, r).map_err(Ok)?;
                    if add {
                        self.flags_add(q, d, r, Size::L);
                    } else {
                        self.flags_sub(q, d, r, Size::L, false);
                    }
                }
            }
            Form::AddToD | Form::SubToD => {
                let s = self.read(bus, &ea(0), Size::L).map_err(Ok)?;
                let Ea::Dn(r) = ea(1) else { unreachable!() };
                let d = self.d[r as usize];
                if i.form == Form::AddToD {
                    let v = d.wrapping_add(s);
                    self.d[r as usize] = v;
                    self.flags_add(s, d, v, Size::L);
                } else {
                    let v = d.wrapping_sub(s);
                    self.d[r as usize] = v;
                    self.flags_sub(s, d, v, Size::L, false);
                }
            }
            Form::Cmp | Form::CmpaW | Form::CmpaL => {
                let s = self.read(bus, &ea(0), size).map_err(Ok)?;
                let (d, s, sz) = match ea(1) {
                    Ea::Dn(r) => (self.d[r as usize], s, size),
                    // CMPA.W sign-extends the source to 32 bits (CFPRM p.95)
                    Ea::An(r) => (
                        self.a[r as usize],
                        if size == Size::W {
                            s as u16 as i16 as i32 as u32
                        } else {
                            s
                        },
                        Size::L,
                    ),
                    _ => unreachable!(),
                };
                self.flags_sub(s, d, d.wrapping_sub(s), sz, true);
            }
            Form::Bra | Form::Bcc | Form::Bsr => {
                let Operand::Target(t) = ops[0] else {
                    unreachable!()
                };
                if i.form == Form::Bsr {
                    let next = self.pc;
                    self.push32(bus, next).map_err(Ok)?;
                    self.pc = t;
                } else if i.form == Form::Bra || self.cond(i.cond) {
                    self.pc = t;
                }
            }
            Form::Jmp | Form::Jsr => {
                let t = self.ea_addr(&ea(0), Size::L).map_err(Ok)?;
                if i.form == Form::Jsr {
                    let next = self.pc;
                    self.push32(bus, next).map_err(Ok)?;
                }
                self.pc = t;
            }
            Form::Rts => {
                self.pc = self.pop32(bus).map_err(Ok)?;
            }
            Form::Link => {
                let Ea::An(r) = ea(0) else { unreachable!() };
                let v = self.a[r as usize];
                self.push32(bus, v).map_err(Ok)?;
                self.a[r as usize] = self.a[7];
                self.a[7] = self.a[7].wrapping_add(imm(1));
            }
            Form::Unlk => {
                let Ea::An(r) = ea(0) else { unreachable!() };
                self.a[7] = self.a[r as usize];
                let v = self.pop32(bus).map_err(Ok)?;
                self.a[r as usize] = v;
            }
            Form::Illegal => {
                return Err(Ok(Exc {
                    vector: vector::ILLEGAL,
                    pc,
                }));
            }
            Form::Trap => {
                let next = self.pc;
                return Err(Ok(Exc {
                    vector: vector::TRAP0 + imm(0) as u8,
                    pc: next,
                }));
            }
            Form::Halt => {
                self.state = RunState::Halted;
                return Err(Err(Stop::Halted));
            }
            f => return Err(Err(Stop::Unimplemented(f))),
        }
        Ok(())
    }
}

fn mask_sign(size: Size) -> (u32, u32) {
    match size {
        Size::B => (0xff, 0x80),
        Size::W => (0xffff, 0x8000),
        _ => (0xffff_ffff, 0x8000_0000),
    }
}
