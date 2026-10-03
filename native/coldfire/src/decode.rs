//! Instruction decoding: the hand-written part (effective addresses, the
//! instruction record, text form). The per-form decoders and the dispatch
//! are generated into `decode_gen.rs` from `tools/cfisa/coldfire.json`.

use core::fmt;

pub use crate::decode_gen::{
    COND_KIND, CONDITIONS, FLOWS, FORM_COUNT, FORM_IDS, FORMS, FP_CONDITIONS, Form, MNEMONICS,
    PAGES, PRIVILEGED, UNITS, control_register_name,
};

/// Operand size. `S` and `D` are the FPU single and double formats.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Size {
    None,
    B,
    W,
    L,
    S,
    D,
}

impl Size {
    pub fn suffix(self) -> &'static str {
        match self {
            Size::None => "",
            Size::B => ".b",
            Size::W => ".w",
            Size::L => ".l",
            Size::S => ".s",
            Size::D => ".d",
        }
    }
}

/// EA mode numbers, in the bit order of the table's EA sets.
pub const M_DN: u8 = 0;
pub const M_AN: u8 = 1;
pub const M_IND: u8 = 2;
pub const M_POST: u8 = 3;
pub const M_PRE: u8 = 4;
pub const M_DISP: u8 = 5;
pub const M_IDX: u8 = 6;
pub const M_ABSW: u8 = 7;
pub const M_ABSL: u8 = 8;
pub const M_PCDISP: u8 = 9;
pub const M_PCIDX: u8 = 10;
pub const M_IMM: u8 = 11;

/// The EA mode for a 3-bit mode and register field (CFPRM p.44 Table 2-3).
#[inline]
pub(crate) fn ea_mode(mode: u32, reg: u32) -> Option<u8> {
    if mode < 7 {
        return Some(mode as u8);
    }
    match reg {
        0 => Some(M_ABSW),
        1 => Some(M_ABSL),
        2 => Some(M_PCDISP),
        3 => Some(M_PCIDX),
        4 => Some(M_IMM),
        _ => None,
    }
}

/// An effective address (CFPRM p.36-44). Index registers are 0-7 for D0-D7
/// and 8-15 for A0-A7. `wl` is the brief extension word's W/L bit: 0 (word
/// index) is not supported and takes an address error (CFPRM p.36 Table 2-1).
/// `scale` is the scale code 0-3 (x1, x2, x4, x8; x8 only with an FPU).
/// PC-relative modes carry `base`, the address of their extension word;
/// `PcIdx` carries `base + d8` instead (`disp`), the address before the
/// index is added, so an `Ea` (and an `Operand`) fits in 8 bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ea {
    Dn(u8),
    An(u8),
    Ind(u8),
    Post(u8),
    Pre(u8),
    Disp(u8, i16),
    Idx {
        an: u8,
        xn: u8,
        wl: bool,
        scale: u8,
        d8: i8,
    },
    AbsW(i16),
    AbsL(u32),
    PcDisp {
        base: u32,
        d16: i16,
    },
    PcIdx {
        disp: u32,
        xn: u8,
        wl: bool,
        scale: u8,
    },
    Imm(u32),
}

impl Ea {
    /// Register direct for a 4-bit register number (0-7 Dn, 8-15 An).
    #[inline]
    pub fn rn(r: u8) -> Ea {
        if r < 8 { Ea::Dn(r) } else { Ea::An(r - 8) }
    }

    pub fn mode(&self) -> u8 {
        match self {
            Ea::Dn(_) => M_DN,
            Ea::An(_) => M_AN,
            Ea::Ind(_) => M_IND,
            Ea::Post(_) => M_POST,
            Ea::Pre(_) => M_PRE,
            Ea::Disp(..) => M_DISP,
            Ea::Idx { .. } => M_IDX,
            Ea::AbsW(_) => M_ABSW,
            Ea::AbsL(_) => M_ABSL,
            Ea::PcDisp { .. } => M_PCDISP,
            Ea::PcIdx { .. } => M_PCIDX,
            Ea::Imm(_) => M_IMM,
        }
    }
}

