//! Host tests of `Storage`. `FakeStorage` implements `dkrt_abi::storage::Storage` over memory: two roots, each a map of root-relative path to a file or a
//! directory. `FakePlatform` holds one and delegates to it.

use core::cell::RefCell;
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

/// The words of a `ListCursor` this provider uses: the root, the index of the next child, the length of the directory path, then the path one byte per word.
const W_ROOT: usize = 0;
const W_NEXT: usize = 1;
const W_DIR_LEN: usize = 2;
const W_DIR: usize = 3;

pub struct FakeStorage {
    roots: [RefCell<BTreeMap<Vec<u8>, Node>>; 2],
}

impl FakeStorage {
    pub fn new() -> Self {
        FakeStorage { roots: [RefCell::new(BTreeMap::new()), RefCell::new(BTreeMap::new())] }
    }
    fn map(&self, root: u32) -> Result<&RefCell<BTreeMap<Vec<u8>, Node>>, i32> {
        match root {
            ROOT_SAMPLES => Ok(&self.roots[0]),
            ROOT_STORE => Ok(&self.roots[1]),
            _ => Err(E_FORMAT),
        }
    }
    pub fn add_file(&self, root: u32, path: &[u8], bytes: &[u8], hash_kind: u8, hash: [u8; 32]) {
        let node = Node { kind: EntryKind::File, bytes: bytes.to_vec(), hash_kind, hash };
        self.map(root).unwrap().borrow_mut().insert(path.to_vec(), node);
    }
    pub fn add_dir(&self, root: u32, path: &[u8]) {
        let node = Node { kind: EntryKind::Dir, bytes: Vec::new(), hash_kind: HASH_NONE, hash: [0; 32] };
        self.map(root).unwrap().borrow_mut().insert(path.to_vec(), node);
    }
    fn node(&self, root: u32, path: &[u8]) -> Result<Node, i32> {
        self.map(root)?.borrow().get(path).cloned().ok_or(E_NOT_FOUND)
    }
    fn file(&self, root: u32, path: &[u8]) -> Result<Node, i32> {
        let n = self.node(root, path)?;
        if n.kind == EntryKind::Dir {
            return Err(E_FORMAT);
        }
        Ok(n)
    }
    fn stat_of(n: &Node) -> Stat {
        Stat { kind: n.kind, size: n.bytes.len() as u32, hash_kind: n.hash_kind, hash_valid: n.hash_kind != HASH_NONE, hash: n.hash }
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
            _ => 0,
        }
    }
    unsafe fn stat(&self, root: u32, path: &[u8]) -> Result<Stat, i32> {
        if path.is_empty() {
            self.map(root)?;
            return Ok(Stat { kind: EntryKind::Dir, size: 0, hash_kind: HASH_NONE, hash_valid: false, hash: [0; 32] });
        }
        Ok(Self::stat_of(&self.node(root, path)?))
    }
    unsafe fn read_head(&self, root: u32, path: &[u8], out: &mut [u8; 64]) -> Result<(), i32> {
        let n = self.file(root, path)?;
        out.fill(0);
        let k = n.bytes.len().min(64);
        out[..k].copy_from_slice(&n.bytes[..k]);
        Ok(())
    }
    unsafe fn read_range(&self, root: u32, path: &[u8], offset: u32, out: &mut [u8]) -> Result<usize, i32> {
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
    unsafe fn list_open(&self, root: u32, dir: &[u8], cur: *mut ListCursor) -> Result<(), i32> {
        if dir.len() > LIST_CURSOR_WORDS - W_DIR {
            return Err(E_TOO_BIG);
        }
        if !dir.is_empty() && self.node(root, dir)?.kind != EntryKind::Dir {
            return Err(E_FORMAT);
        }
        self.map(root)?;
        let w = &mut (*cur).words;
        w[W_ROOT] = root;
        w[W_NEXT] = 0;
        w[W_DIR_LEN] = dir.len() as u32;
        for (i, b) in dir.iter().enumerate() {
            w[W_DIR + i] = *b as u32;
        }
        Ok(())
    }
    unsafe fn list_next(&self, cur: *mut ListCursor, out: &mut DirEntry) -> Result<bool, i32> {
        let w = &mut (*cur).words;
        let dir: Vec<u8> = (0..w[W_DIR_LEN] as usize).map(|i| w[W_DIR + i] as u8).collect();
        let map = self.map(w[W_ROOT])?.borrow();
        let found = map.iter().filter_map(|(p, n)| child_name(&dir, p).map(|name| (name, n))).nth(w[W_NEXT] as usize);
        match found {
            None => Ok(false),
            Some((name, n)) => {
                w[W_NEXT] += 1;
                out.name = [0; 256];
                out.name[..name.len()].copy_from_slice(name);
                out.name_len = name.len() as u16;
                out.stat = Self::stat_of(n);
                Ok(true)
            }
        }
    }
}

const H: [u8; 32] = [7; 32];

fn sample_store() -> FakeStorage {
    let s = FakeStorage::new();
    s.add_dir(ROOT_SAMPLES, b"KICKS");
    s.add_file(ROOT_SAMPLES, b"KICKS/K1", b"0123456789", HASH_ELEKTRON32, H);
    s.add_file(ROOT_SAMPLES, b"KICKS/K2", &[9u8; 100], HASH_NONE, [0; 32]);
    s.add_dir(ROOT_SAMPLES, b"KICKS/SUB");
    s.add_file(ROOT_SAMPLES, b"KICKS/SUB/deep", b"x", HASH_NONE, [0; 32]);
    s.add_dir(ROOT_SAMPLES, b"EMPTY");
    s.add_dir(ROOT_STORE, b"samples");
    s.add_file(ROOT_STORE, b"samples/a", b"abc", HASH_ELEKTRON32, H);
    s
}

