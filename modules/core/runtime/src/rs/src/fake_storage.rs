//! Host tests of `Storage`. `FakeStorage` implements `dkrt_abi::storage::Storage` over memory: two roots, each a map of root-relative path to a file or a
//! directory. `FakePlatform` holds one and delegates to it.

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
        FakeStorage { roots: [RefCell::new(BTreeMap::new()), RefCell::new(BTreeMap::new())], pending_every: Cell::new(0), samples_path_max: Cell::new(255) }
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
        Stat { kind: EntryKind::Dir, size: 0, hash_kind: HASH_NONE, hash_valid: false, hash: [0; 32] }
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
    unsafe fn stat_head(&self, root: u32, path: &[u8], out: &mut [u8; 64]) -> Result<(Stat, usize), i32> {
        self.check(root, path)?;
        if path.is_empty() {
            return Ok((Self::dir_stat(), 0));
        }
        let n = self.found(root, path)?;
        Self::head(&n, out)?;
        Ok((Self::stat_of(&n), 64))
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
    fn list_state_bytes(&self, _root: u32) -> usize {
        (core::mem::size_of::<FakeList>() + 15) & !15
    }
    unsafe fn list_open(&self, root: u32, dir: &[u8], state: *mut u8) -> Result<(), i32> {
        self.check(root, dir)?;
        if !dir.is_empty() && !matches!(self.map(root)?.borrow().get(dir), Some(n) if n.kind == EntryKind::Dir) {
            return Err(E_NOT_FOUND);
        }
        let mut copy = [0u8; 256];
        copy[..dir.len()].copy_from_slice(dir);
        core::ptr::write(state as *mut FakeList, FakeList { root, next: 0, dir_len: dir.len() as u32, pended: false, dir: copy });
        Ok(())
    }
    unsafe fn list_next(&self, state: *mut u8, out: &mut DirEntry) -> Result<ListStep, i32> {
        let w = &mut *(state as *mut FakeList);
        let dir: Vec<u8> = w.dir[..w.dir_len as usize].to_vec();
        let map = self.map(w.root)?.borrow();
        let found = map.iter().filter_map(|(p, n)| child_name(&dir, p).map(|name| (name, n))).nth(w.next as usize);
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

/// A 16-byte aligned block as the arena gives it.
#[repr(C, align(16))]
struct Block([u8; 512]);

const H: [u8; 32] = [7; 32];

fn list(s: &FakeStorage, root: u32, dir: &[u8]) -> Result<Vec<(Vec<u8>, Stat)>, i32> {
    let mut block = Block([0; 512]);
    assert!(s.list_state_bytes(root) <= block.0.len());
    let cur = block.0.as_mut_ptr();
    let mut e = DirEntry::empty();
    let mut rows = Vec::new();
    unsafe {
        s.list_open(root, dir, cur)?;
        loop {
            match s.list_next(cur, &mut e)? {
                ListStep::Entry => rows.push((e.name().to_vec(), e.stat)),
                ListStep::Pending => {}
                ListStep::End => break,
            }
        }
    }
    Ok(rows)
}

// ---- the contract table (tests/storage/contract.toml) ------------------------------------------------------------------------

/// One `[[file]]` or `[[case]]` of the table: its `key = value` lines.
type Row = BTreeMap<String, String>;

/// The rows of the table: the files and the cases. The format is line based: a header line `[[file]]` or `[[case]]`, then `key = value` lines.
fn rows(table: &str) -> (Vec<Row>, Vec<Row>) {
    let (mut files, mut cases) = (Vec::new(), Vec::new());
    let mut current: Option<(bool, Row)> = None;
    let mut done = |c: &mut Option<(bool, Row)>| {
        if let Some((is_file, row)) = c.take() {
            if is_file { files.push(row) } else { cases.push(row) }
        }
    };
    for line in table.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        if line == "[[file]]" || line == "[[case]]" {
            done(&mut current);
            current = Some((line == "[[file]]", Row::new()));
        } else if let (Some((_, row)), Some((k, v))) = (current.as_mut(), line.split_once('=')) {
            row.insert(k.trim().to_string(), v.trim().trim_matches('"').to_string());
        }
    }
    done(&mut current);
    (files, cases)
}

fn root_of(name: &str) -> u32 {
    match name {
        "samples" => ROOT_SAMPLES,
        "store" => ROOT_STORE,
        "other" => 99,
        _ => panic!("root {name}"),
    }
}

/// The ramp every file of the table holds.
fn ramp(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i * 31 + 7) as u8).collect()
}

