//! Exact fused execution of short loops the Elektron firmware spends much
//! of its time in (a word interleave, a 16-byte MOVEM copy, a 16-byte
//! clear and two EMAC filter loops). `Cpu::run_fused` runs whole iterations
//! of a recognised loop with the same bus accesses in the same order and
//! the same register, flag and instruction-count results as stepping its
//! instructions one at a time. The caller decides when that is allowed (no
//! event may fall inside the batch); this module only checks what the loop
//! itself touches. The first three are written out; the EMAC loops run
//! their decoded instructions through the interpreter's own handlers and
//! only skip the per-instruction fetch and dispatch bookkeeping.

use std::sync::OnceLock;

use crate::cpu::{Bus, Cpu, RunState};
use crate::decode::{Insn, Size, decode_at};

/// A recognised loop; the PC is at its first instruction (the head).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Loop {
    /// `cmp.l d2,d0; ble.b +14; move.w #0x8001,(a0); addq.l #1,d2;
    /// addq.l #4,a0; move.w (As)+,(-2,a0); bra.b head`, `src` = s (1-6).
    Interleave { src: u8 },
    /// `movem.l (a0),{d2-d5}; adda.l d1,a0; movem.l {d2-d5},(a1);
    /// adda.l d1,a1; sub.l d1,d0; bge.b head`.
    Copy16,
    /// `clr.l (a0)+` four times; `sub.l d1,d0; bge.b head`.
    Clear16,
    /// `mac.w d6.u,d0.u,(a1),d4,acc0; mac.l d5,d4,acc0; move.l d1,(a2)+;
    /// mac.w d6.u,d0.l,(4,a1),d0,acc1; mac.l d5,d0,(a0)+,d0,acc1;
    /// movclr.l acc0,d1; move.l d1,(a1)+; movclr.l acc1,d4;
    /// move.l d4,(a1)+; add.l d2,d4; swap d4; add.l d2,d1; move.w d4,d1;
    /// subq.l #2,d3; bgt.b head` (15 instructions).
    MacA,
    /// `mvs.w d2,d3; msac.w d1.l,d2.u,(a1)+,d2,acc0; mvz.w (0,a0,d3.l*2),d4;
    /// swap d4; subi.l #0x40000000,d4; movclr.l acc0,d5; sub.l d5,d4;
    /// sats d4; add.l d4,d4; sats d4; asr.l #1,d4; addi.l #0x40000000,d4;
    /// swap d4; move.w d4,(0,a0,d3.l*2); subq.l #1,d0; bgt.b head` (16).
    MacB,
}

const INTERLEAVE: [u8; 18] = [
    0xb0, 0x82, 0x6f, 0x0e, 0x30, 0xbc, 0x80, 0x01, 0x52, 0x82, 0x58, 0x88, 0x31, 0x58, 0xff, 0xfe,
    0x60, 0xee,
];
const COPY16: [u8; 16] = [
    0x4c, 0xd0, 0x00, 0x3c, 0xd1, 0xc1, 0x48, 0xd1, 0x00, 0x3c, 0xd3, 0xc1, 0x90, 0x81, 0x6c, 0xf0,
];
const CLEAR16: [u8; 12] = [
    0x42, 0x98, 0x42, 0x98, 0x42, 0x98, 0x42, 0x98, 0x90, 0x81, 0x6c, 0xf4,
];
/// Byte 13 of `INTERLEAVE` holds the source register in its low 3 bits.
const INTERLEAVE_SRC: usize = 13;
const MAC_A: [u8; 40] = [
    0xa8, 0x91, 0x00, 0xc6, 0xa8, 0x05, 0x08, 0x00, 0x24, 0xc1, 0xa0, 0x29, 0x00, 0x46, 0x00, 0x04,
    0xa0, 0x18, 0x08, 0x05, 0xa1, 0xc1, 0x22, 0xc1, 0xa3, 0xc4, 0x22, 0xc4, 0xd8, 0x82, 0x48, 0x44,
    0xd2, 0x82, 0x32, 0x04, 0x55, 0x83, 0x6e, 0xd8,
];
const MAC_A_OFFSETS: [u32; 15] = [0, 4, 8, 10, 16, 20, 22, 24, 26, 28, 30, 32, 34, 36, 38];
const MAC_B: [u8; 46] = [
    0x77, 0x42, 0xa4, 0x99, 0x21, 0x81, 0x79, 0xf0, 0x3a, 0x00, 0x48, 0x44, 0x04, 0x84, 0x40, 0x00,
    0x00, 0x00, 0xa1, 0xc5, 0x98, 0x85, 0x4c, 0x84, 0xd8, 0x84, 0x4c, 0x84, 0xe2, 0x84, 0x06, 0x84,
    0x40, 0x00, 0x00, 0x00, 0x48, 0x44, 0x31, 0x84, 0x3a, 0x00, 0x53, 0x80, 0x6e, 0xd2,
];
const MAC_B_OFFSETS: [u32; 16] = [0, 2, 6, 10, 12, 18, 20, 22, 24, 26, 28, 30, 36, 38, 42, 44];