fn list(s: &FakeStorage, root: u32, dir: &[u8]) -> Vec<(Vec<u8>, Stat)> {
    let mut cur = ListCursor::new();
    let mut e = DirEntry::empty();
    let mut rows = Vec::new();
    unsafe {
        s.list_open(root, dir, &mut cur).unwrap();
        while s.list_next(&mut cur, &mut e).unwrap() {
            rows.push((e.name().to_vec(), e.stat));
        }
    }
    rows
}

#[test]
fn path_max_follows_the_profile() {
    use crate::fake_platform::FakePlatform;
    use dkrt_abi::profiles;
    for p in profiles::ALL {
        let f = FakePlatform::new(p);
        assert_eq!(f.path_max(ROOT_SAMPLES), p.sample_path_max, "{p:?}");
        assert_eq!(f.path_max(ROOT_STORE), dkrt_sdk::store::PATH_MAX as u32, "{p:?}");
        assert_eq!(f.path_max(99), 0);
    }
}

#[test]
fn stat_of_a_file_a_dir_the_root_and_a_missing_path() {
    let s = sample_store();
    unsafe {
        let f = s.stat(ROOT_SAMPLES, b"KICKS/K1").unwrap();
        assert_eq!((f.kind, f.size, f.hash_kind, f.hash_valid, f.hash), (EntryKind::File, 10, HASH_ELEKTRON32, true, H));
        let g = s.stat(ROOT_SAMPLES, b"KICKS/K2").unwrap();
        assert_eq!((g.size, g.hash_valid), (100, false));
        assert_eq!(s.stat(ROOT_SAMPLES, b"KICKS").unwrap().kind, EntryKind::Dir);
        assert_eq!(s.stat(ROOT_STORE, b"").unwrap().kind, EntryKind::Dir);
        assert_eq!(s.stat(ROOT_SAMPLES, b"KICKS/none"), Err(E_NOT_FOUND));
        assert_eq!(s.stat(ROOT_STORE, b"KICKS/K1"), Err(E_NOT_FOUND));
    }
}

#[test]
fn read_head_of_a_short_file_pads_with_zeros() {
    let s = sample_store();
    let mut out = [0xaa; 64];
    unsafe {
        s.read_head(ROOT_SAMPLES, b"KICKS/K1", &mut out).unwrap();
        assert_eq!(&out[..10], b"0123456789");
        assert!(out[10..].iter().all(|b| *b == 0));
        assert_eq!(s.read_head(ROOT_SAMPLES, b"KICKS/none", &mut out), Err(E_NOT_FOUND));
    }
}

#[test]
fn read_range_inside_at_the_end_and_past_the_end() {
    let s = sample_store();
    let mut out = [0u8; 4];
    unsafe {
        assert_eq!(s.read_range(ROOT_SAMPLES, b"KICKS/K1", 2, &mut out), Ok(4));
        assert_eq!(&out, b"2345");
        assert_eq!(s.read_range(ROOT_SAMPLES, b"KICKS/K1", 8, &mut out), Ok(2));
        assert_eq!(&out[..2], b"89");
        assert_eq!(s.read_range(ROOT_SAMPLES, b"KICKS/K1", 10, &mut out), Ok(0));
        assert_eq!(s.read_range(ROOT_SAMPLES, b"KICKS/K1", 500, &mut out), Ok(0));
    }
}

#[test]
fn read_all_with_enough_and_with_too_small_a_buffer() {
    let s = sample_store();
    let mut buf = [0u8; 16];
    unsafe {
        assert_eq!(s.read_all(ROOT_SAMPLES, b"KICKS/K1", buf.as_mut_ptr(), 16), Ok(10));
        assert_eq!(&buf[..10], b"0123456789");
        let mut small = [0xeeu8; 4];
        assert_eq!(s.read_all(ROOT_SAMPLES, b"KICKS/K1", small.as_mut_ptr(), 4), Err(E_TOO_BIG));
        assert_eq!(small, [0xee; 4]);
    }
}

#[test]
fn list_gives_the_children_in_path_order_then_the_end() {
    let s = sample_store();
    let rows = list(&s, ROOT_SAMPLES, b"KICKS");
    let names: Vec<&[u8]> = rows.iter().map(|r| r.0.as_slice()).collect();
    assert_eq!(names, [b"K1".as_slice(), b"K2", b"SUB"]);
    assert_eq!(rows[0].1.size, 10);
    assert_eq!(rows[2].1.kind, EntryKind::Dir);
    let top: Vec<Vec<u8>> = list(&s, ROOT_SAMPLES, b"").into_iter().map(|r| r.0).collect();
    assert_eq!(top, [b"EMPTY".to_vec(), b"KICKS".to_vec()]);
    // the end stays the end
    let mut cur = ListCursor::new();
    let mut e = DirEntry::empty();
    unsafe {
        s.list_open(ROOT_SAMPLES, b"EMPTY", &mut cur).unwrap();
        assert_eq!(s.list_next(&mut cur, &mut e), Ok(false));
        assert_eq!(s.list_next(&mut cur, &mut e), Ok(false));
        assert_eq!(s.list_open(ROOT_SAMPLES, b"KICKS/K1", &mut cur), Err(E_FORMAT));
        assert_eq!(s.list_open(ROOT_SAMPLES, b"NOPE", &mut cur), Err(E_NOT_FOUND));
    }
}

#[test]
fn listing_an_empty_dir_is_empty() {
    let s = sample_store();
    assert!(list(&s, ROOT_SAMPLES, b"EMPTY").is_empty());
    assert!(list(&s, ROOT_STORE, b"").len() == 1);
}