/// A decoded operand. Register direct operands are `Ea::Dn`/`Ea::An`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Operand {
    None,
    Ea(Ea),
    /// An immediate that is not an EA: immediate data after ORI/ADDI/...,
    /// quick values (ADDQ 1-8, MOV3Q -1..7, MOVEQ), bit numbers, TRAP
    /// vectors, LINK displacements, STOP data. Sign-extended to 32 bits
    /// where the instruction defines it as signed.
    Imm(u32),
    /// Absolute branch target.
    Target(u32),
    RegList(u16),
    FpList(u8),
    Ctrl(u16),
    /// CPUSHL cache field: 1 data, 2 instruction, 3 both (CFPRM p.238).
    Cache(u8),
    Acc(u8),
    /// A MAC source register (0-15) and, for word operations, its upper half.
    MacReg {
        r: u8,
        upper: bool,
    },
    /// MAC scale factor field: 0 none, 1 <<1, 2 reserved, 3 >>1 (CFPRM p.171).
    Scale(u8),
    /// MAC with load: the MASK register is applied to the EA.
    MaskFlag(bool),
    Fp(u8),
    Sr,
    Ccr,
    Usp,
    Macsr,
    Mask,
    AccExt01,
    AccExt23,
    Fpcr,
    Fpsr,
    Fpiar,
}

/// The most operands an instruction has (MAC with load has seven).
pub const MAX_OPS: usize = 7;
/// Operands an `Insn` stores as `Operand`s: every form except the four
/// EMAC MAC/MSAC forms has at most three; those keep their register,
/// scale, mask and accumulator fields packed in `Insn::mac` instead.
const STORED_OPS: usize = 3;

// The decode cache stores one `Insn` per code halfword; keep it compact.
const _: () = assert!(core::mem::size_of::<Ea>() == 8);
const _: () = assert!(core::mem::size_of::<Operand>() == 8);
const _: () = assert!(core::mem::size_of::<Insn>() == 32);

/// `Insn::mac` layout: Ry (4-bit register, upper-half flag) in bits 0-4,
/// Rx in bits 5-9, the scale factor in 10-11, the MASK flag in 12, the
/// accumulator in 13-14; bit 15 records that Ry was pushed (decode only).
const MAC_RY_SET: u16 = 1 << 15;

/// The operand list `Insn::operands` returns (at most `MAX_OPS`).
#[derive(Clone, Copy, Debug)]
pub struct Operands {
    ops: [Operand; MAX_OPS],
    n: u8,
}

impl core::ops::Deref for Operands {
    type Target = [Operand];
    fn deref(&self) -> &[Operand] {
        &self.ops[..self.n as usize]
    }
}

impl<'a> IntoIterator for &'a Operands {
    type Item = &'a Operand;
    type IntoIter = core::slice::Iter<'a, Operand>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

const fn str_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut k = 0;
    while k < a.len() {
        if a[k] != b[k] {
            return false;
        }
        k += 1;
    }
    true
}

/// Per form: `TRAP_PRIVILEGED` (PRIVILEGED) and `TRAP_FPU` (unit "fpu"),
/// the two checks `Cpu::step` makes before executing, in one load.
pub(crate) const TRAP_PRIVILEGED: u8 = 1;
pub(crate) const TRAP_FPU: u8 = 2;
pub(crate) const FORM_TRAPS: [u8; FORM_COUNT] = {
    let mut t = [0u8; FORM_COUNT];
    let mut k = 0;
    while k < FORM_COUNT {
        if PRIVILEGED[k] {
            t[k] |= TRAP_PRIVILEGED;
        }
        if str_eq(UNITS[k], "fpu") {
            t[k] |= TRAP_FPU;
        }
        k += 1;
    }
    t
};

/// A decoded instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Insn {
    pub form: Form,
    pub size: Size,
    /// Length in bytes (2, 4 or 6).
    pub len: u8,
    /// Condition code for Bcc/Scc (CONDITIONS) and FBcc (FP_CONDITIONS).
    pub cond: u8,
    /// Stored operands (`ops[..nops]`); see `operands` for the full list.
    pub(crate) nops: u8,
    /// MAC/MSAC (with or without load) packed fields; 0 for other forms.
    pub(crate) mac: u16,
    pub(crate) ops: [Operand; STORED_OPS],
}

impl Insn {
    pub(crate) fn new(form: Form, size: Size) -> Insn {
        Insn {
            form,
            size,
            len: 0,
            cond: 0,
            nops: 0,
            mac: 0,
            ops: [Operand::None; STORED_OPS],
        }
    }

    #[inline]
    fn is_mac(&self) -> bool {
        matches!(
            self.form,
            Form::Mac | Form::Msac | Form::MacLoad | Form::MsacLoad
        )
    }

