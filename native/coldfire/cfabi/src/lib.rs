//! A minimal C ABI for `tools/cf_lockstep.py` (ctypes, no PyO3): a `Cpu`
//! plus a sparse-page `Bus` the harness loads snapshot memory into, and a
//! one-shot read-override table. Peripheral (MMIO) reads are not modelled
//! here; the harness replays the value Unicorn's own read of that address
//! returned (`cf_set_override`), consumed by the next read at that exact
//! address, so only the CPU core is under test (see docs/plan-native-emulator.md
//! P3 stage 2, "Peripheral accesses").
//!
//! A separate crate (not a module of `coldfire` itself) so the pure,
//! wasm32-buildable `coldfire` rlib never declares a `cdylib` crate-type:
//! wasm32 has no native dynamic linker, and asking Cargo for a `cdylib`
//! there drags in the wasm *linker* (rust-lld) even for a plain `--lib`
//! build. This crate depends on `coldfire` as an ordinary path dependency
//! and is desktop-only.

use coldfire::{Bus, BusError, Cpu, Stop};
use std::collections::HashMap;

const PAGE_BITS: u32 = 12;
const PAGE_SIZE: usize = 1 << PAGE_BITS;
const PAGE_MASK: u32 = (PAGE_SIZE as u32) - 1;

#[derive(Default)]
struct SparseMem {
    pages: HashMap<u32, Box<[u8; PAGE_SIZE]>>,
    /// addr -> (size in bytes, value), consumed by the next read that
    /// starts at that address.
    overrides: HashMap<u32, (u8, u32)>,
}

impl SparseMem {
    fn page_mut(&mut self, base: u32) -> &mut [u8; PAGE_SIZE] {
        self.pages
            .entry(base)
            .or_insert_with(|| Box::new([0; PAGE_SIZE]))
    }

    /// Per-page `copy_from_slice`, not a byte loop: the harness loads whole
    /// snapshot pages (up to 1 MiB each) this way, and a byte-at-a-time copy
    /// made that visibly slow.
    fn write_bytes(&mut self, addr: u32, data: &[u8]) {
        let mut off = 0usize;
        while off < data.len() {
            let a = addr.wrapping_add(off as u32);
            let base = a & !PAGE_MASK;
            let page_off = (a & PAGE_MASK) as usize;
            let n = (PAGE_SIZE - page_off).min(data.len() - off);
            self.page_mut(base)[page_off..page_off + n].copy_from_slice(&data[off..off + n]);
            off += n;
        }
    }

    fn read_bytes(&self, addr: u32, out: &mut [u8]) -> bool {
        let mut off = 0usize;
        while off < out.len() {
            let a = addr.wrapping_add(off as u32);
            let base = a & !PAGE_MASK;
            let page_off = (a & PAGE_MASK) as usize;
            let n = (PAGE_SIZE - page_off).min(out.len() - off);
            match self.pages.get(&base) {
                Some(p) => out[off..off + n].copy_from_slice(&p[page_off..page_off + n]),
                None => return false,
            }
            off += n;
        }
        true
    }

    fn override_read(&mut self, addr: u32, size: u8) -> Option<u32> {
        match self.overrides.remove(&addr) {
            Some((s, v)) if s == size => Some(v),
            Some(other) => {
                // Put it back: a mismatched size at this address is a
                // harness bug, not something to silently eat.
                self.overrides.insert(addr, other);
                None
            }
            None => None,
        }
    }
}

