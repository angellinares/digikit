//! Fixed-point ALU computes: add, subtract, negate, pass, and/or/xor/not.
//!
//! Flags (`flags.py _arith_flag_bits`): AC AV AN AZ from the adder, AS AI AF
//! cleared; the logical ops set AN and AZ from the result and clear the rest.

use super::*;
use crate::fast::FlagKind;

impl Lower {
    pub fn ialu_add(&mut self, rn: u32, a: Val, b: Val) -> LR<()> {
        let r = self.bin(Bin::Add, a, b);
        self.wr_i(rn, r)?;
        self.pend_flag(FlagKind::Iadd, vec![a, b]);
        Ok(())
    }

    pub fn ialu_sub(&mut self, rn: u32, a: Val, b: Val) -> LR<()> {
        let r = self.bin(Bin::Sub, a, b);
        self.wr_i(rn, r)?;
        self.pend_flag(FlagKind::Isub, vec![a, b]);
        Ok(())
    }

    pub fn ialu_logical(&mut self, rn: u32, r: Val) -> LR<()> {
        self.wr_i(rn, r)?;
        self.pend_flag(FlagKind::Logical, vec![r]);
        Ok(())
    }
}
