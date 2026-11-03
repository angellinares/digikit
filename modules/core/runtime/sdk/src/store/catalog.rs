//! Internal allocation-free metadata enumeration for task/background context.
//! Opening verifies the whole table once (CRC and tail scan), with the existing
//! redundant-table fallback. Pages read only one bounded table chunk. Output is
//! staging: discard every returned entry after any error. Fingerprints detect
//! observed commits, not an immutable snapshot, lease, or ABA/collision protection.
use super::*;

/// Owned metadata only: neither file contents nor its SHA has been validated.
/// The full path includes a NUL terminator; bytes after it are zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CatalogEntry {
    pub path: [u8; PATH_BYTES],
    pub entry: Entry,
}

pub struct CatalogCursor {
    io: Io,
    lay: Layout,
    header: Hdr,
    generation: [u32; 4],
    next: u32,
    error: i32,
}

fn buffer_ok(ptr: *mut u8, cap: usize) -> bool {
    !ptr.is_null() && ptr as usize & 15 == 0 && cap >= 512 && cap & 511 == 0 && cap <= isize::MAX as usize && (ptr as usize).checked_add(cap).is_some()
}

impl CatalogCursor {
    /// Verify the store once between matching fingerprints; no buffers retained.
    /// This full-table operation belongs in task/background context, never audio.
    /// Initial lookup preserves `store::open` redundant-copy fallback semantics.
    ///
    /// # Safety
    /// `fp` holds 512 writable bytes and `scratch` holds `cap` writable bytes,
    /// private, disjoint and 16-byte aligned. I/O and layout remain valid for the
    /// cursor lifetime. Subsequent calls must not overlap their buffers/output.
    pub unsafe fn open(io: &Io, lay: &Layout, fp: *mut u8, scratch: *mut u8, cap: usize) -> Result<Self, i32> {
        if lay.sectors <= TAIL || lay.first.checked_add(lay.sectors).is_none() || !buffer_ok(fp, 512) || !buffer_ok(scratch, cap) {
            return Err(E_BAD_STORE);
        }
        let generation = fingerprint(io, lay, fp).ok_or(E_READ)?;
        let table = super::open(io, lay, fp, scratch, core::cmp::min(cap, MAX_IO_SECTORS as usize * 512), true)?;
        if fingerprint(io, lay, fp).ok_or(E_READ)? != generation {
            return Err(crate::E_BUSY);
        }
        Ok(Self { io: *io, lay: *lay, header: table.h, generation, next: 0, error: 0 })
    }

    fn fail<T>(&mut self, error: i32) -> Result<T, i32> {
        self.error = error;
        Err(error)
    }

    unsafe fn check(&mut self, fp: *mut u8) -> Result<(), i32> {
        let observed = fingerprint(&self.io, &self.lay, fp);
        if observed.is_none() {
            return self.fail(E_READ);
        }
        if observed != Some(self.generation) {
            return self.fail(crate::E_BUSY);
        }
        Ok(())
    }

    /// Restart enumeration only within the retained generation. This performs
    /// two header reads, never table verification or generation adoption. Errors
    /// invalidate all prior output and remain sticky, as with `read_page`.
    ///
    /// # Safety
    /// `fp` is a private aligned writable 512-byte buffer.
    pub unsafe fn rewind(&mut self, fp: *mut u8) -> Result<(), i32> {
        if self.error != 0 {
            return Err(self.error);
        }
        if !buffer_ok(fp, 512) {
            return self.fail(E_BAD_STORE);
        }
        self.check(fp)?;
        self.next = 0;
        Ok(())
    }