impl Bus for SparseMem {
    fn read8(&mut self, addr: u32) -> Result<u8, BusError> {
        if let Some(v) = self.override_read(addr, 1) {
            return Ok(v as u8);
        }
        let mut b = [0u8; 1];
        if self.read_bytes(addr, &mut b) {
            Ok(b[0])
        } else {
            Err(BusError { addr, write: false })
        }
    }
    fn read16(&mut self, addr: u32) -> Result<u16, BusError> {
        if let Some(v) = self.override_read(addr, 2) {
            return Ok(v as u16);
        }
        let mut b = [0u8; 2];
        if self.read_bytes(addr, &mut b) {
            Ok(u16::from_be_bytes(b))
        } else {
            Err(BusError { addr, write: false })
        }
    }
    fn read32(&mut self, addr: u32) -> Result<u32, BusError> {
        if let Some(v) = self.override_read(addr, 4) {
            return Ok(v);
        }
        let mut b = [0u8; 4];
        if self.read_bytes(addr, &mut b) {
            Ok(u32::from_be_bytes(b))
        } else {
            Err(BusError { addr, write: false })
        }
    }
    fn write8(&mut self, addr: u32, v: u8) -> Result<(), BusError> {
        self.write_bytes(addr, &[v]);
        Ok(())
    }
    fn write16(&mut self, addr: u32, v: u16) -> Result<(), BusError> {
        self.write_bytes(addr, &v.to_be_bytes());
        Ok(())
    }
    fn write32(&mut self, addr: u32, v: u32) -> Result<(), BusError> {
        self.write_bytes(addr, &v.to_be_bytes());
        Ok(())
    }
}

/// All registers the harness compares, in one fixed-layout struct (see
/// `tools/cf_lockstep.py`'s matching `ctypes.Structure`).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct CfRegs {
    pub d: [u32; 8],
    pub a: [u32; 8],
    pub other_a7: u32,
    pub pc: u32,
    pub sr: u32,
    pub vbr: u32,
    pub cacr: u32,
    pub asid: u32,
    pub acr: [u32; 8],
    pub mmubar: u32,
    pub rgpiobar: u32,
    pub rambar: u32,
    pub macsr: u32,
    pub acc: [u32; 4],
    pub accext01: u32,
    pub accext23: u32,
    pub mask: u32,
}

pub struct CfCore {
    cpu: Cpu,
    mem: SparseMem,
}

fn regs_from_cpu(cpu: &Cpu) -> CfRegs {
    CfRegs {
        d: cpu.d,
        a: cpu.a,
        other_a7: cpu.other_a7,
        pc: cpu.pc,
        sr: cpu.sr as u32,
        vbr: cpu.ctrl.vbr,
        cacr: cpu.ctrl.cacr,
        asid: cpu.ctrl.asid,
        acr: cpu.ctrl.acr,
        mmubar: cpu.ctrl.mmubar,
        rgpiobar: cpu.ctrl.rgpiobar,
        rambar: cpu.ctrl.rambar,
        macsr: cpu.emac.macsr,
        acc: cpu.emac.acc,
        accext01: cpu.emac.accext01,
        accext23: cpu.emac.accext23,
        mask: cpu.emac.mask,
    }
}

fn apply_regs(cpu: &mut Cpu, r: &CfRegs) {
    cpu.d = r.d;
    cpu.a = r.a;
    cpu.other_a7 = r.other_a7;
    cpu.pc = r.pc;
    cpu.sr = r.sr as u16;
    cpu.ctrl.vbr = r.vbr;
    cpu.ctrl.cacr = r.cacr;
    cpu.ctrl.asid = r.asid;
    cpu.ctrl.acr = r.acr;
    cpu.ctrl.mmubar = r.mmubar;
    cpu.ctrl.rgpiobar = r.rgpiobar;
    cpu.ctrl.rambar = r.rambar;
    cpu.emac.macsr = r.macsr;
    cpu.emac.acc = r.acc;
    cpu.emac.accext01 = r.accext01;
    cpu.emac.accext23 = r.accext23;
    cpu.emac.mask = r.mask;
}

/// # Safety
/// The returned pointer is owned by the caller and must be freed with
/// `cf_free` exactly once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cf_new() -> *mut CfCore {
    Box::into_raw(Box::new(CfCore {
        cpu: Cpu::new(),
        mem: SparseMem::default(),
    }))
}

/// # Safety
/// `p` must be a pointer returned by `cf_new`, not yet freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cf_free(p: *mut CfCore) {
    if !p.is_null() {
        drop(unsafe { Box::from_raw(p) });
    }
}