/// The decoded instructions of an EMAC loop (none of them PC-relative; the
/// closing branch is never executed through its handler).
fn decoded(code: &[u8], offsets: &[u32]) -> Vec<Insn> {
    offsets
        .iter()
        .map(|&o| decode_at(code, 0, o as usize).expect("valid loop encoding"))
        .collect()
}
static MAC_A_INSNS: OnceLock<Vec<Insn>> = OnceLock::new();
static MAC_B_INSNS: OnceLock<Vec<Insn>> = OnceLock::new();

impl Loop {
    /// The loop encoded at the start of `code`, if any.
    pub fn recognise(code: &[u8]) -> Option<Loop> {
        if code.starts_with(&COPY16) {
            return Some(Loop::Copy16);
        }
        if code.starts_with(&CLEAR16) {
            return Some(Loop::Clear16);
        }
        if code.starts_with(&MAC_A) {
            return Some(Loop::MacA);
        }
        if code.starts_with(&MAC_B) {
            return Some(Loop::MacB);
        }
        let head = code.get(..INTERLEAVE.len())?;
        let src = head[INTERLEAVE_SRC] & 7;
        let same = head.iter().zip(INTERLEAVE).enumerate().all(|(k, (&b, p))| {
            if k == INTERLEAVE_SRC {
                b & !7 == p
            } else {
                b == p
            }
        });
        (same && (1..=6).contains(&src)).then_some(Loop::Interleave { src })
    }

    /// The loop's encoded bytes (what `recognise` matched).
    pub fn code(self) -> Vec<u8> {
        match self {
            Loop::Interleave { src } => {
                let mut c = INTERLEAVE.to_vec();
                c[INTERLEAVE_SRC] |= src;
                c
            }
            Loop::Copy16 => COPY16.to_vec(),
            Loop::Clear16 => CLEAR16.to_vec(),
            Loop::MacA => MAC_A.to_vec(),
            Loop::MacB => MAC_B.to_vec(),
        }
    }

    /// Instructions per iteration.
    pub fn instructions(self) -> u32 {
        match self {
            Loop::Interleave { .. } => 7,
            Loop::Copy16 | Loop::Clear16 => 6,
            Loop::MacA => 15,
            Loop::MacB => 16,
        }
    }

    /// Byte offsets of the instructions after the head, within the loop.
    pub fn offsets(self) -> &'static [u32] {
        match self {
            Loop::Interleave { .. } => &[0, 2, 4, 8, 10, 12, 16],
            Loop::Copy16 => &[0, 4, 6, 10, 12, 14],
            Loop::Clear16 => &[0, 2, 4, 6, 8, 10],
            Loop::MacA => &MAC_A_OFFSETS,
            Loop::MacB => &MAC_B_OFFSETS,
        }
    }
}

/// N/V of `d - s` (32-bit), as CMP/SUB leave them.
#[inline]
fn sub_nv(s: u32, d: u32) -> (bool, bool, bool) {
    let r = d.wrapping_sub(s);
    let n = r & 0x8000_0000 != 0;
    let z = r == 0;
    let v = (s ^ d) & (r ^ d) & 0x8000_0000 != 0;
    (n, z, v)
}

