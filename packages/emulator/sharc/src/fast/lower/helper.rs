//! A helper call in place of an instruction the kernel does not lower
//! (`SHARC_FAST_HELPER`, `fast::helper`). Stub for one class: the `3a`
//! transfer through a data-dependent address (`DM(I0, M5) = R8`).
//!
//! Around the call the kernel synchronises exactly what the instruction
//! touches: the index, modify and data registers it reads go into the engine
//! state (`StSt32`; their known masks are already full, the region requires
//! every register it touches to be known at entry), the registers it writes
//! are read back (`Op::St32`). The condition flags, loop counters, PC and the
//! instruction counts stay in the kernel: the instruction is unconditional
//! and its sequencer effect is undone by `helper_exec`.

use super::*;
use crate::fast::decode_view::Dec;
use crate::fast::helper::HelperRec;

impl Lower {
    pub fn lower_helper(&mut self, idx: u32, d: &Dec, pc: u32) -> LR<()> {
        self.begin_insn(idx);
        let r = self.lower_helper_inner(d, pc);
        r.map_err(|e| Refuse(format!("insn {idx} ({}, helper): {}", d.form, e.0)))
    }

    fn lower_helper_inner(&mut self, d: &Dec, pc: u32) -> LR<()> {
        if self.mode1_cur != self.env.mode1 || self.mode1_cur & (1 << 21) != 0 {
            return refuse("MODE1 differs from the engine's or SIMD");
        }
        let Some(p) = crate::fast::helper::plan(d, &crate::fast::helper::EnvRegs(&self.env)) else {
            return refuse("no generated helper body for these fields");
        };
        // The body reads these from the engine state: the kernel's current
        // values go there first (their known masks are full: the region
        // requires every register it touches to be known at entry). L and B
        // are never written by a kernel (`Lower::wr`), MODE1 is the engine's.
        for &c in &p.touch.reads {
            let v = self.rd_bits(c)?;
            self.body.push(Inst::StSt32 {
                off: st_reg_b(c),
                v,
            });
        }
        let rec = HelperRec::leak_gen(pc, Some((p.id, p.f)));
        let status = self.emit(Ty::I32, Op::Helper(rec));
        let zero = self.ci(0);
        let ok = self.bin(Bin::Eq, status, zero);
        self.guard(ok);
        for &c in &p.touch.writes {
            let v = self.emit(Ty::I32, Op::St32(st_reg_b(c)));
            self.wr(c, v)?;
        }
        self.commit()
    }
}