    #[inline]
    pub(crate) fn push(&mut self, op: Operand) {
        if self.is_mac() {
            let reg = |r: u8, upper: bool| u16::from(r & 15) | u16::from(upper) << 4;
            match op {
                Operand::MacReg { r, upper } if self.mac & MAC_RY_SET == 0 => {
                    self.mac |= reg(r, upper) | MAC_RY_SET;
                    return;
                }
                Operand::MacReg { r, upper } => {
                    self.mac |= reg(r, upper) << 5;
                    return;
                }
                Operand::Scale(sf) => {
                    self.mac |= u16::from(sf & 3) << 10;
                    return;
                }
                Operand::MaskFlag(m) => {
                    self.mac |= u16::from(m) << 12;
                    return;
                }
                Operand::Acc(a) => {
                    self.mac |= u16::from(a & 3) << 13;
                    return;
                }
                _ => {}
            }
        }
        self.ops[self.nops as usize] = op;
        self.nops += 1;
    }

    /// MAC Ry: (register 0-15, upper half).
    #[inline(always)]
    pub(crate) fn mac_ry(&self) -> (u8, bool) {
        ((self.mac & 15) as u8, self.mac & 0x10 != 0)
    }

    /// MAC Rx: (register 0-15, upper half).
    #[inline(always)]
    pub(crate) fn mac_rx(&self) -> (u8, bool) {
        ((self.mac >> 5 & 15) as u8, self.mac & 0x200 != 0)
    }

    #[inline(always)]
    pub(crate) fn mac_scale(&self) -> u8 {
        (self.mac >> 10 & 3) as u8
    }

    #[inline(always)]
    pub(crate) fn mac_mask(&self) -> bool {
        self.mac & 0x1000 != 0
    }

    #[inline(always)]
    pub(crate) fn mac_acc(&self) -> u8 {
        (self.mac >> 13 & 3) as u8
    }

    /// Every operand in the table's order (the MAC forms rebuilt from their
    /// packed fields).
    pub fn operands(&self) -> Operands {
        let mut ops = [Operand::None; MAX_OPS];
        let mut n = 0;
        let mut put = |op: Operand| {
            ops[n] = op;
            n += 1;
        };
        if self.is_mac() {
            let (ry, uy) = self.mac_ry();
            let (rx, ux) = self.mac_rx();
            put(Operand::MacReg { r: ry, upper: uy });
            put(Operand::MacReg { r: rx, upper: ux });
            put(Operand::Scale(self.mac_scale()));
            if matches!(self.form, Form::MacLoad | Form::MsacLoad) {
                put(self.ops[0]);
                put(Operand::MaskFlag(self.mac_mask()));
                put(self.ops[1]);
            }
            put(Operand::Acc(self.mac_acc()));
        } else {
            for &op in &self.ops[..self.nops as usize] {
                put(op);
            }
        }
        Operands { ops, n: n as u8 }
    }

    /// `FORM_TRAPS` for this form.
    #[inline(always)]
    pub(crate) fn traps(&self) -> u8 {
        FORM_TRAPS[self.form as usize]
    }

    pub fn id(&self) -> &'static str {
        FORM_IDS[self.form as usize]
    }

    pub fn unit(&self) -> &'static str {
        UNITS[self.form as usize]
    }

    pub fn privileged(&self) -> bool {
        PRIVILEGED[self.form as usize]
    }

    /// "" for straight-line code, else branch, jump, call, ret, trap, halt.
    pub fn flow(&self) -> &'static str {
        FLOWS[self.form as usize]
    }

    /// The mnemonic with its condition, without the size suffix.
    pub fn mnemonic(&self) -> String {
        let base = MNEMONICS[self.form as usize];
        match COND_KIND[self.form as usize] {
            1 => format!("{}{}", base, CONDITIONS[self.cond as usize]),
            2 => format!("{}{}", base, FP_CONDITIONS[self.cond as usize]),
            _ => base.to_string(),
        }
    }
}

/// The words of one instruction (at most three, CFPRM p.35) and the read
/// position. `pc` is the address of the opword.
pub(crate) struct Cur {
    pub(crate) pc: u32,
    pub(crate) w: [u16; 3],
    pub(crate) pos: usize,
}

impl Cur {
    #[inline]
    pub(crate) fn reset(&mut self) {
        self.pos = 1;
    }

    /// The next extension word; None past the third word.
    #[inline]
    pub(crate) fn next(&mut self) -> Option<u16> {
        let v = *self.w.get(self.pos)?;
        self.pos += 1;
        Some(v)
    }

    #[inline]
    fn next_addr(&self) -> u32 {
        self.pc.wrapping_add(2 * self.pos as u32)
    }

    #[inline]
    pub(crate) fn len(&self) -> u8 {
        (2 * self.pos) as u8
    }