impl Cpu {
    /// Run up to `max` whole iterations of `lp`, whose code the caller has
    /// verified at `self.pc`. An iteration runs only if it ends by taking
    /// the loop branch back to the head, every access it makes is to plain
    /// memory (`Bus::plain_ram`: it succeeds and has no other effect), and
    /// none of its writes lands in a page holding decoded instructions.
    /// Each iteration performs exactly the bus accesses, register and flag
    /// updates of its instructions, in order; `icount` advances by
    /// `lp.instructions()` per iteration. The caller also makes sure every
    /// instruction of the loop is in the decode cache (stepping would not
    /// insert anything). Returns the iterations run.
    pub fn run_fused(&mut self, bus: &mut impl Bus, lp: Loop, max: u32) -> u32 {
        if self.state != RunState::Running {
            return 0;
        }
        let head = self.pc;
        let insns: &[Insn] = match lp {
            Loop::MacA => MAC_A_INSNS.get_or_init(|| decoded(&MAC_A, &MAC_A_OFFSETS)),
            Loop::MacB => MAC_B_INSNS.get_or_init(|| decoded(&MAC_B, &MAC_B_OFFSETS)),
            _ => &[],
        };
        let mut k = 0;
        while k < max {
            let ok = match lp {
                Loop::Interleave { src } => self.interleave_iteration(bus, src as usize),
                Loop::Copy16 => self.copy16_iteration(bus),
                Loop::Clear16 => self.clear16_iteration(bus),
                Loop::MacA => self.mac_a_iteration(bus, head, insns),
                Loop::MacB => self.mac_b_iteration(bus, head, insns),
            };
            if !ok {
                break;
            }
            k += 1;
        }
        if k != 0 {
            self.icount += u64::from(k) * u64::from(lp.instructions());
            self.last_exception = None;
            self.last_unimplemented = None;
        }
        k
    }

    /// One `Loop::Interleave` iteration, or false (nothing changed).
    #[inline]
    fn interleave_iteration(&mut self, bus: &mut impl Bus, src: usize) -> bool {
        let (d0, d2, a0, s) = (self.d[0], self.d[2], self.a[0], self.a[src]);
        // BLE exits when Z || N != V of d0 - d2.
        let (n, z, v) = sub_nv(d2, d0);
        if z || n != v {
            return false;
        }
        let a0n = a0.wrapping_add(4);
        let second = a0n.wrapping_sub(2);
        if !bus.plain_ram(a0, 2)
            || !bus.plain_ram(s, 2)
            || !bus.plain_ram(second, 2)
            || self.decode_cached(a0, 2)
            || self.decode_cached(second, 2)
        {
            return false;
        }
        // cmp.l d2,d0; ble.b (not taken)
        self.flags_sub(d2, d0, d0.wrapping_sub(d2), Size::L, true);
        let exit = self.cond(15);
        debug_assert!(!exit);
        // move.w #0x8001,(a0)
        bus.write16(a0, 0x8001).expect("checked plain memory");
        self.set_nz(0x8001, Size::W);
        // addq.l #1,d2
        let d2n = d2.wrapping_add(1);
        self.d[2] = d2n;
        self.flags_add(1, d2, d2n, Size::L);
        // addq.l #4,a0
        self.a[0] = a0n;
        // move.w (As)+,(-2,a0)
        self.a[src] = s.wrapping_add(2);
        let w = bus.read16(s).expect("checked plain memory") as u32;
        bus.write16(second, w as u16).expect("checked plain memory");
        self.set_nz(w, Size::W);
        // bra.b head
        true
    }

    /// One `Loop::Copy16` iteration, or false (nothing changed).
    #[inline]
    fn copy16_iteration(&mut self, bus: &mut impl Bus) -> bool {
        let (d0, d1, a0, a1) = (self.d[0], self.d[1], self.a[0], self.a[1]);
        // BGE continues when N == V of d0 - d1.
        let (n, _, v) = sub_nv(d1, d0);
        if n != v {
            return false;
        }
        if (0..4u32).any(|k| {
            let (s, d) = (a0.wrapping_add(4 * k), a1.wrapping_add(4 * k));
            !bus.plain_ram(s, 4) || !bus.plain_ram(d, 4) || self.decode_cached(d, 4)
        }) {
            return false;
        }
        // movem.l (a0),{d2-d5}
        for k in 0..4u32 {
            self.d[2 + k as usize] = bus
                .read32(a0.wrapping_add(4 * k))
                .expect("checked plain memory");
        }
        // adda.l d1,a0
        self.a[0] = a0.wrapping_add(d1);
        // movem.l {d2-d5},(a1)
        for k in 0..4u32 {
            bus.write32(a1.wrapping_add(4 * k), self.d[2 + k as usize])
                .expect("checked plain memory");
        }
        // adda.l d1,a1
        self.a[1] = a1.wrapping_add(d1);
        self.sub_l_bge(d1, d0)
    }

