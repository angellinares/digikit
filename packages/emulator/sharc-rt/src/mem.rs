//! The SHARC+ data memory as tools/sharc_dev/sharc_core sees it: the loader image
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

pub const PAGE_BITS: u32 = 16;
pub const PAGE_SIZE: usize = 1 << PAGE_BITS;
const NPAGES: usize = 1 << (32 - PAGE_BITS);
const WORDS: usize = PAGE_SIZE / 64;

#[repr(C)]
pub struct Page {
    pub data: [u8; PAGE_SIZE],
    pub present: [u64; WORDS],
    pub dirty: [u64; WORDS],
}

/// Byte offsets of a page's present and dirty bitmaps (`Mem::flat_tables`).
pub const PAGE_PRESENT_OFF: u32 = std::mem::offset_of!(Page, present) as u32;
pub const PAGE_DIRTY_OFF: u32 = std::mem::offset_of!(Page, dirty) as u32;

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

/// Origin of the first overlapping attempted access. Storage events include
/// host/DMA traffic and journal bookkeeping; they are not guest attribution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DdrAccessKind {
    CpuRead,
    CpuWrite,
    StorageRead,
    StorageWrite,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DdrAccess {
    pub address: u32,
    pub width: u32,
    pub kind: DdrAccessKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidDdrRange;

pub struct Mem {
    ddr_range: Option<(u32, u32)>,
    ddr_first: std::cell::Cell<Option<DdrAccess>>,
    /// Changes on monitor configuration, including replacement/removal.
    pub ddr_monitor_gen: u64,
    pages: Box<[Option<Box<Page>>; NPAGES]>,
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
    /// Per page: bit 0, the page (or its short-word alias page) holds
    /// generated block code; bit 1, it holds a word a cached runtime decode
    /// read (`watch_sw`). A write to a bit-0 page bumps `code_gen` (the
    /// engine re-verifies the code the blocks were generated for), to a
    /// bit-1 page `dec_gen` (cached decodes are re-checked word by word).
    watch: Box<[u8; NPAGES]>,
    /// The exact byte spans [lo, hi) of the watched code (sorted, merged, with
    /// the short-word alias form of each). A bit-0 page is a 64 KiB unit: a
    /// store into it that touches none of these spans (module data placed
    /// after the last code of the image) does not bump `code_gen`, so the
    /// engine does not hash the code again for it. Window writes (no exact
    /// extent at the note) still count for the whole page.
    code_spans: Vec<(u32, u32)>,
    pub code_gen: u64,
    pub dec_gen: u64,
    /// Changes whenever a host pointer into a page or the present/dirty
    /// state of a byte could have been invalidated: a page is allocated or
    /// dropped, `eff` is recomputed, bits are cleared (`reset`, `restore`).
    /// Writes only ever set present/dirty bits and do not change it. The
    /// start value differs between `Mem`s, so a value cached against one
    /// never matches another. The fast tier keeps resolved windows by it.
    pub layout_gen: u64,
}

/// Start values of `Mem::layout_gen`, 2^40 apart.
static NEXT_LAYOUT_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1 << 40);

const WATCH_CODE: u8 = 1;
const WATCH_DECODE: u8 = 2;

/// The short-word alias base in pages (memory.SW_ALIAS_BASE >> 16).
const ALIAS_PAGES: usize = 0x2800_0000 >> PAGE_BITS;

impl Default for Mem {
    fn default() -> Self {
        Self::new()
    }
}

