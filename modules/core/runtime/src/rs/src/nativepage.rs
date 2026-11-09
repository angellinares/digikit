//! Validated registration/lifetime for native callback page v1.
//! Firmware view, bitmap and event details belong to core/machines.
use crate::rd32;
use dkrt_sdk::*;

pub const KIND: u32 = 6;
pub const LEN: u32 = 0x90;
pub const KIND_V2: u32 = 7;
pub const LEN_V2: u32 = 0x98;

pub fn is_kind(kind: u32) -> bool { kind == KIND || kind == KIND_V2 }
pub fn binding_offset(kind: u32) -> usize { if kind == KIND_V2 { 48 } else { 40 } }
pub unsafe fn page_index(kind: u32, body: *const u8) -> u32 { if kind == KIND_V2 { rd32(body.add(40)) } else { 1 } }

/// Bounded owner/index validation shared by registry checking and host tests.
pub struct PageSet<const N: usize> {
    owners: [u32; N], modes: [u32; N], masks: [u8; N], count: usize,
}
impl<const N: usize> PageSet<N> {
    pub const fn new() -> Self { Self { owners: [0; N], modes: [0; N], masks: [0; N], count: 0 } }
    pub fn claim(&mut self, kind: u32, owner: u32, page: u32) -> bool {
        if !is_kind(kind) || page > 1 || (kind == KIND && page != 1) { return false; }
        let slot = match self.owners[..self.count].iter().position(|&x| x == owner) {
            Some(slot) => slot,
            None => {
                if self.count == N { return false; }
                let slot = self.count; self.count += 1;
                self.owners[slot] = owner; self.modes[slot] = kind; slot
            }
        };
        let bit = 1u8 << page;
        if self.modes[slot] != kind || self.masks[slot] & bit != 0 { return false; }
        self.masks[slot] |= bit;
        true
    }
    pub fn complete(&self) -> bool {
        (0..self.count).all(|i| self.modes[i] != KIND_V2 || self.masks[i] == 3)
    }
}

#[cfg(not(test))]
extern "C" {
    fn mf_native_register(kind: u32, title: *const u8, draw: u32, event: u32, bindings: *const u32, words: *const u32, word_count: u32) -> i32;
    fn mf_native_register_v2(kind: u32, page: u32, flags: u32, title: *const u8, draw: u32, event: u32, bindings: *const u32, words: *const u32, word_count: u32) -> i32;
    fn mf_native_remove(kind: u32);
    fn mf_native_turn(view: *const u8, event: *const u8) -> u32;
}

pub fn available(kind: u32) -> bool {
    #[cfg(test)] { let _ = kind; true }
    #[cfg(not(test))] { if kind == KIND_V2 { core::hint::black_box(mf_native_register_v2 as usize) != 0 } else { core::hint::black_box(mf_native_register as usize) != 0 } }
}

/// Body excludes kind/length. PARAM indexes belong to this file; the caller also
/// checks that the referenced machine owns each PARAM through its ordinary rows.
pub unsafe fn check(kind: u32, body: *const u8, params: u32, image_size: u32) -> i32 {
    if !is_kind(kind) { return E_FORMAT; }
    if !available(kind) { return E_NO_REGISTRY; }
    let flags = if kind == KIND_V2 { rd32(body.add(44)) } else { 0 };
    if flags & !3 != 0 || (flags & 1 != 0 && page_index(kind, body) != 0) || (flags & 2 != 0 && page_index(kind, body) != 1) { return E_FORMAT; }
    let draw = rd32(body.add(8));
    let event = rd32(body.add(12));
    if rd32(body) != (if kind == KIND_V2 { 2 } else { 1 }) || page_index(kind, body) > 1 || draw >= image_size || event >= image_size || (draw | event) & 1 != 0 {
        return E_FORMAT;
    }
    let mut terminated = false;
    let mut n = 0;
    while n < 24 {
        let c = *body.add(16 + n);
        if c == 0 { terminated = n != 0; break; }
        if !(0x20..0x7f).contains(&c) { return E_FORMAT; }
        n += 1;
    }
    if !terminated { return E_FORMAT; }
    let mut positions = 0u32;
    let mut absent = false;
    let mut i = 0;
    while i < 8 {
        let b = body.add(binding_offset(kind) + 12 * i);
        let (kind, value, pos) = (rd32(b), rd32(b.add(4)), rd32(b.add(8)));
        if kind == 0 {
            if value != 0 || pos != 0 { return E_FORMAT; }
            absent = true;
        } else {
            if absent || kind > 2 || pos >= 8 || positions & (1 << pos) != 0 ||
                (kind == 1 && value >= params) || (kind == 2 && value >= 256) { return E_FORMAT; }
            positions |= 1 << pos;
        }
        i += 1;
    }
    if positions == 0 { E_FORMAT } else { OK }
}

