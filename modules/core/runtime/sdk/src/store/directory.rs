//! Pure immediate-child projection from a complete generic catalogue scan.
//! Retains the smallest N unique child paths after an exclusive continuation.
//! No I/O, heap, implicit scan, sample filtering, or content validation occurs.
//! Any error invalidates staged output; callers must discard it.
//! Every page requires another complete scan: O(table size) input work per page,
//! with O(N) retained entries. Continuation pages must replay the same valid
//! snapshot; the caller owns generation consistency across pages too. This is
//! not a measured latency/speed guarantee.
use super::catalog::CatalogEntry;
use super::*;

const DIRECTORY: Entry = Entry { first: 0, size: 0, kind: KIND_DIR, sha: [0; 32] };
const EMPTY: CatalogEntry = CatalogEntry { path: [0; PATH_BYTES], entry: DIRECTORY };

/// A completed page, sorted by full path bytes. Directories have canonical zero
/// metadata; files retain their exact Entry, including SHA (not validated here).
pub struct DirectoryPage<'a> {
    pub entries: &'a [CatalogEntry],
    /// More distinct matching children exist after this page, not just more raw
    /// rows. Use its last full path as the next exclusive continuation key.
    pub more: bool,
}

/// Owns input paths and fixed page storage; never retains caller input buffers.
/// Conflict checks cover retained selected children only, not the whole table.
/// Rejected/evicted larger keys cannot reenter as the retained cutoff decreases.
/// All malformed paths/kinds and requested-path/ancestor-file errors are global.
pub struct DirectoryCollector<const N: usize> {
    directory: [u8; PATH_BYTES],
    directory_len: usize,
    after: [u8; PATH_BYTES],
    entries: [CatalogEntry; N],
    len: usize,
    exists: bool,
    more: bool,
    error: i32,
    finished: bool,
}

/// Non-NUL byte slices at the boundary; reuse store path rules on an owned,
/// terminated copy so embedded NUL cannot silently shorten an input path.
fn owned_path(path: &[u8], root: bool) -> Result<[u8; PATH_BYTES], i32> {
    if path.len() > PATH_MAX || path.contains(&0) {
        return Err(E_BAD_STORE);
    }
    let mut out = [0; PATH_BYTES];
    out[..path.len()].copy_from_slice(path);
    if (path.is_empty() && root) || unsafe { path_ok(out.as_ptr(), PATH_BYTES) } == Some(path.len()) {
        Ok(out)
    } else {
        Err(E_BAD_STORE)
    }
}

impl<const N: usize> DirectoryCollector<N> {
    /// Canonical non-NUL path bytes; empty directory means the always-existing
    /// root. `after`, if present, must be a full immediate-child path in this
    /// directory; it need not still exist. There is no virtual mount/root flag.
    pub fn new(directory: &[u8], after: Option<&[u8]>) -> Result<Self, i32> {
        if N == 0 {
            return Err(E_BAD_STORE);
        }
        let path = owned_path(directory, true)?;
        let mut key = [0; PATH_BYTES];
        if let Some(after) = after {
            key = owned_path(after, false)?;
            let start = if directory.is_empty() { 0 } else { directory.len() + 1 };
            if after.len() <= start
                || (!directory.is_empty() && (!after.starts_with(directory) || after[directory.len()] != b'/'))
                || after[start..].contains(&b'/')
            {
                return Err(E_BAD_STORE);
            }
        }
        Ok(Self {
            directory: path,
            directory_len: directory.len(),
            after: key,
            entries: [EMPTY; N],
            len: 0,
            exists: directory.is_empty(),
            more: false,
            error: 0,
            finished: false,
        })
    }

    fn fail<T>(&mut self, error: i32) -> Result<T, i32> {
        self.error = error;
        Err(error)
    }

    fn retain(&mut self, child: CatalogEntry) -> Result<(), i32> {
        if child.path <= self.after {
            return Ok(());
        }
        let mut at = 0;
        while at < self.len && self.entries[at].path < child.path {
            at += 1;
        }
        if at < self.len && self.entries[at].path == child.path {
            if self.entries[at].entry != child.entry {
                return self.fail(E_BAD_STORE);
            }
            return Ok(());
        }
        if self.len == N {
            self.more = true;
            if at == N {
                return Ok(());
            }
        } else {
            self.len += 1;
        }
        let mut i = self.len - 1;
        while i > at {
            self.entries[i] = self.entries[i - 1];
            i -= 1;
        }
        self.entries[at] = child;
        Ok(())
    }

