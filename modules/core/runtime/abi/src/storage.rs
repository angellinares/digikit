//! `Storage`: the read side of the files the core can see, behind the platform. Device differences (the DT2 firmware file system, the digikit store of
//! the DN2) live in the implementation; callers name a root and a root-relative path.
//!
//! Every call runs in task context (the UI task or the I/O task) and never in an interrupt. Errors are the `dkrt_sdk` `E_*` codes. The same rules hold on
//! every implementation (`modules/core/runtime/tests/storage/contract.toml`): an unknown root is `E_FORMAT`; a path longer than `path_max` of its root is
//! `E_TOO_BIG`; a path with a NUL is `E_SEL_PATH`. Only the root (the empty path) is a directory for `stat`; below it a directory is not found.

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

pub trait Storage {
    /// Longest root-relative path of `root`: the device's `sample_path_max` for `ROOT_SAMPLES`, the store path limit for `ROOT_STORE`; 0 for another root.
    fn path_max(&self, root: u32) -> u32;
    /// Kind, size and hash of `path` (empty: the root itself, a `Dir`). A directory below the root and a missing path give `E_NOT_FOUND`.
    unsafe fn stat(&self, root: u32, path: &[u8]) -> Result<Stat, i32>;
    /// The first 64 bytes of a file. An empty path is `E_SEL_PATH`; a missing path or a directory `E_NOT_FOUND`; a file under 64 bytes (0 included) `E_READ`.
    unsafe fn read_head(&self, root: u32, path: &[u8], out: &mut [u8; 64]) -> Result<(), i32>;
    /// `stat` and `read_head` of a file with one lookup (one find or open). The root (empty path) gives its `Stat` and 0 head bytes; for a file, `out` holds
    /// the first 64 bytes and the count is 64. Errors are those of `read_head`, except that the empty path is the root.
    unsafe fn stat_head(
        &self,
        root: u32,
        path: &[u8],
        out: &mut [u8; 64],
    ) -> Result<(Stat, usize), i32>;
    /// Read up to `out.len()` bytes from `offset`; the count read (0 at or past the end). Errors of the path are those of `read_head`.
    unsafe fn read_range(
        &self,
        root: u32,
        path: &[u8],
        offset: u32,
        out: &mut [u8],
    ) -> Result<usize, i32>;
    /// Read a whole file into `dst`, which holds `cap` bytes; the file size. The caller takes the buffer from the arena after `stat`; a file larger than `cap`
    /// gives `E_TOO_BIG` and writes nothing. Errors of the path are those of `read_head`.
    unsafe fn read_all(&self, root: u32, path: &[u8], dst: *mut u8, cap: u32) -> Result<u32, i32>;
    /// Bytes of listing state for `root`; the caller provides a block of at least this size, 16-byte aligned (an arena block), for the whole listing. The
    /// block starts with a small header the provider writes (a route tag): the caller must not touch it.
    fn list_state_bytes(&self, root: u32) -> usize;
    /// Starts a listing of `dir` (empty: the root); all provider state lives in `state` (no other memory is kept between calls). A directory that is not
    /// there, and a file, give `E_NOT_FOUND`. The order of the entries is not part of the contract.
    unsafe fn list_open(&self, root: u32, dir: &[u8], state: *mut u8) -> Result<(), i32>;
    /// One bounded provider step. `Entry` = `out` holds an entry. `Pending` = progress made, no entry yet: call again on a later step. `End` = the listing
    /// is finished (further calls return `End`). Err(`E_BUSY`) = the store lock was taken: call again later.
    unsafe fn list_next(&self, state: *mut u8, out: &mut DirEntry) -> Result<ListStep, i32>;
}

/// The result of one `Storage::list_next` step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListStep {
    Entry,
    Pending,
    End,
}
