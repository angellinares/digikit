//! `FakeStorage` implements `dkrt_abi::storage::Storage` over memory: two roots, each a map of root-relative path to a file or a directory.
//! The runtime crate's `FakePlatform` holds one and delegates to it; `FakeHost` (the platform facts of `FakeCore`) holds one too.

use core::cell::{Cell, RefCell};
use dkrt_abi::storage::*;
use dkrt_sdk::*;
use std::collections::BTreeMap;
use std::vec::Vec;

#[derive(Clone)]
struct Node {
    kind: EntryKind,
    bytes: Vec<u8>,
    hash_kind: u8,
    hash: [u8; 32],
}

/// The listing state this provider keeps in the caller's block: the root, the index of the next child, the directory path and its length.
#[repr(C)]
struct FakeList {
    root: u32,
    next: u32,
    dir_len: u32,
    pended: bool,
    dir: [u8; 256],
}

pub struct FakeStorage {
    roots: [RefCell<BTreeMap<Vec<u8>, Node>>; 2],
    /// When not 0, `list_next` gives one `Pending` before each entry (so caller code can be tested against `Pending`).
    pub pending_every: Cell<u32>,
    /// The longest root-relative path of `ROOT_SAMPLES`: the device profile's `sample_path_max` (`FakePlatform::new` sets it).
    pub samples_path_max: Cell<u32>,
}

impl FakeStorage {
    pub fn new() -> Self {
        FakeStorage {
            roots: [RefCell::new(BTreeMap::new()), RefCell::new(BTreeMap::new())],
            pending_every: Cell::new(0),
            samples_path_max: Cell::new(255),
        }
    }
    fn map(&self, root: u32) -> Result<&RefCell<BTreeMap<Vec<u8>, Node>>, i32> {
        match root {
            ROOT_SAMPLES => Ok(&self.roots[0]),
            ROOT_STORE => Ok(&self.roots[1]),
            _ => Err(E_FORMAT),
        }
    }
    pub fn add_file(&self, root: u32, path: &[u8], bytes: &[u8], hash_kind: u8, hash: [u8; 32]) {
        let node = Node {
            kind: EntryKind::File,
            bytes: bytes.to_vec(),
            hash_kind,
            hash,
        };
        self.map(root)
            .unwrap()
            .borrow_mut()
            .insert(path.to_vec(), node);
    }
    pub fn add_dir(&self, root: u32, path: &[u8]) {
        let node = Node {
            kind: EntryKind::Dir,
            bytes: Vec::new(),
            hash_kind: HASH_NONE,
            hash: [0; 32],
        };
        self.map(root)
            .unwrap()
            .borrow_mut()
            .insert(path.to_vec(), node);
    }
    /// The root and the path rules every call shares, in the real adapter's order: an unknown root is `E_FORMAT`, a path over the root's limit `E_TOO_BIG`,
    /// a path with a NUL `E_SEL_PATH`.
    fn check(&self, root: u32, path: &[u8]) -> Result<(), i32> {
        self.map(root)?;
        if path.len() > self.path_max(root) as usize {
            return Err(E_TOO_BIG);
        }
        if path.contains(&0) {
            return Err(E_SEL_PATH);
        }
        Ok(())
    }
    /// The file at `path` (not the root, not a directory), for the calls that read it: an empty path is `E_SEL_PATH`, a missing path or a directory
    /// `E_NOT_FOUND`.
    fn file(&self, root: u32, path: &[u8]) -> Result<Node, i32> {
        self.check(root, path)?;
        if path.is_empty() {
            return Err(E_SEL_PATH);
        }
        self.found(root, path)
    }
    fn found(&self, root: u32, path: &[u8]) -> Result<Node, i32> {
        match self.map(root)?.borrow().get(path) {
            Some(n) if n.kind == EntryKind::File => Ok(n.clone()),
            _ => Err(E_NOT_FOUND),
        }
    }
    /// The first 64 bytes of `n`, zero padded; a file under 64 bytes is `E_READ`.
    fn head(n: &Node, out: &mut [u8; 64]) -> Result<(), i32> {
        if n.bytes.len() < 64 {
            return Err(E_READ);
        }
        out.copy_from_slice(&n.bytes[..64]);
        Ok(())
    }
    fn dir_stat() -> Stat {
        Stat {
            kind: EntryKind::Dir,
            size: 0,
            hash_kind: HASH_NONE,
            hash_valid: false,
            hash: [0; 32],
        }
    }
    fn stat_of(n: &Node) -> Stat {
        Stat {
            kind: n.kind,
            size: n.bytes.len() as u32,
            hash_kind: n.hash_kind,
            hash_valid: n.hash_kind != HASH_NONE,
            hash: n.hash,
        }
    }
}