    /// Feed any owned catalogue chunk, in any order. Empty input is progress,
    /// never EOF. Errors are sticky and invalidate all retained output. Files at
    /// the requested directory or any ancestor return E_NOT_FOUND. Only selected
    /// child conflicts are audited; malformed paths/kinds are rejected globally.
    pub fn feed(&mut self, entries: &[CatalogEntry]) -> Result<(), i32> {
        if self.error != 0 {
            return Err(self.error);
        }
        if self.finished {
            return self.fail(crate::E_FORMAT);
        }
        for value in entries {
            let n = match unsafe { path_ok(value.path.as_ptr(), PATH_BYTES) } {
                Some(n) => n,
                None => return self.fail(E_BAD_STORE),
            };
            if value.entry.kind != KIND_FILE && value.entry.kind != KIND_DIR {
                return self.fail(E_BAD_STORE);
            }
            let path = &value.path[..n];
            let directory = &self.directory[..self.directory_len];
            if path == directory {
                if value.entry.kind == KIND_FILE {
                    return self.fail(E_NOT_FOUND);
                }
                self.exists = true;
                continue;
            }
            if value.entry.kind == KIND_FILE && directory.starts_with(path) && directory.get(n) == Some(&b'/') {
                return self.fail(E_NOT_FOUND);
            }
            let start = if directory.is_empty() {
                0
            } else {
                if !path.starts_with(directory) || path.get(directory.len()) != Some(&b'/') {
                    continue;
                }
                directory.len() + 1
            };
            self.exists = true;
            let mut end = start;
            while end < n && path[end] != b'/' {
                end += 1;
            }
            let mut child = EMPTY;
            child.path[..end].copy_from_slice(&path[..end]);
            if end == n && value.entry.kind == KIND_FILE {
                child.entry = value.entry;
            }
            self.retain(child)?;
        }
        Ok(())
    }

    /// `Ok(())` explicitly attests that the caller fed the ENTIRE raw scan to EOF
    /// and validated its generation. This pure collector cannot prove either;
    /// use the cursor's actual result, not an empty filtered chunk, as completion.
    /// Pass any scan/generation failure here to invalidate the page permanently.
    /// No SHA/PCM validation, lease, READY state, or publication follows.
    pub fn finish(&mut self, scan_result: Result<(), i32>) -> Result<(), i32> {
        if self.error != 0 {
            return Err(self.error);
        }
        if let Err(error) = scan_result {
            return self.fail(if error == OK { E_BAD_STORE } else { error });
        }
        if !self.exists {
            return self.fail(E_NOT_FOUND);
        }
        self.finished = true;
        Ok(())
    }

    /// No partial page is exposed. An early observation returns E_BUSY without
    /// ending collection; any sticky error remains visible after finish too.
    pub fn page(&self) -> Result<DirectoryPage<'_>, i32> {
        if self.error != 0 {
            return Err(self.error);
        }
        if !self.finished {
            return Err(crate::E_BUSY);
        }
        Ok(DirectoryPage { entries: &self.entries[..self.len], more: self.more })
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    fn value(path: &[u8], kind: u8, id: u32) -> CatalogEntry {
        let mut out = EMPTY;
        out.path[..path.len()].copy_from_slice(path);
        out.entry = Entry { first: id, size: id * 71, kind, sha: [id as u8; 32] };
        out
    }
    fn file(path: &[u8], id: u32) -> CatalogEntry {
        value(path, KIND_FILE, id)
    }
    fn dir(path: &[u8]) -> CatalogEntry {
        value(path, KIND_DIR, 17)
    }
    fn names(page: &DirectoryPage<'_>) -> Vec<Vec<u8>> {
        page.entries.iter().map(|e| e.path[..e.path.iter().position(|b| *b == 0).unwrap()].to_vec()).collect()
    }

    #[test]
    fn unsorted_chunks_project_implied_and_empty_directories_with_exact_files() {
        let data = [
            file(b"a/z/deep/file", 9),
            file(b"ab/not-a-child", 8),
            dir(b"a/empty"),
            file(b"a/file", 7),
            dir(b"a/z"),
            file(b"a/z/other", 6),
            file(b"a/file", 7),
            dir(b"a"),
        ];
        let mut c = DirectoryCollector::<8>::new(b"a", None).unwrap();
        assert_eq!(c.page().err(), Some(crate::E_BUSY));
        c.feed(&data[..3]).unwrap();
        c.feed(&[]).unwrap();
        assert_eq!(c.page().err(), Some(crate::E_BUSY));
        c.feed(&data[3..]).unwrap();
        c.finish(Ok(())).unwrap();
        let p = c.page().unwrap();
        assert_eq!(names(&p), [b"a/empty".to_vec(), b"a/file".to_vec(), b"a/z".to_vec()]);
        assert_eq!(p.entries[0].entry, DIRECTORY);
        assert_eq!(p.entries[1], file(b"a/file", 7));
        assert_eq!(p.entries[2].entry, DIRECTORY);
        assert!(!p.more);
    }