    /// Immediate data of the given size (CFPRM p.43 Table 2-2).
    pub(crate) fn imm(&mut self, size: Size) -> Option<u32> {
        match size {
            Size::B => Some((self.next()? & 0xff) as u32),
            Size::W => Some(self.next()? as u32),
            Size::L | Size::S => {
                let hi = self.next()? as u32;
                Some(hi << 16 | self.next()? as u32)
            }
            Size::D | Size::None => None,
        }
    }

    /// Brief extension word (CFPRM p.36 Figure 2-2). Bit 8 must be 0.
    fn index(&mut self) -> Option<(u8, bool, u8, i8)> {
        let e = self.next()?;
        if e & 0x0100 != 0 {
            return None;
        }
        Some((
            ((e >> 12) & 0xf) as u8,
            e & 0x0800 != 0,
            ((e >> 9) & 3) as u8,
            e as u8 as i8,
        ))
    }

    pub(crate) fn ea(&mut self, mode: u8, reg: u32, size: Size) -> Option<Ea> {
        let r = reg as u8;
        Some(match mode {
            M_DN => Ea::Dn(r),
            M_AN => Ea::An(r),
            M_IND => Ea::Ind(r),
            M_POST => Ea::Post(r),
            M_PRE => Ea::Pre(r),
            M_DISP => Ea::Disp(r, self.next()? as i16),
            M_IDX => {
                let (xn, wl, scale, d8) = self.index()?;
                Ea::Idx {
                    an: r,
                    xn,
                    wl,
                    scale,
                    d8,
                }
            }
            M_ABSW => Ea::AbsW(self.next()? as i16),
            M_ABSL => {
                let hi = self.next()? as u32;
                Ea::AbsL(hi << 16 | self.next()? as u32)
            }
            M_PCDISP => {
                let base = self.next_addr();
                Ea::PcDisp {
                    base,
                    d16: self.next()? as i16,
                }
            }
            M_PCIDX => {
                let base = self.next_addr();
                let (xn, wl, scale, d8) = self.index()?;
                Ea::PcIdx {
                    disp: base.wrapping_add(d8 as i32 as u32),
                    xn,
                    wl,
                    scale,
                }
            }
            M_IMM => Ea::Imm(self.imm(size)?),
            _ => return None,
        })
    }

    /// Bcc/BRA/BSR displacement (CFPRM p.82): 8-bit in the opword, 0x00 selects
    /// a 16-bit and 0xFF a 32-bit extension. The base is the opword address + 2.
    pub(crate) fn bdisp(&mut self, d8: u32) -> Option<(u32, Size)> {
        let base = self.pc.wrapping_add(2);
        match d8 {
            0x00 => Some((base.wrapping_add(self.next()? as i16 as u32), Size::W)),
            0xff => {
                let hi = self.next()? as u32;
                let d = hi << 16 | self.next()? as u32;
                Some((base.wrapping_add(d), Size::L))
            }
            _ => Some((base.wrapping_add(d8 as u8 as i8 as u32), Size::B)),
        }
    }

    /// FBcc displacement (CFPRM p.205): size bit 0 = 16-bit, 1 = 32-bit.
    pub(crate) fn fbdisp(&mut self, size: u32) -> Option<u32> {
        let base = self.pc.wrapping_add(2);
        if size == 0 {
            Some(base.wrapping_add(self.next()? as i16 as u32))
        } else {
            let hi = self.next()? as u32;
            Some(base.wrapping_add(hi << 16 | self.next()? as u32))
        }
    }
}

/// Decode the instruction at `pc` from its first three words (pad with any
/// value past the end of memory: the length depends only on the words used).
/// None is an illegal or unsupported encoding.
pub fn decode(pc: u32, words: [u16; 3]) -> Option<Insn> {
    let mut c = Cur {
        pc,
        w: words,
        pos: 1,
    };
    crate::decode_gen::dispatch(&mut c)
}

/// Decode at `offset` of a big-endian byte image loaded at `base`.
pub fn decode_at(image: &[u8], base: u32, offset: usize) -> Option<Insn> {
    let word = |i: usize| -> u16 {
        let o = offset + 2 * i;
        if o + 1 < image.len() {
            u16::from_be_bytes([image[o], image[o + 1]])
        } else {
            0
        }
    };
    if offset + 1 >= image.len() {
        return None;
    }
    decode(
        base.wrapping_add(offset as u32),
        [word(0), word(1), word(2)],
    )
}

// ---------------------------------------------------------------------------
// Text form. Numbers are signed hex (sign-extended from their size),
// registers lower case, no spaces: close to the Ghidra/SLEIGH listing so the
// oracle comparison (tools/cfisa/oracle.py) needs little normalisation.