/// The tree of the table in a `FakeStorage` whose samples limit is `samples_max`.
fn tree(files: &[Row], samples_max: u32) -> FakeStorage {
    let s = FakeStorage::new();
    s.samples_path_max.set(samples_max);
    for f in files {
        let root = root_of(&f["root"]);
        let path = f["path"].as_bytes();
        for (at, _) in path.iter().enumerate().filter(|(_, b)| **b == b'/') {
            s.add_dir(root, &path[..at]);
        }
        s.add_file(root, path, &ramp(f["size"].parse().unwrap()), HASH_ELEKTRON32, H);
    }
    s
}

/// The path of a case: `path`, `path_len` (N times "a", or the root's limit `max` and `max+1`) or `path_nul`.
fn path_of(case: &Row, limit: u32) -> Vec<u8> {
    if let Some(n) = case.get("path_len") {
        let n = match n.as_str() {
            "max" => limit as usize,
            "max+1" => limit as usize + 1,
            n => n.parse().unwrap(),
        };
        return std::vec![b'a'; n];
    }
    if case.contains_key("path_nul") {
        return b"a\0b".to_vec();
    }
    case.get("path").map_or_else(Vec::new, |p| p.as_bytes().to_vec())
}

fn err_name(e: i32) -> String {
    for (name, code) in [("E_NOT_FOUND", E_NOT_FOUND), ("E_FORMAT", E_FORMAT), ("E_READ", E_READ), ("E_TOO_BIG", E_TOO_BIG), ("E_SEL_PATH", E_SEL_PATH)] {
        if e == code {
            return name.to_string();
        }
    }
    std::format!("error {e}")
}

fn entry_text(name: &[u8], st: &Stat) -> String {
    let name = String::from_utf8_lossy(name);
    match st.kind {
        EntryKind::Dir => std::format!("{name}:dir"),
        EntryKind::File => std::format!("{name}:file:{}", st.size),
    }
}

/// What `case` gives on `s`, in the words of the table's `expect`.
fn answer(s: &FakeStorage, case: &Row) -> String {
    let root = root_of(&case["root"]);
    let limit = s.path_max(root);
    let path = path_of(case, limit);
    let num = |k: &str| case[k].parse::<u32>().unwrap();
    let kind = |st: &Stat| match st.kind {
        EntryKind::Dir => "dir".to_string(),
        EntryKind::File => std::format!("file size={}", st.size),
    };
    let mut out = [0u8; 64];
    unsafe {
        match case["call"].as_str() {
            "path_max" => s.path_max(root).to_string(),
            "stat" => s.stat(root, &path).map_or_else(err_name, |st| kind(&st)),
            "read_head" => match s.read_head(root, &path, &mut out) {
                Ok(()) => {
                    assert_eq!(out.to_vec(), ramp(64), "{}: the head is the ramp", case["id"]);
                    "ok".to_string()
                }
                Err(e) => err_name(e),
            },
            "stat_head" => s.stat_head(root, &path, &mut out).map_or_else(err_name, |(st, n)| {
                if n == 64 {
                    assert_eq!(out.to_vec(), ramp(64), "{}: the head is the ramp", case["id"]);
                }
                std::format!("{} head={n}", kind(&st))
            }),
            "read_range" => {
                let mut buf = std::vec![0xaau8; num("len") as usize];
                s.read_range(root, &path, num("offset"), &mut buf).map_or_else(err_name, |n| {
                    assert_eq!(buf[..n], ramp(num("offset") as usize + n)[num("offset") as usize..], "{}: the bytes are the ramp", case["id"]);
                    std::format!("n={n}")
                })
            }
            "read_all" => {
                let cap = num("cap") as usize;
                let mut buf = std::vec![0xeeu8; cap];
                let r = s.read_all(root, &path, buf.as_mut_ptr(), cap as u32);
                match r {
                    Ok(n) => {
                        assert_eq!(buf[..n as usize], ramp(n as usize), "{}: the bytes are the ramp", case["id"]);
                        std::format!("size={n}")
                    }
                    Err(e) => {
                        assert!(buf.iter().all(|b| *b == 0xee), "{}: an error writes nothing", case["id"]);
                        err_name(e)
                    }
                }
            }
            "list" => match list(s, root, &path) {
                Err(e) => err_name(e),
                Ok(found) => {
                    let mut got: Vec<String> = found.iter().map(|(n, st)| entry_text(n, st)).collect();
                    got.sort();
                    let want = |k: &str| {
                        let mut w: Vec<String> = case[k].split(',').map(str::to_string).collect();
                        w.sort();
                        w
                    };
                    if let Some(_) = case.get("entries") {
                        assert_eq!(got, want("entries"), "{}", case["id"]);
                    } else {
                        for w in want("includes") {
                            assert!(got.contains(&w), "{}: {w} in {got:?}", case["id"]);
                        }
                    }
                    "list".to_string()
                }
            },
            call => panic!("call {call}"),
        }
    }
}

