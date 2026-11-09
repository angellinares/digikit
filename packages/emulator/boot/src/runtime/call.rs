//! Call a function on the ColdFire core (patchkit unit tests).
//!
//! `Emulator::call_function` runs one guest function to its return with the
//! gcc m68k ABI (arguments on the stack, return address pushed last, result
//! in D0/A0; D2-D7 and A2-A6 are callee-saved). The caller picks a scratch
//! stack and a return sentinel address; the sentinel is never executed.
//! Code may live outside the MAIN image (the expansion region): those
//! instructions run through the bare CPU step plus `finish_step`, without the
//! boot-progress observers that only matter inside the MAIN image.

use super::*;

#[derive(Clone, Debug)]
pub struct CallSpec {
    pub pc: u32,
    pub args: Vec<u32>,
    /// Initial A7 (word aligned, inside SDRAM); args and the return address go below it.
    pub sp: u32,
    /// Address whose arrival ends the call.
    pub ret_pc: u32,
    pub sr: u16,
    pub max_insns: u64,
}

#[derive(Clone, Debug)]
pub struct CallResult {
    pub finished: bool,
    /// Registers before the call (the caller checks the callee-saved ones come back).
    pub d_in: [u32; 8],
    pub a_in: [u32; 8],
    pub d: [u32; 8],
    pub a: [u32; 8],
    pub pc: u32,
    pub insns: u64,
    pub error: Option<String>,
}

impl Emulator {
    /// Write bytes into guest SDRAM (words are read-modify-written at the edges).
    pub fn poke_bytes(&mut self, addr: u32, bytes: &[u8]) -> Result<(), String> {
        let mut a = addr & !3;
        let end = addr + bytes.len() as u32;
        while a < end {
            let mut w = self.peek32(a).unwrap_or(0).to_be_bytes();
            for (k, b) in w.iter_mut().enumerate() {
                let at = a + k as u32;
                if at >= addr && at < end {
                    *b = bytes[(at - addr) as usize];
                }
            }
            self.poke32(a, u32::from_be_bytes(w))?;
            a += 4;
        }
        Ok(())
    }

    pub fn peek_bytes(&mut self, addr: u32, len: usize) -> Result<Vec<u8>, String> {
        let mut out = Vec::with_capacity(len + 4);
        let mut a = addr & !3;
        while a < addr + len as u32 {
            let w = self
                .peek32(a)
                .ok_or_else(|| format!("peek {a:#x}: unmapped"))?;
            out.extend_from_slice(&w.to_be_bytes());
            a += 4;
        }
        let skip = (addr & 3) as usize;
        Ok(out[skip..skip + len].to_vec())
    }

    /// Run `spec.pc` as a C function and return the register file at the
    /// return. The CPU registers are restored afterwards; memory and
    /// peripheral state are not.
    pub fn call_function(&mut self, spec: &CallSpec) -> Result<CallResult, String> {
        if self.error.is_some() {
            return Err(format!("emulator faulted: {:?}", self.error));
        }
        let saved = (
            self.cpu.d,
            self.cpu.a,
            self.cpu.pc,
            self.cpu.sr,
            self.cpu.other_a7,
        );
        let mut sp = spec.sp;
        for &arg in spec.args.iter().rev() {
            sp -= 4;
            self.poke32(sp, arg)?;
        }
        sp -= 4;
        self.poke32(sp, spec.ret_pc)?;
        self.cpu.a[7] = sp;
        self.cpu.pc = spec.pc;
        self.cpu.sr = spec.sr;
        let start = self.cpu.icount;
        let mut finished = false;
        while self.error.is_none() && self.cpu.icount - start < spec.max_insns {
            if self.cpu.pc == spec.ret_pc {
                finished = true;
                break;
            }
            let pc = self.cpu.pc;
            let in_main = pc >= MAIN_LOAD && ((pc - MAIN_LOAD) as usize) + 6 <= self.main.len();
            if in_main {
                self.step_once();
            } else {
                match self.oracle_ticks() {
                    Ok(ticks) => self.bus.current_icount = ticks,
                    Err(e) => {
                        self.set_error(e);
                        break;
                    }
                }
                self.bus.clear();
                self.bus.current_pc = pc;
                let before = self.cpu.icount;
                let result = self.cpu.step(&mut self.bus).map_err(|e| format!("{e:?}"));
                self.interpreted_instructions += self.cpu.icount.saturating_sub(before);
                self.finish_step(result);
            }
        }
        let out = CallResult {
            finished,
            d_in: saved.0,
            a_in: saved.1,
            d: self.cpu.d,
            a: self.cpu.a,
            pc: self.cpu.pc,
            insns: self.cpu.icount - start,
            error: self.error.clone(),
        };
        if self.error.is_none() {
            (
                self.cpu.d,
                self.cpu.a,
                self.cpu.pc,
                self.cpu.sr,
                self.cpu.other_a7,
            ) = saved;
        }
        Ok(out)
    }
}
