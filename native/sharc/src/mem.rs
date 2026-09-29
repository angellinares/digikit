//! The SHARC+ data memory as tools/sharc_core sees it: the loader image
//! (`sharcldr.LoadedMemory`, immutable, a byte present or not) under a
//! write overlay (`State.overlay`, the bytes a run wrote). Byte addresses
//! are the loader's canonical 32-bit addresses; the alias rule that maps
//! an architectural DM pointer onto them lives in
//! `bnd::_canonical_dm_address`, like `memory._canonical_dm_address`.
//!
//! Storage is a one-level table of 64 KiB pages. Each page holds the
//! current bytes, a present bit per byte (loader-backed or written) and a
//! dirty bit per byte (in the overlay). The loader image is kept apart so
//! a state import can return memory to it.

const PAGE_BITS: u32 = 16;
pub const PAGE_SIZE: usize = 1 << PAGE_BITS;
const NPAGES: usize = 1 << (32 - PAGE_BITS);
const WORDS: usize = PAGE_SIZE / 64;

pub struct Page {
    pub data: [u8; PAGE_SIZE],
    pub present: [u64; WORDS],
    pub dirty: [u64; WORDS],
}

impl Page {
    fn empty() -> Box<Page> {
        // Allocate zeroed without a large stack temporary.
        let layout = std::alloc::Layout::new::<Page>();
        // SAFETY: Page is plain old data; all-zero bytes are a valid Page.
        unsafe {
            let p = std::alloc::alloc_zeroed(layout) as *mut Page;
            if p.is_null() {
                std::alloc::handle_alloc_error(layout);
            }
            Box::from_raw(p)
        }
    }
}

pub struct Mem {
    pages: Vec<Option<Box<Page>>>,
    loader: Vec<Option<Box<Page>>>,
    /// Pages holding overlay bytes (for export and reset).
    touched: Vec<u32>,
    /// Per page: where a plain-RAM access there finds its bytes (the fast
    /// paths): the page itself when it exists; for a page below the
    /// short-word alias base that does not exist, the aliased page (every
    /// byte there is absent, so _canonical_dm_address aliases it); null
    /// for an MMR page (`mmr`) or none. Kept in step with `pages`.
    eff: Box<[*mut Page; NPAGES]>,
    mmr: Vec<bool>,
}

/// The short-word alias base in pages (memory.SW_ALIAS_BASE >> 16).
const ALIAS_PAGES: usize = 0x2800_0000 >> PAGE_BITS;

impl Default for Mem {
    fn default() -> Self {
        Self::new()
    }
}

impl Mem {
    pub fn new() -> Mem {
        let mut pages = Vec::with_capacity(NPAGES);
        pages.resize_with(NPAGES, || None);
        let mut loader = Vec::with_capacity(NPAGES);
        loader.resize_with(NPAGES, || None);
        Mem {
            pages,
            loader,
            touched: Vec::new(),
            eff: vec![std::ptr::null_mut(); NPAGES]
                .into_boxed_slice()
                .try_into()
                .expect("NPAGES entries"),
            mmr: vec![false; NPAGES],
        }
    }

    fn page_ptr(&mut self, idx: usize) -> *mut Page {
        match self.pages[idx].as_deref_mut() {
            Some(p) => p as *mut Page,
            None => std::ptr::null_mut(),
        }
    }

    /// Recompute `eff` for page IDX and the page aliased onto it.
    fn refresh(&mut self, idx: usize) {
        self.refresh_one(idx);
        if idx >= ALIAS_PAGES {
            self.refresh_one(idx - ALIAS_PAGES);
        }
    }

    fn refresh_one(&mut self, idx: usize) {
        let own = self.page_ptr(idx);
        self.eff[idx] = if self.mmr[idx] {
            std::ptr::null_mut()
        } else if !own.is_null() || idx >= ALIAS_PAGES {
            own
        } else {
            self.page_ptr(idx + ALIAS_PAGES)
        };
    }

