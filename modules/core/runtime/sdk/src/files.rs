//! Shared API1.4 read-only listing record. Stored metadata is not recomputed and
//! never establishes source identity, content validation, or an immutable lease.
pub const SAMPLES: u32 = 1;
pub const STORE: u32 = 2;
pub const FILE: u32 = 1;
pub const DIRECTORY: u32 = 2;
pub const SIZE_VALID: u32 = 1;
pub const HASH_VALID: u32 = 2;
pub const HASH_NONE: u32 = 0;
pub const HASH_SHA256: u32 = 1;
pub const HASH_ELEKTRON32: u32 = 2;
pub const PATH_BYTES: usize = 256;
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileEntry {
    pub kind: u32,
    pub flags: u32,
    pub size_bytes: u32,
    pub hash_kind: u32,
    pub hash: [u8; 32],
    pub path: [u8; PATH_BYTES],
}
impl FileEntry {
    pub const fn empty() -> Self {
        Self {
            kind: 0,
            flags: 0,
            size_bytes: 0,
            hash_kind: HASH_NONE,
            hash: [0; 32],
            path: [0; PATH_BYTES],
        }
    }
}
const _: () = assert!(core::mem::size_of::<FileEntry>() == 304);

/// Validate a root-relative NUL-terminated path bounded by the slice. Empty is
/// root; no empty/dot/dotdot parts or control/DEL bytes. Bytes >=128 stay intact.
pub fn path_len(path: &[u8]) -> Option<usize> {
    let n = path.iter().position(|b| *b == 0)?;
    if n >= PATH_BYTES {
        return None;
    }
    if n == 0 {
        return Some(0);
    }
    let mut start = 0;
    let mut i = 0;
    while i <= n {
        let byte = if i == n { b'/' } else { path[i] };
        if i < n && (byte < 32 || byte == 127) {
            return None;
        }
        if byte == b'/' {
            let part = &path[start..i];
            if part.is_empty() || part == b"." || part == b".." {
                return None;
            }
            start = i + 1;
        }
        i += 1;
    }
    Some(n)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn record_offsets_support_exact_foreign_copy_and_paths_are_not_truncated() {
        assert_eq!(core::mem::offset_of!(FileEntry, hash), 16);
        assert_eq!(core::mem::offset_of!(FileEntry, path), 48);
        let mut row = FileEntry::empty();
        row.path[..255].fill(0x80);
        assert_eq!(path_len(&row.path), Some(255));
        row.path[255] = b'x';
        assert_eq!(path_len(&row.path), None);
        for path in [
            b"a/../b\0".as_slice(),
            b"/a\0",
            b"a//b\0",
            b"a/\0",
            b"a\x7f\0",
        ] {
            assert_eq!(path_len(path), None);
        }
        assert_eq!(path_len(b"\0"), Some(0));
        assert_eq!(path_len(b"a/\x80\0"), Some(3));
    }
}
