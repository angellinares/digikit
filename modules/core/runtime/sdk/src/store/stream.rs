//! A single-use, allocation-free store file cursor. Returned bytes are staging:
//! any error invalidates all prior output. EOF is not validation; only successful
//! `finish` checks the caller's SHA-256 of the exact bytes returned by `read`.
//! No READY state is published here. Header fingerprints detect observed commits,
//! but provide neither an immutable lease nor a guarantee against ABA/collisions.
use super::*;

/// A retained entry and generation, never a retained table buffer. No retry can
/// combine bytes from different observed generations. Fields stay private so
/// only exact EOF and digest completion can produce a validated entry.
pub struct FileStream {
    io: Io,
    lay: Layout,
    entry: Entry,
    generation: [u32; 4],
    offset: u32,
    error: i32,
    finished: bool,
}

impl FileStream {
    /// Find a verified nonempty file between matching header fingerprints.
    /// Initial lookup preserves `store::open` redundant-copy fallback behavior.
    ///
    /// # Safety
    /// `path` is NUL terminated. `fp` holds 512 writable bytes; `scratch` holds
    /// `cap` writable bytes. Both are private, disjoint and 16-byte aligned.
    /// The I/O functions and layout must remain valid for this cursor's lifetime.
    pub unsafe fn open(
        io: &Io,
        lay: &Layout,
        path: *const u8,
        fp: *mut u8,
        scratch: *mut u8,
        cap: usize,
        max_bytes: u32,
    ) -> Result<Self, i32> {
        if lay.sectors <= TAIL
            || lay.first.checked_add(lay.sectors).is_none()
            || fp.is_null()
            || scratch.is_null()
            || (fp as usize | scratch as usize) & 15 != 0
            || cap < 512
            || cap & 511 != 0
        {
            return Err(E_BAD_STORE);
        }
        let generation = fingerprint(io, lay, fp).ok_or(E_READ)?;
        let table = super::open(io, lay, fp, scratch, cap, true)?;
        let (_, entry) = find(io, lay, &table.h, scratch, cap, path)?.ok_or(E_NOT_FOUND)?;
        if fingerprint(io, lay, fp).ok_or(E_READ)? != generation {
            return Err(crate::E_BUSY);
        }
        if entry.kind != KIND_FILE {
            return Err(E_NOT_FOUND);
        }
        if entry.size == 0 || entry.size > max_bytes || entry.size > u32::MAX - 511 {
            return Err(crate::E_TOO_BIG);
        }
        let sectors = sectors_of(entry.size);
        if entry.first == 0 || entry.first >= lay.limit() || sectors > lay.limit() - entry.first {
            return Err(E_BAD_STORE);
        }
        Ok(Self {
            io: *io,
            lay: *lay,
            entry,
            generation,
            offset: 0,
            error: 0,
            finished: false,
        })
    }

    fn fail<T>(&mut self, error: i32) -> Result<T, i32> {
        self.error = error;
        Err(error)
    }

    unsafe fn check(&mut self, fp: *mut u8) -> Result<(), i32> {
        if fp.is_null() || fp as usize & 15 != 0 {
            return self.fail(E_BAD_STORE);
        }
        let observed = fingerprint(&self.io, &self.lay, fp);
        if observed.is_none() {
            return self.fail(E_READ);
        }
        if observed != Some(self.generation) {
            return self.fail(crate::E_BUSY);
        }
        Ok(())
    }

    /// Read at most `cap`, `MAX_IO_SECTORS * 512`, and the remaining extent.
    /// Returns valid bytes, excluding final sector padding; zero means exact EOF.
    /// The caller must incrementally hash only these returned bytes, in order.
    /// All errors are sticky, including bad buffer bounds. Successful completion
    /// makes subsequent reads return zero without more I/O.
    ///
    /// # Safety
    /// `fp` is a private aligned 512-byte buffer; `dst` is a disjoint private
    /// aligned `cap`-byte buffer. Padding may be written but must not be consumed.
    pub unsafe fn read(&mut self, fp: *mut u8, dst: *mut u8, cap: usize) -> Result<usize, i32> {
        if self.error != 0 {
            return Err(self.error);
        }
        if self.finished {
            return Ok(0);
        }
        if dst.is_null() || dst as usize & 15 != 0 || cap < 512 || cap & 511 != 0 {
            return self.fail(E_BAD_STORE);
        }
        self.check(fp)?;
        let left = self.entry.size - self.offset;
        if left == 0 {
            return Ok(0);
        }
        let count = core::cmp::min(sectors_of(left), chunk_sectors(cap));
        let r = read_sectors(&self.io, &self.lay, self.entry.first + self.offset / 512, count, dst, cap);
        if r != OK {
            return self.fail(r);
        }
        self.check(fp)?;
        let bytes = core::cmp::min(left, count * 512);
        self.offset += bytes;
        Ok(bytes as usize)
    }

    /// Validate exact EOF, a final generation check, and the caller's incremental
    /// SHA-256 over every returned byte (excluding padding). A premature finish
    /// or wrong digest permanently invalidates all staging output. The caller
    /// must fulfill the hash obligation; this API cannot prove how it was made.
    /// Repeated successful finishes return the same entry without further I/O.
    ///
    /// # Safety
    /// `fp` is a private, 16-byte aligned, writable 512-byte buffer.
    pub unsafe fn finish(&mut self, fp: *mut u8, sha: [u8; 32]) -> Result<Entry, i32> {
        if self.error != 0 {
            return Err(self.error);
        }
        if self.finished {
            return Ok(self.entry);
        }
        if self.offset != self.entry.size {
            return self.fail(crate::E_FORMAT);
        }
        self.check(fp)?;
        if sha != self.entry.sha {
            return self.fail(crate::E_SHA);
        }
        self.finished = true;
        Ok(self.entry)
    }
}
