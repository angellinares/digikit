//! Opt-in, guarded ABI-result substitution for three libgcc binary32 calls.
//!
//! This deliberately is not an instruction-equivalent implementation: callee
//! scratch registers, stack scratch writes, and exit CCR are firmware-owned.

use coldfire::{Bus, Cpu, RunState};
use machine::Board;
use serde::Serialize;

use crate::common::{MAIN_LOAD, hex, unique};

const ADD_SIG: &str = "4e56ffe848d700fc202e0008222e000c2040d080670002082241d2816700021c283c00ffffff2a3c010000002c00c0844684cc84670001d6bc84670002504846";
const MUL_SIG: &str = "4e56ffe848d700fc202e0008222e000c2e00b3870287800000002c3c7f8000002a064685283c008000000880001f2400670000c20881001f2601670000b0b086";
const DIV_SIG: &str = "4e56ffe848d700fc202e0008222e000c2e00b3870287800000002c3c7f8000002a064685283c008000000880001f2400670000b00881001f2601670000d0b086";
const CMP_SIG: &str = "4e560000487800012f2e000c2f2e000861fffffffd464e5e4e7500004e560000487800012f2e000c2f2e000861fffffffd2a4e5e4e7500004e56ffe848d7047c";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExecutionPolicy {
    #[default]
    Reference,
    SoftfloatAbiV1,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct SoftfloatCounts {
    pub add_hits: u64,
    pub mul_hits: u64,
    pub div_hits: u64,
    pub add_defers: u64,
    pub mul_defers: u64,
    pub div_defers: u64,
}
impl SoftfloatCounts {
    pub fn calls(self) -> u64 {
        self.add_hits + self.mul_hits + self.div_hits
    }
}

#[derive(Clone, Copy)]
enum Op {
    Add,
    Mul,
    Div,
}
impl Op {
    fn apply(self, a: f64, b: f64) -> f64 {
        match self {
            Self::Add => a + b,
            Self::Mul => a * b,
            Self::Div => a / b,
        }
    }
}

pub(crate) struct SoftfloatAbi {
    add: u32,
    mul: u32,
    div: u32,
    code: Vec<u8>,
    pub(crate) enabled: bool,
    pub(crate) reason: Option<String>,
    pub(crate) counts: SoftfloatCounts,
}

impl SoftfloatAbi {
    pub(crate) fn disabled(reason: impl Into<String>) -> Self {
        Self {
            add: 0,
            mul: 0,
            div: 0,
            code: Vec::new(),
            enabled: false,
            reason: Some(reason.into()),
            counts: SoftfloatCounts::default(),
        }
    }

    /// Resolves only the libgcc block whose relevant 64-byte routine prefixes
    /// are wholly before cmp; this is the conservative static coverage check.
    pub(crate) fn resolve(main: &[u8], supported: bool, board: &Board) -> Self {
        if !supported {
            return Self::disabled("unsupported verified image");
        }
        let (add, mul, div, cmp) = match resolve_entries(main) {
            Ok(entries) => entries,
            Err(reason) => return Self::disabled(reason),
        };
        let code = main[(add - MAIN_LOAD) as usize..(cmp - MAIN_LOAD) as usize].to_vec();
        if !board.ram_matches(add, &code) {
            return Self::disabled("softfloat code region unavailable or modified");
        }
        Self {
            add,
            mul,
            div,
            code,
            enabled: true,
            reason: None,
            counts: SoftfloatCounts::default(),
        }
    }

    pub(crate) fn try_call(&mut self, cpu: &mut Cpu, board: &mut Board, main_end: u32) -> bool {
        let op = match cpu.pc {
            pc if pc == self.add => Op::Add,
            pc if pc == self.mul => Op::Mul,
            pc if pc == self.div => Op::Div,
            _ => return false,
        };
        if !self.enabled || cpu.state != RunState::Running || cpu.sr & 0x8000 != 0 {
            self.defer(op);
            return false;
        }
        if !board.ram_matches(self.add, &self.code) {
            self.defer(op);
            return false;
        }
        let sp = cpu.a[7];
        let Some(end) = sp.checked_add(12) else {
            self.defer(op);
            return false;
        };
        if sp & 3 != 0 || !board.can_write_ram_range(sp, (end - sp) as usize) {
            self.defer(op);
            return false;
        }
        let (Ok(ret), Ok(a), Ok(b)) =
            (board.read32(sp), board.read32(sp + 4), board.read32(sp + 8))
        else {
            self.defer(op);
            return false;
        };
        if ret & 1 != 0 || !(MAIN_LOAD..main_end).contains(&ret) {
            self.defer(op);
            return false;
        }
        let Some(result) = binary32(op, a, b) else {
            self.defer(op);
            return false;
        };
        cpu.resolve_nzv();
        cpu.d[0] = result;
        cpu.a[7] = sp + 4;
        cpu.pc = ret;
        cpu.last_exception = None;
        cpu.last_unimplemented = None;
        match op {
            Op::Add => self.counts.add_hits += 1,
            Op::Mul => self.counts.mul_hits += 1,
            Op::Div => self.counts.div_hits += 1,
        }
        true
    }
    fn defer(&mut self, op: Op) {
        match op {
            Op::Add => self.counts.add_defers += 1,
            Op::Mul => self.counts.mul_defers += 1,
            Op::Div => self.counts.div_defers += 1,
        }
    }
}

fn resolve_entries(main: &[u8]) -> Result<(u32, u32, u32, u32), String> {
    let entry = |signature: &str, name: &str| {
        unique(main, &hex(signature), &vec![false; signature.len() / 2])
            .map_err(|_| format!("{name} signature unavailable"))
            .and_then(|address| {
                (address & 1 == 0)
                    .then_some(address)
                    .ok_or_else(|| format!("{name} signature odd"))
            })
    };
    let (add, mul, div, cmp) = (
        entry(ADD_SIG, "add")?,
        entry(MUL_SIG, "mul")?,
        entry(DIV_SIG, "div")?,
        entry(CMP_SIG, "cmp")?,
    );
    if !(add < mul && mul < div && div < cmp)
        || div
            .checked_add(DIV_SIG.len() as u32)
            .is_none_or(|end| end > cmp)
    {
        return Err("softfloat region ordering/coverage failed".into());
    }
    Ok((add, mul, div, cmp))
}

fn normal_or_zero(bits: u32) -> bool {
    let exponent = (bits >> 23) & 0xff;
    (exponent == 0 && bits & 0x007f_ffff == 0) || (1..=254).contains(&exponent)
}
fn binary32(op: Op, a: u32, b: u32) -> Option<u32> {
    if !normal_or_zero(a) || !normal_or_zero(b) || (matches!(op, Op::Div) && b & 0x7fff_ffff == 0) {
        return None;
    }
    let output = op.apply(f32::from_bits(a) as f64, f32::from_bits(b) as f64);
    const TINY: f64 = f32::MIN_POSITIVE as f64;
    if !output.is_finite() || output == 0.0 || output.abs() < TINY || output.abs() > 3.402_823_5e38
    {
        return None;
    }
    // Keep the wider acceptance guard, but round division directly in binary32.
    let rounded = if matches!(op, Op::Div) {
        f32::from_bits(a) / f32::from_bits(b)
    } else {
        output as f32
    };
    (rounded.is_finite() && rounded.is_normal()).then_some(rounded.to_bits())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::PAGE;
    use emmc_card::{Card, DEFAULT_CAPACITY_BLOCKS};
    use machine::{CompletionPolicy, SemaphoreAddresses};

    const PAGE_U32: u32 = PAGE as u32;

    fn fixture() -> (SoftfloatAbi, Cpu, Board) {
        let mut main = vec![0; 0x2000];
        for (at, sig) in [
            (0x100, ADD_SIG),
            (0x500, MUL_SIG),
            (0x900, DIV_SIG),
            (0xd00, CMP_SIG),
        ] {
            main[at..at + sig.len() / 2].copy_from_slice(&hex(sig));
        }
        let mut board = Board::new(
            Card::new(DEFAULT_CAPACITY_BLOCKS).unwrap(),
            SemaphoreAddresses::default(),
            CompletionPolicy::Oracle,
        );
        board
            .map_zeroed_ram_page(MAIN_LOAD & !(PAGE_U32 - 1))
            .unwrap();
        for (offset, byte) in main.iter().enumerate() {
            board
                .write_guest(MAIN_LOAD + offset as u32, 1, u32::from(*byte))
                .unwrap();
        }
        let stack = 0x4080_0000;
        board.map_zeroed_ram_page(stack).unwrap();
        board.write32(stack, MAIN_LOAD + 0x1800).unwrap();
        board.write32(stack + 4, 3.0f32.to_bits()).unwrap();
        board.write32(stack + 8, 2.0f32.to_bits()).unwrap();
        let abi = SoftfloatAbi::resolve(&main, true, &board);
        let mut cpu = Cpu::new();
        cpu.pc = MAIN_LOAD + 0x100;
        cpu.a[7] = stack;
        (abi, cpu, board)
    }
    #[test]
    fn finite_normal_and_zero_inputs_are_accepted() {
        assert_eq!(
            binary32(Op::Add, 0, 1.0f32.to_bits()),
            Some(1.0f32.to_bits())
        );
    }
    #[test]
    fn exceptional_and_non_normal_results_defer() {
        assert_eq!(binary32(Op::Div, 1.0f32.to_bits(), 0), None);
        assert_eq!(binary32(Op::Add, 1.0f32.to_bits(), 0x7f80_0000), None);
        assert_eq!(binary32(Op::Add, 1.0f32.to_bits(), 1), None);
        assert_eq!(
            binary32(Op::Add, 1.0f32.to_bits(), (-1.0f32).to_bits()),
            None
        );
        assert_eq!(
            binary32(Op::Mul, f32::MIN_POSITIVE.to_bits(), 0.5f32.to_bits()),
            None
        );
    }
    #[test]
    fn division_rounds_directly_in_binary32() {
        assert_eq!(
            binary32(Op::Div, 0x3f80_0000, 0x4040_0000),
            Some(0x3eaa_aaab)
        );
        assert_eq!(
            binary32(Op::Div, 0x3f80_0001, 0x3f80_0002),
            Some(0x3f7f_fffe)
        );
        assert_eq!(
            binary32(Op::Div, 0xbf80_0000, 0x4040_0000),
            Some(0xbeaa_aaab)
        );
    }
    #[test]
    fn subtraction_sign_flip_then_add_has_add_semantics() {
        assert_eq!(
            binary32(Op::Add, 3.0f32.to_bits(), (-2.0f32).to_bits()),
            Some(1.0f32.to_bits())
        );
    }
    #[test]
    fn accepted_transaction_changes_only_abi_result_state() {
        let (mut abi, mut cpu, mut board) = fixture();
        cpu.d[1] = 0x1234_5678;
        cpu.sr = 0x2705;
        cpu.last_exception = Some(2);
        let before_stack = board.read32(cpu.a[7]).unwrap();
        assert!(abi.try_call(&mut cpu, &mut board, MAIN_LOAD + 0x2000));
        assert_eq!(cpu.d[0], 5.0f32.to_bits());
        assert_eq!(cpu.d[1], 0x1234_5678);
        assert_eq!(cpu.a[7], 0x4080_0004);
        assert_eq!(cpu.pc, MAIN_LOAD + 0x1800);
        assert_eq!(cpu.last_exception, None);
        assert_eq!(cpu.sr, 0x2705);
        assert_eq!(board.read32(0x4080_0000).unwrap(), before_stack);
        assert_eq!(abi.counts.add_hits, 1);
    }
    #[test]
    fn modified_code_and_invalid_preconditions_defer_without_cpu_mutation() {
        let (mut abi, mut cpu, mut board) = fixture();
        board.write_guest(MAIN_LOAD + 0x100, 1, 0).unwrap();
        let before = cpu.clone();
        assert!(!abi.try_call(&mut cpu, &mut board, MAIN_LOAD + 0x2000));
        assert_eq!(cpu, before);
        let (mut abi, mut cpu, mut board) = fixture();
        board.write_guest(MAIN_LOAD + 0xcff, 1, 1).unwrap();
        let before = cpu.clone();
        assert!(!abi.try_call(&mut cpu, &mut board, MAIN_LOAD + 0x2000));
        assert_eq!(cpu, before);
        let (mut abi, mut cpu, mut board) = fixture();
        cpu.a[7] = 0xffff_fffc;
        let before = cpu.clone();
        assert!(!abi.try_call(&mut cpu, &mut board, MAIN_LOAD + 0x2000));
        assert_eq!(cpu, before);
        let (mut abi, mut cpu, mut board) = fixture();
        cpu.sr |= 0x8000;
        let before = cpu.clone();
        assert!(!abi.try_call(&mut cpu, &mut board, MAIN_LOAD + 0x2000));
        assert_eq!(cpu, before);
        let (mut abi, mut cpu, mut board) = fixture();
        board.write32(0x4080_0000, MAIN_LOAD + 1).unwrap();
        let before = cpu.clone();
        assert!(!abi.try_call(&mut cpu, &mut board, MAIN_LOAD + 0x2000));
        assert_eq!(cpu, before);
    }
    #[test]
    fn reference_is_the_default_policy() {
        assert_eq!(ExecutionPolicy::default(), ExecutionPolicy::Reference);
    }
    #[test]
    fn resolver_rejects_missing_duplicate_and_reordered_signatures() {
        let mut image = vec![0; 0x2000];
        for (at, sig) in [
            (0x100, ADD_SIG),
            (0x500, MUL_SIG),
            (0x900, DIV_SIG),
            (0xd00, CMP_SIG),
        ] {
            image[at..at + sig.len() / 2].copy_from_slice(&hex(sig));
        }
        assert!(resolve_entries(&image).is_ok());
        image[0x100] ^= 1;
        assert!(resolve_entries(&image).is_err());
        image[0x100..0x100 + ADD_SIG.len() / 2].copy_from_slice(&hex(ADD_SIG));
        image[0x180..0x180 + ADD_SIG.len() / 2].copy_from_slice(&hex(ADD_SIG));
        assert!(resolve_entries(&image).is_err());
        let mut reordered = vec![0; 0x2000];
        for (at, sig) in [
            (0x500, ADD_SIG),
            (0x100, MUL_SIG),
            (0x900, DIV_SIG),
            (0xd00, CMP_SIG),
        ] {
            reordered[at..at + sig.len() / 2].copy_from_slice(&hex(sig));
        }
        assert!(resolve_entries(&reordered).is_err());
    }
}