    /// The address a fast access at A used (A, or its alias).
    #[inline(always)]
    pub fn canonical_of(&self, a: u32) -> u32 {
        let idx = (a >> PAGE_BITS) as usize;
        if self.pages[idx].is_none() && idx < ALIAS_PAGES {
            a + (ALIAS_PAGES << PAGE_BITS) as u32
        } else {
            a
        }
    }

    /// The pages that may hold an MMR (no fast path there).
    pub fn set_mmr_pages(&mut self, mmr: Vec<bool>) {
        self.mmr = mmr;
        for idx in 0..NPAGES {
            self.refresh_one(idx);
        }
    }

    /// Plain-RAM read of WIDTH (<= 4) bytes at A through `eff`: Some when
    /// they are all present on one page.
    #[inline(always)]
    pub fn fast_read(&self, a: u32, width: u32) -> Option<u32> {
        let p = self.eff[(a >> PAGE_BITS) as usize];
        if p.is_null() {
            return None;
        }
        // SAFETY: eff holds null or a page `pages` owns (kept in step).
        let p = unsafe { &*p };
        let off = (a as usize) & (PAGE_SIZE - 1);
        let bit = off & 63;
        if off + 4 > PAGE_SIZE || bit + width as usize > 64 {
            return None;
        }
        let m = ((1u64 << width) - 1) << bit;
        if p.present[off >> 6] & m != m {
            return None;
        }
        let w = u32::from_le_bytes(p.data[off..off + 4].try_into().unwrap());
        Some(if width >= 4 {
            w
        } else {
            w & ((1u32 << (8 * width)) - 1)
        })
    }

    /// Plain-RAM write of WIDTH (<= 4) bytes at A through `eff` over bytes
    /// that are all overlay bytes on one page; returns the old value, or
    /// None (nothing written).
    #[inline(always)]
    pub fn fast_write(&mut self, a: u32, width: u32, v: u32) -> Option<u32> {
        let p = self.eff[(a >> PAGE_BITS) as usize];
        if p.is_null() {
            return None;
        }
        // SAFETY: eff holds null or a page `pages` owns (kept in step).
        let p = unsafe { &mut *p };
        let off = (a as usize) & (PAGE_SIZE - 1);
        let bit = off & 63;
        if off + 4 > PAGE_SIZE || bit + width as usize > 64 {
            return None;
        }
        let m = ((1u64 << width) - 1) << bit;
        if p.dirty[off >> 6] & m != m {
            return None;
        }
        let old = u32::from_le_bytes(p.data[off..off + 4].try_into().unwrap());
        if width >= 4 {
            p.data[off..off + 4].copy_from_slice(&v.to_le_bytes());
        } else {
            for k in 0..width as usize {
                p.data[off + k] = (v >> (8 * k)) as u8;
            }
        }
        Some(if width >= 4 {
            old
        } else {
            old & ((1u32 << (8 * width)) - 1)
        })
    }

    /// Add loader-image bytes at byte address A (building the image).
    pub fn load(&mut self, a: u32, bytes: &[u8]) {
        for (k, &b) in bytes.iter().enumerate() {
            let addr = a.wrapping_add(k as u32);
            let idx = (addr >> PAGE_BITS) as usize;
            let off = (addr as usize) & (PAGE_SIZE - 1);
            let page = self.loader[idx].get_or_insert_with(Page::empty);
            page.data[off] = b;
            page.present[off >> 6] |= 1 << (off & 63);
        }
    }

    /// Discard every overlay byte: memory is the loader image again.
    pub fn reset(&mut self) {
        for idx in 0..NPAGES {
            match &self.loader[idx] {
                Some(src) => {
                    let dst = self.pages[idx].get_or_insert_with(Page::empty);
                    dst.data.copy_from_slice(&src.data);
                    dst.present.copy_from_slice(&src.present);
                    dst.dirty.fill(0);
                }
                None => self.pages[idx] = None,
            }
        }
        self.touched.clear();
        for idx in 0..NPAGES {
            self.refresh_one(idx);
        }
    }

