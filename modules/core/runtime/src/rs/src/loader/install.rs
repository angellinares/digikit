//! The hook installer (docs/plans/runtime-and-services.md, R12-3): writes a `jmp.l` to a trampoline over each BIND site and writes the stock bytes
//! back on restore. Generic over `Platform`, so the host tests run it on `FakePlatform`, whose call log is the fake of the installer.
//!
//! All-or-nothing per provider: pass 1 checks every site and writes nothing; pass 2 writes under the same interrupt mask, flushes the covered
//! lines and fills the slot words. Standalone for now: the Loader calls it in R12-3a/R12-3.

use dkneeds::keys::BindRef;
use dkrt_abi::platform::Platform;
use dkrt_sdk::*;

/// Installed sites at most (`[limits.format] install_sites` of sdk/sdk.toml): machines 21 + fx 9 and room.
pub const SITES_MAX: usize = dkneeds::INSTALL_SITES as usize;

/// `jmp.l` and `nop`, as big-endian words.
const JMP_L: u16 = 0x4ef9;
const NOP: u16 = 0x4e71;

/// One installed site, owned by record `rec`.
#[derive(Clone, Copy)]
pub struct Site {
    pub rec: u8,
    pub addr: u32,
    pub len: u8,
    pub stock: [u8; 64],
}

const EMPTY_SITE: Site = Site { rec: 0, addr: 0, len: 0, stock: [0; 64] };

pub struct Installer {
    sites: [Site; SITES_MAX],
    n: usize,
    /// False in safe mode: `install` does nothing.
    armed: bool,
}

impl Installer {
    pub const fn new() -> Installer {
        Installer { sites: [EMPTY_SITE; SITES_MAX], n: 0, armed: true }
    }

    pub fn set_armed(&mut self, armed: bool) {
        self.armed = armed;
    }

    /// Installed sites of all records.
    pub fn count(&self) -> usize {
        self.n
    }

    /// Pass 1: check every item, write nothing. Errors: `E_RANGE` (length, trampoline, stock slice), `E_PLAN_FIT` (no room for the sites),
    /// `E_PLAN_OWNER` (an installed site or an earlier item overlaps), `E_HOOK_STOCK` (the live bytes are not the stock bytes).
    #[inline(never)]
    pub unsafe fn check<P: Platform>(&self, p: &P, binds: &[BindRef], _image_base: u32, image_size: u32) -> Result<(), i32> {
        if self.n + binds.len() > SITES_MAX {
            return Err(E_PLAN_FIT);
        }
        let mut i = 0;
        while i < binds.len() {
            let b = &binds[i];
            let len = b.len as usize;
            if !(6..=64).contains(&len) || len % 2 != 0 || b.tramp >= image_size || b.stock.len() < len {
                return Err(E_RANGE);
            }
            let end = b.addr.wrapping_add(b.len);
            let mut j = 0;
            while j < self.n {
                let s = &self.sites[j];
                if b.addr < s.addr + s.len as u32 && s.addr < end {
                    return Err(E_PLAN_OWNER);
                }
                j += 1;
            }
            let mut j = 0;
            while j < i {
                let o = &binds[j];
                if b.addr < o.addr + o.len && o.addr < end {
                    return Err(E_PLAN_OWNER);
                }
                j += 1;
            }
            let mut cur = [0u8; 64];
            p.read_code(b.addr, &mut cur[..len]);
            if cur[..len] != b.stock[..len] {
                return Err(E_HOOK_STOCK);
            }
            i += 1;
        }
        Ok(())
    }

    /// Install the hooks of record `rec` and fill `slots` (slot address, value). Safe mode (not armed): `Ok` with no platform call. One mask:
    /// pass 1 again, then the writes, then the flush of the covered lines, then the slot words, then the restore. A refusal in pass 1 writes
    /// nothing.
    pub unsafe fn install<P: Platform>(
        &mut self,
        p: &P,
        rec: u8,
        binds: &[BindRef],
        image_base: u32,
        image_size: u32,
        slots: &[(u32, u32)],
    ) -> Result<(), i32> {
        if !self.armed {
            return Ok(());
        }
        let sr = p.irq_mask();
        if let Err(e) = self.check(p, binds, image_base, image_size) {
            p.irq_restore(sr);
            return Err(e);
        }
        let first = self.n;
        let mut i = 0;
        while i < binds.len() {
            let b = &binds[i];
            let len = b.len as usize;
            let mut bytes = [0u8; 64];
            let mut k = 0;
            while k < 64 {
                bytes[k..k + 2].copy_from_slice(&NOP.to_be_bytes());
                k += 2;
            }
            bytes[0..2].copy_from_slice(&JMP_L.to_be_bytes());
            bytes[2..6].copy_from_slice(&image_base.wrapping_add(b.tramp).to_be_bytes());
            p.write_code(b.addr, &bytes[..len]);
            let mut stock = [0u8; 64];
            stock[..len].copy_from_slice(&b.stock[..len]);
            self.sites[self.n] = Site { rec, addr: b.addr, len: b.len as u8, stock };
            self.n += 1;
            i += 1;
        }
        let mut i = first;
        while i < self.n {
            flush_site(p, self.sites[i].addr, self.sites[i].len);
            i += 1;
        }
        let mut i = 0;
        while i < slots.len() {
            p.write_word(slots[i].0, slots[i].1);
            p.flush(slots[i].0 & !15, 16);
            i += 1;
        }
        p.irq_restore(sr);
        Ok(())
    }