    #[test]
    fn byte_order_preserves_non_ascii_full_paths_and_owned_input_keys() {
        let mut requested = *b"a";
        let mut after = *b"a/x";
        let mut c = DirectoryCollector::<3>::new(&requested, Some(&after)).unwrap();
        requested[0] = b'b';
        after[2] = b'z';
        assert_eq!(requested, *b"b");
        assert_eq!(after, *b"a/z");
        let mut long = [b'y'; PATH_MAX];
        long[0] = b'a';
        long[1] = b'/';
        let high = [b'a', b'/', 0x80, 0xff];
        let data = [file(&high, 2), file(&long, 3), file(b"a/x", 4), file(b"a/z", 5)];
        c.feed(&data).unwrap();
        c.finish(Ok(())).unwrap();
        let p = c.page().unwrap();
        assert_eq!(names(&p), [long.to_vec(), b"a/z".to_vec(), high.to_vec()]);
        assert_eq!(p.entries[0], data[1]);
        assert_eq!(p.entries[2].entry.sha, [2; 32]);
        assert_eq!(p.entries[0].path[PATH_MAX], 0);
        assert!(!p.more);
    }

    #[test]
    fn continuation_pages_replay_full_unsorted_scan_and_count_distinct_children() {
        let data = [file(b"z", 9), file(b"d/q", 8), file(b"b", 2), dir(b"d"), file(b"a", 1), file(b"c", 3), file(b"b", 2), file(b"d/r", 7), file(b"z", 9)];
        let mut after: Option<Vec<u8>> = None;
        let mut all = Vec::new();
        for expected_more in [true, true, false] {
            let mut c = DirectoryCollector::<2>::new(b"", after.as_deref()).unwrap();
            for chunk in data.chunks(2) {
                c.feed(chunk).unwrap();
            }
            c.finish(Ok(())).unwrap();
            let page = c.page().unwrap();
            assert_eq!(page.more, expected_more);
            let paths = names(&page);
            after = paths.last().cloned();
            all.extend(paths);
        }
        assert_eq!(all, [b"a".to_vec(), b"b".to_vec(), b"c".to_vec(), b"d".to_vec(), b"z".to_vec()]);
        let mut c = DirectoryCollector::<1>::new(b"", Some(b"z")).unwrap();
        c.feed(&data).unwrap();
        c.finish(Ok(())).unwrap();
        assert!(c.page().unwrap().entries.is_empty());
        assert!(!c.page().unwrap().more);
    }

    #[test]
    fn cap_one_handles_evictions_duplicates_and_exactly_full_page() {
        let mut c = DirectoryCollector::<1>::new(b"", None).unwrap();
        c.feed(&[file(b"z", 1), file(b"b", 2), file(b"a", 3), file(b"a", 3)]).unwrap();
        c.finish(Ok(())).unwrap();
        assert_eq!(names(&c.page().unwrap()), [b"a".to_vec()]);
        assert!(c.page().unwrap().more);
        let mut c = DirectoryCollector::<1>::new(b"", None).unwrap();
        c.feed(&[dir(b"only/child"), dir(b"only"), dir(b"only/other")]).unwrap();
        c.finish(Ok(())).unwrap();
        assert!(!c.page().unwrap().more);
        assert_eq!(c.page().unwrap().entries[0].entry, DIRECTORY);
        assert_eq!(DirectoryCollector::<0>::new(b"", None).err().map(|e| e), Some(E_BAD_STORE));
    }

    #[test]
    fn existence_is_independent_of_filtering_and_only_root_is_virtual() {
        let mut root = DirectoryCollector::<2>::new(b"", None).unwrap();
        root.feed(&[]).unwrap();
        root.finish(Ok(())).unwrap();
        assert!(root.page().unwrap().entries.is_empty());
        let mut empty = DirectoryCollector::<2>::new(b"empty", None).unwrap();
        empty.feed(&[dir(b"empty")]).unwrap();
        empty.finish(Ok(())).unwrap();
        assert!(empty.page().unwrap().entries.is_empty());
        let mut filtered = DirectoryCollector::<1>::new(b"a", Some(b"a/z")).unwrap();
        filtered.feed(&[file(b"a/child/deeper", 2)]).unwrap();
        filtered.finish(Ok(())).unwrap();
        assert!(filtered.page().unwrap().entries.is_empty());
        assert!(!filtered.page().unwrap().more);
        let mut missing = DirectoryCollector::<1>::new(b"a", None).unwrap();
        missing.feed(&[dir(b"ab"), file(b"ab/item", 1)]).unwrap();
        assert_eq!(missing.finish(Ok(())), Err(E_NOT_FOUND));
        assert_eq!(missing.page().err(), Some(E_NOT_FOUND));
        for target in [b"file".as_slice(), b"file/child".as_slice()] {
            let mut c = DirectoryCollector::<1>::new(target, None).unwrap();
            assert_eq!(c.feed(&[file(b"file", 1)]), Err(E_NOT_FOUND));
            assert_eq!(c.finish(Ok(())), Err(E_NOT_FOUND));
        }
    }