impl Mem {
    pub fn new() -> Mem {
        // Fixed-size tables: an index by `a >> PAGE_BITS` of a u32 needs no
        // bounds check in the inline fast paths.
        let mut pages = Vec::with_capacity(NPAGES);
        pages.resize_with(NPAGES, || None);
        let pages: Box<[Option<Box<Page>>; NPAGES]> =
            match pages.into_boxed_slice().try_into() {
                Ok(b) => b,
                Err(_) => unreachable!("NPAGES entries"),
            };
        let mut loader = Vec::with_capacity(NPAGES);
        loader.resize_with(NPAGES, || None);
        Mem {
            ddr_range: None,
            ddr_first: std::cell::Cell::new(None),
            ddr_monitor_gen: 0,
            pages,
            loader,
            touched: Vec::new(),
            eff: vec![std::ptr::null_mut(); NPAGES]
                .into_boxed_slice()
                .try_into()
                .expect("NPAGES entries"),
            mmr: vec![false; NPAGES],
            watch: vec![0u8; NPAGES]
                .into_boxed_slice()
                .try_into()
                .expect("NPAGES entries"),
            code_spans: Vec::new(),
            code_gen: 0,
            dec_gen: 0,
            layout_gen: NEXT_LAYOUT_GEN.fetch_add(1 << 40, std::sync::atomic::Ordering::Relaxed),
        }
    }

    /// Configure a byte-addressed half-open DDR interval. Invalid requests leave
    /// the previous configuration and latch intact. Every successful set/remove
    /// clears the latch and invalidates resolved fast windows. This diagnostic
    /// observes attempts, does not block access, and is not an ownership proof.
    pub fn set_ddr_monitor(&mut self, range: Option<(u32, u32)>) -> Result<(), InvalidDdrRange> {
        if let Some((lo, hi)) = range
            && !(0x8000_0000 <= lo && lo < hi && hi <= 0xa000_0000)
        {
            return Err(InvalidDdrRange);
        }
        let old = self.ddr_range;
        self.ddr_range = range;
        self.clear_ddr_access();
        self.ddr_monitor_gen += 1;
        self.layout_gen += 1;
        for (lo, hi) in old.into_iter().chain(range) {
            for idx in (lo >> PAGE_BITS)..=((hi - 1) >> PAGE_BITS) {
                self.refresh_one(idx as usize);
            }
        }
        Ok(())
    }

    pub fn ddr_monitor_range(&self) -> Option<(u32, u32)> {
        self.ddr_range
    }

    pub fn ddr_monitor_active(&self) -> bool {
        self.ddr_range.is_some()
    }

    pub fn first_ddr_access(&self) -> Option<DdrAccess> {
        self.ddr_first.get()
    }

    pub fn clear_ddr_access(&self) {
        self.ddr_first.set(None);
    }

    /// Record only known byte addresses; symbolic guest addresses are outside
    /// this diagnostic's coverage. Duplicate fallback/bookkeeping calls cannot
    /// replace the first event. Width is the attempted extent, not a count.
    #[inline(always)]
    pub fn note_ddr_access(&self, address: u32, width: u32, kind: DdrAccessKind) {
        if let Some((lo, hi)) = self.ddr_range
            && self.ddr_first.get().is_none()
            && width != 0
            && address < hi
            && (address as u64 + width as u64) > lo as u64
        {
            self.ddr_first.set(Some(DdrAccess {
                address,
                width,
                kind,
            }));
        }
    }

    /// Watch the pages holding the bytes [lo, hi) (and their aliases): any
    /// write there bumps `code_gen`.
    pub fn watch_code(&mut self, lo: u32, hi: u32) {
        if hi <= lo {
            return;
        }
        for idx in (lo >> PAGE_BITS)..=((hi - 1) >> PAGE_BITS) {
            self.watch_page(idx as usize, WATCH_CODE);
        }
        const ALIAS: u32 = (ALIAS_PAGES as u32) << PAGE_BITS;
        self.code_spans.push((lo, hi));
        if lo >= ALIAS {
            self.code_spans.push((lo - ALIAS, hi - ALIAS));
        }
        self.code_spans.sort_unstable();
        let mut merged: Vec<(u32, u32)> = Vec::with_capacity(self.code_spans.len());
        for &(l, h) in &self.code_spans {
            match merged.last_mut() {
                Some(last) if l <= last.1 => last.1 = last.1.max(h),
                _ => merged.push((l, h)),
            }
        }
        self.code_spans = merged;
    }

