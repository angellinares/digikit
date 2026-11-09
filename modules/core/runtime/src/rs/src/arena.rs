//! The module arena: first fit over an address-ordered free list with coalescing on free. Blocks are 16-byte aligned
//! (the ColdFire cache line). A block starts with a 16-byte header: word 0 the block size (header included, a multiple
//! of 16), word 1 the next free block while free or `USED` while allocated.

use crate::{st, ARENA_BYTES, DKRT_ARENA};
use core::ptr::{addr_of_mut, null_mut};

const HDR: u32 = dkneeds::ARENA_HDR;
const USED: u32 = 0xa110_c8ed;

pub unsafe fn init() {
    let s = st();
    let base = addr_of_mut!(DKRT_ARENA) as u32;
    let lo = (base + 15) & !15;
    let hi = (base + ARENA_BYTES as u32) & !15;
    s.arena_lo = lo;
    s.arena_hi = hi;
    let b = lo as *mut u32;
    *b = hi - lo;
    *b.add(1) = 0;
    s.free_head = lo;
}

/// A block of at least `n` bytes, 16-byte aligned; null when none fits.
pub unsafe fn alloc(n: u32) -> *mut u8 {
    let s = st();
    if n == 0 || n > ARENA_BYTES as u32 {
        return null_mut();
    }
    let mut need = ((n + 15) & !15) + HDR;
    let mut prev: *mut u32 = null_mut();
    let mut cur = s.free_head as *mut u32;
    while !cur.is_null() {
        let sz = *cur;
        let next = *cur.add(1);
        if sz >= need {
            let rem = sz - need;
            let replacement = if rem >= 2 * HDR {
                let nb = (cur as u32 + need) as *mut u32;
                *nb = rem;
                *nb.add(1) = next;
                nb as u32
            } else {
                need = sz;
                next
            };
            if prev.is_null() {
                s.free_head = replacement;
            } else {
                *prev.add(1) = replacement;
            }
            *cur = need;
            *cur.add(1) = USED;
            return (cur as *mut u8).add(HDR as usize);
        }
        prev = cur;
        cur = next as *mut u32;
    }
    null_mut()
}

/// Return a block. A pointer that is not a live block of this arena is ignored; returns whether it was freed.
pub unsafe fn free(p: *mut u8) -> bool {
    #[cfg(test)] { return crate::cleanup_tests::arena_free(p); }
    #[cfg(not(test))]
    {
    let s = st();
    let a = p as u32;
    if a < s.arena_lo + HDR || a >= s.arena_hi || a & 15 != 0 {
        return false;
    }
    let h = (a - HDR) as *mut u32;
    if *h.add(1) != USED {
        return false;
    }
    let size = *h;
    let mut prev: *mut u32 = null_mut();
    let mut cur = s.free_head as *mut u32;
    while !cur.is_null() && (cur as u32) < (h as u32) {
        prev = cur;
        cur = *cur.add(1) as *mut u32;
    }
    *h.add(1) = cur as u32;
    // merge with the next block
    if !cur.is_null() && (h as u32) + size == cur as u32 {
        *h = size + *cur;
        *h.add(1) = *cur.add(1);
    }
    // merge with the previous block
    if !prev.is_null() && (prev as u32) + *prev == h as u32 {
        *prev += *h;
        *prev.add(1) = *h.add(1);
    } else if prev.is_null() {
        s.free_head = h as u32;
    } else {
        *prev.add(1) = h as u32;
    }
    true
    }
}

/// (total free bytes, largest free block) in payload bytes.
pub unsafe fn stats() -> (u32, u32) {
    let s = st();
    let mut total = 0;
    let mut largest = 0;
    let mut cur = s.free_head as *mut u32;
    while !cur.is_null() {
        let sz = *cur;
        total += sz;
        if sz > largest {
            largest = sz;
        }
        cur = *cur.add(1) as *mut u32;
    }
    (total, if largest > HDR { largest - HDR } else { 0 })
}