/// # Safety
/// `p` must be live; `out` must point to a valid `CfRegs`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cf_get_regs(p: *mut CfCore, out: *mut CfRegs) {
    let c = unsafe { &*p };
    unsafe { *out = regs_from_cpu(&c.cpu) };
}

/// # Safety
/// `p` must be live; `inp` must point to a valid `CfRegs`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cf_set_regs(p: *mut CfCore, inp: *const CfRegs) {
    let c = unsafe { &mut *p };
    apply_regs(&mut c.cpu, unsafe { &*inp });
}

/// Load `len` bytes at `addr` into the sparse memory (snapshot pages).
///
/// # Safety
/// `p` must be live; `data` must point to `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cf_write_mem(p: *mut CfCore, addr: u32, data: *const u8, len: usize) {
    let c = unsafe { &mut *p };
    let slice = unsafe { std::slice::from_raw_parts(data, len) };
    c.mem.write_bytes(addr, slice);
}

/// Read `len` bytes at `addr`; returns 0 on success, 1 if any byte in the
/// range has never been written (unmapped).
///
/// # Safety
/// `p` must be live; `data` must point to `len` writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cf_read_mem(p: *mut CfCore, addr: u32, data: *mut u8, len: usize) -> i32 {
    let c = unsafe { &*p };
    let out = unsafe { std::slice::from_raw_parts_mut(data, len) };
    if c.mem.read_bytes(addr, out) { 0 } else { 1 }
}

/// Queue a one-shot replayed read: the next read of exactly `size` bytes
/// (1, 2 or 4) starting at `addr` returns `value` instead of consulting
/// memory, and the override is then consumed.
///
/// # Safety
/// `p` must be live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cf_set_override(p: *mut CfCore, addr: u32, size: u8, value: u32) {
    let c = unsafe { &mut *p };
    c.mem.overrides.insert(addr, (size, value));
}

/// Drop any queued overrides that the step did not consume (a harness bug
/// otherwise hides itself: the read the oracle made never happened here).
///
/// # Safety
/// `p` must be live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cf_pending_overrides(p: *mut CfCore) -> usize {
    let c = unsafe { &*p };
    c.mem.overrides.len()
}

/// # Safety
/// `p` must be live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cf_clear_overrides(p: *mut CfCore) {
    let c = unsafe { &mut *p };
    c.mem.overrides.clear();
}

/// Step result codes for `cf_step`.
pub mod step_result {
    pub const OK: i32 = 0;
    pub const EXCEPTION: i32 = 1;
    pub const UNIMPLEMENTED: i32 = 2;
    pub const HALTED: i32 = 3;
}

/// Execute exactly one instruction. Returns a `step_result` code;
/// `cf_last_vector`/`cf_last_form` give detail for `EXCEPTION`/
/// `UNIMPLEMENTED`.
///
/// # Safety
/// `p` must be live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cf_step(p: *mut CfCore) -> i32 {
    let c = unsafe { &mut *p };
    match c.cpu.step(&mut c.mem) {
        Ok(()) => {
            if c.cpu.last_exception.is_some() {
                step_result::EXCEPTION
            } else {
                step_result::OK
            }
        }
        Err(Stop::Halted) => step_result::HALTED,
        Err(Stop::Unimplemented(_)) => step_result::UNIMPLEMENTED,
    }
}

/// # Safety
/// `p` must be live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cf_last_vector(p: *mut CfCore) -> i32 {
    let c = unsafe { &*p };
    c.cpu.last_exception.map_or(-1, |v| v as i32)
}

/// The `Form` discriminant of the instruction `cf_step` could not execute
/// (`UNIMPLEMENTED`), for a human-readable name via `coldfire::Form`'s
/// `Debug` on the Rust side, or `tools/cfisa/coldfire.json`'s form order on
/// the Python side.
///
/// # Safety
/// `p` must be live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cf_last_form(p: *mut CfCore) -> i32 {
    let c = unsafe { &*p };
    match c.cpu.last_unimplemented {
        Some(f) => f as i32,
        None => -1,
    }
}

/// # Safety
/// `p` must be live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cf_icount(p: *mut CfCore) -> u64 {
    let c = unsafe { &*p };
    c.cpu.icount
}
