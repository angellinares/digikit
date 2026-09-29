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

/// Pre-decode cache (`cf-interp-speed.md` step 1): decode each instruction
/// once, keyed by PC, so a hot loop's `step` skips both the bus fetch and
/// `decode()` on every later pass. 4 KiB pages (`DC_PAGE_SIZE`), one
/// `Option<Insn>` slot per word-aligned offset -- `Insn` is `Copy`
/// (~doz. bytes), so a page is a plain array and a hit is a shift plus an
/// array index, not a hash lookup.
///
/// Invalidation: every one of `Cpu`'s own memory-write call sites (there
/// are exactly four: `write_at`'s memory arm, `push32`, `exception`'s frame
/// push, and `MovemStore`) evicts the whole 4 KiB page a write landed in
/// before issuing the write. Coarse -- a write anywhere in the page evicts
/// every cached instruction in it, not just the touched word -- but exact
/// per-instruction tracking would need a second index from byte address to
/// the instructions overlapping it, for no benefit here (self-modifying
/// code is rare and this only costs a re-decode, never a wrong answer).
/// Known gap: this only sees writes `Cpu::step` itself issues through
/// `Bus`. A write from outside `step` (DMA, a peer device mutating RAM
/// behind the `Bus` impl) is invisible here; no `Bus` impl in this crate
/// does that today (`cfrealmix`'s `SparseBus`, `cfabi`'s `Bus`), so it is
/// not exercised by this crate's own gates, but a future one that models a
/// live DMA writer into code space would need to invalidate on `Cpu`'s
/// behalf.
const DC_PAGE_BITS: u32 = 12;
const DC_PAGE_SIZE: u32 = 1 << DC_PAGE_BITS;
const DC_PAGE_MASK: u32 = DC_PAGE_SIZE - 1;
const DC_SLOTS: usize = (DC_PAGE_SIZE / 2) as usize;

#[derive(Clone, Debug, PartialEq)]
struct DecodePage {
    slots: Box<[Option<Insn>]>,
}