/// Every row of the contract table holds for `FakeStorage`, with the samples limit of each device profile. The device side runs the `device = "list"` rows
/// on the real adapter (`storage.device.test.ts`).
#[test]
fn the_contract_table_holds_for_the_fake() {
    use dkrt_abi::profiles;
    let (files, cases) = rows(include_str!("../../../tests/storage/contract.toml"));
    assert!(cases.len() >= 40 && files.len() >= 5);
    let mut ids = std::vec::Vec::new();
    for case in &cases {
        let id = &case["id"];
        assert!(!ids.contains(id), "{id}: twice");
        ids.push(id.clone());
        assert!(case.contains_key("what"), "{id}: says what it checks");
        assert!(case.contains_key("device") != case.contains_key("device_pending"), "{id}: a device check or a reason, not both");
        if let Some(d) = case.get("device") {
            assert_eq!(d, "list", "{id}: the device runs listings");
            assert_eq!(case["call"], "list", "{id}: a device row is a listing");
        }
    }
    for p in profiles::ALL {
        let s = tree(&files, p.sample_path_max);
        for case in &cases {
            let want = &case["expect"];
            let got = answer(&s, case);
            let want = if want == "profile" { p.sample_path_max.to_string() } else { want.clone() };
            assert_eq!(got.split_whitespace().next(), want.split_whitespace().next(), "{} on {p:?}", case["id"]);
            if !want.starts_with("list") {
                assert_eq!(got, want, "{} on {p:?}", case["id"]);
            }
        }
    }
}

#[test]
fn the_platform_path_max_is_the_profile_limit() {
    use crate::fake_platform::FakePlatform;
    use dkrt_abi::profiles;
    for p in profiles::ALL {
        let f = FakePlatform::new(p);
        assert_eq!(f.path_max(ROOT_SAMPLES), p.sample_path_max, "{p:?}");
        assert_eq!(f.path_max(ROOT_STORE), dkrt_sdk::store::PATH_MAX as u32, "{p:?}");
    }
}

#[test]
fn list_with_pending_gives_pending_before_each_entry() {
    let s = FakeStorage::new();
    s.add_dir(ROOT_SAMPLES, b"EMPTY");
    s.pending_every.set(1);
    let mut block = Block([0; 512]);
    let cur = block.0.as_mut_ptr();
    let mut e = DirEntry::empty();
    unsafe {
        s.list_open(ROOT_SAMPLES, b"EMPTY", cur).unwrap();
        assert_eq!(s.list_next(cur, &mut e), Ok(ListStep::End));
        s.add_file(ROOT_SAMPLES, b"EMPTY/a", b"1", HASH_NONE, [0; 32]);
        s.add_file(ROOT_SAMPLES, b"EMPTY/b", b"2", HASH_NONE, [0; 32]);
        s.list_open(ROOT_SAMPLES, b"EMPTY", cur).unwrap();
        let steps: Vec<ListStep> = (0..5).map(|_| s.list_next(cur, &mut e).unwrap()).collect();
        assert_eq!(steps, [ListStep::Pending, ListStep::Entry, ListStep::Pending, ListStep::Entry, ListStep::End]);
        assert_eq!(e.name(), b"b");
        // the end stays the end
        assert_eq!(s.list_next(cur, &mut e), Ok(ListStep::End));
    }
}