fn hex(v: i64) -> String {
    if v < 0 {
        format!("-0x{:x}", -v)
    } else {
        format!("0x{:x}", v)
    }
}

fn reg(r: u8) -> String {
    if r < 8 {
        format!("d{}", r)
    } else {
        format!("a{}", r - 8)
    }
}

fn sext(v: u32, size: Size) -> i64 {
    match size {
        Size::B => v as u8 as i8 as i64,
        Size::W => v as u16 as i16 as i64,
        _ => v as i32 as i64,
    }
}

fn index(xn: u8, wl: bool, scale: u8) -> String {
    format!(
        "{}{}*{}",
        reg(xn),
        if wl { "" } else { ".w" },
        hex(1i64 << scale)
    )
}

pub fn fmt_ea(ea: &Ea, size: Size) -> String {
    match *ea {
        Ea::Dn(r) => format!("d{}", r),
        Ea::An(r) => format!("a{}", r),
        Ea::Ind(r) => format!("(a{})", r),
        Ea::Post(r) => format!("(a{})+", r),
        Ea::Pre(r) => format!("-(a{})", r),
        Ea::Disp(r, d) => format!("({},a{})", hex(d as i64), r),
        Ea::Idx {
            an,
            xn,
            wl,
            scale,
            d8,
        } => {
            format!("({},a{},{})", hex(d8 as i64), an, index(xn, wl, scale))
        }
        Ea::AbsW(a) => format!("({}).w", hex(a as i64)),
        Ea::AbsL(a) => format!("({}).l", hex(a as i32 as i64)),
        Ea::PcDisp { d16, .. } => format!("({},pc)", hex(d16 as i64)),
        Ea::PcIdx {
            disp,
            xn,
            wl,
            scale,
        } => {
            format!("({},pc,{})", hex(disp as i32 as i64), index(xn, wl, scale))
        }
        Ea::Imm(v) => format!("#{}", hex(sext(v, size))),
    }
}

fn reglist(mask: u16) -> String {
    let regs: Vec<String> = (0..16)
        .filter(|i| mask >> i & 1 != 0)
        .map(|i| reg(i as u8))
        .collect();
    format!("{{{}}}", regs.join(" "))
}

impl fmt::Display for Insn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.mnemonic(), self.size.suffix())?;
        let word_mac = self.size == Size::W;
        let mut parts: Vec<String> = Vec::new();
        for op in &self.operands() {
            let s = match *op {
                Operand::None => continue,
                Operand::Ea(ref ea) => fmt_ea(ea, self.size),
                Operand::Imm(v) => format!("#{}", hex(v as i32 as i64)),
                Operand::Target(t) => format!("0x{:x}", t),
                Operand::RegList(m) => reglist(m),
                Operand::FpList(m) => {
                    let regs: Vec<String> = (0..8)
                        .filter(|i| m >> (7 - i) & 1 != 0)
                        .map(|i| format!("fp{}", i))
                        .collect();
                    format!("{{{}}}", regs.join(" "))
                }
                Operand::Ctrl(rc) => match control_register_name(rc) {
                    Some(n) => n.to_string(),
                    None => format!("0x{:x}", rc),
                },
                Operand::Cache(c) => ["?", "dc", "ic", "bc"][c as usize & 3].to_string(),
                Operand::Acc(a) => format!("acc{}", a),
                Operand::MacReg { r, upper } => {
                    if word_mac {
                        format!("{}.{}", reg(r), if upper { "u" } else { "l" })
                    } else {
                        reg(r)
                    }
                }
                Operand::Scale(sf) => match sf {
                    0 => continue,
                    1 => "<<1".to_string(),
                    3 => ">>1".to_string(),
                    _ => "sf2".to_string(),
                },
                Operand::MaskFlag(m) => {
                    if let (true, Some(last)) = (m, parts.last_mut()) {
                        last.push('&');
                    }
                    continue;
                }
                Operand::Fp(r) => format!("fp{}", r),
                Operand::Sr => "sr".into(),
                Operand::Ccr => "ccr".into(),
                Operand::Usp => "usp".into(),
                Operand::Macsr => "macsr".into(),
                Operand::Mask => "mask".into(),
                Operand::AccExt01 => "accext01".into(),
                Operand::AccExt23 => "accext23".into(),
                Operand::Fpcr => "fpcr".into(),
                Operand::Fpsr => "fpsr".into(),
                Operand::Fpiar => "fpiar".into(),
            };
            parts.push(s);
        }
        if !parts.is_empty() {
            write!(f, " {}", parts.join(","))?;
        }
        Ok(())
    }
}