impl DecodePage {
    fn new() -> DecodePage {
        DecodePage {
            slots: vec![None; DC_SLOTS].into_boxed_slice(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
struct DecodeCache {
    pages: std::collections::HashMap<u32, DecodePage>,
}

impl DecodeCache {
    #[inline]
    fn get(&self, pc: u32) -> Option<Insn> {
        let page = pc & !DC_PAGE_MASK;
        let slot = ((pc & DC_PAGE_MASK) >> 1) as usize;
        self.pages.get(&page).and_then(|p| p.slots[slot])
    }

    #[inline]
    fn insert(&mut self, pc: u32, insn: Insn) {
        let page = pc & !DC_PAGE_MASK;
        let slot = ((pc & DC_PAGE_MASK) >> 1) as usize;
        self.pages.entry(page).or_insert_with(DecodePage::new).slots[slot] = Some(insn);
    }

    /// Evict every decoded instruction in the 4 KiB page(s) a write of
    /// `len` bytes (1-8: a byte/word/long EA write or an 8-byte exception
    /// frame push) at `addr` touched -- at most 2 pages, when the write
    /// straddles a page boundary.
    #[inline]
    fn invalidate(&mut self, addr: u32, len: u32) {
        if self.pages.is_empty() {
            return;
        }
        let last = addr.wrapping_add(len - 1);
        let p0 = addr & !DC_PAGE_MASK;
        let p1 = last & !DC_PAGE_MASK;
        self.pages.remove(&p0);
        if p1 != p0 {
            self.pages.remove(&p1);
        }
    }
}

/// Perf step 3 (cf-interp-speed.md): lazy N/Z/V. C and X stay eager --
/// they're always written immediately by whichever ALU/shift op sets them
/// -- because ADDX/SUBX/NEGX read X as a carry-in *during their own
/// execution* (not just as a flag), and CFPRM's ADDX/SUBX Z rule ("clear Z
/// only if the result is nonzero, otherwise leave it as it was") makes Z a
/// hidden reader of the *previous* instruction's flags. Keeping C/X (and,
/// for ADDX/SUBX, N/Z/V too -- see `flags_addx`) always-accurate sidesteps
/// having to reconstruct a flag from a discarded, never-applied pending
/// computation. N/Z/V, by contrast, are always fully recomputed from this
/// instruction's own operands in every producer below (never preserved
/// from old CCR bits), so deferring them is safe: a later producer simply
/// overwrites `pending_nzv` and the never-read old computation is dropped,
/// exactly like eager execution would have discarded its unread result.
///
/// Any code that reads N/Z/V (`cond`, MOVE from SR/CCR, SATS's V test,
/// exception entry's frame push) or that only partially overwrites them
/// while preserving the rest (BTST/BCHG/BCLR/BSET's Z-only write) calls
/// `resolve_nzv` first. Any code that fully overwrites N/Z/V on its own
/// (MOVE to CCR, `set_sr` and everything that goes through it, the MUL/DIV
/// overflow cases) clears `pending_nzv` instead of resolving it, since its
/// own write already makes `sr` authoritative and a stale pending
/// computation must not be allowed to land on top of it later.
#[derive(Clone, Copy, Debug, PartialEq)]
enum PendingNzv {
    /// `sr`'s N/Z/V bits are already authoritative; nothing to do.
    None,
    /// `set_nz`: N/Z from `v` (masked/signed per `size`); V is always 0.
    Nz { v: u32, size: Size },
    /// `flags_add`: N/Z from `r`; V from the ADD overflow test. `src`/
    /// `dst`/`r` are already masked to `size`.
    Add {
        src: u32,
        dst: u32,
        r: u32,
        size: Size,
    },
    /// `flags_sub`: N/Z from `r`; V from the SUB overflow test. Same
    /// pre-masking as `Add`.
    Sub {
        src: u32,
        dst: u32,
        r: u32,
        size: Size,
    },
    /// `flags_shift`: N/Z from `r` (not pre-masked: masked at resolve
    /// time, matching the original `flags_shift`); V is `v`, precomputed
    /// by the shift loop itself.
    Shift { v: bool, r: u32, size: Size },
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
    /// The vector taken by the most recent `step`, if any; cleared at the
    /// start of each `step`. Exists for callers (the lockstep harness) that
    /// need to know an exception was taken, since `step`'s own `Ok(())`
    /// covers both a straight-line instruction and one that faulted into a
    /// handler.
    pub last_exception: Option<u8>,
    /// The form `step` could not execute, if its last call returned
    /// `Err(Stop::Unimplemented(_))`; cleared at the start of each `step`.
    pub last_unimplemented: Option<Form>,
    /// Perf step 1's pre-decode cache; see `DecodeCache`'s doc comment. Not
    /// architectural state -- reset (`Cpu::new`/`reset`) always starts it
    /// empty -- but included in the derived `PartialEq`/`Clone` like any
    /// other field (no caller compares/clones a `Cpu` mid-run today; if one
    /// starts to, an equality check that looks past this field would need
    /// a manual `impl PartialEq` instead of the derive).
    decode_cache: DecodeCache,
    /// Perf step 3's deferred N/Z/V; see `PendingNzv`'s doc comment. Not
    /// architectural state, same reasoning as `decode_cache`: always
    /// `PendingNzv::None` immediately after `resolve_nzv`, and any code
    /// that reads `sr`'s N/Z/V bits calls it first, so `sr` and
    /// `pending_nzv` together always mean the same machine state a fully
    /// eager `Cpu` would be in -- `sr` alone just may not show it yet.
    pending_nzv: PendingNzv,
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
            last_exception: None,
            last_unimplemented: None,
            decode_cache: DecodeCache::default(),
            pending_nzv: PendingNzv::None,
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

    /// CACR bit 5 (RM p.169 Table 6-3 EUSP; CFPRM p.286 calls the same bit
    /// DSPE): "0 USP disabled, core uses a single stack pointer" -- and it
    /// is 0 at reset, so A7/OTHER_A7 do NOT swap on an SR[S] change unless
    /// software has set this bit (confirmed against Unicorn: the firmware
    /// never executes MOVE to/from USP, ISA_B's only other EUSP-gated
    /// feature, so it never turns EUSP on).
    const CACR_EUSP: u32 = 0x20;

    fn set_sr(&mut self, v: u16) {
        // switching between supervisor and user swaps A7 and OTHER_A7, but
        // only when the core has dual stack pointers enabled
        if (self.sr ^ v) & sr::S != 0 && self.ctrl.cacr & Self::CACR_EUSP != 0 {
            core::mem::swap(&mut self.a[7], &mut self.other_a7);
        }
        self.sr = v;
        // `v` fully specifies every CCR bit (it's a whole new SR, from
        // MOVE to SR, RTE's popped frame, or `exception`'s own already-
        // resolved `old`); a pending N/Z/V computation from before this
        // write must not be allowed to land on top of it later.
        self.pending_nzv = PendingNzv::None;
    }

    /// Exception entry (CFPRM p.286-287 11.1.2): a two-longword frame on the
    /// supervisor stack, format 4-7 recording A7[1:0], then PC from VBR.
    fn exception(&mut self, bus: &mut impl Bus, vec: u8, pc: u32) -> Result<(), Stop> {
        self.last_exception = Some(vec);
        self.resolve_nzv(); // `old` below is pushed to the stack, so it
        // needs sr's real, current N/Z/V, not a stale value.
        let old = self.sr;
        self.set_sr((old | sr::S) & !(sr::T | sr::M));
        let sp = self.a[7];
        let format = 4 + (sp & 3);
        let sp = (sp & !3).wrapping_sub(8);
        self.a[7] = sp;
        let fv = format << 28 | (vec as u32) << 18 | old as u32;
        self.decode_cache.invalidate(sp, 8);
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
        self.last_exception = None;
        self.last_unimplemented = None;
        if self.state == RunState::Halted {
            return Err(Stop::Halted);
        }
        let pc = self.pc;
        // Pre-decode cache: a hit needs no bus access at all (the cached
        // record was produced by exactly this fetch+decode on a past miss,
        // and every write this core issues evicts the page it lands in --
        // see `DecodeCache`'s doc comment), so it skips straight to the
        // privilege/unit checks below.
        let insn = if let Some(i) = self.decode_cache.get(pc) {
            i
        } else {
            match self.fetch(bus) {
                Ok(Some(i)) => {
                    self.decode_cache.insert(pc, i);
                    i
                }
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
                if let Stop::Unimplemented(f) = stop {
                    // nothing was executed: leave the core at the instruction
                    self.pc = pc;
                    self.icount -= 1;
                    self.last_unimplemented = Some(f);
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
            _ => {
                let len = match size {
                    Size::B => 1,
                    Size::W => 2,
                    _ => 4,
                };
                self.decode_cache.invalidate(addr, len);
                match size {
                    Size::B => bus.write8(addr, v as u8)?,
                    Size::W => bus.write16(addr, v as u16)?,
                    _ => bus.write32(addr, v)?,
                }
            }
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
        self.decode_cache.invalidate(self.a[7], 4);
        bus.write32(self.a[7], v)?;
        Ok(())
    }

    fn pop32(&mut self, bus: &mut impl Bus) -> Result<u32, Exc> {
        let v = bus.read32(self.a[7])?;
        self.a[7] = self.a[7].wrapping_add(4);
        Ok(v)
    }

    // -- condition codes (CFPRM chapter 4 condition code tables) -------------

    /// Apply whatever's in `pending_nzv` to `sr` and clear it. A no-op
    /// (one comparison, no write) when nothing is pending -- the common
    /// case for straight-line code between branches/CCR reads. Every
    /// producer below is written so its N/Z/V computation depends only on
    /// its own operands, never on `sr`'s current N/Z/V, so resolving late
    /// (or not at all, if a later producer overwrites `pending_nzv`
    /// unread) gives exactly the same bits eager evaluation would have
    /// written at the time -- see `PendingNzv`'s doc comment for the two
    /// producers (`flags_addx`/`flags_subx`) that don't fit that pattern
    /// and stay fully eager instead.
    ///
    /// Public because `sr` is a public field and laziness is meant to
    /// persist *across* `step` calls (that's the whole benefit, in a hot
    /// loop): `step` does not call this itself, so any caller that reads
    /// `sr` between `step` calls -- not just this crate's own internal
    /// consumers, which all call it already -- must call it first to see
    /// N/Z/V that `step`'s own doc-visible behaviour has actually settled
    /// on, not a stale value left over from an earlier instruction.
    pub fn resolve_nzv(&mut self) {
        let p = self.pending_nzv;
        if p == PendingNzv::None {
            return;
        }
        self.pending_nzv = PendingNzv::None;
        let mut ccr = self.sr & !(sr::N | sr::Z | sr::V);
        match p {
            PendingNzv::None => unreachable!(),
            PendingNzv::Nz { v, size } => {
                let (m, s) = mask_sign(size);
                if v & m == 0 {
                    ccr |= sr::Z;
                }
                if v & s != 0 {
                    ccr |= sr::N;
                }
            }
            PendingNzv::Add { src, dst, r, size } => {
                let (_, s) = mask_sign(size);
                if r == 0 {
                    ccr |= sr::Z;
                }
                if r & s != 0 {
                    ccr |= sr::N;
                }
                if (src ^ r) & (dst ^ r) & s != 0 {
                    ccr |= sr::V;
                }
            }
            PendingNzv::Sub { src, dst, r, size } => {
                let (_, s) = mask_sign(size);
                if r == 0 {
                    ccr |= sr::Z;
                }
                if r & s != 0 {
                    ccr |= sr::N;
                }
                if (src ^ dst) & (r ^ dst) & s != 0 {
                    ccr |= sr::V;
                }
            }
            PendingNzv::Shift { v, r, size } => {
                let (m, s) = mask_sign(size);
                if r & m == 0 {
                    ccr |= sr::Z;
                }
                if r & s != 0 {
                    ccr |= sr::N;
                }
                if v {
                    ccr |= sr::V;
                }
            }
        }
        self.sr = ccr;
    }

    fn set_nz(&mut self, v: u32, size: Size) {
        self.sr &= !sr::C;
        self.pending_nzv = PendingNzv::Nz { v, size };
    }

    /// Flags of dst + src = r (ADD, ADDQ): X and C are the carry, computed
    /// (and written) immediately; N/Z/V are deferred (`PendingNzv::Add`).
    fn flags_add(&mut self, src: u32, dst: u32, r: u32, size: Size) {
        let (m, s) = mask_sign(size);
        let (src, dst, r) = (src & m, dst & m, r & m);
        if (src & dst | !r & (src | dst)) & s != 0 {
            self.sr |= sr::C | sr::X;
        } else {
            self.sr &= !(sr::C | sr::X);
        }
        self.pending_nzv = PendingNzv::Add { src, dst, r, size };
    }

    /// Flags of dst - src = r. `cmp` leaves X unchanged (CMP, CMPA); C is
    /// always freshly computed either way and written immediately, same as
    /// `flags_add`. N/Z/V are deferred (`PendingNzv::Sub`).
    fn flags_sub(&mut self, src: u32, dst: u32, r: u32, size: Size, cmp: bool) {
        let (m, s) = mask_sign(size);
        let (src, dst, r) = (src & m, dst & m, r & m);
        let c = (src & !dst | r & !dst | src & r) & s != 0;
        if cmp {
            if c {
                self.sr |= sr::C;
            } else {
                self.sr &= !sr::C;
            }
        } else if c {
            self.sr |= sr::C | sr::X;
        } else {
            self.sr &= !(sr::C | sr::X);
        }
        self.pending_nzv = PendingNzv::Sub { src, dst, r, size };
    }

    /// Flags of dst + src + X_in = r (ADDX, CFPRM p.75): Z is cleared if `r`
    /// is nonzero, otherwise left unchanged, so a chain of ADDX only ever
    /// clears Z (the last one tests whether the whole chain summed to
    /// zero); X mirrors C. `src` already includes X_in (the caller adds it).
    /// Unlike the other flags_* functions, this stays fully eager: its own
    /// Z bit depends on `sr`'s *current* Z (the preserve rule above), so it
    /// resolves any pending N/Z/V first rather than deferring its own.
    fn flags_addx(&mut self, src: u32, dst: u32, r: u32, size: Size) {
        self.resolve_nzv();
        let (m, s) = mask_sign(size);
        let (src, dst, r) = (src & m, dst & m, r & m);
        let mut ccr = self.sr & !(sr::N | sr::V | sr::C | sr::X);
        if r & s != 0 {
            ccr |= sr::N;
        }
        if r != 0 {
            ccr &= !sr::Z;
        }
        if (src ^ r) & (dst ^ r) & s != 0 {
            ccr |= sr::V;
        }
        if (src & dst | !r & (src | dst)) & s != 0 {
            ccr |= sr::C | sr::X;
        }
        self.sr = ccr;
    }

    /// Flags of dst - src - X_in = r (SUBX/NEGX, CFPRM p.127,145): same Z
    /// rule (and same "stays eager" reasoning) as `flags_addx`; `src`
    /// already includes X_in.
    fn flags_subx(&mut self, src: u32, dst: u32, r: u32, size: Size) {
        self.resolve_nzv();
        let (m, s) = mask_sign(size);
        let (src, dst, r) = (src & m, dst & m, r & m);
        let mut ccr = self.sr & !(sr::N | sr::V | sr::C | sr::X);
        if r & s != 0 {
            ccr |= sr::N;
        }
        if r != 0 {
            ccr &= !sr::Z;
        }
        if (src ^ dst) & (r ^ dst) & s != 0 {
            ccr |= sr::V;
        }
        if (src & !dst | r & !dst | src & r) & s != 0 {
            ccr |= sr::C | sr::X;
        }
        self.sr = ccr;
    }

    /// Shift result flags (CFPRM p.79-80 ASx, p.109-110 LSx): N/Z from the
    /// result; C and X take the last bit shifted out, except that a count of
    /// 0 clears C but leaves X unaffected (already eager/accurate, so
    /// leaving it alone is correct with no resolve); V is set only by ASL,
    /// if the msb changed value at any point during the shift. C/X are
    /// written immediately; N/Z/V are deferred (`PendingNzv::Shift`).
    fn flags_shift(&mut self, count: u32, last_out: bool, v: bool, r: u32, size: Size) {
        if count != 0 {
            if last_out {
                self.sr |= sr::C | sr::X;
            } else {
                self.sr &= !(sr::C | sr::X);
            }
        } else {
            self.sr &= !sr::C;
        }
        self.pending_nzv = PendingNzv::Shift { v, r, size };
    }

    /// Shift `v` (size bits) by `count` one bit at a time (CFPRM p.79-80,
    /// 109-110). Returns (result, last bit shifted out, ASL's V: the msb
    /// changed value at some point during the shift).
    fn shift(&self, v: u32, count: u32, left: bool, arith: bool, size: Size) -> (u32, bool, bool) {
        let (m, s) = mask_sign(size);
        let mut v = v & m;
        let mut last = false;
        let mut vflag = false;
        for _ in 0..count {
            if left {
                last = v & s != 0;
                let before_sign = v & s != 0;
                v = (v << 1) & m;
                // V is ASL-only (CFPRM p.79-80): LSL never sets it.
                if arith && (v & s != 0) != before_sign {
                    vflag = true;
                }
            } else if arith {
                last = v & 1 != 0;
                let sign = v & s;
                v = (v >> 1) | sign;
            } else {
                last = v & 1 != 0;
                v >>= 1;
            }
        }
        (v & m, last, vflag)
    }

    // -- control registers (MOVEC, CFPRM p.249-250; RM p.89-90 Table 3-1) ---

    /// Not all control registers are modelled (RGPIOBAR and VBR are the only
    /// ones the firmware writes, per the P3 stage-1 census); an unmodelled
    /// but recognised register is accepted and discarded, matching "Attempted
    /// access to ... an unimplemented control register produces undefined
    /// results" (CFPRM p.249).
    fn write_ctrl(&mut self, rc: u16, v: u32) {
        match rc {
            0x002 => self.ctrl.cacr = v,
            0x003 => self.ctrl.asid = v,
            0x004..=0x007 => self.ctrl.acr[(rc - 0x004) as usize] = v,
            0x008 => self.ctrl.mmubar = v,
            0x009 => self.ctrl.rgpiobar = v,
            0x00c..=0x00f => self.ctrl.acr[(rc - 0x00c + 4) as usize] = v,
            0x800 => self.other_a7 = v,
            0x801 => self.ctrl.vbr = v,
            0x80e => self.set_sr(v as u16),
            0x80f => self.pc = v,
            0xc04 | 0xc05 => self.ctrl.rambar = v,
            _ => {}
        }
    }

    // -- EMAC (CFPRM chapter 6; RM chapter 5) --------------------------------

    /// The 16-bit extension register of accumulator `n` (CFPRM p.178-179):
    /// ACCext01 holds ACC0's extension in its low half and ACC1's in its
    /// high half; ACCext23 holds ACC2 low, ACC3 high.
    fn ext16(&self, n: usize) -> u16 {
        let w = if n < 2 {
            self.emac.accext01
        } else {
            self.emac.accext23
        };
        if n & 1 == 0 {
            w as u16
        } else {
            (w >> 16) as u16
        }
    }

    fn set_ext16(&mut self, n: usize, v: u16) {
        let w = if n < 2 {
            &mut self.emac.accext01
        } else {
            &mut self.emac.accext23
        };
        *w = if n & 1 == 0 {
            (*w & 0xffff_0000) | v as u32
        } else {
            (*w & 0x0000_ffff) | (v as u32) << 16
        };
    }

    fn pav_bit(n: usize) -> u32 {
        1 << (8 + n)
    }

    fn get_pav(&self, n: usize) -> bool {
        self.emac.macsr & Self::pav_bit(n) != 0
    }

    fn set_pav(&mut self, n: usize, v: bool) {
        if v {
            self.emac.macsr |= Self::pav_bit(n);
        } else {
            self.emac.macsr &= !Self::pav_bit(n);
        }
    }

    /// The operand bits for a MAC source register (CFPRM p.171 U/Lx, U/Ly):
    /// the raw register value, or the selected 16-bit half (in the result's
    /// low bits) for a word-sized operation.
    fn select_mac_reg(&self, r: u8, upper: bool, word: bool) -> u32 {
        let v = if r < 8 {
            self.d[r as usize]
        } else {
            self.a[(r - 8) as usize]
        };
        if word {
            if upper { v >> 16 } else { v & 0xffff }
        } else {
            v
        }
    }

    /// The 48-bit "complete accumulator" (RM p.150-151): the concatenation
    /// of ACCn and its extension is laid out differently in integer mode
    /// ({ext16, acc32}) and fractional mode ({ext_hi8, acc32, ext_lo8}).
    fn acc48(&self, n: usize) -> u64 {
        let acc32 = self.emac.acc[n] as u64;
        let ext = self.ext16(n) as u64;
        if self.emac.macsr & 0x20 != 0 {
            ((ext >> 8) << 40) | (acc32 << 8) | (ext & 0xff)
        } else {
            (ext << 32) | acc32
        }
    }

    fn set_acc48(&mut self, n: usize, v: u64) {
        let v = v & 0xffff_ffff_ffff;
        if self.emac.macsr & 0x20 != 0 {
            self.emac.acc[n] = ((v >> 8) & 0xffff_ffff) as u32;
            self.set_ext16(n, (((v >> 40) & 0xff) << 8 | (v & 0xff)) as u16);
        } else {
            self.emac.acc[n] = v as u32;
            self.set_ext16(n, (v >> 32) as u16);
        }
    }

    /// Set MACSR's N/Z/V from the final accumulator value and PAVn, and its
    /// EV flag: "set if accumulation overflows the lower 32 bits in integer
    /// mode or the lower 40 bits in fractional mode" (CFPRM MAC/MSAC pages).
    fn mac_flags(&mut self, acc: usize) {
        let v48 = self.acc48(acc);
        let fractional = self.emac.macsr & 0x20 != 0;
        let pav = self.get_pav(acc);
        let mut m = self.emac.macsr & !0x0e; // clear N,Z,V (bits 3,2,1)
        if v48 == 0 {
            m |= 0x04;
        }
        if v48 & 0x8000_0000_0000 != 0 {
            m |= 0x08;
        }
        if pav {
            m |= 0x02;
        }
        let ev = if fractional {
            let top = (v48 >> 39) & 0x1ff;
            !(top == 0 || top == 0x1ff)
        } else {
            let top = (v48 >> 31) & 0x1ffff;
            !(top == 0 || top == 0x1ffff)
        };
        // EV has no CCR/MACSR bit of its own in this model (not read by any
        // instruction the firmware uses); computed for documentation only.
        let _ = ev;
        self.emac.macsr = m;
    }

    /// One MAC/MSAC compute (RM p.156-161 pseudocode, the manual's arbiter
    /// for this whole unit): multiply the selected 16- or 32-bit operand
    /// bits `ry`/`rx` per the mode in MACSR[S/U,F/I], scale (integer modes
    /// only), and add (`sub=false`) or subtract (`sub=true`) into
    /// accumulator `acc`. Firmware (both images, P3 stage-1 census) only
    /// ever runs with MACSR = 0x00 (signed integer) or 0x20 (signed
    /// fractional, OMC=0); the unsigned-integer path and the two
    /// saturating (OMC=1) paths are transcribed from the same pseudocode
    /// but are not exercised by either image, so are unverified by lockstep.
    fn mac_op(&mut self, ry: u32, rx: u32, word: bool, sf: u8, acc: usize, sub: bool) {
        let macsr = self.emac.macsr;
        let omc = macsr & 0x80 != 0;
        let su = macsr & 0x40 != 0;
        let fi = macsr & 0x20 != 0;
        if omc && self.get_pav(acc) {
            // Sticky saturated overflow with saturation enabled: RM p.156's
            // outer "if (OMC==0 || PAVn==0)" is false, so this instruction's
            // arithmetic does not run at all; only the flags below apply.
            self.mac_flags(acc);
            return;
        }
        self.set_pav(acc, false);
        if fi {
            // signed fractional (RM p.158-159): 32-bit operands only scale
            // implicitly (product << 1); SF is ignored (RM p.154 5.3.1.4).
            let (opy, opx): (i64, i64) = if word {
                (
                    ((ry as u16 as i16 as i32) << 16) as i64,
                    ((rx as u16 as i16 as i32) << 16) as i64,
                )
            } else {
                (ry as i32 as i64, rx as i32 as i64)
            };
            let p64 = (opy.wrapping_mul(opx) as u64) << 1;
            // -1 * -1 special case: the doubled product would be the sole
            // value that does not fit in 64 signed bits; the manual defines
            // it to zero-fill instead of sign-extend the top byte.
            let minus_one = 0x8000_0000u32;
            let special = (opy == minus_one as i32 as i64) && (opx == minus_one as i32 as i64);
            let ext72_top8 = if special {
                0u64
            } else if p64 & 0x8000_0000_0000_0000 != 0 {
                0xff
            } else {
                0
            };
            // product[71:24]: the 40-bit kept product with its sign/zero-fill
            // extension already attached, so a rounding carry (below) ripples
            // through the full 48 bits the way an adder would.
            let mut prod48 = (ext72_top8 << 40) | ((p64 >> 24) & 0xffff_ffff_ffff);
            if macsr & 0x10 != 0 {
                // R/T = round (RM p.152-153 round-to-nearest-even on the
                // bits shifted away, product[23:0]).
                let frac = p64 & 0xff_ffff;
                if frac > 0x80_0000 || (frac == 0x80_0000 && prod48 & 1 != 0) {
                    prod48 = prod48.wrapping_add(1) & 0xffff_ffff_ffff;
                }
            }
            let old = self.acc48(acc);
            let result = if sub {
                old.wrapping_sub(prod48)
            } else {
                old.wrapping_add(prod48)
            } & 0xffff_ffff_ffff;
            let sign_old = old & 0x8000_0000_0000 != 0;
            let sign_p = prod48 & 0x8000_0000_0000 != 0;
            let sign_p_eff = if sub { !sign_p } else { sign_p };
            let sign_r = result & 0x8000_0000_0000 != 0;
            if sign_old == sign_p_eff && sign_r != sign_old {
                self.set_pav(acc, true);
                if omc {
                    self.set_acc48(
                        acc,
                        if sign_r {
                            0x007f_ffff_ff00
                        } else {
                            0xff80_0000_0000
                        },
                    );
                } else {
                    self.set_acc48(acc, result);
                }
            } else {
                self.set_acc48(acc, result);
            }
        } else if su {
            // unsigned integer (RM p.159-161): not exercised by either
            // image (census: only MACSR 0x00 and 0x20 are ever loaded).
            let (opy, opx): (u64, u64) = if word {
                ((ry & 0xffff) as u64, (rx & 0xffff) as u64)
            } else {
                (ry as u64, rx as u64)
            };
            let p64 = opy * opx;
            let overflow = (p64 >> 40) != 0;
            if overflow {
                self.set_pav(acc, true);
                if omc {
                    self.set_acc48(acc, if sub { 0 } else { 0xffff_ffff_ffff });
                }
                // OMC==0: accumulator left unchanged (see the signed-integer
                // branch's comment on the same manual gap).
            } else {
                let mut prod48 = p64 & 0xffff_ffff_ffff;
                prod48 = match sf {
                    1 => (prod48 << 1) & 0xffff_ffff_ffff,
                    3 => prod48 >> 1,
                    _ => prod48,
                };
                let old = self.acc48(acc);
                let result = if sub {
                    old.wrapping_sub(prod48)
                } else {
                    old.wrapping_add(prod48)
                } & 0xffff_ffff_ffff;
                // unsigned accumulation overflow: a carry/borrow out of bit 47
                let acc_overflow = if sub {
                    old < prod48
                } else {
                    old + prod48 > 0xffff_ffff_ffff
                };
                if acc_overflow {
                    self.set_pav(acc, true);
                    self.set_acc48(
                        acc,
                        if omc {
                            if sub { 0 } else { 0xffff_ffff_ffff }
                        } else {
                            result
                        },
                    );
                } else {
                    self.set_acc48(acc, result);
                }
            }
        } else {
            // signed integer (RM p.156-158)
            let (opy, opx): (i64, i64) = if word {
                (ry as u16 as i16 as i64, rx as u16 as i16 as i64)
            } else {
                (ry as i32 as i64, rx as i32 as i64)
            };
            let p64 = opy.wrapping_mul(opx) as u64;
            let top25 = (p64 >> 39) & 0x1ff_ffff;
            let overflow = top25 != 0 && top25 != 0x1ff_ffff;
            if overflow {
                self.set_pav(acc, true);
                if omc {
                    let sign = p64 >> 63 & 1 != 0;
                    let sat = if sign == sub {
                        0x0000_7fff_ffffu64
                    } else {
                        0xffff_8000_0000u64
                    };
                    self.set_acc48(acc, sat);
                }
                // OMC==0: accumulator left unchanged; RM p.157-158's
                // pseudocode never assigns `result` on this path, so the
                // only defined effect is on the flags (set below).
            } else {
                let p40 = p64 & 0xff_ffff_ffff;
                let mut prod48 = if p40 & 0x80_0000_0000 != 0 {
                    0xff00_0000_0000 | p40
                } else {
                    p40
                };
                prod48 = match sf {
                    1 => (prod48 << 1) & 0xffff_ffff_ffff,
                    3 => {
                        let sign = prod48 & 0x8000_0000_0000 != 0;
                        (prod48 >> 1) | if sign { 0x8000_0000_0000 } else { 0 }
                    }
                    _ => prod48,
                };
                let old = self.acc48(acc);
                let result = if sub {
                    old.wrapping_sub(prod48)
                } else {
                    old.wrapping_add(prod48)
                } & 0xffff_ffff_ffff;
                let sign_old = old & 0x8000_0000_0000 != 0;
                let sign_p = prod48 & 0x8000_0000_0000 != 0;
                let sign_p_eff = if sub { !sign_p } else { sign_p };
                let sign_r = result & 0x8000_0000_0000 != 0;
                if sign_old == sign_p_eff && sign_r != sign_old {
                    self.set_pav(acc, true);
                    if omc {
                        self.set_acc48(
                            acc,
                            if sign_r {
                                0x0000_7fff_ffff
                            } else {
                                0xffff_8000_0000
                            },
                        );
                    } else {
                        self.set_acc48(acc, result);
                    }
                } else {
                    self.set_acc48(acc, result);
                }
            }
        }
        self.mac_flags(acc);
    }

    /// MOVCLR/MOVE-from-ACC store value (CFPRM p.174-177 pseudocode, the
    /// manual's arbiter): reduce the 48-bit accumulator to the 32-bit value
    /// a register move sees, per MACSR's mode, saturation and rounding
    /// bits. Firmware (both images) only runs this with MACSR = 0x00 or
    /// 0x20 (OMC=0, R/T=0), so only those two branches are lockstep-checked
    /// so far; the rest is transcribed from the same pseudocode.
    fn acc_to_reg(&self, acc: usize) -> u32 {
        let v = self.acc48(acc);
        let macsr = self.emac.macsr;
        let omc = macsr & 0x80 != 0;
        let su = macsr & 0x40 != 0;
        let fi = macsr & 0x20 != 0;
        let rt = macsr & 0x10 != 0;
        // Round-to-nearest-even (RM p.152-153): `hi` is the value kept,
        // `frac` the `keep` low bits being rounded away.
        let round = |bits: u64, keep: u32| -> u64 {
            let half = 1u64 << (keep - 1);
            let frac = bits & ((1 << keep) - 1);
            let hi = bits >> keep;
            if frac > half || (frac == half && hi & 1 != 0) {
                hi + 1
            } else {
                hi
            }
        };
        if !fi && !su {
            // signed integer (CFPRM p.174, MACSR[6:5]==00)
            if !omc {
                return v as u32;
            }
            let guard = (v >> 31) & 0x1ffff; // ACC[47:31], 17 bits
            if guard == 0 || guard == 0x1ffff {
                v as u32
            } else if v & 0x8000_0000_0000 == 0 {
                0x7fff_ffff
            } else {
                0x8000_0000
            }
        } else if !fi {
            // unsigned integer (MACSR[6:5]==10)
            if !omc {
                return v as u32;
            }
            let guard = (v >> 32) & 0xffff; // ACC[47:32], 16 bits
            if guard == 0 { v as u32 } else { 0xffff_ffff }
        } else if !omc && !su && !rt {
            (v >> 8) as u32 // ACC[39:8], no rounding, no saturation
        } else if !omc && !su {
            round(v, 8) as u32 // ACC[39:8] rounded by ACC[7:0]
        } else if !omc {
            // 16-bit rounding: ACC[39:24] rounded by ACC[23:0], into Rx[15:0]
            (round(v, 24) as u32) & 0xffff
        } else if !su && !rt {
            let guard = (v >> 39) & 0x1ff; // ACC[47:39], 9 bits
            if guard == 0 || guard == 0x1ff {
                (v >> 8) as u32
            } else if v & 0x8000_0000_0000 == 0 {
                0x7fff_ffff
            } else {
                0x8000_0000
            }
        } else if !su {
            // Temp[47:8] = ACC[47:8] rounded by ACC[7:0]; temp's bit k is
            // original bit k+8, so Temp[47:39] is temp[39:31].
            let temp = round(v, 8);
            let guard = (temp >> 31) & 0x1ff;
            if guard == 0 || guard == 0x1ff {
                temp as u32
            } else if temp & 0x80_0000_0000 == 0 {
                0x7fff_ffff
            } else {
                0x8000_0000
            }
        } else {
            // Temp[47:24] = ACC[47:24] rounded by ACC[23:0]; temp's bit k is
            // original bit k+24, so Temp[47:39] is temp[23:15].
            let temp = round(v, 24);
            let guard = (temp >> 15) & 0x1ff;
            if guard == 0 || guard == 0x1ff {
                (temp as u32) & 0xffff
            } else if temp & 0x80_0000 == 0 {
                0x0000_7fff
            } else {
                0x0000_8000
            }
        }
    }

    /// Bcc/Scc condition (CFPRM p.82 Bcc condition table).
    fn cond(&mut self, c: u8) -> bool {
        self.resolve_nzv();
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

            // -- immediate logic/arithmetic on Dn (CFPRM p.132,78,143,73,102) --
            Form::Ori | Form::Andi | Form::Subi | Form::Addi | Form::Eori => {
                let v = imm(0);
                let Ea::Dn(r) = ea(1) else { unreachable!() };
                let d = self.d[r as usize];
                let (result, is_add, is_sub) = match i.form {
                    Form::Ori => (d | v, false, false),
                    Form::Andi => (d & v, false, false),
                    Form::Eori => (d ^ v, false, false),
                    Form::Addi => (d.wrapping_add(v), true, false),
                    _ => (d.wrapping_sub(v), false, true),
                };
                self.d[r as usize] = result;
                if is_add {
                    self.flags_add(v, d, result, Size::L);
                } else if is_sub {
                    self.flags_sub(v, d, result, Size::L, false);
                } else {
                    self.set_nz(result, Size::L);
                }
            }
            Form::Cmpi => {
                let v = imm(0);
                let d = self.read(bus, &ea(1), size).map_err(Ok)?;
                self.flags_sub(v, d, d.wrapping_sub(v), size, true);
            }

            // -- bit instructions (CFPRM p.83-92) --
            Form::BtstR
            | Form::BchgR
            | Form::BclrR
            | Form::BsetR
            | Form::BtstI
            | Form::BchgI
            | Form::BclrI
            | Form::BsetI => {
                let bitnum = match i.form {
                    Form::BtstR | Form::BchgR | Form::BclrR | Form::BsetR => {
                        let Ea::Dn(r) = ea(0) else { unreachable!() };
                        self.d[r as usize]
                    }
                    _ => imm(0),
                };
                let dst = ea(1);
                let bit = bitnum & (if size == Size::L { 31 } else { 7 });
                let addr = match dst {
                    Ea::Dn(_) => 0,
                    _ => self.ea_addr(&dst, size).map_err(Ok)?,
                };
                let old = match dst {
                    Ea::Dn(r) => self.d[r as usize],
                    _ => match size {
                        Size::B => bus.read8(addr).map_err(|e| Ok(e.into()))? as u32,
                        _ => bus.read32(addr).map_err(|e| Ok(e.into()))?,
                    },
                };
                let mask = 1u32 << bit;
                // Preserves N/V (and C/X); N/V must be resolved first, or
                // this write would freeze whatever was pending in place
                // and a later `resolve_nzv` would clobber this Z with a
                // stale recomputation.
                self.resolve_nzv();
                self.sr = if old & mask == 0 {
                    self.sr | sr::Z
                } else {
                    self.sr & !sr::Z
                };
                if !matches!(i.form, Form::BtstR | Form::BtstI) {
                    let new = match i.form {
                        Form::BchgR | Form::BchgI => old ^ mask,
                        Form::BclrR | Form::BclrI => old & !mask,
                        _ => old | mask,
                    };
                    self.write_at(bus, &dst, addr, size, new).map_err(Ok)?;
                }
            }

            // -- moves and register ops (CFPRM p.245-248,118-119,126-129,103,146) --
            Form::MoveFromSr => {
                self.resolve_nzv();
                self.write(bus, &ea(1), size, self.sr as u32).map_err(Ok)?;
            }
            Form::MoveToSr => {
                let v = self.read(bus, &ea(0), size).map_err(Ok)?;
                self.set_sr(v as u16);
            }
            Form::MoveFromCcr => {
                self.resolve_nzv();
                self.write(bus, &ea(1), size, (self.sr & sr::CCR) as u32)
                    .map_err(Ok)?;
            }
            Form::MoveToCcr => {
                let v = self.read(bus, &ea(0), size).map_err(Ok)?;
                // Fully overwrites every CCR bit from `v`; no dependency on
                // (and so no need to resolve) whatever was pending.
                self.sr = (self.sr & !sr::CCR) | (v as u16 & sr::CCR);
                self.pending_nzv = PendingNzv::None;
            }
            Form::MoveToUsp => {
                let Ea::An(r) = ea(0) else { unreachable!() };
                self.other_a7 = self.a[r as usize];
            }
            Form::MoveFromUsp => {
                let Ea::An(r) = ea(1) else { unreachable!() };
                self.a[r as usize] = self.other_a7;
            }
            Form::Neg => {
                let Ea::Dn(r) = ea(0) else { unreachable!() };
                let d = self.d[r as usize];
                let v = 0u32.wrapping_sub(d);
                self.d[r as usize] = v;
                self.flags_sub(d, 0, v, Size::L, false);
            }
            Form::Negx => {
                let Ea::Dn(r) = ea(0) else { unreachable!() };
                let d = self.d[r as usize];
                let x = (self.sr & sr::X != 0) as u32;
                let src = d.wrapping_add(x);
                let v = 0u32.wrapping_sub(src);
                self.d[r as usize] = v;
                self.flags_subx(src, 0, v, Size::L);
            }
            Form::Not => {
                let Ea::Dn(r) = ea(0) else { unreachable!() };
                let v = !self.d[r as usize];
                self.d[r as usize] = v;
                self.set_nz(v, Size::L);
            }
            Form::Swap => {
                let Ea::Dn(r) = ea(0) else { unreachable!() };
                let v = self.d[r as usize].rotate_left(16);
                self.d[r as usize] = v;
                self.set_nz(v, Size::L);
            }
            Form::ExtW => {
                let Ea::Dn(r) = ea(0) else { unreachable!() };
                let d = &mut self.d[r as usize];
                let w = (*d as u8 as i8 as i16 as u16) as u32;
                *d = (*d & 0xffff_0000) | w;
                self.set_nz(w, Size::W);
            }
            Form::ExtL => {
                let Ea::Dn(r) = ea(0) else { unreachable!() };
                let l = self.d[r as usize] as u16 as i16 as i32 as u32;
                self.d[r as usize] = l;
                self.set_nz(l, Size::L);
            }
            Form::ExtbL => {
                let Ea::Dn(r) = ea(0) else { unreachable!() };
                let l = self.d[r as usize] as u8 as i8 as i32 as u32;
                self.d[r as usize] = l;
                self.set_nz(l, Size::L);
            }
            Form::Sats => {
                // CFPRM p.138: only touches Dx when CCR[V] is already set
                // (saturating an accumulation this SATS follows), but N/Z
                // are (re)computed from Dx either way ("condition codes are
                // set according to the result").
                let Ea::Dn(r) = ea(0) else { unreachable!() };
                self.resolve_nzv();
                if self.sr & sr::V != 0 {
                    let d = self.d[r as usize];
                    self.d[r as usize] = if d & 0x8000_0000 == 0 {
                        0x8000_0000
                    } else {
                        0x7fff_ffff
                    };
                }
                self.set_nz(self.d[r as usize], Size::L);
            }
            Form::Tas => {
                let dst = ea(0);
                let addr = match dst {
                    Ea::Dn(_) => 0,
                    _ => self.ea_addr(&dst, Size::B).map_err(Ok)?,
                };
                let old = match dst {
                    Ea::Dn(r) => self.d[r as usize] as u8,
                    _ => bus.read8(addr).map_err(|e| Ok(e.into()))?,
                };
                self.set_nz(old as u32, Size::B);
                self.write_at(bus, &dst, addr, Size::B, (old | 0x80) as u32)
                    .map_err(Ok)?;
            }

            // -- MOVEM (CFPRM p.115-116) --
            Form::MovemStore => {
                let Operand::RegList(mask) = ops[0] else {
                    unreachable!()
                };
                let mask = mask as u32;
                let target = ea(1);
                // Only (An)+/-(An) auto-update the address register; for the
                // other modes ColdFire allows (CFPRM p.115-116: (An) and
                // (d16,An)) the transfer still walks four bytes at a time
                // from a fixed base, computed once.
                let auto = matches!(target, Ea::Pre(_) | Ea::Post(_));
                let predec = matches!(target, Ea::Pre(_));
                let base = if auto {
                    0
                } else {
                    self.ea_addr(&target, Size::L).map_err(Ok)?
                };
                let mut k = 0u32;
                for bit in 0..16u32 {
                    if mask & (1 << bit) == 0 {
                        continue;
                    }
                    let reg = if predec { 15 - bit } else { bit } as u8;
                    let addr = if auto {
                        self.ea_addr(&target, Size::L).map_err(Ok)?
                    } else {
                        base.wrapping_add(4 * k)
                    };
                    let v = if reg < 8 {
                        self.d[reg as usize]
                    } else {
                        self.a[(reg - 8) as usize]
                    };
                    self.decode_cache.invalidate(addr, 4);
                    bus.write32(addr, v).map_err(|e| Ok(e.into()))?;
                    k += 1;
                }
            }
            Form::MovemLoad => {
                let target = ea(0);
                let Operand::RegList(mask) = ops[1] else {
                    unreachable!()
                };
                let mask = mask as u32;
                let auto = matches!(target, Ea::Pre(_) | Ea::Post(_));
                let predec = matches!(target, Ea::Pre(_));
                let base = if auto {
                    0
                } else {
                    self.ea_addr(&target, Size::L).map_err(Ok)?
                };
                let mut k = 0u32;
                for bit in 0..16u32 {
                    if mask & (1 << bit) == 0 {
                        continue;
                    }
                    let reg = if predec { 15 - bit } else { bit } as u8;
                    let addr = if auto {
                        self.ea_addr(&target, Size::L).map_err(Ok)?
                    } else {
                        base.wrapping_add(4 * k)
                    };
                    let v = bus.read32(addr).map_err(|e| Ok(e.into()))?;
                    if reg < 8 {
                        self.d[reg as usize] = v;
                    } else {
                        self.a[(reg - 8) as usize] = v;
                    }
                    k += 1;
                }
            }

            // -- arithmetic/logic to D or to EA (CFPRM p.70-78,101-102,
            // 130-131,140-145) --
            Form::OrToD | Form::AndToD => {
                let s = self.read(bus, &ea(0), Size::L).map_err(Ok)?;
                let Ea::Dn(r) = ea(1) else { unreachable!() };
                let v = if i.form == Form::OrToD {
                    self.d[r as usize] | s
                } else {
                    self.d[r as usize] & s
                };
                self.d[r as usize] = v;
                self.set_nz(v, Size::L);
            }
            Form::OrToEa | Form::AndToEa | Form::Eor => {
                let Ea::Dn(r) = ea(0) else { unreachable!() };
                let s = self.d[r as usize];
                let dst = ea(1);
                let addr = match dst {
                    Ea::Dn(_) => 0,
                    _ => self.ea_addr(&dst, Size::L).map_err(Ok)?,
                };
                let d = match dst {
                    Ea::Dn(r) => self.d[r as usize],
                    _ => bus.read32(addr).map_err(|e| Ok(e.into()))?,
                };
                let v = match i.form {
                    Form::OrToEa => d | s,
                    Form::AndToEa => d & s,
                    _ => d ^ s,
                };
                self.write_at(bus, &dst, addr, Size::L, v).map_err(Ok)?;
                self.set_nz(v, Size::L);
            }
            Form::SubToEa | Form::AddToEa => {
                let Ea::Dn(r) = ea(0) else { unreachable!() };
                let s = self.d[r as usize];
                let dst = ea(1);
                let addr = match dst {
                    Ea::Dn(_) => 0,
                    _ => self.ea_addr(&dst, Size::L).map_err(Ok)?,
                };
                let d = match dst {
                    Ea::Dn(r) => self.d[r as usize],
                    _ => bus.read32(addr).map_err(|e| Ok(e.into()))?,
                };
                let add = i.form == Form::AddToEa;
                let v = if add {
                    d.wrapping_add(s)
                } else {
                    d.wrapping_sub(s)
                };
                self.write_at(bus, &dst, addr, Size::L, v).map_err(Ok)?;
                if add {
                    self.flags_add(s, d, v, Size::L);
                } else {
                    self.flags_sub(s, d, v, Size::L, false);
                }
            }
            Form::Adda | Form::Suba => {
                let s = self.read(bus, &ea(0), size).map_err(Ok)?;
                let s = if size == Size::W {
                    s as u16 as i16 as i32 as u32
                } else {
                    s
                };
                let Ea::An(r) = ea(1) else { unreachable!() };
                self.a[r as usize] = if i.form == Form::Adda {
                    self.a[r as usize].wrapping_add(s)
                } else {
                    self.a[r as usize].wrapping_sub(s)
                };
            }
            Form::Addx | Form::Subx => {
                let Ea::Dn(y) = ea(0) else { unreachable!() };
                let Ea::Dn(x) = ea(1) else { unreachable!() };
                let xin = (self.sr & sr::X != 0) as u32;
                let src = self.d[y as usize].wrapping_add(xin);
                let dst = self.d[x as usize];
                if i.form == Form::Addx {
                    let v = dst.wrapping_add(src);
                    self.d[x as usize] = v;
                    self.flags_addx(src, dst, v, Size::L);
                } else {
                    let v = dst.wrapping_sub(src);
                    self.d[x as usize] = v;
                    self.flags_subx(src, dst, v, Size::L);
                }
            }

            // -- shifts (CFPRM p.79-80,109-110) --
            Form::AsrI | Form::AslI | Form::LsrI | Form::LslI => {
                let count = imm(0) & 63;
                let Ea::Dn(r) = ea(1) else { unreachable!() };
                let left = matches!(i.form, Form::AslI | Form::LslI);
                let arith = matches!(i.form, Form::AsrI | Form::AslI);
                let (res, last, vflag) =
                    self.shift(self.d[r as usize], count, left, arith, Size::L);
                self.d[r as usize] = res;
                self.flags_shift(count, last, vflag, res, Size::L);
            }
            Form::AsrR | Form::AslR | Form::LsrR | Form::LslR => {
                let Ea::Dn(y) = ea(0) else { unreachable!() };
                let count = self.d[y as usize] & 63;
                let Ea::Dn(r) = ea(1) else { unreachable!() };
                let left = matches!(i.form, Form::AslR | Form::LslR);
                let arith = matches!(i.form, Form::AsrR | Form::AslR);
                let (res, last, vflag) =
                    self.shift(self.d[r as usize], count, left, arith, Size::L);
                self.d[r as usize] = res;
                self.flags_shift(count, last, vflag, res, Size::L);
            }

            // -- multiply/divide (CFPRM p.97-100,120-123,135-136) --
            Form::MuluL | Form::MuluW => {
                let s = self.read(bus, &ea(0), size).map_err(Ok)?;
                let Ea::Dn(r) = ea(1) else { unreachable!() };
                let d = self.d[r as usize];
                let (a, b): (u64, u64) = if size == Size::W {
                    (d as u16 as u64, s as u16 as u64)
                } else {
                    (d as u64, s as u64)
                };
                let v = (a * b) as u32;
                self.d[r as usize] = v;
                self.set_nz(v, Size::L);
            }
            Form::MulsL | Form::MulsW => {
                let s = self.read(bus, &ea(0), size).map_err(Ok)?;
                let Ea::Dn(r) = ea(1) else { unreachable!() };
                let d = self.d[r as usize];
                let (a, b): (i64, i64) = if size == Size::W {
                    (d as u16 as i16 as i64, s as u16 as i16 as i64)
                } else {
                    (d as i32 as i64, s as i32 as i64)
                };
                let v = (a * b) as u32;
                self.d[r as usize] = v;
                self.set_nz(v, Size::L);
            }
            Form::DivuW => {
                let s = self.read(bus, &ea(0), Size::W).map_err(Ok)? as u16 as u32;
                let Ea::Dn(r) = ea(1) else { unreachable!() };
                if s == 0 {
                    return Err(Ok(Exc {
                        vector: vector::DIVIDE_BY_ZERO,
                        pc,
                    }));
                }
                let d = self.d[r as usize];
                let q = d / s;
                if q > 0xffff {
                    // Fully overwrites N/Z/C (to 0) and sets V; no
                    // dependency on old N/Z/C, so no resolve needed first,
                    // but this makes sr authoritative again either way.
                    self.sr = (self.sr & !(sr::N | sr::Z | sr::C)) | sr::V;
                    self.pending_nzv = PendingNzv::None;
                } else {
                    let rem = d % s;
                    self.d[r as usize] = (rem << 16) | q;
                    self.set_nz(q, Size::W);
                }
            }
            Form::DivsW => {
                let s = self.read(bus, &ea(0), Size::W).map_err(Ok)? as u16 as i16;
                let Ea::Dn(r) = ea(1) else { unreachable!() };
                if s == 0 {
                    return Err(Ok(Exc {
                        vector: vector::DIVIDE_BY_ZERO,
                        pc,
                    }));
                }
                let d = self.d[r as usize] as i32;
                let s32 = s as i32;
                let q = d / s32;
                if !(-32768..=32767).contains(&q) {
                    // Fully overwrites N/Z/C (to 0) and sets V; no
                    // dependency on old N/Z/C, so no resolve needed first,
                    // but this makes sr authoritative again either way.
                    self.sr = (self.sr & !(sr::N | sr::Z | sr::C)) | sr::V;
                    self.pending_nzv = PendingNzv::None;
                } else {
                    let rem = d % s32;
                    self.d[r as usize] = ((rem as u32) << 16) | (q as u16 as u32);
                    self.set_nz(q as u16 as u32, Size::W);
                }
            }
            Form::DivuL => {
                let s = self.read(bus, &ea(0), Size::L).map_err(Ok)?;
                let Ea::Dn(r) = ea(1) else { unreachable!() };
                if s == 0 {
                    return Err(Ok(Exc {
                        vector: vector::DIVIDE_BY_ZERO,
                        pc,
                    }));
                }
                let q = self.d[r as usize] / s;
                self.d[r as usize] = q;
                self.set_nz(q, Size::L);
            }
            Form::DivsL => {
                let s = self.read(bus, &ea(0), Size::L).map_err(Ok)? as i32;
                let Ea::Dn(r) = ea(1) else { unreachable!() };
                if s == 0 {
                    return Err(Ok(Exc {
                        vector: vector::DIVIDE_BY_ZERO,
                        pc,
                    }));
                }
                let d = self.d[r as usize] as i32;
                if d == i32::MIN && s == -1 {
                    // Fully overwrites N/Z/C (to 0) and sets V; no
                    // dependency on old N/Z/C, so no resolve needed first,
                    // but this makes sr authoritative again either way.
                    self.sr = (self.sr & !(sr::N | sr::Z | sr::C)) | sr::V;
                    self.pending_nzv = PendingNzv::None;
                } else {
                    let q = d / s;
                    self.d[r as usize] = q as u32;
                    self.set_nz(q as u32, Size::L);
                }
            }
            Form::RemuL => {
                // Confirmed against Unicorn: unlike DIVU.L, the dividend
                // register (Dq/Dx here) is a source only -- REMU.L never
                // writes a quotient back into it ("To determine the
                // quotient, use DIVU", CFPRM p.136); only Dw (the
                // remainder) and the flags (from the quotient) change.
                let s = self.read(bus, &ea(0), Size::L).map_err(Ok)?;
                let Ea::Dn(w) = ea(1) else { unreachable!() };
                let Ea::Dn(q) = ea(2) else { unreachable!() };
                if s == 0 {
                    return Err(Ok(Exc {
                        vector: vector::DIVIDE_BY_ZERO,
                        pc,
                    }));
                }
                let d = self.d[q as usize];
                let (quot, rem) = (d / s, d % s);
                self.d[w as usize] = rem;
                self.set_nz(quot, Size::L);
            }
            Form::RemsL => {
                // See RemuL: the dividend register is never overwritten.
                let s = self.read(bus, &ea(0), Size::L).map_err(Ok)? as i32;
                let Ea::Dn(w) = ea(1) else { unreachable!() };
                let Ea::Dn(q) = ea(2) else { unreachable!() };
                if s == 0 {
                    return Err(Ok(Exc {
                        vector: vector::DIVIDE_BY_ZERO,
                        pc,
                    }));
                }
                let d = self.d[q as usize] as i32;
                if d == i32::MIN && s == -1 {
                    // Fully overwrites N/Z/C (to 0) and sets V; no
                    // dependency on old N/Z/C, so no resolve needed first,
                    // but this makes sr authoritative again either way.
                    self.sr = (self.sr & !(sr::N | sr::Z | sr::C)) | sr::V;
                    self.pending_nzv = PendingNzv::None;
                } else {
                    let (quot, rem) = (d / s, d % s);
                    self.d[w as usize] = rem as u32;
                    self.set_nz(quot as u32, Size::L);
                }
            }

            // -- ISA_C (RM p.97 Table 3-4; not documented as affecting CCR) --
            Form::Bitrev => {
                let Ea::Dn(r) = ea(0) else { unreachable!() };
                self.d[r as usize] = self.d[r as usize].reverse_bits();
            }
            Form::Byterev => {
                let Ea::Dn(r) = ea(0) else { unreachable!() };
                self.d[r as usize] = self.d[r as usize].swap_bytes();
            }
            Form::Ff1 => {
                let Ea::Dn(r) = ea(0) else { unreachable!() };
                self.d[r as usize] = self.d[r as usize].leading_zeros();
            }

            // -- ISA_B moves (CFPRM p.124-125) --
            Form::Mvs => {
                let v = self.read(bus, &ea(0), size).map_err(Ok)?;
                let Ea::Dn(r) = ea(1) else { unreachable!() };
                let sx = match size {
                    Size::B => v as u8 as i8 as i32 as u32,
                    _ => v as u16 as i16 as i32 as u32,
                };
                self.d[r as usize] = sx;
                self.set_nz(sx, Size::L);
            }
            Form::Mvz => {
                let v = self.read(bus, &ea(0), size).map_err(Ok)?;
                let Ea::Dn(r) = ea(1) else { unreachable!() };
                let zx = match size {
                    Size::B => v & 0xff,
                    _ => v & 0xffff,
                };
                self.d[r as usize] = zx;
                self.set_nz(zx, Size::L);
            }

            // -- flow/system (CFPRM p.139,148,238,249-251) --
            Form::Scc => {
                let v = if self.cond(i.cond) { 0xffu32 } else { 0 };
                self.write(bus, &ea(0), Size::B, v).map_err(Ok)?;
            }
            Form::Tpf => {}
            Form::Cpushl => {}
            Form::Movec => {
                let v = self.read(bus, &ea(0), Size::L).map_err(Ok)?;
                let Operand::Ctrl(rc) = ops[1] else {
                    unreachable!()
                };
                self.write_ctrl(rc, v);
            }
            Form::Rte => {
                let fv = self.pop32(bus).map_err(Ok)?;
                let new_pc = self.pop32(bus).map_err(Ok)?;
                let format = fv >> 28;
                if !(4..=7).contains(&format) {
                    return Err(Ok(Exc {
                        vector: vector::FORMAT_ERROR,
                        pc,
                    }));
                }
                self.a[7] = self.a[7].wrapping_add(format.wrapping_sub(4));
                self.set_sr(fv as u16);
                self.pc = new_pc;
            }

            // -- EMAC (CFPRM chapter 6; RM chapter 5) --
            Form::Mac | Form::Msac => {
                let Operand::MacReg { r: ry, upper: uy } = ops[0] else {
                    unreachable!()
                };
                let Operand::MacReg { r: rx, upper: ux } = ops[1] else {
                    unreachable!()
                };
                let Operand::Scale(sf) = ops[2] else {
                    unreachable!()
                };
                let Operand::Acc(acc) = ops[3] else {
                    unreachable!()
                };
                let word = size == Size::W;
                let ry = self.select_mac_reg(ry, uy, word);
                let rx = self.select_mac_reg(rx, ux, word);
                self.mac_op(ry, rx, word, sf, acc as usize, i.form == Form::Msac);
            }
            Form::MacLoad | Form::MsacLoad => {
                let Operand::MacReg { r: ry, upper: uy } = ops[0] else {
                    unreachable!()
                };
                let Operand::MacReg { r: rx, upper: ux } = ops[1] else {
                    unreachable!()
                };
                let Operand::Scale(sf) = ops[2] else {
                    unreachable!()
                };
                let load_ea = ea(3);
                let Operand::MaskFlag(use_mask) = ops[4] else {
                    unreachable!()
                };
                let rw_ea = ea(5);
                let Operand::Acc(acc) = ops[6] else {
                    unreachable!()
                };
                let word = size == Size::W;
                let ryv = self.select_mac_reg(ry, uy, word);
                let rxv = self.select_mac_reg(rx, ux, word);
                // The memory load happens "in parallel" with the compute; we
                // just sequence it first since neither reads the other's
                // result (RM p.172-173).
                let addr = self.ea_addr(&load_ea, Size::L).map_err(Ok)?;
                let addr = if use_mask {
                    addr & self.emac.mask
                } else {
                    addr
                };
                let loaded = bus.read32(addr).map_err(|e| Ok(e.into()))?;
                match rw_ea {
                    Ea::Dn(r) => self.d[r as usize] = loaded,
                    Ea::An(r) => self.a[r as usize] = loaded,
                    _ => unreachable!(),
                }
                self.mac_op(ryv, rxv, word, sf, acc as usize, i.form == Form::MsacLoad);
            }
            Form::MoveToAcc => {
                let v = self.read(bus, &ea(0), Size::L).map_err(Ok)?;
                let Operand::Acc(a) = ops[1] else {
                    unreachable!()
                };
                let a = a as usize;
                self.emac.acc[a] = v;
                let su = self.emac.macsr & 0x40 != 0;
                let fi = self.emac.macsr & 0x20 != 0;
                let ext = if fi {
                    (if v & 0x8000_0000 != 0 { 0xff00 } else { 0 }) as u16
                } else if su {
                    0
                } else {
                    (if v & 0x8000_0000 != 0 { 0xffff } else { 0 }) as u16
                };
                self.set_ext16(a, ext);
                self.set_pav(a, false);
                let mut m = self.emac.macsr & !0x0e;
                if v == 0 {
                    m |= 0x04;
                }
                if v & 0x8000_0000 != 0 {
                    m |= 0x08;
                }
                self.emac.macsr = m;
            }
            Form::MoveFromAcc => {
                let Operand::Acc(a) = ops[0] else {
                    unreachable!()
                };
                let v = self.acc_to_reg(a as usize);
                self.write(bus, &ea(1), Size::L, v).map_err(Ok)?;
            }
            Form::Movclr => {
                let Operand::Acc(a) = ops[0] else {
                    unreachable!()
                };
                let a = a as usize;
                let v = self.acc_to_reg(a);
                self.write(bus, &ea(1), Size::L, v).map_err(Ok)?;
                self.emac.acc[a] = 0;
                self.set_ext16(a, 0);
                self.set_pav(a, false);
            }
            Form::MoveToMacsr => {
                let v = self.read(bus, &ea(0), Size::L).map_err(Ok)?;
                self.emac.macsr = v & 0x0fff;
            }
            Form::MoveFromMacsr => {
                self.write(bus, &ea(1), Size::L, self.emac.macsr)
                    .map_err(Ok)?;
            }
            Form::MoveToAccext01 => {
                let v = self.read(bus, &ea(0), Size::L).map_err(Ok)?;
                self.emac.accext01 = v;
            }
            Form::MoveFromAccext01 => {
                self.write(bus, &ea(1), Size::L, self.emac.accext01)
                    .map_err(Ok)?;
            }
            Form::MoveToAccext23 => {
                let v = self.read(bus, &ea(0), Size::L).map_err(Ok)?;
                self.emac.accext23 = v;
            }
            Form::MoveFromAccext23 => {
                self.write(bus, &ea(1), Size::L, self.emac.accext23)
                    .map_err(Ok)?;
            }
            Form::MoveToMask => {
                let v = self.read(bus, &ea(0), Size::L).map_err(Ok)?;
                self.emac.mask = 0xffff_0000 | (v & 0xffff);
            }
            Form::MoveFromMask => {
                self.write(bus, &ea(1), Size::L, self.emac.mask)
                    .map_err(Ok)?;
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