/// The name of `path` when it is a direct child of `dir` (empty `dir`: the root).
fn child_name<'a>(dir: &[u8], path: &'a [u8]) -> Option<&'a [u8]> {
    let rest = if dir.is_empty() {
        path
    } else {
        if path.len() <= dir.len() + 1 || !path.starts_with(dir) || path[dir.len()] != b'/' {
            return None;
        }
        &path[dir.len() + 1..]
    };
    if rest.is_empty() || rest.contains(&b'/') {
        return None;
    }
    Some(rest)
}

impl Storage for FakeStorage {
    fn path_max(&self, root: u32) -> u32 {
        match root {
            ROOT_STORE => dkrt_sdk::store::PATH_MAX as u32,
            ROOT_SAMPLES => self.samples_path_max.get(),
            _ => 0,
        }
    }
    unsafe fn stat(&self, root: u32, path: &[u8]) -> Result<Stat, i32> {
        self.check(root, path)?;
        if path.is_empty() {
            return Ok(Self::dir_stat());
        }
        Ok(Self::stat_of(&self.found(root, path)?))
    }
    unsafe fn read_head(&self, root: u32, path: &[u8], out: &mut [u8; 64]) -> Result<(), i32> {
        Self::head(&self.file(root, path)?, out)
    }
    unsafe fn stat_head(
        &self,
        root: u32,
        path: &[u8],
        out: &mut [u8; 64],
    ) -> Result<(Stat, usize), i32> {
        self.check(root, path)?;
        if path.is_empty() {
            return Ok((Self::dir_stat(), 0));
        }
        let n = self.found(root, path)?;
        Self::head(&n, out)?;
        Ok((Self::stat_of(&n), 64))
    }
    unsafe fn read_range(
        &self,
        root: u32,
        path: &[u8],
        offset: u32,
        out: &mut [u8],
    ) -> Result<usize, i32> {
        let n = self.file(root, path)?;
        let start = (offset as usize).min(n.bytes.len());
        let k = out.len().min(n.bytes.len() - start);
        out[..k].copy_from_slice(&n.bytes[start..start + k]);
        Ok(k)
    }
    unsafe fn read_all(&self, root: u32, path: &[u8], dst: *mut u8, cap: u32) -> Result<u32, i32> {
        let n = self.file(root, path)?;
        if n.bytes.len() > cap as usize {
            return Err(E_TOO_BIG);
        }
        core::ptr::copy_nonoverlapping(n.bytes.as_ptr(), dst, n.bytes.len());
        Ok(n.bytes.len() as u32)
    }
    fn list_state_bytes(&self, _root: u32) -> usize {
        (core::mem::size_of::<FakeList>() + 15) & !15
    }
    unsafe fn list_open(&self, root: u32, dir: &[u8], state: *mut u8) -> Result<(), i32> {
        self.check(root, dir)?;
        if !dir.is_empty()
            && !matches!(self.map(root)?.borrow().get(dir), Some(n) if n.kind == EntryKind::Dir)
        {
            return Err(E_NOT_FOUND);
        }
        let mut copy = [0u8; 256];
        copy[..dir.len()].copy_from_slice(dir);
        core::ptr::write(
            state as *mut FakeList,
            FakeList {
                root,
                next: 0,
                dir_len: dir.len() as u32,
                pended: false,
                dir: copy,
            },
        );
        Ok(())
    }
    unsafe fn list_next(&self, state: *mut u8, out: &mut DirEntry) -> Result<ListStep, i32> {
        let w = &mut *(state as *mut FakeList);
        let dir: Vec<u8> = w.dir[..w.dir_len as usize].to_vec();
        let map = self.map(w.root)?.borrow();
        let found = map
            .iter()
            .filter_map(|(p, n)| child_name(&dir, p).map(|name| (name, n)))
            .nth(w.next as usize);
        match found {
            None => Ok(ListStep::End),
            Some((name, n)) => {
                let every = self.pending_every.get();
                if every != 0 && !w.pended {
                    w.pended = true;
                    return Ok(ListStep::Pending);
                }
                w.pended = false;
                w.next += 1;
                out.name = [0; 256];
                out.name[..name.len()].copy_from_slice(name);
                out.name_len = name.len() as u16;
                out.stat = Self::stat_of(n);
                Ok(ListStep::Entry)
            }
        }
    }
}
