//! System forms and system-register moves, as `tools/sharc_dev/sharc_core` executes
//! them (`forms_system.py`, `forms_move._type_17a`, `_type_5a_move`).
//!
//! * 17b: `Ureg = <16-bit immediate>`, sign-extended (`_signed(data, 16)`),
//!   written like 17a.
//! * 21a, 21c: the core advances the PC and does nothing else.
//! * 18a set/clear of MODE1 (`Type18a` BOP 0/1): lowered only when the new
//!   MODE1 equals the old one, which the entry requirement establishes
//!   (`Req::Mode1` on the bits of the mask). The write is then a no-op on
//!   the register file; the one architectural side effect of a MODE1 write,
//!   the delayed bank selection (`_bank_request`), exists only with the bank
//!   model, which is refused (`Env::bank_model`).
//! * MODE1 as a move source: its value at entry, which the kernel has not
//!   written (no lowered form writes it), baked in under a full-width
//!   `Req::Mode1`.

use super::*;

/// UREG code of MODE1.
pub const MODE1_CODE: u32 = 114;

/// System registers the core reads and writes as plain register-file words
/// (`_write_ureg` and `_ureg` have no case for them under any model): PX1,
/// PX2, USTAT1, USTAT2, MMASK, MODE2, USTAT3, USTAT4. The kernel holds them
/// like any other register (lane F4).
pub fn plain_sys(code: u32) -> bool {
    matches!(code, 108 | 109 | 112 | 113 | 115 | 116 | 126 | 127)
}
const USTAT1_CODE: u32 = 112;

impl Lower {
    /// 17b: `Ureg = imm16`.
    pub fn form_17b(&mut self, d: &Dec) -> LR<()> {
        let ureg = d.field("ureg").ok_or(Refuse("17b ureg".into()))? as u32;
        let raw = d.get("data[15:0]").ok_or(Refuse("17b data".into()))?;
        let value = ((raw & 0xffff) as i16 as i32) as u32;
        let v = self.ci(value);
        self.wr(ureg, v)
    }

    /// 21a, 21c: no architectural effect.
    pub fn form_nop(&mut self) -> LR<()> {
        Ok(())
    }

    /// 18a MODE1 set/clear that leaves MODE1 unchanged (see the module doc).
    pub fn form_18a(&mut self, d: &Dec) -> LR<()> {
        let bop = d.field("bop").ok_or(Refuse("18a bop".into()))?;
        let sreg = d.field("sreg").ok_or(Refuse("18a sreg".into()))? as u32;
        if USTAT1_CODE + sreg != MODE1_CODE || !matches!(bop, 0 | 1) {
            return refuse(format!("18a bop {bop} sreg {sreg}"));
        }
        let mask = d.wide("data").ok_or(Refuse("18a data".into()))?;
        let mode1 = self.mode1_cur;
        let unchanged = match bop {
            0 => mode1 & mask == mask,
            _ => mode1 & mask == 0,
        };
        if !unchanged {
            return refuse("18a MODE1 write changes MODE1");
        }
        self.mode1_mask |= mask;
        self.bank_pair()?;
        Ok(())
    }

    /// A MODE1 write with the bank model on (`state._bank_request`): it
    /// latches MODE1's bank bits and applies them after the next instruction.
    /// With the applied selection equal to MODE1's (`Req::BankSync`) and the
    /// bank bits unchanged by the write, it changes nothing but the
    /// one-instruction `bank_pending_mask`. The pair is exact when no exit can
    /// fall between the two instructions: the next instruction is a plain
    /// successor in this block (`bank_next_ok`), and its exits report the state
    /// before the write (`bank_site_next`, used by `site_k`).
    pub fn bank_pair(&mut self) -> LR<()> {
        if self.env.bank_model {
            let Some(x) = self.cfgx.as_ref() else {
                if self.pass == 0 && self.cfgx.is_none() && !self.cfg_shape {
                    // The scratch lowering that asks whether the instruction
                    // can be lowered at all (`cfg::lowerable`).
                    return Ok(());
                }
                return refuse("MODE1 write with the bank model outside a CFG kernel");
            };
            if !self.bank_next_ok || x.pending.is_some() {
                return refuse("MODE1 write with the bank model: no plain successor");
            }
            self.bank_site_next = Some((x.pc, x.nblock));
            if !self.reqs.contains(&Req::BankSync) {
                self.reqs.push(Req::BankSync);
            }
        }
        Ok(())
    }

    /// A register read of a system register the lowering can bake in:
    /// MODE1.
    pub fn rd_sys(&mut self, code: u32) -> LR<Val> {
        if code == MODE1_CODE {
            self.mode1_mask = u32::MAX;
            let k = self.mode1_cur;
            return Ok(self.ci(k));
        }
        refuse(format!("register code {code} outside R/I/M"))
    }
}