    /// Write the stock bytes back for every site of `rec`, clear its slot words (`slots`: their addresses) and drop the sites. One mask. Kept for
    /// hot reload.
    pub unsafe fn restore<P: Platform>(&mut self, p: &P, rec: u8, slots: &[u32]) {
        let sr = p.irq_mask();
        let mut i = 0;
        while i < self.n {
            if self.sites[i].rec == rec {
                let s = self.sites[i];
                p.write_code(s.addr, &s.stock[..s.len as usize]);
                flush_site(p, s.addr, s.len);
                self.n -= 1;
                self.sites[i] = self.sites[self.n];
                self.sites[self.n] = EMPTY_SITE;
            } else {
                i += 1;
            }
        }
        let mut i = 0;
        while i < slots.len() {
            p.write_word(slots[i], 0);
            p.flush(slots[i] & !15, 16);
            i += 1;
        }
        p.irq_restore(sr);
    }
}

/// Flush the 16-byte lines that cover `len` bytes at `addr`.
fn flush_site<P: Platform>(p: &P, addr: u32, len: u8) {
    let start = addr & !15;
    let end = (addr + len as u32 + 15) & !15;
    p.flush(start, end - start);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_platform::{Call, FakePlatform};
    use dkrt_abi::profiles;
    use std::vec::Vec;

    const BASE: u32 = 0x4100_0000;
    const SIZE: u32 = 0x1000;

    fn bind(addr: u32, len: u32, stock: &[u8]) -> BindRef<'_> {
        BindRef { site: addr, addr, len, tramp: 0x40, stock }
    }
    fn fake(binds: &[BindRef]) -> FakePlatform {
        let f = FakePlatform::new(profiles::DN2);
        for b in binds {
            f.put_code(b.addr, &b.stock[..b.len as usize]);
        }
        f
    }
    fn code(f: &FakePlatform, addr: u32, n: usize) -> Vec<u8> {
        let mut out = std::vec![0u8; n];
        unsafe { f.read_code(addr, &mut out) };
        out
    }

    #[test]
    fn one_provider_is_mask_writes_flush_slots_restore() {
        let (sa, sb) = ([0xaa; 6], [0xbb; 8]);
        let bs = [bind(0x1000_0000, 6, &sa), bind(0x1000_0100, 8, &sb)];
        let f = fake(&bs);
        let mut ins = Installer::new();
        unsafe { ins.install(&f, 3, &bs, BASE, SIZE, &[(0x2000_0000, 0x4100_0080), (0x2000_0004, 0x4100_0090)]).unwrap() };
        assert_eq!(
            *f.calls.borrow(),
            [
                Call::Mask,
                Call::Write { addr: 0x1000_0000, len: 6 },
                Call::Write { addr: 0x1000_0100, len: 8 },
                Call::Flush { addr: 0x1000_0000, len: 16 },
                Call::Flush { addr: 0x1000_0100, len: 16 },
                Call::Word { addr: 0x2000_0000, value: 0x4100_0080 },
                Call::Flush { addr: 0x2000_0000, len: 16 },
                Call::Word { addr: 0x2000_0004, value: 0x4100_0090 },
                Call::Flush { addr: 0x2000_0000, len: 16 },
                Call::Restore,
            ]
        );
        assert_eq!(code(&f, 0x1000_0000, 6), [0x4e, 0xf9, 0x41, 0x00, 0x00, 0x40]);
        assert_eq!(code(&f, 0x1000_0100, 8), [0x4e, 0xf9, 0x41, 0x00, 0x00, 0x40, 0x4e, 0x71]);
        assert_eq!((ins.count(), f.depth.get()), (2, 0));
    }

    /// A raw hook with a tail (`patchkit bind`): its BIND item holds the stock bytes of the site like any hook and its trampoline is the entry of the service's
    /// asm, whose tail was made from the same stock. A firmware that differs from those bytes is refused before any write, and the stock one installs `jmp entry`.
    #[test]
    fn a_raw_hook_with_a_tail_is_refused_on_other_stock_and_installed_on_its_own() {
        let stock = [0x70, 0x06, 0xb2, 0x80, 0x67, 0x00, 0x00, 0x20];
        let bs = [BindRef { site: 0x1000_0000, addr: 0x1000_0000, len: 8, tramp: 0x80, stock: &stock }];
        let f = fake(&bs);
        let mut other = stock;
        other[1] = 0x07;
        f.put_code(0x1000_0000, &other);
        let mut ins = Installer::new();
        assert_eq!(unsafe { ins.install(&f, 0, &bs, BASE, SIZE, &[]) }, Err(E_HOOK_STOCK));
        assert_eq!(*f.calls.borrow(), [Call::Mask, Call::Restore]);
        assert_eq!(code(&f, 0x1000_0000, 8), other, "nothing was written");
        f.put_code(0x1000_0000, &stock);
        unsafe { ins.install(&f, 0, &bs, BASE, SIZE, &[]).unwrap() };
        assert_eq!(code(&f, 0x1000_0000, 6), [0x4e, 0xf9, 0x41, 0x00, 0x00, 0x80]);
    }

    #[test]
    fn a_stock_mismatch_writes_nothing() {
        let (sa, sb) = ([0xaa; 6], [0xbb; 6]);
        let bs = [bind(0x1000_0000, 6, &sa), bind(0x1000_0100, 6, &sb)];
        let f = fake(&bs);
        f.put_code(0x1000_0100, &[0; 6]);
        let mut ins = Installer::new();
        assert_eq!(unsafe { ins.install(&f, 0, &bs, BASE, SIZE, &[(0x2000_0000, 1)]) }, Err(E_HOOK_STOCK));
        assert_eq!(*f.calls.borrow(), [Call::Mask, Call::Restore]);
        assert_eq!(ins.count(), 0);
        assert_eq!(code(&f, 0x1000_0000, 6), sa, "the first site is still stock");
    }

    #[test]
    fn check_makes_no_call() {
        let sa = [0xaa; 6];
        let bs = [bind(0x1000_0000, 6, &sa)];
        let f = fake(&bs);
        let ins = Installer::new();
        assert_eq!(unsafe { ins.check(&f, &bs, BASE, SIZE) }, Ok(()));
        assert!(f.calls.borrow().is_empty());
    }

    #[test]
    fn a_second_claimant_of_an_installed_site_is_refused() {
        let (sa, sc) = ([0xaa; 6], [0xcc; 6]);
        let bs = [bind(0x1000_0000, 6, &sa)];
        let f = fake(&bs);
        let mut ins = Installer::new();
        unsafe { ins.install(&f, 0, &bs, BASE, SIZE, &[]).unwrap() };
        f.calls.borrow_mut().clear();
        // overlapping range, stock that matches nothing is irrelevant: the owner check comes first
        let again = [bind(0x1000_0004, 6, &sc)];
        f.put_code(0x1000_0004, &sc);
        assert_eq!(unsafe { ins.install(&f, 1, &again, BASE, SIZE, &[]) }, Err(E_PLAN_OWNER));
        assert_eq!(*f.calls.borrow(), [Call::Mask, Call::Restore]);
        assert_eq!(ins.count(), 1);
    }

    #[test]
    fn overlap_inside_one_batch_is_refused() {
        let s = [0xaa; 8];
        let bs = [bind(0x1000_0000, 8, &s), bind(0x1000_0006, 6, &s)];
        let f = fake(&bs);
        let mut ins = Installer::new();
        assert_eq!(unsafe { ins.install(&f, 0, &bs, BASE, SIZE, &[]) }, Err(E_PLAN_OWNER));
        assert!(!f.calls.borrow().iter().any(|c| matches!(c, Call::Write { .. })));
        assert_eq!(ins.count(), 0);
    }

    #[test]
    fn bad_length_or_trampoline_is_refused() {
        let s = [0xaa; 66];
        let f = FakePlatform::new(profiles::DN2);
        let ins = Installer::new();
        for len in [4u32, 7, 66] {
            assert_eq!(unsafe { ins.check(&f, &[bind(0x1000_0000, len, &s)], BASE, SIZE) }, Err(E_RANGE), "len {len}");
        }
        let mut b = bind(0x1000_0000, 6, &s);
        b.tramp = SIZE;
        assert_eq!(unsafe { ins.check(&f, &[b], BASE, SIZE) }, Err(E_RANGE));
    }

    #[test]
    fn safe_mode_makes_no_call() {
        let sa = [0xaa; 6];
        let bs = [bind(0x1000_0000, 6, &sa)];
        let f = fake(&bs);
        let mut ins = Installer::new();
        ins.set_armed(false);
        assert_eq!(unsafe { ins.install(&f, 0, &bs, BASE, SIZE, &[(0x2000_0000, 1)]) }, Ok(()));
        assert!(f.calls.borrow().is_empty());
        assert_eq!(ins.count(), 0);
        assert_eq!(code(&f, 0x1000_0000, 6), sa);
    }

    #[test]
    fn restore_writes_the_stock_back_and_clears_the_slot() {
        let (sa, sb) = ([0xaa; 6], [0xbb; 6]);
        let bs = [bind(0x1000_0000, 6, &sa), bind(0x1000_0100, 6, &sb)];
        let f = fake(&bs);
        let mut ins = Installer::new();
        unsafe { ins.install(&f, 4, &bs, BASE, SIZE, &[(0x2000_0000, 0x4100_0080)]).unwrap() };
        f.calls.borrow_mut().clear();
        unsafe { ins.restore(&f, 4, &[0x2000_0000]) };
        assert_eq!(code(&f, 0x1000_0000, 6), sa);
        assert_eq!(code(&f, 0x1000_0100, 6), sb);
        assert_eq!(code(&f, 0x2000_0000, 4), [0, 0, 0, 0]);
        let c = f.calls.borrow();
        assert_eq!((c[0], *c.last().unwrap()), (Call::Mask, Call::Restore));
        let w = c.iter().position(|x| *x == Call::Word { addr: 0x2000_0000, value: 0 }).unwrap();
        assert_eq!(c[w + 1], Call::Flush { addr: 0x2000_0000, len: 16 });
        assert_eq!((ins.count(), f.depth.get()), (0, 0));
    }

    #[test]
    fn restore_keeps_the_sites_of_other_records() {
        let (sa, sb) = ([0xaa; 6], [0xbb; 6]);
        let a = [bind(0x1000_0000, 6, &sa)];
        let b = [bind(0x1000_0100, 6, &sb)];
        let f = fake(&a);
        f.put_code(0x1000_0100, &sb);
        let mut ins = Installer::new();
        unsafe {
            ins.install(&f, 1, &a, BASE, SIZE, &[]).unwrap();
            ins.install(&f, 2, &b, BASE, SIZE, &[]).unwrap();
            ins.restore(&f, 1, &[]);
        }
        assert_eq!(ins.count(), 1);
        assert_eq!(code(&f, 0x1000_0000, 6), sa);
        assert_eq!(code(&f, 0x1000_0100, 2), [0x4e, 0xf9]);
    }

    #[test]
    fn the_flush_covers_a_site_that_straddles_a_line() {
        let (sa, sb) = ([0xaa; 6], [0xbb; 6]);
        let bs = [bind(0x1000_000c, 6, &sa), bind(0x1000_0100, 6, &sb)];
        let f = fake(&bs);
        let mut ins = Installer::new();
        unsafe { ins.install(&f, 0, &bs, BASE, SIZE, &[]).unwrap() };
        let c = f.calls.borrow();
        assert!(c.contains(&Call::Flush { addr: 0x1000_0000, len: 32 }));
        assert!(c.contains(&Call::Flush { addr: 0x1000_0100, len: 16 }));
    }

    #[test]
    fn more_sites_than_the_table_holds_are_refused() {
        let s = [0xaa; 6];
        let bs: Vec<BindRef> = (0..SITES_MAX as u32 + 1).map(|i| bind(0x1000_0000 + i * 16, 6, &s)).collect();
        let f = fake(&bs);
        let mut ins = Installer::new();
        assert_eq!(unsafe { ins.install(&f, 0, &bs, BASE, SIZE, &[]) }, Err(E_PLAN_FIT));
        assert_eq!(ins.count(), 0);
        // exactly the capacity fits, and the next provider then finds no room
        unsafe { ins.install(&f, 0, &bs[..SITES_MAX], BASE, SIZE, &[]).unwrap() };
        assert_eq!(unsafe { ins.check(&f, &bs[SITES_MAX..], BASE, SIZE) }, Err(E_PLAN_FIT));
    }
}