    /// One `Loop::Clear16` iteration, or false (nothing changed).
    #[inline]
    fn clear16_iteration(&mut self, bus: &mut impl Bus) -> bool {
        let (d0, d1, a0) = (self.d[0], self.d[1], self.a[0]);
        let (n, _, v) = sub_nv(d1, d0);
        if n != v {
            return false;
        }
        if (0..4u32).any(|k| {
            let d = a0.wrapping_add(4 * k);
            !bus.plain_ram(d, 4) || self.decode_cached(d, 4)
        }) {
            return false;
        }
        // clr.l (a0)+ four times
        for k in 0..4u32 {
            let d = a0.wrapping_add(4 * k);
            self.a[0] = d.wrapping_add(4);
            bus.write32(d, 0).expect("checked plain memory");
            self.set_nz(0, Size::L);
        }
        self.sub_l_bge(d1, d0)
    }

    /// The address an EMAC load uses: `addr`, ANDed with MASK if the
    /// instruction says so.
    #[inline]
    fn mac_load_addr(&self, insn: &Insn, addr: u32) -> u32 {
        if insn.mac_mask() {
            addr & self.emac.mask
        } else {
            addr
        }
    }

    /// Run `insns[..n]` (at `head + offsets`) through the interpreter's
    /// handlers, as `step` would after the decode (none traps: no
    /// privileged or FPU form), then the closing `bgt.b head`, which the
    /// caller checked is taken.
    #[inline]
    fn run_decoded(&mut self, bus: &mut impl Bus, head: u32, insns: &[Insn], offsets: &[u32]) {
        let last = insns.len() - 1;
        for (insn, &o) in insns[..last].iter().zip(offsets) {
            let pc = head.wrapping_add(o);
            self.pc = pc.wrapping_add(insn.len as u32);
            if self.execute(bus, insn, pc).is_err() {
                unreachable!("checked fused instruction faulted");
            }
        }
        let taken = self.cond(14);
        debug_assert!(taken);
        self.pc = head;
    }

    /// One `Loop::MacA` iteration, or false (nothing changed).
    #[inline]
    fn mac_a_iteration(&mut self, bus: &mut impl Bus, head: u32, insns: &[Insn]) -> bool {
        let (a0, a1, a2, d3) = (self.a[0], self.a[1], self.a[2], self.d[3]);
        // BGT after subq.l #2,d3 continues when !Z && N == V of d3 - 2.
        let (n, z, v) = sub_nv(2, d3);
        if z || n != v {
            return false;
        }
        let reads = [
            self.mac_load_addr(&insns[0], a1),
            self.mac_load_addr(&insns[3], a1.wrapping_add(4)),
            self.mac_load_addr(&insns[4], a0),
        ];
        let writes = [a2, a1, a1.wrapping_add(4)];
        if reads.iter().any(|&r| !bus.plain_ram(r, 4))
            || writes
                .iter()
                .any(|&w| !bus.plain_ram(w, 4) || self.decode_cached(w, 4))
        {
            return false;
        }
        self.run_decoded(bus, head, insns, &MAC_A_OFFSETS);
        true
    }

    /// One `Loop::MacB` iteration, or false (nothing changed).
    #[inline]
    fn mac_b_iteration(&mut self, bus: &mut impl Bus, head: u32, insns: &[Insn]) -> bool {
        let (a0, a1, d0, d2) = (self.a[0], self.a[1], self.d[0], self.d[2]);
        // BGT after subq.l #1,d0 continues when !Z && N == V of d0 - 1.
        let (n, z, v) = sub_nv(1, d0);
        if z || n != v {
            return false;
        }
        // mvs.w d2,d3 sets the index that (0,a0,d3.l*2) uses
        let index = (d2 as u16 as i16 as i32 as u32).wrapping_shl(1);
        let word = a0.wrapping_add(index);
        let load = self.mac_load_addr(&insns[1], a1);
        if !bus.plain_ram(load, 4) || !bus.plain_ram(word, 2) || self.decode_cached(word, 2) {
            return false;
        }
        self.run_decoded(bus, head, insns, &MAC_B_OFFSETS);
        true
    }

    /// `sub.l d1,d0; bge.b head` (taken: the caller checked N == V).
    #[inline]
    fn sub_l_bge(&mut self, d1: u32, d0: u32) -> bool {
        let r = d0.wrapping_sub(d1);
        self.d[0] = r;
        self.flags_sub(d1, d0, r, Size::L, false);
        let taken = self.cond(12);
        debug_assert!(taken);
        true
    }
}
