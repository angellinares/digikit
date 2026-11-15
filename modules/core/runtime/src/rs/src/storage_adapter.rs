//! The two routes behind `Storage for RealPlatform` (platform.rs calls only this file): the digikit store (`ROOT_STORE` on both devices, and `ROOT_SAMPLES`
//! as the `samples/` prefix of the store when the device keeps its samples there) and the DT2 firmware file system (`ROOT_SAMPLES` otherwise).
//! This file names no firmware crate: the file system is reached through the device tables `dkrt_sample_fs` and `dkrt_files_fs` of the runtime's
//! assembly objects (zeros on a device without them).
//!
//! Every call runs in task context. Device facts come from `Facts` (the profile), never from a device id.
//!
//! A listing keeps all its state in a block the caller provides (`list_state_bytes`): `Dt2List` or `StoreList`, told apart by the tag in the first word.

use core::ptr::{addr_of, addr_of_mut, read_volatile};
use dkrt_abi::storage::{DirEntry, EntryKind, ListStep, Stat, HASH_ELEKTRON32, HASH_NONE, ROOT_SAMPLES, ROOT_STORE};
use dkrt_sdk::store::catalog::{CatalogCursor, CatalogEntry, CatalogOpen};
use dkrt_sdk::store::directory::DirectoryCollector;
use dkrt_sdk::store::{PATH_BYTES, PATH_MAX};
use dkrt_sdk::{E_BAD_STORE, E_BUSY, E_FORMAT, E_NOT_FOUND, E_READ, E_SEL_PATH, E_TOO_BIG, OK};

extern "C" {
    // Per-device tables of the runtime's assembly objects (DT2 1.17 anchors; zeros on DN2). Only this file declares them.
    static dkrt_sample_fs: [u32; 4];
    static dkrt_files_fs: [u32; 3];
}

type FsOpen = unsafe extern "C" fn(*const u8, *const u8, *mut u32) -> i32;
type FsRead = unsafe extern "C" fn(*mut u8, u32, *mut u32) -> i32;
type FsClose = unsafe extern "C" fn(*mut u32) -> i32;
type FsSize = unsafe extern "C" fn(u32) -> u32;
type FsMeta = unsafe extern "C" fn(u32, *mut u32) -> i32;
type DirOpen = unsafe extern "C" fn(*const u8, *mut u32) -> i32;
type DirNext = unsafe extern "C" fn(*mut u32, *mut u8) -> i32;