    #[test]
    fn paths_and_kinds_are_checked_globally_and_failures_stick() {
        for path in [b"/a".as_slice(), b"a/", b"a//b", b"a/.", b"a/../b", b".", b"..", b"a\x1f", b"a\x7f", b"a\0b"] {
            assert_eq!(DirectoryCollector::<1>::new(path, None).err(), Some(E_BAD_STORE));
        }
        assert_eq!(DirectoryCollector::<1>::new(&[b'a'; PATH_BYTES], None).err(), Some(E_BAD_STORE));
        for after in [b"".as_slice(), b"a", b"ab/c", b"a/b/c", b"a/..", b"a/b\0c"] {
            assert_eq!(DirectoryCollector::<1>::new(b"a", Some(after)).err(), Some(E_BAD_STORE));
        }
        let mut bad = Vec::new();
        for path in [b"".as_slice(), b"x//y", b"x/..", b"x\x01"] {
            bad.push(file(path, 1));
        }
        bad.push(CatalogEntry { path: [b'x'; PATH_BYTES], entry: DIRECTORY });
        bad.push(value(b"unrelated", 9, 1));
        for entry in bad {
            let mut c = DirectoryCollector::<1>::new(b"a", Some(b"a/z")).unwrap();
            assert_eq!(c.feed(&[entry]), Err(E_BAD_STORE));
            assert_eq!(c.feed(&[]), Err(E_BAD_STORE));
            assert_eq!(c.finish(Ok(())), Err(E_BAD_STORE));
            assert_eq!(c.page().err(), Some(E_BAD_STORE));
        }
    }

    #[test]
    fn selected_file_metadata_and_file_directory_conflicts_are_rejected() {
        let mut changed_sha = file(b"a", 1);
        changed_sha.entry.sha[31] ^= 0xff;
        for pair in [
            [file(b"a", 1), changed_sha],
            [file(b"a", 1), file(b"a", 2)],
            [file(b"a", 1), dir(b"a")],
            [file(b"a", 1), file(b"a/deeper", 2)],
            [file(b"a/deeper", 2), file(b"a", 1)],
        ] {
            let mut c = DirectoryCollector::<1>::new(b"", None).unwrap();
            c.feed(&pair[..1]).unwrap();
            assert_eq!(c.feed(&pair[1..]), Err(E_BAD_STORE));
            assert_eq!(c.finish(Ok(())), Err(E_BAD_STORE));
        }
        // Deliberately not a global consistency audit for keys outside the page.
        let mut c = DirectoryCollector::<1>::new(b"", None).unwrap();
        c.feed(&[file(b"a", 1), file(b"z", 2), file(b"z", 3), dir(b"z")]).unwrap();
        c.finish(Ok(())).unwrap();
        assert_eq!(names(&c.page().unwrap()), [b"a".to_vec()]);
        assert!(c.page().unwrap().more);
    }

    #[test]
    fn completion_requires_explicit_valid_scan_and_late_failure_invalidates_page() {
        let mut c = DirectoryCollector::<1>::new(b"", None).unwrap();
        c.feed(&[file(b"a", 1)]).unwrap();
        assert_eq!(c.page().err(), Some(crate::E_BUSY));
        assert_eq!(c.finish(Err(crate::E_BUSY)), Err(crate::E_BUSY));
        assert_eq!(c.page().err(), Some(crate::E_BUSY));
        assert_eq!(c.feed(&[]), Err(crate::E_BUSY));
        let mut c = DirectoryCollector::<1>::new(b"", None).unwrap();
        c.finish(Ok(())).unwrap();
        c.finish(Ok(())).unwrap();
        assert_eq!(c.finish(Err(E_READ)), Err(E_READ));
        assert_eq!(c.page().err(), Some(E_READ));
        let mut c = DirectoryCollector::<1>::new(b"", None).unwrap();
        c.finish(Ok(())).unwrap();
        assert_eq!(c.feed(&[]), Err(crate::E_FORMAT));
        assert_eq!(c.page().err(), Some(crate::E_FORMAT));
        // Array storage is proportional only to fixed N, never the scan length.
        assert_eq!(core::mem::size_of::<DirectoryCollector<9>>() - core::mem::size_of::<DirectoryCollector<1>>(), 8 * core::mem::size_of::<CatalogEntry>());
    }
}