pub unsafe fn apply(kind: u32, body: *const u8, base: u32, codes: &[u32], words: &[u32]) -> i32 {
    if words.len() > 8 { return E_FORMAT; }
    let mut bindings = [[0u32; 3]; 8];
    for (i, binding) in bindings.iter_mut().enumerate() {
        let b = body.add(binding_offset(kind) + 12 * i);
        binding[0] = rd32(b);
        let value = rd32(b.add(4));
        binding[1] = if binding[0] == 1 { codes[value as usize] } else { value };
        binding[2] = rd32(b.add(8));
    }
    #[cfg(not(test))] {
        let owner = crate::assign::resolve(rd32(body.add(4)));
        if kind == KIND_V2 {
            mf_native_register_v2(owner, page_index(kind, body), rd32(body.add(44)), body.add(16),
                base + rd32(body.add(8)), base + rd32(body.add(12)), bindings.as_ptr() as *const u32, words.as_ptr(), words.len() as u32)
        } else {
            mf_native_register(owner, body.add(16), base + rd32(body.add(8)), base + rd32(body.add(12)),
                bindings.as_ptr() as *const u32, words.as_ptr(), words.len() as u32)
        }
    }
    #[cfg(test)] { let _ = (base, bindings); OK }
}

pub unsafe fn remove(kind: u32) {
    #[cfg(not(test))] {
        if core::hint::black_box(mf_native_remove as usize) != 0 { mf_native_remove(kind); }
    }
    #[cfg(test)] { let _ = kind; }
}

pub unsafe fn turn(view: *const u8, event: *const u8) -> u32 {
    #[cfg(not(test))] {
        if core::hint::black_box(mf_native_turn as usize) != 0 { mf_native_turn(view, event) } else { 0 }
    }
    #[cfg(test)] { let _ = (view, event); 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(body: &mut [u8], offset: usize, value: u32) {
        body[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
    }

    #[test]
    fn replacement_pairs_are_complete_unique_unmixed_and_bounded() {
        let mut pages = PageSet::<2>::new();
        assert!(pages.claim(KIND_V2, 10, 0));
        assert!(!pages.complete());
        assert!(!pages.claim(KIND, 10, 1));
        assert!(!pages.claim(KIND_V2, 10, 0));
        assert!(!pages.claim(KIND_V2, 10, 2));
        assert!(pages.claim(KIND_V2, 10, 1));
        assert!(pages.complete());
        assert!(pages.claim(KIND, 20, 1));
        assert!(pages.complete());
        assert!(!pages.claim(KIND, 30, 1));
    }

    #[test]
    fn replacement_callback_checks_version_index_and_binding_bounds() {
        let mut body = [0u8; LEN_V2 as usize - 8];
        put(&mut body, 0, 2); put(&mut body, 8, 2); put(&mut body, 12, 4);
        body[16..20].copy_from_slice(b"PAGE");
        put(&mut body, 48, 1); put(&mut body, 56, 7);
        assert_eq!(unsafe { check(KIND_V2, body.as_ptr(), 1, 8) }, OK);
        put(&mut body, 44, 1);
        assert_eq!(unsafe { check(KIND_V2, body.as_ptr(), 1, 8) }, OK);
        let mut page1 = body; put(&mut page1, 40, 1);
        assert_eq!(unsafe { check(KIND_V2, page1.as_ptr(), 1, 8) }, E_FORMAT);
        put(&mut page1, 44, 2);
        assert_eq!(unsafe { check(KIND_V2, page1.as_ptr(), 1, 8) }, OK);
        put(&mut page1, 44, 3);
        assert_eq!(unsafe { check(KIND_V2, page1.as_ptr(), 1, 8) }, E_FORMAT);
        for (offset, value) in [(0, 1), (40, 2), (44, 2), (52, 1), (56, 8)] {
            let mut bad = body; put(&mut bad, offset, value);
            assert_eq!(unsafe { check(KIND_V2, bad.as_ptr(), 1, 8) }, E_FORMAT);
        }
    }

    #[test]
    fn validates_callback_bounds_and_binding_records() {
        let mut body = [0u8; LEN as usize - 8];
        put(&mut body, 0, 1);
        put(&mut body, 8, 2);
        put(&mut body, 12, 4);
        body[16..20].copy_from_slice(b"PAGE");
        put(&mut body, 40, 1);
        assert_eq!(unsafe { check(KIND, body.as_ptr(), 1, 8) }, OK);
        for (offset, value) in [(0, 2), (8, 3), (12, 8), (40, 3), (44, 1), (48, 8)] {
            let mut invalid = body;
            put(&mut invalid, offset, value);
            assert_eq!(unsafe { check(KIND, invalid.as_ptr(), 1, 8) }, E_FORMAT);
        }
        let mut duplicate = body;
        put(&mut duplicate, 52, 2);
        assert_eq!(unsafe { check(KIND, duplicate.as_ptr(), 1, 8) }, E_FORMAT);
        let mut gap = body;
        put(&mut gap, 64, 2);
        put(&mut gap, 72, 2);
        assert_eq!(unsafe { check(KIND, gap.as_ptr(), 1, 8) }, E_FORMAT);
        let mut title = body;
        title[16..40].fill(b'A');
        assert_eq!(unsafe { check(KIND, title.as_ptr(), 1, 8) }, E_FORMAT);
    }
}