/// The device facts the routes need (`DeviceProfile::samples_in_store` and `sample_path_max`).
#[derive(Clone, Copy)]
pub(crate) struct Facts {
    pub in_store: bool,
    pub sample_path_max: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Route {
    Store,
    Fs,
}

fn route(root: u32, f: &Facts) -> Result<Route, i32> {
    if root == ROOT_STORE {
        return Ok(Route::Store);
    }
    if root == ROOT_SAMPLES {
        return Ok(if f.in_store { Route::Store } else { Route::Fs });
    }
    Err(E_FORMAT)
}

/// Longest root-relative path of `root` (0 for another root).
fn limit(root: u32, f: &Facts) -> usize {
    if root == ROOT_STORE {
        return PATH_MAX;
    }
    if root == ROOT_SAMPLES {
        return f.sample_path_max as usize;
    }
    0
}

/// The route of `root`, after the length check of `path` (the same `E_TOO_BIG` as the listing request).
fn prepare(root: u32, f: &Facts, path: &[u8]) -> Result<Route, i32> {
    let r = route(root, f)?;
    if path.len() > limit(root, f) {
        return Err(E_TOO_BIG);
    }
    Ok(r)
}

fn dir_stat() -> Stat {
    Stat { kind: EntryKind::Dir, size: 0, hash_kind: HASH_NONE, hash_valid: false, hash: [0; 32] }
}

// ---- store route -------------------------------------------------------------------------------------------------------------

/// The store path of a root-relative `path`: `samples/` in front for `ROOT_SAMPLES`, NUL-terminated in `out`. Returns the length without the NUL.
fn store_key(root: u32, path: &[u8], out: &mut [u8; PATH_BYTES]) -> Result<usize, i32> {
    if path.contains(&0) {
        return Err(E_SEL_PATH);
    }
    let prefix: &[u8] = if root == ROOT_SAMPLES { b"samples" } else { b"" };
    let sep = if !prefix.is_empty() && !path.is_empty() { 1 } else { 0 };
    let total = prefix.len() + sep + path.len();
    if total > PATH_MAX {
        return Err(E_TOO_BIG);
    }
    *out = [0; PATH_BYTES];
    out[..prefix.len()].copy_from_slice(prefix);
    if sep == 1 {
        out[prefix.len()] = b'/';
    }
    out[prefix.len() + sep..total].copy_from_slice(path);
    Ok(total)
}

/// 512 bytes, 16-byte aligned: the buffer of one store sector read.
#[repr(C, align(16))]
struct Sector([u8; 512]);

#[repr(C, align(16))]
struct Scratch([u8; 4096]);
#[repr(C, align(16))]
struct Fingerprint([u8; 512]);
static mut SCRATCH: Scratch = Scratch([0; 4096]);
static mut FP: Fingerprint = Fingerprint([0; 512]);
const RAW: CatalogEntry = CatalogEntry { path: [0; 160], entry: dkrt_sdk::store::Entry { first: 0, size: 0, kind: 0, sha: [0; 32] } };
static mut RAW_PAGE: [CatalogEntry; 16] = [RAW; 16];
/// The store buffers of a catalog read (fingerprint, 4096-byte scratch, raw page). They are shared by all listings, not part of a listing's state: callers
/// hold the store lock for every step that uses them (`list_next` does).
fn store_buffers() -> (*mut u8, *mut u8, *mut [CatalogEntry; 16]) {
    unsafe { (addr_of_mut!(FP) as *mut u8, addr_of_mut!(SCRATCH) as *mut u8, addr_of_mut!(RAW_PAGE)) }
}

// ---- DT2 firmware file system ------------------------------------------------------------------------------------------------

/// The path of the firmware file system: `/` and the root-relative `path`, NUL-terminated. `out` holds at least `path.len() + 2` bytes.
fn fs_key(path: &[u8], out: &mut [u8]) -> Result<(), i32> {
    if path.is_empty() || path.contains(&0) {
        return Err(E_SEL_PATH);
    }
    if path.len() > 255 {
        return Err(E_TOO_BIG);
    }
    out[0] = b'/';
    out[1..1 + path.len()].copy_from_slice(path);
    out[1 + path.len()] = 0;
    Ok(())
}

/// The file hash of the Elektron file table as the firmware's metadata call reports it (words: id, odd hash, physical size, node): the odd hash is
/// the first 4 bytes, big-endian, the rest is 0.
fn fs_hash(values: &[u32; 4]) -> [u8; 32] {
    let mut h = [0u8; 32];
    h[..4].copy_from_slice(&values[1].to_be_bytes());
    h
}

struct Fs {
    open: FsOpen,
    read: FsRead,
    close: FsClose,
    size: FsSize,
    meta: Option<FsMeta>,
}

unsafe fn fs_table() -> Result<Fs, i32> {
    let table = addr_of!(dkrt_sample_fs) as *const u32;
    let (open_addr, read_addr, close_addr, size_addr) =
        (read_volatile(table), read_volatile(table.add(1)), read_volatile(table.add(2)), read_volatile(table.add(3)));
    let meta_addr = read_volatile((addr_of!(dkrt_files_fs) as *const u32).add(2));
    if open_addr == 0 || read_addr == 0 || close_addr == 0 || size_addr == 0 {
        return Err(E_NOT_FOUND);
    }
    Ok(Fs {
        open: core::mem::transmute(open_addr as usize),
        read: core::mem::transmute(read_addr as usize),
        close: core::mem::transmute(close_addr as usize),
        size: core::mem::transmute(size_addr as usize),
        meta: if meta_addr != 0 { Some(core::mem::transmute(meta_addr as usize)) } else { None },
    })
}

/// Open the file `z` (NUL-terminated), run `f` on the handle, close it. `E_NOT_FOUND` when it does not open; a failed close is `E_READ` (as before).
unsafe fn fs_with<T>(fs: &Fs, z: &[u8], f: impl FnOnce(&Fs, &mut [u32; 4]) -> Result<T, i32>) -> Result<T, i32> {
    let mut handle = [0u32; 4];
    if (fs.open)(z.as_ptr(), b"r\0".as_ptr(), handle.as_mut_ptr()) != 0 {
        return Err(E_NOT_FOUND);
    }
    let result = f(fs, &mut handle);
    if (fs.close)(handle.as_mut_ptr()) != 0 {
        return Err(E_READ);
    }
    result
}

/// The size of an open file (`u32::MAX` is the firmware's failure).
unsafe fn fs_size(fs: &Fs, handle: &mut [u32; 4]) -> Result<u32, i32> {
    let size = (fs.size)(handle[0]);
    if size == u32::MAX {
        return Err(E_READ);
    }
    Ok(size)
}

/// Read exactly `n` bytes at the file position into `dst`.
unsafe fn fs_read_exact(fs: &Fs, handle: &mut [u32; 4], dst: *mut u8, n: usize) -> Result<(), i32> {
    let mut done = 0usize;
    while done < n {
        let want = core::cmp::min(n - done, 0x10000);
        let got = (fs.read)(dst.add(done), want as u32, handle.as_mut_ptr());
        if got <= 0 || got as usize > want {
            return Err(E_READ);
        }
        done += got as usize;
    }
    Ok(())
}

// ---- the Storage calls -------------------------------------------------------------------------------------------------------

/// Kind, size and hash. The store gives the table entry's size and file hash (kind `HASH_ELEKTRON32`, valid, the 32 bytes), as `store_identity`
/// (`sample_tracks.rs` of the SDK) does for `dk_file_id` and as the store listing rows do (files.rs, phase 3). The firmware file system gives the size of
/// the open file and, when the metadata call answers with a physical size equal to it, the odd hash as `HASH_ELEKTRON32` (4 bytes): the rule of
/// tracks.rs `probe`. A folder is the root itself (empty path) only: the store's lookup finds files, and a folder does not open as a file on DT2.
pub(crate) unsafe fn stat(root: u32, f: &Facts, path: &[u8]) -> Result<Stat, i32> {
    let r = prepare(root, f, path)?;
    if path.is_empty() {
        return Ok(dir_stat());
    }
    if r == Route::Store {
        let mut z = [0u8; PATH_BYTES];
        store_key(root, path, &mut z)?;
        let file = crate::store::find(z.as_ptr())?;
        return Ok(Stat { kind: EntryKind::File, size: file.size, hash_kind: HASH_ELEKTRON32, hash_valid: true, hash: file.sha });
    }
    let mut z = [0u8; 258];
    fs_key(path, &mut z)?;
    let fs = fs_table()?;
    fs_with(&fs, &z, |fs, handle| {
        let size = fs_size(fs, handle)?;
        let mut st = Stat { kind: EntryKind::File, size, hash_kind: HASH_NONE, hash_valid: false, hash: [0; 32] };
        if let Some(meta) = fs.meta {
            let mut values = [0u32; 4];
            if meta(handle[0], values.as_mut_ptr()) == 0 && values[2] == size {
                st.hash_kind = HASH_ELEKTRON32;
                st.hash_valid = true;
                st.hash = fs_hash(&values);
            }
        }
        Ok(st)
    })
}

/// The first 64 bytes. Store: `store::read_head` (a file of size 0 is `E_FORMAT`). DT2: a file under 64 bytes or a short read is `E_READ`.
pub(crate) unsafe fn read_head(root: u32, f: &Facts, path: &[u8], out: &mut [u8; 64]) -> Result<(), i32> {
    let r = prepare(root, f, path)?;
    if path.is_empty() {
        return Err(E_SEL_PATH);
    }
    if r == Route::Store {
        let mut z = [0u8; PATH_BYTES];
        store_key(root, path, &mut z)?;
        let file = crate::store::find(z.as_ptr())?;
        return crate::store::read_head(&file, out);
    }
    let mut z = [0u8; 258];
    fs_key(path, &mut z)?;
    let fs = fs_table()?;
    fs_with(&fs, &z, |fs, handle| {
        let size = fs_size(fs, handle)?;
        if size < 64 {
            return Err(E_READ);
        }
        fs_read_exact(fs, handle, out.as_mut_ptr(), 64).map_err(|_| E_READ)
    })
}

/// `stat` and `read_head` with one lookup: the store route does one `store::find` and reads the head of that entry; the DT2 route opens the file once, takes
/// its size and metadata and reads 64 bytes. The root itself (empty path) gives its `Stat` and 0 head bytes; a folder does not open on either route. The head
/// count is `min(size, 64)` on the store route and 64 on DT2. Errors are those of `read_head`.
pub(crate) unsafe fn stat_head(root: u32, f: &Facts, path: &[u8], out: &mut [u8; 64]) -> Result<(Stat, usize), i32> {
    let r = prepare(root, f, path)?;
    if path.is_empty() {
        return Ok((dir_stat(), 0));
    }
    if r == Route::Store {
        let mut z = [0u8; PATH_BYTES];
        store_key(root, path, &mut z)?;
        let file = crate::store::find(z.as_ptr())?;
        let st = Stat { kind: EntryKind::File, size: file.size, hash_kind: HASH_ELEKTRON32, hash_valid: true, hash: file.sha };
        crate::store::read_head(&file, out)?;
        return Ok((st, core::cmp::min(file.size, 64) as usize));
    }
    let mut z = [0u8; 258];
    fs_key(path, &mut z)?;
    let fs = fs_table()?;
    fs_with(&fs, &z, |fs, handle| {
        let size = fs_size(fs, handle)?;
        let mut st = Stat { kind: EntryKind::File, size, hash_kind: HASH_NONE, hash_valid: false, hash: [0; 32] };
        if let Some(meta) = fs.meta {
            let mut values = [0u32; 4];
            if meta(handle[0], values.as_mut_ptr()) == 0 && values[2] == size {
                st.hash_kind = HASH_ELEKTRON32;
                st.hash_valid = true;
                st.hash = fs_hash(&values);
            }
        }
        if size < 64 {
            return Err(E_READ);
        }
        fs_read_exact(fs, handle, out.as_mut_ptr(), 64).map_err(|_| E_READ)?;
        Ok((st, 64))
    })
}

/// Read up to `out.len()` bytes from `offset`. Store: one lookup that also leases the file's extent (`store::lease`, owner 0) for the whole read, so a commit
/// cannot give its sectors to another file between two sector reads; the lease ends on every exit path. Then one store sector read at a time
/// (`store::read_sector` takes the store lock for each sector, the same lock every store read takes, and waits for it as `store_writer::acquire` does);
/// the table's file hash is not checked (only `read_all` does). DT2: the firmware file system has no seek in the device table, so the bytes before `offset` are read and dropped (cost grows
/// with `offset`).
pub(crate) unsafe fn read_range(root: u32, f: &Facts, path: &[u8], offset: u32, out: &mut [u8]) -> Result<usize, i32> {
    let r = prepare(root, f, path)?;
    if path.is_empty() {
        return Err(E_SEL_PATH);
    }
    if r == Route::Store {
        let mut z = [0u8; PATH_BYTES];
        store_key(root, path, &mut z)?;
        let mut file = dkrt_sdk::DkrtStoreFile { first: 0, size: 0, sha: [0; 32] };
        let id = crate::store::lease(0, z.as_ptr(), &mut file);
        if id <= 0 {
            return Err(if id == 0 { E_NOT_FOUND } else { id });
        }
        let result = (|| {
            if offset >= file.size || out.is_empty() {
                return Ok(0);
            }
            let n = core::cmp::min(out.len(), (file.size - offset) as usize);
            let mut sector = Sector([0; 512]);
            let mut done = 0usize;
            while done < n {
                let at = offset as usize + done;
                let within = at % 512;
                let take = core::cmp::min(512 - within, n - done);
                let rc = crate::store::read_sector(file.first + (at / 512) as u32, sector.0.as_mut_ptr());
                if rc != OK {
                    return Err(rc);
                }
                out[done..done + take].copy_from_slice(&sector.0[within..within + take]);
                done += take;
            }
            Ok(n)
        })();
        crate::store::dkrt_store_unlease(id);
        return result;
    }
    let mut z = [0u8; 258];
    fs_key(path, &mut z)?;
    let fs = fs_table()?;
    fs_with(&fs, &z, |fs, handle| {
        let size = fs_size(fs, handle)?;
        if offset >= size || out.is_empty() {
            return Ok(0);
        }
        let mut skip = offset as usize;
        let mut junk = [0u8; 256];
        while skip != 0 {
            let step = core::cmp::min(skip, junk.len());
            fs_read_exact(fs, handle, junk.as_mut_ptr(), step)?;
            skip -= step;
        }
        let n = core::cmp::min(out.len(), (size - offset) as usize);
        fs_read_exact(fs, handle, out.as_mut_ptr(), n)?;
        Ok(n)
    })
}

/// Read the whole file into `dst` (`cap` bytes); the size. A file over `cap` is `E_TOO_BIG` and nothing is written. Store: `store::read_file` (checks the
/// table's file hash). DT2: size from the open file, then sequential reads.
pub(crate) unsafe fn read_all(root: u32, f: &Facts, path: &[u8], dst: *mut u8, cap: u32) -> Result<u32, i32> {
    let r = prepare(root, f, path)?;
    if path.is_empty() {
        return Err(E_SEL_PATH);
    }
    if r == Route::Store {
        let mut z = [0u8; PATH_BYTES];
        store_key(root, path, &mut z)?;
        let n = crate::store::read_file(z.as_ptr(), dst, cap);
        return if n < 0 { Err(n) } else { Ok(n as u32) };
    }
    let mut z = [0u8; 258];
    fs_key(path, &mut z)?;
    let fs = fs_table()?;
    fs_with(&fs, &z, |fs, handle| {
        let size = fs_size(fs, handle)?;
        if size > cap {
            return Err(E_TOO_BIG);
        }
        fs_read_exact(fs, handle, dst, size as usize)?;
        Ok(size)
    })
}

// ---- listing ---------------------------------------------------------------------------------------------------------------

/// Tags of the first word of the state block (the header the caller must not touch).
const TAG_FS: u32 = 0x4653_0001;
const TAG_STORE: u32 = 0x5354_0001;

/// DT2 listing state: the firmware's directory state (4 words), the output buffer of its `next` (0x114 bytes: id, kind, ..., name at +20) and the path
/// handed to its directory open (the firmware may keep the pointer in its directory state, so the path lives here as long as the listing).
#[repr(C)]
struct Dt2List {
    tag: u32,
    /// The firmware's `next` has given the end: later calls give `End` without calling it.
    ended: bool,
    dir_state: [u32; 4],
    out: [u8; 0x114],
    path: [u8; 258],
}

/// Store listing state: the phases of files.rs `Backend` (1 open the catalog, 2 read a page, 3 give the page rows, 4 end), without the DT2 and queue fields.
#[repr(C)]
struct StoreList {
    tag: u32,
    phase: u8,
    virtual_root: bool,
    more: bool,
    directory_len: usize,
    index: usize,
    directory: [u8; 160],
    after: [u8; 160],
    opener: Option<CatalogOpen>,
    cursor: Option<CatalogCursor>,
    collector: Option<DirectoryCollector<8>>,
}

/// Bytes of listing state: the larger of the two route structs, a multiple of 16.
pub(crate) fn list_state_bytes() -> usize {
    let n = core::cmp::max(core::mem::size_of::<Dt2List>(), core::mem::size_of::<StoreList>());
    (n + 15) & !15
}

/// Start listing `dir` into `state` (at least `list_state_bytes()` bytes, 16-byte aligned). The tag is written last, so a failed open leaves no valid state.
pub(crate) unsafe fn list_open(root: u32, f: &Facts, dir: &[u8], state: *mut u8) -> Result<(), i32> {
    let r = prepare(root, f, dir)?;
    if r == Route::Store {
        return store_open(root, dir, state as *mut StoreList);
    }
    if dir.contains(&0) {
        return Err(E_SEL_PATH);
    }
    if dir.len() > 255 {
        return Err(E_TOO_BIG);
    }
    let table = addr_of!(dkrt_files_fs) as *const u32;
    let (open, next, meta) = (read_volatile(table), read_volatile(table.add(1)), read_volatile(table.add(2)));
    if open == 0 || next == 0 || meta == 0 {
        return Err(E_NOT_FOUND);
    }
    let l = state as *mut Dt2List;
    addr_of_mut!((*l).tag).write(0);
    addr_of_mut!((*l).ended).write(false);
    addr_of_mut!((*l).dir_state).write([0; 4]);
    addr_of_mut!((*l).out).write([0; 0x114]);
    addr_of_mut!((*l).path).write([0; 258]);
    let path = &mut (*l).path;
    path[0] = b'/';
    path[1..1 + dir.len()].copy_from_slice(dir);
    let open: DirOpen = core::mem::transmute(open as usize);
    if open(path.as_ptr(), (*l).dir_state.as_mut_ptr()) != 0 {
        return Err(E_NOT_FOUND);
    }
    (*l).tag = TAG_FS;
    Ok(())
}

/// Start a store listing in place (no `StoreList` value is built on the stack; the collector is built by `DirectoryCollector::new` as in files.rs). The
/// directory is the store key of `dir` (`samples/` in front for `ROOT_SAMPLES`).
unsafe fn store_open(root: u32, dir: &[u8], l: *mut StoreList) -> Result<(), i32> {
    let mut key = [0u8; PATH_BYTES];
    let n = store_key(root, dir, &mut key)?;
    addr_of_mut!((*l).tag).write(0);
    addr_of_mut!((*l).phase).write(0);
    addr_of_mut!((*l).virtual_root).write(root == ROOT_SAMPLES && dir.is_empty());
    addr_of_mut!((*l).more).write(false);
    addr_of_mut!((*l).directory_len).write(n);
    addr_of_mut!((*l).index).write(0);
    addr_of_mut!((*l).directory).write([0; 160]);
    addr_of_mut!((*l).after).write([0; 160]);
    addr_of_mut!((*l).opener).write(None);
    addr_of_mut!((*l).cursor).write(None);
    addr_of_mut!((*l).collector).write(None);
    let l = &mut *l;
    l.directory[..n].copy_from_slice(&key[..n]);
    l.collector = Some(DirectoryCollector::new(&l.directory[..n], None)?);
    l.opener = Some(CatalogOpen::begin(&crate::store::reader(), &crate::store::layout())?);
    l.phase = 1;
    l.tag = TAG_STORE;
    Ok(())
}

/// One provider step. DT2: one firmware `next` call; `.` and `..` give `Pending` (as files.rs phase 11); a file's
/// size and hash come from the metadata call by entry id and are set only when it answers (as the old phase 12: no size check against a physical size
/// here, a failed call leaves size 0 and no hash).
pub(crate) unsafe fn list_next(state: *mut u8, out: &mut DirEntry) -> Result<ListStep, i32> {
    let tag = *(state as *const u32);
    if tag == TAG_STORE {
        if !crate::store_writer::try_lock() {
            return Err(E_BUSY);
        }
        let r = store_next(&mut *(state as *mut StoreList), out);
        crate::store_writer::unlock();
        return r;
    }
    if tag != TAG_FS {
        return Err(E_FORMAT);
    }
    let l = &mut *(state as *mut Dt2List);
    if l.ended {
        return Ok(ListStep::End);
    }
    let table = addr_of!(dkrt_files_fs) as *const u32;
    let (next, meta) = (read_volatile(table.add(1)), read_volatile(table.add(2)));
    if next == 0 || meta == 0 {
        return Err(E_NOT_FOUND);
    }
    let next: DirNext = core::mem::transmute(next as usize);
    let meta: FsMeta = core::mem::transmute(meta as usize);
    let buf = &mut l.out;
    {
        buf.fill(0);
        let rc = next(l.dir_state.as_mut_ptr(), buf.as_mut_ptr());
        if rc == 0 {
            l.ended = true;
            return Ok(ListStep::End);
        }
        if rc < 0 {
            return Err(E_READ);
        }
        if rc != 1 {
            return Err(E_BAD_STORE);
        }
        let name = &buf[20..];
        let n = name.iter().position(|b| *b == 0).ok_or(E_BAD_STORE)?;
        if &name[..n] == b"." || &name[..n] == b".." {
            return Ok(ListStep::Pending);
        }
        if n == 0 || name[..n].contains(&b'/') {
            return Err(E_BAD_STORE);
        }
        *out = DirEntry::empty();
        out.name[..n].copy_from_slice(&name[..n]);
        out.name_len = n as u16;
        let kind = u16::from_be_bytes([buf[4], buf[5]]);
        if kind == 1 {
            out.stat = dir_stat();
            return Ok(ListStep::Entry);
        }
        out.stat = Stat { kind: EntryKind::File, size: 0, hash_kind: HASH_NONE, hash_valid: false, hash: [0; 32] };
        let id = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
        let mut values = [0u32; 4];
        if meta(id, values.as_mut_ptr()) == 0 {
            out.stat.size = values[2];
            out.stat.hash_kind = HASH_ELEKTRON32;
            out.stat.hash_valid = true;
            out.stat.hash = fs_hash(&values);
        }
        Ok(ListStep::Entry)
    }
}

/// One store phase of files.rs `Backend::step_inner`, run under the store lock (taken by the caller): open the catalog, read a page and feed the
/// collector, finish, give the page rows one per call, rewind and continue after the last path while `more`. A phase that gives no row returns `Pending`.
/// A virtual `samples` root that does not exist in the store is an empty listing.
unsafe fn store_next(l: &mut StoreList, out: &mut DirEntry) -> Result<ListStep, i32> {
    let (fp, scratch, raw) = store_buffers();
    {
        match l.phase {
            1 => {
                let complete = match l.opener.as_mut().unwrap().step(fp, scratch, 4096) {
                    Err(error) if error == E_NOT_FOUND && l.virtual_root => {
                        l.phase = 4;
                        return Ok(ListStep::End);
                    }
                    result => result?,
                };
                if complete {
                    l.cursor = Some(l.opener.take().unwrap().take()?);
                    l.phase = 2;
                }
                Ok(ListStep::Pending)
            }
            2 => {
                let n = l.cursor.as_mut().unwrap().read_page(fp, scratch, 4096, &mut *raw)?;
                if n != 0 {
                    l.collector.as_mut().unwrap().feed(&(&*raw)[..n])?;
                } else {
                    if let Err(error) = l.collector.as_mut().unwrap().finish(Ok(())) {
                        if error == E_NOT_FOUND && l.virtual_root {
                            l.phase = 4;
                            return Ok(ListStep::End);
                        }
                        return Err(error);
                    }
                    let page = l.collector.as_ref().unwrap().page()?;
                    l.more = page.more;
                    if let Some(last) = page.entries.last() {
                        l.after = last.path;
                    }
                    l.index = 0;
                    l.phase = 3;
                }
                Ok(ListStep::Pending)
            }
            3 => {
                let page = l.collector.as_ref().unwrap().page()?;
                if l.index < page.entries.len() {
                    let value = page.entries[l.index];
                    l.index += 1;
                    let n = dkrt_sdk::store::path_len(value.path.as_ptr());
                    let start = if l.directory_len == 0 { 0 } else { l.directory_len + 1 };
                    if n <= start || n - start > 255 {
                        return Err(E_BAD_STORE);
                    }
                    *out = DirEntry::empty();
                    out.name[..n - start].copy_from_slice(&value.path[start..n]);
                    out.name_len = (n - start) as u16;
                    if value.entry.kind == dkrt_sdk::store::KIND_DIR {
                        out.stat = dir_stat();
                    } else {
                        out.stat = Stat {
                            kind: EntryKind::File,
                            size: value.entry.size,
                            hash_kind: HASH_ELEKTRON32,
                            hash_valid: true,
                            hash: value.entry.sha,
                        };
                    }
                    return Ok(ListStep::Entry);
                }
                if !l.more {
                    l.phase = 4;
                    return Ok(ListStep::End);
                }
                l.cursor.as_mut().unwrap().rewind(fp)?;
                let n = dkrt_sdk::store::path_len(l.after.as_ptr());
                l.collector = Some(DirectoryCollector::new(&l.directory[..l.directory_len], Some(&l.after[..n]))?);
                l.phase = 2;
                Ok(ListStep::Pending)
            }
            4 => Ok(ListStep::End),
            _ => Err(E_FORMAT),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IN_STORE: Facts = Facts { in_store: true, sample_path_max: 150 };
    const ON_FS: Facts = Facts { in_store: false, sample_path_max: 255 };

    #[test]
    fn store_key_prefixes_samples() {
        let mut z = [0xffu8; PATH_BYTES];
        let n = store_key(ROOT_SAMPLES, b"KICKS/K1", &mut z).unwrap();
        assert_eq!(&z[..n], b"samples/KICKS/K1");
        assert_eq!(z[n], 0);
        let n = store_key(ROOT_SAMPLES, b"", &mut z).unwrap();
        assert_eq!(&z[..n], b"samples");
        let n = store_key(ROOT_STORE, b"modules/a.dkm", &mut z).unwrap();
        assert_eq!(&z[..n], b"modules/a.dkm");
    }

    #[test]
    fn store_key_limits_and_nul() {
        let mut z = [0u8; PATH_BYTES];
        assert_eq!(store_key(ROOT_SAMPLES, &[b'a'; 151], &mut z), Ok(159));
        assert_eq!(store_key(ROOT_SAMPLES, &[b'a'; 152], &mut z), Err(E_TOO_BIG));
        assert_eq!(store_key(ROOT_STORE, &[b'a'; 159], &mut z), Ok(159));
        assert_eq!(store_key(ROOT_STORE, &[b'a'; 160], &mut z), Err(E_TOO_BIG));
        assert_eq!(store_key(ROOT_STORE, b"a\0b", &mut z), Err(E_SEL_PATH));
    }

    #[test]
    fn fs_key_is_rooted_and_terminated() {
        let mut z = [0xffu8; 258];
        fs_key(b"KICKS/K1", &mut z).unwrap();
        assert_eq!(&z[..10], b"/KICKS/K1\0");
        assert_eq!(fs_key(b"", &mut z), Err(E_SEL_PATH));
        assert_eq!(fs_key(b"a\0", &mut z), Err(E_SEL_PATH));
        assert!(fs_key(&[b'a'; 255], &mut z).is_ok());
        assert_eq!(fs_key(&[b'a'; 256], &mut z), Err(E_TOO_BIG));
    }

    #[test]
    fn routes_follow_the_profile_facts() {
        assert_eq!(route(ROOT_STORE, &IN_STORE), Ok(Route::Store));
        assert_eq!(route(ROOT_STORE, &ON_FS), Ok(Route::Store));
        assert_eq!(route(ROOT_SAMPLES, &IN_STORE), Ok(Route::Store));
        assert_eq!(route(ROOT_SAMPLES, &ON_FS), Ok(Route::Fs));
        assert_eq!(route(99, &ON_FS), Err(E_FORMAT));
    }

    #[test]
    fn path_limits_per_root() {
        assert_eq!(limit(ROOT_STORE, &IN_STORE), 159);
        assert_eq!(limit(ROOT_SAMPLES, &IN_STORE), 150);
        assert_eq!(limit(99, &IN_STORE), 0);
        assert_eq!(prepare(ROOT_SAMPLES, &IN_STORE, &[b'a'; 150]), Ok(Route::Store));
        assert_eq!(prepare(ROOT_SAMPLES, &IN_STORE, &[b'a'; 151]), Err(E_TOO_BIG));
        assert_eq!(prepare(ROOT_STORE, &IN_STORE, &[b'a'; 160]), Err(E_TOO_BIG));
    }

    #[test]
    fn list_state_covers_both_routes_in_16_byte_units() {
        let n = list_state_bytes();
        assert!(n >= core::mem::size_of::<Dt2List>());
        assert!(n >= core::mem::size_of::<StoreList>());
        assert_eq!(n % 16, 0);
        assert!(core::mem::align_of::<Dt2List>() <= 16 && core::mem::align_of::<StoreList>() <= 16);
    }

    #[test]
    fn an_ended_dt2_listing_gives_end_without_the_firmware() {
        let mut l = Dt2List { tag: TAG_FS, ended: true, dir_state: [0; 4], out: [0; 0x114], path: [0; 258] };
        let mut e = DirEntry::empty();
        let r = unsafe { list_next(&mut l as *mut Dt2List as *mut u8, &mut e) };
        assert!(matches!(r, Ok(ListStep::End)));
    }

    #[test]
    fn fs_hash_is_the_odd_hash_first() {
        let h = fs_hash(&[1, 0x0102_0304, 9, 0]);
        assert_eq!(&h[..4], &[1, 2, 3, 4]);
        assert!(h[4..].iter().all(|b| *b == 0));
    }
}