    #[inline(always)]
    fn page(&self, a: u32) -> Option<&Page> {
        self.pages[(a >> PAGE_BITS) as usize].as_deref()
    }

    #[inline(always)]
    pub fn present(&self, a: u32) -> bool {
        match self.page(a) {
            Some(p) => {
                let off = (a as usize) & (PAGE_SIZE - 1);
                p.present[off >> 6] & (1 << (off & 63)) != 0
            }
            None => false,
        }
    }

    #[inline(always)]
    pub fn dirty(&self, a: u32) -> bool {
        match self.page(a) {
            Some(p) => {
                let off = (a as usize) & (PAGE_SIZE - 1);
                p.dirty[off >> 6] & (1 << (off & 63)) != 0
            }
            None => false,
        }
    }

    /// Bytes [A, A+WIDTH) all present (loader or overlay).
    #[inline(always)]
    pub fn all_present(&self, a: i128, width: i128) -> bool {
        if a < 0 || a + width > (1i128 << 32) {
            return false;
        }
        let a = a as u32;
        let off = (a as usize) & (PAGE_SIZE - 1);
        if off + width as usize <= PAGE_SIZE {
            if let Some(p) = self.page(a) {
                let w = off >> 6;
                let bit = off & 63;
                if bit + width as usize <= 64 {
                    let m = (((1u128 << width) - 1) as u64) << bit;
                    return p.present[w] & m == m;
                }
            } else {
                return false;
            }
        }
        (0..width as u32).all(|k| self.present(a + k))
    }

    #[inline(always)]
    pub fn all_dirty(&self, a: i128, width: i128) -> bool {
        if a < 0 || a + width > (1i128 << 32) {
            return false;
        }
        (0..width as u32).all(|k| self.dirty(a as u32 + k))
    }

    /// Little-endian read of WIDTH (<= 4) present bytes.
    #[inline(always)]
    pub fn read_le(&self, a: u32, width: u32) -> u32 {
        let off = (a as usize) & (PAGE_SIZE - 1);
        if let Some(p) = self.page(a)
            && off + 4 <= PAGE_SIZE
        {
            let w = u32::from_le_bytes([
                p.data[off],
                p.data[off + 1],
                p.data[off + 2],
                p.data[off + 3],
            ]);
            return if width >= 4 {
                w
            } else {
                w & ((1u32 << (8 * width)) - 1)
            };
        }
        let mut v = 0u32;
        for k in 0..width {
            v |= (self.byte(a + k) as u32) << (8 * k);
        }
        v
    }

    #[inline(always)]
    pub fn byte(&self, a: u32) -> u8 {
        match self.page(a) {
            Some(p) => p.data[(a as usize) & (PAGE_SIZE - 1)],
            None => 0,
        }
    }

    /// A loader-image byte (ignores the overlay).
    pub fn loader_byte(&self, a: u32) -> Option<u8> {
        let p = self.loader[(a >> PAGE_BITS) as usize].as_deref()?;
        let off = (a as usize) & (PAGE_SIZE - 1);
        if p.present[off >> 6] & (1 << (off & 63)) != 0 {
            Some(p.data[off])
        } else {
            None
        }
    }

    /// Write one overlay byte. Returns the old byte and its old
    /// present/dirty bits (bit 0, bit 1) for the undo journal.
    #[inline(always)]
    pub fn write_byte(&mut self, a: u32, v: u8) -> (u8, u8) {
        let idx = (a >> PAGE_BITS) as usize;
        if self.pages[idx].is_none() {
            self.pages[idx] = Some(Page::empty());
            self.refresh(idx);
        }
        let p = self.pages[idx].as_deref_mut().unwrap();
        let off = (a as usize) & (PAGE_SIZE - 1);
        let (w, bit) = (off >> 6, 1u64 << (off & 63));
        let old = p.data[off];
        let flags = ((p.present[w] & bit != 0) as u8) | (((p.dirty[w] & bit != 0) as u8) << 1);
        if flags & 2 == 0 {
            self.touched.push(a >> PAGE_BITS);
        }
        p.data[off] = v;
        p.present[w] |= bit;
        p.dirty[w] |= bit;
        (old, flags)
    }