    pub fn unwatch_code(&mut self) {
        self.code_spans.clear();
        for w in self.watch.iter_mut() {
            *w &= !WATCH_CODE;
        }
    }

    /// Addresses of the per-page pointer table (`eff`: the page a plain-RAM
    /// access at a page finds its bytes in, null for none or an MMR page) and
    /// of the per-page code-watch bytes: what a kernel's flat memory access
    /// reads (`fast_read` / `fast_write` inline). Stable for this `Mem`.
    pub fn flat_tables(&self) -> (usize, usize) {
        (self.eff.as_ptr() as usize, self.watch.as_ptr() as usize)
    }

    /// Mark page IDX (and its short-word alias page, through which a fast
    /// write can reach it) with BIT.
    fn watch_page(&mut self, idx: usize, bit: u8) {
        self.watch[idx] |= bit;
        if idx >= ALIAS_PAGES {
            self.watch[idx - ALIAS_PAGES] |= bit;
        }
    }

    /// Watch the bytes `read_sw(pc)` reads: any write there bumps `dec_gen`.
    pub fn watch_sw(&mut self, pc: u32) {
        if pc >= (1 << 24) {
            return;
        }
        let a = 0x2800_0000 + pc * 2;
        self.watch_page((a >> PAGE_BITS) as usize, WATCH_DECODE);
        self.watch_page(((a + 1) >> PAGE_BITS) as usize, WATCH_DECODE);
        if (0xb80000..0xc00000).contains(&pc) {
            let b = 0x2000_0000 + (pc - 0xb80000) * 2;
            self.watch_page((b >> PAGE_BITS) as usize, WATCH_DECODE);
            self.watch_page(((b + 1) >> PAGE_BITS) as usize, WATCH_DECODE);
            // code the DSP wrote at run time into L2 that no boot block loaded (the digikit runtime's DSP area) lives on the
            // alias page of that byte address (`canonical_of`: the address plus the short-word alias base)
            let c = 0x4800_0000 + (pc - 0xb80000) * 2;
            self.watch_page((c >> PAGE_BITS) as usize, WATCH_DECODE);
            self.watch_page(((c + 1) >> PAGE_BITS) as usize, WATCH_DECODE);
        }
    }

    /// A store of LEN bytes at A: `note_write` when it overlaps watched code
    /// or its page has no code watch.
    #[inline(always)]
    fn note_write_n(&mut self, a: u32, len: u32) {
        let w = self.watch[(a >> PAGE_BITS) as usize];
        if w != 0 {
            self.note_watched_n(w, a, len);
        }
    }

    /// The store is on a watched page: only the spans themselves count for
    /// the code watch (`code_spans`), the decode watch stays per page.
    #[inline(never)]
    fn note_watched_n(&mut self, w: u8, a: u32, len: u32) {
        if w & WATCH_CODE != 0 {
            let end = a as u64 + len as u64;
            let i = self.code_spans.partition_point(|s| s.1 <= a);
            if i < self.code_spans.len() && (self.code_spans[i].0 as u64) < end {
                self.code_gen += 1;
            }
        }
        self.dec_gen += ((w & WATCH_DECODE) >> 1) as u64;
    }

