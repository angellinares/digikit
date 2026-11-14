//! `Storage`: the read side of the files the core can see, behind the platform. Device differences (the DT2 firmware file system, the digikit store of
//! the DN2) live in the implementation; callers name a root and a root-relative path.
//!
//! Every call runs in task context (the UI task or the I/O task) and never in an interrupt. Errors are the `dkrt_sdk` `E_*` codes.

use dkrt_sdk::files::{HASH_ELEKTRON32 as SDK_HASH_ELEKTRON32, SAMPLES, STORE};

/// The two roots (`DK_FILES_SAMPLES`, `DK_FILES_STORE`).
pub const ROOT_SAMPLES: u32 = SAMPLES;
pub const ROOT_STORE: u32 = STORE;

/// `Stat::hash_kind` values: no hash, or the 32-byte file hash of the Elektron file table (the kind of the listing rows).
pub const HASH_NONE: u8 = 0;
pub const HASH_ELEKTRON32: u8 = SDK_HASH_ELEKTRON32 as u8;

/// Most bytes of a root-relative path (the listing row has 256 bytes with a NUL).
pub const NAME_MAX: usize = 255;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stat {
    pub kind: EntryKind,
    pub size: u32,
    pub hash_kind: u8,
    pub hash_valid: bool,
    pub hash: [u8; 32],
}

/// One listing entry. `name` holds the entry's own name (not the path), `name_len` bytes of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub name: [u8; 256],
    pub name_len: u16,
    pub stat: Stat,
}

impl DirEntry {
    pub const fn empty() -> Self {
        DirEntry {
            name: [0; 256],
            name_len: 0,
            stat: Stat {
                kind: EntryKind::File,
                size: 0,
                hash_kind: HASH_NONE,
                hash_valid: false,
                hash: [0; 32],
            },
        }
    }
    pub fn name(&self) -> &[u8] {
        &self.name[..self.name_len as usize]
    }
}

/// Words of a `ListCursor`. The DT2 firmware directory state is 4 words (its 0x114-byte output buffer stays with the provider); the store catalog cursor
/// (`Io` 2 pointers, `Layout` 2 words, `Hdr` 7 words, generation 4 words, next and error: 17 words on the device, 19 on a 64-bit host) needs the most.
/// 32 words leave room for an index and flags next to it.
pub const LIST_CURSOR_WORDS: usize = 32;

/// Opaque listing state owned by the caller (it lives in the caller's arena block). Only the provider reads or writes the words.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ListCursor {
    pub words: [u32; LIST_CURSOR_WORDS],
}

impl ListCursor {
    pub const fn new() -> Self {
        ListCursor {
            words: [0; LIST_CURSOR_WORDS],
        }
    }
}

pub trait Storage {
    /// Longest root-relative path of `root`: the device's `sample_path_max` for `ROOT_SAMPLES`, the store path limit for `ROOT_STORE`; 0 for another root.
    fn path_max(&self, root: u32) -> u32;
    /// Kind, size and hash of `path` (empty: the root itself).
    unsafe fn stat(&self, root: u32, path: &[u8]) -> Result<Stat, i32>;
    /// The first 64 bytes of a file.
    unsafe fn read_head(&self, root: u32, path: &[u8], out: &mut [u8; 64]) -> Result<(), i32>;
    /// Read up to `out.len()` bytes from `offset`; the count read (0 at or past the end).
    unsafe fn read_range(
        &self,
        root: u32,
        path: &[u8],
        offset: u32,
        out: &mut [u8],
    ) -> Result<usize, i32>;
    /// Read a whole file into `dst`, which holds `cap` bytes; the file size. The caller takes the buffer from the arena after `stat`; a file larger than `cap`
    /// gives `E_TOO_BIG` and writes nothing.
    unsafe fn read_all(&self, root: u32, path: &[u8], dst: *mut u8, cap: u32) -> Result<u32, i32>;
    /// Start listing `dir` into the caller's cursor.
    unsafe fn list_open(&self, root: u32, dir: &[u8], cur: *mut ListCursor) -> Result<(), i32>;
    /// One provider step. Ok(true) = `out` holds an entry, Ok(false) = end of listing.
    unsafe fn list_next(&self, cur: *mut ListCursor, out: &mut DirEntry) -> Result<bool, i32>;
}