    /// The WIDTH (<= 4) bytes at A when all are present on one page.
    #[inline(always)]
    pub fn read_present(&self, a: u32, width: u32) -> Option<u32> {
        let off = (a as usize) & (PAGE_SIZE - 1);
        let p = self.page(a)?;
        if off + 4 > PAGE_SIZE {
            return None;
        }
        let bit = off & 63;
        if bit + width as usize > 64 {
            return None;
        }
        let m = ((1u64 << width) - 1) << bit;
        if p.present[off >> 6] & m != m {
            return None;
        }
        let w = u32::from_le_bytes([
            p.data[off],
            p.data[off + 1],
            p.data[off + 2],
            p.data[off + 3],
        ]);
        Some(if width >= 4 {
            w
        } else {
            w & ((1u32 << (8 * width)) - 1)
        })
    }

    /// The WIDTH (<= 4) bytes at A when all are overlay bytes on one page.
    #[inline(always)]
    pub fn read_dirty(&self, a: u32, width: u32) -> Option<u32> {
        let off = (a as usize) & (PAGE_SIZE - 1);
        let p = self.page(a)?;
        if off + 4 > PAGE_SIZE {
            return None;
        }
        let bit = off & 63;
        if bit + width as usize > 64 {
            return None;
        }
        let m = ((1u64 << width) - 1) << bit;
        if p.dirty[off >> 6] & m != m {
            return None;
        }
        let w = u32::from_le_bytes([
            p.data[off],
            p.data[off + 1],
            p.data[off + 2],
            p.data[off + 3],
        ]);
        Some(if width >= 4 {
            w
        } else {
            w & ((1u32 << (8 * width)) - 1)
        })
    }

    /// Write WIDTH (<= 4) bytes of V at A when they are all overlay bytes
    /// on one page already (nothing else changes); false otherwise.
    #[inline(always)]
    pub fn write_dirty(&mut self, a: u32, width: u32, v: u32) -> bool {
        let off = (a as usize) & (PAGE_SIZE - 1);
        let Some(p) = self.pages[(a >> PAGE_BITS) as usize].as_deref_mut() else {
            return false;
        };
        let bit = off & 63;
        if off + width as usize > PAGE_SIZE || bit + width as usize > 64 {
            return false;
        }
        let m = ((1u64 << width) - 1) << bit;
        if p.dirty[off >> 6] & m != m {
            return false;
        }
        for k in 0..width as usize {
            p.data[off + k] = (v >> (8 * k)) as u8;
        }
        true
    }

    /// Undo a write_byte.
    pub fn restore(&mut self, a: u32, byte: u8, flags: u8) {
        if let Some(p) = self.pages[(a >> PAGE_BITS) as usize].as_deref_mut() {
            let off = (a as usize) & (PAGE_SIZE - 1);
            let (w, bit) = (off >> 6, 1u64 << (off & 63));
            p.data[off] = byte;
            if flags & 1 != 0 {
                p.present[w] |= bit;
            } else {
                p.present[w] &= !bit;
            }
            if flags & 2 != 0 {
                p.dirty[w] |= bit;
            } else {
                p.dirty[w] &= !bit;
            }
        }
    }

    #[inline(always)]
    pub fn commit(&mut self) {}

    /// Every overlay byte, sorted by address.
    pub fn dirty_bytes(&self) -> Vec<(u32, u8)> {
        let mut pages: Vec<u32> = self.touched.clone();
        pages.sort_unstable();
        pages.dedup();
        let mut out = Vec::new();
        for pg in pages {
            let Some(p) = self.pages[pg as usize].as_deref() else {
                continue;
            };
            for w in 0..WORDS {
                let mut bits = p.dirty[w];
                while bits != 0 {
                    let b = bits.trailing_zeros() as usize;
                    bits &= bits - 1;
                    let off = w * 64 + b;
                    out.push(((pg << PAGE_BITS) | off as u32, p.data[off]));
                }
            }
        }
        out
    }
}