    #[inline(always)]
    fn note_write(&mut self, a: u32) {
        let w = self.watch[(a >> PAGE_BITS) as usize];
        if w != 0 {
            self.code_gen += (w & WATCH_CODE) as u64;
            self.dec_gen += ((w & WATCH_DECODE) >> 1) as u64;
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
        self.eff[idx] = if self.mmr[idx]
            || self.ddr_range.is_some_and(|(lo, hi)| {
                idx >= (lo >> PAGE_BITS) as usize && idx <= ((hi - 1) >> PAGE_BITS) as usize
            }) {
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
        self.layout_gen += 1;
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
        if width < 3 {
            // One or two bytes: no 4-byte load, so nothing to cross at the
            // page end; the present word test also bounds the access.
            if bit + width as usize > 64 {
                return None;
            }
            let m = ((1u64 << width) - 1) << bit;
            if p.present[off >> 6] & m != m {
                return None;
            }
            // SAFETY: bit + width <= 64 puts off + width within the page.
            return Some(if width == 1 {
                unsafe { *p.data.get_unchecked(off) as u32 }
            } else {
                unsafe { u16::from_le_bytes(*(p.data.as_ptr().add(off) as *const [u8; 2])) as u32 }
            });
        }
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
        self.note_write_n(a, width);
        // SAFETY: eff holds null or a page `pages` owns (kept in step).
        let p = unsafe { &mut *p };
        let off = (a as usize) & (PAGE_SIZE - 1);
        let bit = off & 63;
        if width < 3 {
            // One or two bytes (see `fast_read`).
            if bit + width as usize > 64 {
                return None;
            }
            let m = ((1u64 << width) - 1) << bit;
            if p.dirty[off >> 6] & m != m {
                return None;
            }
            // SAFETY: bit + width <= 64 puts off + width within the page.
            unsafe {
                if width == 1 {
                    let q = p.data.get_unchecked_mut(off);
                    let old = *q as u32;
                    *q = v as u8;
                    return Some(old);
                }
                let q = p.data.as_mut_ptr().add(off) as *mut [u8; 2];
                let old = u16::from_le_bytes(*q) as u32;
                *q = (v as u16).to_le_bytes();
                return Some(old);
            }
        }
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

    /// `fast_read` of an aligned 4-byte word, for block code: the same
    /// answer where it answers (the address is a multiple of 4, so the word
    /// can neither cross the page end nor a 64-bit present word); an
    /// unaligned address declines here and takes the full rules.
    #[inline(always)]
    pub fn fast_read_word(&self, a: u32) -> Option<u32> {
        let p = self.eff[(a >> PAGE_BITS) as usize];
        if p.is_null() || a & 3 != 0 {
            return None;
        }
        // SAFETY: eff holds null or a page `pages` owns (kept in step).
        let p = unsafe { &*p };
        let off = (a as usize) & (PAGE_SIZE - 1);
        if (p.present[off >> 6] >> (off & 63)) & 0xF != 0xF {
            return None;
        }
        // SAFETY: off is a multiple of 4 below PAGE_SIZE.
        Some(u32::from_le_bytes(unsafe {
            *(p.data.as_ptr().add(off) as *const [u8; 4])
        }))
    }

    /// `fast_write` of an aligned 4-byte word, for block code (see
    /// `fast_read_word`); returns the old word. The code watch is noted
    /// before the dirty test, as `fast_write` does.
    #[inline(always)]
    pub fn fast_write_word(&mut self, a: u32, v: u32) -> Option<u32> {
        let p = self.eff[(a >> PAGE_BITS) as usize];
        if p.is_null() || a & 3 != 0 {
            return None;
        }
        self.note_write_n(a, 4);
        // SAFETY: eff holds null or a page `pages` owns (kept in step).
        let p = unsafe { &mut *p };
        let off = (a as usize) & (PAGE_SIZE - 1);
        if (p.dirty[off >> 6] >> (off & 63)) & 0xF != 0xF {
            return None;
        }
        // SAFETY: off is a multiple of 4 below PAGE_SIZE.
        unsafe {
            let q = p.data.as_mut_ptr().add(off) as *mut [u8; 4];
            let old = u32::from_le_bytes(*q);
            *q = v.to_le_bytes();
            Some(old)
        }
    }

    /// A host pointer to the bytes [A, A+LEN) for a fast-tier window: all on
    /// one plain-RAM page (the fast-path page `eff`, so an unwritten low page
    /// is its alias), every byte present when READ and every byte an overlay
    /// byte (dirty) when WRITE, exactly what `fast_read` / `fast_write`
    /// need for each word inside. A write window counts as a write to its
    /// pages for the code watch (`note_write`).
    pub fn window(&mut self, a: u32, len: u32, read: bool, write: bool) -> Option<*mut u8> {
        let p = self.eff[(a >> PAGE_BITS) as usize];
        if p.is_null() || len == 0 {
            return None;
        }
        let off = (a as usize) & (PAGE_SIZE - 1);
        if off + len as usize > PAGE_SIZE {
            return None;
        }
        // SAFETY: eff holds null or a page `pages` owns (kept in step).
        let page = unsafe { &mut *p };
        fn all(bits: &[u64; WORDS], off: usize, len: usize) -> bool {
            let (first, last) = (off >> 6, (off + len - 1) >> 6);
            let head = !0u64 << (off & 63);
            let tail = !0u64 >> (63 - ((off + len - 1) & 63));
            if first == last {
                let m = head & tail;
                return bits[first] & m == m;
            }
            if bits[first] & head != head || bits[last] & tail != tail {
                return false;
            }
            bits[first + 1..last].iter().all(|&w| w == !0)
        }
        if read && !all(&page.present, off, len as usize) {
            return None;
        }
        if write && !all(&page.dirty, off, len as usize) {
            return None;
        }
        if write {
            self.note_write(a);
        }
        // SAFETY: off + len is inside the page.
        Some(unsafe { (&mut *p).data.as_mut_ptr().add(off) })
    }

    /// What `window` does for a write window whose pointer is already
    /// resolved (valid while `layout_gen` is unchanged): the code-watch bump.
    #[inline(always)]
    pub fn note_window_write(&mut self, a: u32) {
        self.note_write(a);
    }

    /// Add loader-image bytes at byte address A (building the image).
    pub fn load(&mut self, a: u32, bytes: &[u8]) {
        self.code_gen += 1;
        self.dec_gen += 1;
        for (k, &b) in bytes.iter().enumerate() {
            let addr = a.wrapping_add(k as u32);
            self.note_ddr_access(addr, 1, DdrAccessKind::StorageWrite);
            let idx = (addr >> PAGE_BITS) as usize;
            let off = (addr as usize) & (PAGE_SIZE - 1);
            let page = self.loader[idx].get_or_insert_with(Page::empty);
            page.data[off] = b;
            page.present[off >> 6] |= 1 << (off & 63);
        }
    }

    /// Discard every overlay byte: memory is the loader image again.
    pub fn reset(&mut self) {
        self.clear_ddr_access();
        self.layout_gen += 1;
        self.code_gen += 1;
        self.dec_gen += 1;
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

    /// The LEN bytes at A as one slice, when they lie in one existing page
    /// (diagnostics that compare a record with a snapshot).
    pub fn bytes_in_page(&self, a: u32, len: usize) -> Option<&[u8]> {
        let off = (a as usize) & (PAGE_SIZE - 1);
        if off + len > PAGE_SIZE {
            return None;
        }
        self.page(a).map(|p| &p.data[off..off + len])
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
        self.note_ddr_access(a, width, DdrAccessKind::StorageRead);
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
        self.note_ddr_access(a, 1, DdrAccessKind::StorageRead);
        match self.page(a) {
            Some(p) => p.data[(a as usize) & (PAGE_SIZE - 1)],
            None => 0,
        }
    }

    /// A VISA short word in the loaded execution aliases. Unknown bytes stay
    /// unknown; reads never synthesize code from unwritten RAM.
    pub fn read_sw(&self, pc: u32) -> Option<u16> {
        if pc >= (1 << 24) {
            return None;
        }
        let word = |address: u32| {
            if self.present(address) && self.present(address + 1) {
                Some(u16::from_le_bytes([
                    self.byte(address),
                    self.byte(address + 1),
                ]))
            } else {
                None
            }
        };
        if let Some(v) = word(0x2800_0000 + pc * 2) {
            return Some(v);
        }
        if (0xb80000..0xc00000).contains(&pc) {
            if let Some(v) = word(0x2000_0000 + (pc - 0xb80000) * 2) {
                return Some(v);
            }
            // bytes a DM store wrote where the loader put none sit on the alias page of the L2 byte address
            return word(0x4800_0000 + (pc - 0xb80000) * 2);
        }
        None
    }

    /// A loader-image byte (ignores the overlay).
    pub fn loader_byte(&self, a: u32) -> Option<u8> {
        self.note_ddr_access(a, 1, DdrAccessKind::StorageRead);
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
        self.note_ddr_access(a, 1, DdrAccessKind::StorageWrite);
        self.note_write_n(a, 1);
        let idx = (a >> PAGE_BITS) as usize;
        if self.pages[idx].is_none() {
            self.layout_gen += 1;
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
        self.note_ddr_access(a, width, DdrAccessKind::StorageRead);
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
        self.note_ddr_access(a, width, DdrAccessKind::StorageRead);
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
        self.note_ddr_access(a, width, DdrAccessKind::StorageWrite);
        self.note_write_n(a, width);
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
        self.note_ddr_access(a, 1, DdrAccessKind::StorageWrite);
        self.layout_gen += 1;
        self.note_write_n(a, 1);
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
                    let address = (pg << PAGE_BITS) | off as u32;
                    self.note_ddr_access(address, 1, DdrAccessKind::StorageRead);
                    out.push((address, p.data[off]));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod word_path_tests {
    use super::*;

    /// The aligned-word paths answer exactly as the general ones where the
    /// address is a multiple of 4, and decline everywhere else.
    #[test]
    fn word_paths_match_the_general_ones() {
        let mut a = Mem::new();
        let mut b = Mem::new();
        // A page with a mix of loader bytes, overlay bytes and gaps.
        let base = 0x2000_0000u32;
        let mut x = 0x1234_5678u64;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let mut bytes = vec![0u8; 0x400];
        for byte in bytes.iter_mut() {
            *byte = next() as u8;
        }
        for m in [&mut a, &mut b] {
            m.load(base, &bytes);
            m.load(base + 0x500, &bytes[..0x100]);
            m.load(base + 0xfff0, &bytes[..0x10]);
            // overlay bytes (dirty) over part of the loaded range
            for k in 0..0x180u32 {
                if k % 7 != 3 {
                    m.write_byte(base + 0x100 + k, (k * 5) as u8);
                }
            }
        }
        for off in (0..0x10020u32).chain(0xffe0..0x1_0004) {
            let addr = base.wrapping_add(off);
            let general = a.fast_read(addr, 4);
            let word = a.fast_read_word(addr);
            if addr & 3 == 0 {
                assert_eq!(word, general, "read {addr:#x}");
            } else {
                assert_eq!(word, None, "read {addr:#x}");
            }
            let v = (off.wrapping_mul(0x9e37_79b9)) ^ 0xa5a5_a5a5;
            let g = a.fast_write(addr, 4, v);
            let w = b.fast_write_word(addr, v);
            if addr & 3 == 0 {
                assert_eq!(w, g, "write {addr:#x}");
                assert_eq!(a.fast_read(addr, 4), b.fast_read(addr, 4), "after {addr:#x}");
                assert_eq!(a.code_gen, b.code_gen);
            } else {
                assert_eq!(w, None, "write {addr:#x}");
                // keep the two memories in step: undo the general write
                if let Some(old) = g {
                    a.fast_write(addr, 4, old);
                }
            }
        }
        // Narrow accesses: where the old rule answers the new one agrees, and
        // it answers whenever the bytes are present on the page.
        for off in 0..0x10020u32 {
            let addr = base.wrapping_add(off);
            for w in [1u32, 2] {
                let want = a.read_present(addr, w);
                let got = a.fast_read(addr, w);
                let same_word = (addr as usize & 63) + w as usize <= 64;
                if same_word {
                    assert_eq!(got, want, "narrow read {addr:#x} {w}");
                } else {
                    assert_eq!(got, None);
                }
            }
        }
        // An absent page and an MMR page decline.
        assert_eq!(a.fast_read_word(0x7000_0000), None);
        assert_eq!(a.fast_write_word(0x7000_0000, 1), None);
    }
}

#[cfg(test)]
mod code_watch_tests {
    use super::*;

    /// A store on a code-watched page bumps `code_gen` only when it touches
    /// a watched span (module data after the last code stays quiet); a store
    /// that straddles a span edge, one through the short-word alias, and a
    /// window write (no exact extent) still count.
    #[test]
    fn code_gen_bumps_only_for_watched_spans() {
        let mut m = Mem::new();
        let (lo, hi) = (0x2000_1000u32, 0x2000_1100u32);
        m.watch_code(lo, hi);
        let cg = |m: &Mem| m.code_gen;
        // Same 64 KiB page, outside the span (before and after).
        let g = cg(&m);
        m.write_byte(lo - 1, 1);
        m.write_byte(hi, 2);
        m.write_dirty(hi + 0x100, 4, 0x1122_3344);
        m.fast_write(hi + 0x200, 4, 7);
        m.fast_write_word(hi + 0x300, 7);
        assert_eq!(cg(&m), g, "stores beside the code do not bump");
        // Inside, and straddling either edge.
        for (a, w) in [(lo, 1u32), (hi - 1, 1), (lo - 2, 4), (hi - 2, 4)] {
            let g = cg(&m);
            if w == 1 {
                m.write_byte(a, 9);
            } else {
                m.write_dirty(a, w, 0x0102_0304);
                m.fast_write(a, w, 0x0102_0304);
            }
            assert!(cg(&m) > g, "store of {w} at {a:#x} bumps");
        }
        // A window write counts for the whole page.
        let g = cg(&m);
        m.write_byte(hi + 0x400, 1); // makes the page exist
        assert!(m.window(hi + 0x400, 1, false, true).is_some());
        assert!(cg(&m) > g, "a write window bumps");
        // The short-word alias form of a high span.
        let mut m = Mem::new();
        let (lo, hi) = (0x2800_0000u32 + 0x2000, 0x2800_0000u32 + 0x2100);
        m.watch_code(lo, hi);
        let g = cg(&m);
        m.write_byte(lo - 0x2800_0000 + 0x10, 1);
        assert!(cg(&m) > g, "alias store inside the span bumps");
        let g = cg(&m);
        m.write_byte(hi - 0x2800_0000 + 0x10, 1);
        m.write_byte(hi + 0x10, 1);
        assert_eq!(cg(&m), g);
        m.unwatch_code();
        m.write_byte(lo, 1);
        assert_eq!(cg(&m), g, "unwatched");
    }
}

#[cfg(test)]
mod ddr_monitor_tests {
    use super::*;
    const A: u32 = 0x8001_0000;

    #[test]
    fn ddr_monitor_ranges_overlap_and_first_attempt() {
        let mut m = Mem::new();
        for range in [
            (0, 1),
            (A, A),
            (A + 1, A),
            (0x7fff_ffff, A),
            (A, 0xa000_0001),
        ] {
            assert_eq!(m.set_ddr_monitor(Some(range)), Err(InvalidDdrRange));
        }
        m.set_ddr_monitor(Some((A, A + 4))).unwrap();
        m.note_ddr_access(A - 4, 4, DdrAccessKind::StorageRead);
        m.note_ddr_access(A + 4, 1, DdrAccessKind::StorageWrite);
        m.note_ddr_access(A, 0, DdrAccessKind::CpuRead);
        assert_eq!(m.first_ddr_access(), None);
        m.note_ddr_access(A - 1, 2, DdrAccessKind::StorageRead);
        let first = DdrAccess {
            address: A - 1,
            width: 2,
            kind: DdrAccessKind::StorageRead,
        };
        assert_eq!(m.first_ddr_access(), Some(first));
        m.write_byte(A, 9);
        assert_eq!(m.first_ddr_access(), Some(first));
        assert!(m.set_ddr_monitor(Some((A, A))).is_err());
        assert_eq!(m.first_ddr_access(), Some(first));
        m.clear_ddr_access();
        m.read_le(A + 3, 1);
        assert_eq!(m.first_ddr_access().unwrap().address, A + 3);
        m.set_ddr_monitor(Some((0x8000_0000, 0xa000_0000))).unwrap();
        assert_eq!(m.first_ddr_access(), None);
    }

    #[test]
    fn ddr_monitor_labels_host_image_and_export_traffic_as_storage() {
        let mut m = Mem::new();
        m.set_ddr_monitor(Some((A, A + 4))).unwrap();
        m.load(A, &[1, 2, 3, 4]);
        assert_eq!(
            m.first_ddr_access().unwrap().kind,
            DdrAccessKind::StorageWrite
        );
        m.reset();
        assert_eq!(m.first_ddr_access(), None);
        m.write_byte(A, 1);
        m.clear_ddr_access();
        assert_eq!(m.dirty_bytes(), vec![(A, 1)]);
        assert_eq!(
            m.first_ddr_access().unwrap().kind,
            DdrAccessKind::StorageRead
        );
    }

    #[test]
    fn ddr_monitor_preserves_data_and_page_policy_through_reset_remove() {
        let mut m = Mem::new();
        m.load(A, &[1, 2, 3, 4]);
        m.reset();
        assert_eq!(m.fast_read_word(A), Some(0x0403_0201));
        assert!(m.window(A, 4, true, false).is_some());
        let gen_ = m.layout_gen;
        m.set_ddr_monitor(Some((A + 1, A + 2))).unwrap();
        assert!(m.layout_gen > gen_);
        assert!(m.fast_read_word(A).is_none());
        assert!(m.window(A, 4, true, false).is_none());
        assert_eq!(m.first_ddr_access(), None); // Refusal is not an access.
        assert_eq!(m.read_le(A, 4), 0x0403_0201);
        assert_eq!(
            m.first_ddr_access().unwrap().kind,
            DdrAccessKind::StorageRead
        );
        m.clear_ddr_access();
        m.write_byte(A + 1, 2); // Same-value write still reports.
        assert_eq!(
            m.first_ddr_access().unwrap().kind,
            DdrAccessKind::StorageWrite
        );
        assert_eq!(m.read_le(A, 4), 0x0403_0201);
        m.reset();
        assert!(m.ddr_monitor_active());
        assert_eq!(m.first_ddr_access(), None);
        assert!(m.fast_read_word(A).is_none());
        m.set_ddr_monitor(None).unwrap();
        assert_eq!(m.fast_read_word(A), Some(0x0403_0201));
        m.set_ddr_monitor(Some((A + 0x10000, A + 0x10004))).unwrap();
        for k in 0..4 {
            m.write_byte(A + 0x10000 + k, 7);
        }
        assert!(m.fast_read_word(A + 0x10000).is_none());
        assert!(m.fast_write_word(A + 0x10000, 0).is_none());
        assert_eq!(m.read_le(A + 0x10000, 4), 0x0707_0707);
        m.reset();
        assert_eq!(m.first_ddr_access(), None);
        m.write_byte(A + 0x10000, 8);
        assert!(m.fast_read(A + 0x10000, 1).is_none());
        m.set_ddr_monitor(None).unwrap();
        assert_eq!(m.fast_read(A + 0x10000, 1), Some(8));
    }
}