    /// Advance the raw table index without filtering. At most one table read,
    /// bounded by scratch sectors, MAX_IO_SECTORS, and output capacity (an odd
    /// index/capacity can require one unused half-sector). Zero is only table EOF;
    /// even repeated EOF checks generation. Errors, including bad bounds, stick.
    /// Discard ALL staging output on error, including entries from earlier pages.
    /// No PCM/hash validation or READY publication occurs here.
    ///
    /// # Safety
    /// `fp` and `scratch` meet `open`'s writable/disjoint buffer obligations and
    /// are disjoint from the caller-owned `out` slice. Nothing retains a buffer.
    pub unsafe fn read_page(&mut self, fp: *mut u8, scratch: *mut u8, cap: usize, out: &mut [CatalogEntry]) -> Result<usize, i32> {
        if self.error != 0 {
            return Err(self.error);
        }
        if !buffer_ok(fp, 512) || !buffer_ok(scratch, cap) || out.is_empty() {
            return self.fail(E_BAD_STORE);
        }
        self.check(fp)?;
        let left = self.header.count - self.next;
        if left == 0 {
            self.check(fp)?;
            return Ok(0);
        }
        let skip = (self.next & 1) as usize;
        let sectors = core::cmp::min(cap / 512, MAX_IO_SECTORS as usize);
        let n = core::cmp::min(left as usize, core::cmp::min(out.len(), sectors * 2 - skip));
        let count = (skip + n + 1) / 2;
        let r = read_sectors(&self.io, &self.lay, self.header.table_first + self.next / 2, count as u32, scratch, cap);
        if r != OK {
            return self.fail(r);
        }
        let mut i = 0;
        while i < n {
            let raw = scratch.add((skip + i) * ENTRY);
            let mut path = [0; PATH_BYTES];
            let mut k = 0;
            while k < PATH_BYTES && *raw.add(k) != 0 {
                path[k] = *raw.add(k);
                k += 1;
            }
            let entry = entry_of(raw);
            if k == PATH_BYTES || (entry.kind != KIND_FILE && entry.kind != KIND_DIR) {
                return self.fail(E_BAD_STORE);
            }
            out[i] = CatalogEntry { path, entry };
            i += 1;
        }
        self.check(fp)?;
        self.next += n as u32;
        Ok(n)
    }
}

/// Incremental scheduler-facing verified open. `begin` performs no I/O. The
/// first step captures both headers between fingerprints (six sector reads);
/// later steps use four header reads plus at most one table read of
/// min(cap/512, MAX_IO_SECTORS) sectors. CRC and tail checks share that chunk.
/// Total verification is still whole-table work, potentially for both captured
/// header copies, including identical copies. Captured-header read errors are
/// deliberately strict (E_READ), including copy B errors hidden by legacy
/// open_headers. Invalid CRC/plausibility still permits the usable other copy.
/// No wall-time bound is claimed:
/// even one firmware read may block. Run in task/background context, not audio.
/// No caller buffer is retained; no READY/cache/public ABI is introduced.
pub struct CatalogOpen {
    io: Io,
    lay: Layout,
    headers: [Option<Hdr>; 2],
    generation: [u32; 4],
    captured: bool,
    candidate: usize,
    done: u32,
    crc: u32,
    tail: bool,
    selected: Option<Hdr>,
    error: i32,
}

impl CatalogOpen {
    pub fn begin(io: &Io, lay: &Layout) -> Result<Self, i32> {
        if lay.sectors <= TAIL || lay.first.checked_add(lay.sectors).is_none() {
            return Err(E_BAD_STORE);
        }
        Ok(Self {
            io: *io,
            lay: *lay,
            headers: [None; 2],
            generation: [0; 4],
            captured: false,
            candidate: 0,
            done: 0,
            crc: 0xffff_ffff,
            tail: false,
            selected: None,
            error: 0,
        })
    }

    fn fail<T>(&mut self, error: i32) -> Result<T, i32> {
        self.error = error;
        self.selected = None;
        Err(error)
    }

    unsafe fn check(&mut self, fp: *mut u8) -> Result<(), i32> {
        let observed = fingerprint(&self.io, &self.lay, fp);
        if observed.is_none() {
            return self.fail(E_READ);
        }
        if observed != Some(self.generation) {
            return self.fail(crate::E_BUSY);
        }
        Ok(())
    }

    fn reject(&mut self) {
        self.candidate += 1;
        self.done = 0;
        self.crc = 0xffff_ffff;
        self.tail = false;
    }

    /// `Ok(true)` means verification completed; consume with `take`. A failed
    /// candidate (CRC, tail extent, table I/O) falls back to the other captured
    /// header on a later step, never a new generation. All terminal errors stick.
    /// Malformed paths/kinds are left to cursor/projection as in store::open(true).
    ///
    /// # Safety
    /// Private disjoint `fp` (512 bytes) and `scratch` (`cap` bytes) are writable
    /// and 16-byte aligned. Io/layout lifetime obligations match CatalogCursor.
    pub unsafe fn step(&mut self, fp: *mut u8, scratch: *mut u8, cap: usize) -> Result<bool, i32> {
        if self.error != 0 {
            return Err(self.error);
        }
        if !buffer_ok(fp, 512) || !buffer_ok(scratch, cap) {
            return self.fail(E_BAD_STORE);
        }
        if !self.captured {
            self.generation = match fingerprint(&self.io, &self.lay, fp) {
                Some(value) => value,
                None => return self.fail(E_READ),
            };
            // Unlike legacy open_headers, do not hide a transient B read error.
            // CRC/plausibility/absent-copy acceptance is otherwise identical.
            let headers = (|| {
                if read_sectors(&self.io, &self.lay, 0, 1, fp, 512) != OK {
                    return Err(E_READ);
                }
                let a_magic = be32(fp) == MAGIC;
                let a = parse_header(fp).filter(|h| plausible(&self.lay, h));
                if read_sectors(&self.io, &self.lay, self.lay.header_b(), 1, fp, 512) != OK {
                    return Err(E_READ);
                }
                let b = parse_header(fp).filter(|h| plausible(&self.lay, h));
                if a.is_none() && b.is_none() {
                    return Err(if a_magic { E_BAD_STORE } else { E_NOT_FOUND });
                }
                Ok((a, b))
            })();
            self.check(fp)?;
            let (a, b) = match headers {
                Ok(value) => value,
                Err(error) => return self.fail(error),
            };
            self.headers = if prefer_b(a.as_ref(), b.as_ref()) { [b, a] } else { [a, b] };
            self.captured = true;
            return Ok(false);
        }
        self.check(fp)?;
        if self.selected.is_some() {
            self.check(fp)?;
            return Ok(true);
        }
        // At most two absent candidates; no table reads in this loop.
        while self.candidate < 2 && self.headers[self.candidate].is_none() {
            self.reject();
        }
        if self.candidate == 2 {
            return self.fail(E_BAD_STORE);
        }
        let h = self.headers[self.candidate].unwrap();
        let mut bad_read = false;
        if self.done < h.table_sectors {
            let count = core::cmp::min(h.table_sectors - self.done, core::cmp::min(cap / 512, MAX_IO_SECTORS as usize) as u32);
            let r = read_sectors(&self.io, &self.lay, h.table_first + self.done, count, scratch, cap);
            if r != OK {
                bad_read = true;
            } else {
                self.crc = crc32(self.crc, scratch, count as usize * 512);
                let mut i = 0;
                while i < count * 2 && self.done * 2 + i < h.count {
                    let entry = entry_of(scratch.add(i as usize * ENTRY));
                    let sectors = entry_sectors(&entry);
                    if sectors != 0 && entry.first.saturating_add(sectors) > self.lay.header_b() {
                        self.tail = true;
                    }
                    i += 1;
                }
                self.done += count;
            }
        }
        self.check(fp)?;
        if bad_read || (self.done == h.table_sectors && (!self.crc != h.table_crc || self.tail)) {
            self.reject();
            if self.candidate == 2 {
                return self.fail(E_BAD_STORE);
            }
            return Ok(false);
        }
        if self.done == h.table_sectors {
            self.selected = Some(h);
        }
        Ok(self.selected.is_some())
    }

    /// Consume only completed verification. The cursor keeps the captured
    /// generation, so its next read/rewind rejects later observed commits. This
    /// is no immutable lease or protection against fingerprint ABA/collisions.
    pub fn take(self) -> Result<CatalogCursor, i32> {
        if self.error != 0 {
            return Err(self.error);
        }
        let header = self.selected.ok_or(crate::E_BUSY)?;
        Ok(CatalogCursor { io: self.io, lay: self.lay, header, generation: self.generation, next: 0, error: 0 })
    }
}
