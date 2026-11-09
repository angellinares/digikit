//! The module and sample store, version 4: files of `plus_drive/<device>/digikit/` in a reserved area of card sectors that the firmware never
//! writes (format and rationale: docs/design/digikit-store.md; the runtime's reader and writer are `dkrt_sdk::store`, which this file matches byte for
//! byte).
//!
//! All integers are big-endian. The area is `[first, end)`; sectors below are relative to `first`:
//!
//! ```text
//! sector 0                  header copy A (one sector)
//! ..                        tables and file data, anywhere in [1, header_b): free space is reused by the runtime
//! header_b = sectors - 35   header copy B
//! sectors - 34 .. - 3       the settings record, two copies of 16 sectors (settings.rs; not a store structure)
//! sectors - 2, - 1          the selection record, two copies (selection.rs; not a store structure)
//!
//! header:  "DKST", u32 version = 4, u32 entry_count, u32 table_sectors, u32 table_first, u32 table_crc (IEEE, over the table's sectors, zero
//!          padding included), u32 seq, u32 used_sectors (header A, the table and every file), u32 area_sectors (the area it was made for),
//!          zero, u32 crc32 of bytes 0..508 at 508
//! table:   entry_count entries of 256 bytes sorted by (first_sector, path): path[160] NUL padded (relative to digikit/, at most 159 bytes),
//!          u32 first_sector, u32 size_bytes, hash slot[32] (`dksample::file_hash`: 4 bytes, then zeros), u8 kind (0 file, 1 folder with nothing in it), zero. An entry with no data (a folder, an
//!          empty file) has first_sector 0 and sorts before every file with data.
//! data:    each file starts on a sector boundary
//! ```
//!
//! The builder lays the data out in path order from sector `1 + table_sectors`; the runtime allocates into gaps and moves the table on every commit, so
//! another store has the same entries in any positions.

use emmc_card::RandomAccessRead;

use crate::SECTOR;

pub const STORE_MAGIC: [u8; 4] = *b"DKST";
pub const STORE_VERSION: u32 = 4;
pub const STORE_ENTRY_BYTES: usize = 256;
pub const STORE_PATH_BYTES: usize = 160;
/// A path has at most this many bytes (the field ends in a NUL).
pub const STORE_PATH_MAX: usize = STORE_PATH_BYTES - 1;
/// Sectors after the last one files may use: header copy B, the settings record (two copies of 16 sectors) and the two sectors of the
/// selection record. Equal to `dkrt_sdk::store::TAIL` (the device files' `[card.store]` fix the same layout).
pub const STORE_TAIL: u64 = 35;
pub const KIND_FILE: u8 = 0;
pub const KIND_DIR: u8 = 1;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreFile {
    /// Relative to `digikit/`, `/`-separated, no leading slash.
    pub path: String,
    pub bytes: Vec<u8>,
    /// A folder with nothing in it (`bytes` empty). A folder with files is implied by their paths.
    pub dir: bool,
}

impl StoreFile {
    pub fn file(path: impl Into<String>, bytes: Vec<u8>) -> Self {
        Self {
            path: path.into(),
            bytes,
            dir: false,
        }
    }
    pub fn folder(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            bytes: Vec::new(),
            dir: true,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreEntry {
    pub path: String,
    /// Relative to the first sector of the store; 0 for an entry with no data.
    pub first_sector: u32,
    pub size_bytes: u32,
    /// The hash slot (`dksample::file_hash`).
    pub hash: [u8; 32],
    /// A folder with nothing in it.
    pub is_dir: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Store {
    /// In table order (by first sector, then path).
    pub entries: Vec<StoreEntry>,
    pub used_sectors: u32,
    /// Always 3.
    pub version: u32,
    /// Sequence number of the header read.
    pub seq: u32,
    /// Which copy was used: false = A (sector 0), true = B.
    pub from_b: bool,
    /// Where the table starts, relative to the store.
    pub table_first_sector: u32,
}

impl Store {
    /// The entries that are files (not empty folders).
    pub fn files(&self) -> impl Iterator<Item = &StoreEntry> {
        self.entries.iter().filter(|e| !e.is_dir)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StoreError {
    /// The files need `needed` sectors, the area has `available`.
    DoesNotFit {
        needed: u64,
        available: u64,
    },
    /// A path is empty, has an empty, `.` or `..` component, a NUL or a byte below 0x20, or is not shorter than 160 bytes.
    InvalidPath(String),
    DuplicatePath(String),
    /// A path that is a file and also a folder of other entries.
    PathConflict(String),
    /// A file over 4 GiB - 1 (the size field).
    TooLarge(String),
    /// The reader found a header that is not version 4 or does not match its table.
    Corrupt(String),
}

impl core::fmt::Display for StoreError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::DoesNotFit { needed, available } => write!(
                f,
                "the digikit/ files need {needed} sectors ({} KiB) but the store has {available} ({} KiB)",
                needed * 512 / 1024,
                available * 512 / 1024
            ),
            Self::InvalidPath(p) => write!(
                f,
                "digikit/{p}: a store path is at most 159 bytes of printable text, with no empty, . or .. part"
            ),
            Self::DuplicatePath(p) => write!(f, "digikit/{p}: twice"),
            Self::PathConflict(p) => write!(f, "digikit/{p}: a file and a folder"),
            Self::TooLarge(p) => write!(f, "digikit/{p}: larger than 4 GiB"),
            Self::Corrupt(why) => write!(f, "module store: {why}"),
        }
    }
}
impl std::error::Error for StoreError {}

/// CRC-32 (IEEE 802.3, reflected, as zlib and `crc32` print it).
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

fn sectors_for(bytes: usize) -> u64 {
    bytes.div_ceil(SECTOR) as u64
}

/// Sectors of a table of `count` entries.
pub fn table_sectors(count: u64) -> u64 {
    count.div_ceil(2)
}

/// A store path: 1 to 159 bytes of printable text (0x20 to 0x7e), no empty, `.` or `..` part.
pub fn valid_store_path(path: &str) -> bool {
    path.len() <= STORE_PATH_MAX
        && !path.is_empty()
        && path.bytes().all(|b| (0x20..0x7f).contains(&b))
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// The bytes of a store holding `files`, a whole number of sectors starting with the header (header copy B is a copy of the first sector). `available`
/// is the area's size in sectors without the tail (header B, the settings record and the selection record) (`StoreLayout::store_limit`). The data is laid out in path order after the
/// table.
pub fn encode_store(files: Vec<StoreFile>, available: u64) -> Result<Vec<u8>, StoreError> {
    let mut files = files;
    files.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    for (i, file) in files.iter().enumerate() {
        if !valid_store_path(&file.path) {
            return Err(StoreError::InvalidPath(file.path.clone()));
        }
        if u32::try_from(file.bytes.len()).is_err() {
            return Err(StoreError::TooLarge(file.path.clone()));
        }
        if i > 0 && files[i - 1].path == file.path {
            return Err(StoreError::DuplicatePath(file.path.clone()));
        }
    }
    // no path may be both a file and the folder of another entry
    for file in &files {
        let prefix = format!("{}/", file.path);
        if !file.dir && files.iter().any(|o| o.path.starts_with(&prefix)) {
            return Err(StoreError::PathConflict(file.path.clone()));
        }
    }
    // a folder entry for a folder that has files is redundant: drop it
    let keep: Vec<bool> = files
        .iter()
        .map(|f| {
            let prefix = format!("{}/", f.path);
            !(f.dir && files.iter().any(|o| o.path.starts_with(&prefix)))
        })
        .collect();
    let files: Vec<StoreFile> = files
        .into_iter()
        .zip(keep)
        .filter_map(|(f, k)| k.then_some(f))
        .collect();
    let with_data = |f: &StoreFile| !f.dir && !f.bytes.is_empty();
    let table_secs = table_sectors(files.len() as u64);
    let mut next = 1 + table_secs;
    let mut firsts = vec![0u32; files.len()];
    for (i, file) in files.iter().enumerate() {
        if with_data(file) {
            firsts[i] = u32::try_from(next).map_err(|_| StoreError::DoesNotFit {
                needed: u64::MAX,
                available,
            })?;
            next += sectors_for(file.bytes.len());
        }
    }
    if next > available {
        return Err(StoreError::DoesNotFit {
            needed: next,
            available,
        });
    }
    // table order: entries with no data first (by path), then the files by first sector
    let mut order: Vec<usize> = (0..files.len()).collect();
    order.sort_by(|&a, &b| {
        firsts[a]
            .cmp(&firsts[b])
            .then_with(|| files[a].path.as_bytes().cmp(files[b].path.as_bytes()))
    });
    let mut table = vec![0u8; table_secs as usize * SECTOR];
    for (slot, &i) in order.iter().enumerate() {
        let file = &files[i];
        let at = slot * STORE_ENTRY_BYTES;
        table[at..at + file.path.len()].copy_from_slice(file.path.as_bytes());
        table[at + 160..at + 164].copy_from_slice(&firsts[i].to_be_bytes());
        table[at + 164..at + 168].copy_from_slice(&(file.bytes.len() as u32).to_be_bytes());
        if !file.dir {
            table[at + 168..at + 200].copy_from_slice(&dksample::file_hash(&file.bytes));
        }
        table[at + 200] = if file.dir { KIND_DIR } else { KIND_FILE };
    }
    let mut out = vec![0u8; next as usize * SECTOR];
    let header = encode_header(&Header {
        count: files.len() as u32,
        table_sectors: table_secs as u32,
        table_first: 1,
        table_crc: if table_secs == 0 { 0 } else { crc32(&table) },
        seq: 1,
        used: next as u32,
        area: (available + STORE_TAIL) as u32,
    });
    out[..SECTOR].copy_from_slice(&header);
    out[SECTOR..SECTOR + table.len()].copy_from_slice(&table);
    for (file, &first) in files.iter().zip(&firsts) {
        if with_data(file) {
            let at = first as usize * SECTOR;
            out[at..at + file.bytes.len()].copy_from_slice(&file.bytes);
        }
    }
    Ok(out)
}

fn read_vec(img: &dyn RandomAccessRead, offset: u64, len: usize) -> Vec<u8> {
    let mut out = vec![0; len];
    img.read_at(offset, &mut out);
    out
}
fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(b[at..at + 4].try_into().expect("4 bytes"))
}

/// One header copy as the sector holds it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Header {
    pub count: u32,
    pub table_sectors: u32,
    pub table_first: u32,
    pub table_crc: u32,
    pub seq: u32,
    pub used: u32,
    pub area: u32,
}

/// Parse a header sector: the magic, version 4 and the header CRC. `None` otherwise.
pub fn parse_header(sector: &[u8]) -> Option<Header> {
    if sector.len() < SECTOR
        || sector[0..4] != STORE_MAGIC
        || be32(sector, 4) != STORE_VERSION
        || be32(sector, 508) != crc32(&sector[..508])
    {
        return None;
    }
    Some(Header {
        count: be32(sector, 8),
        table_sectors: be32(sector, 12),
        table_first: be32(sector, 16),
        table_crc: be32(sector, 20),
        seq: be32(sector, 24),
        used: be32(sector, 28),
        area: be32(sector, 32),
    })
}

/// The 512 bytes of a header copy.
pub fn encode_header(h: &Header) -> [u8; SECTOR] {
    let mut out = [0u8; SECTOR];
    out[0..4].copy_from_slice(&STORE_MAGIC);
    out[4..8].copy_from_slice(&STORE_VERSION.to_be_bytes());
    out[8..12].copy_from_slice(&h.count.to_be_bytes());
    out[12..16].copy_from_slice(&h.table_sectors.to_be_bytes());
    out[16..20].copy_from_slice(&h.table_first.to_be_bytes());
    out[20..24].copy_from_slice(&h.table_crc.to_be_bytes());
    out[24..28].copy_from_slice(&h.seq.to_be_bytes());
    out[28..32].copy_from_slice(&h.used.to_be_bytes());
    out[32..36].copy_from_slice(&h.area.to_be_bytes());
    let c = crc32(&out[..508]);
    out[508..512].copy_from_slice(&c.to_be_bytes());
    out
}

/// Is a decoded header consistent with the area (`available` sectors without the tail) and with itself?
fn plausible(h: &Header, available: u64) -> bool {
    u64::from(h.area) == available + STORE_TAIL
        && u64::from(h.count) <= available * 2
        && u64::from(h.table_sectors) == table_sectors(u64::from(h.count))
        && h.table_first >= 1
        && (h.table_sectors == 0
            || u64::from(h.table_first) + u64::from(h.table_sectors) <= available)
        && u64::from(h.used) >= 1 + u64::from(h.table_sectors)
        && u64::from(h.used) <= available
}

/// Load the table `h` points at and check it.
fn load_store(
    img: &dyn RandomAccessRead,
    base: u64,
    available: u64,
    h: &Header,
    from_b: bool,
) -> Result<Store, StoreError> {
    let corrupt = |why: &str| Err(StoreError::Corrupt(why.into()));
    let count = h.count as usize;
    let table_len = h.table_sectors as usize * SECTOR;
    let table = read_vec(
        img,
        base + u64::from(h.table_first) * SECTOR as u64,
        table_len,
    );
    let crc = if table_len == 0 { 0 } else { crc32(&table) };
    if crc != h.table_crc {
        return corrupt("table checksum");
    }
    let mut entries = Vec::with_capacity(count);
    for i in 0..count {
        let e = &table[i * STORE_ENTRY_BYTES..(i + 1) * STORE_ENTRY_BYTES];
        let name_len = e[..STORE_PATH_BYTES]
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(STORE_PATH_BYTES);
        let first_sector = be32(e, 160);
        let size_bytes = be32(e, 164);
        let is_dir = e[200] == KIND_DIR;
        let data = if is_dir {
            0
        } else {
            sectors_for(size_bytes as usize)
        };
        if data != 0 && (first_sector < 1 || u64::from(first_sector) + data > available) {
            return corrupt("an entry lies outside the store");
        }
        entries.push(StoreEntry {
            path: String::from_utf8_lossy(&e[..name_len]).into_owned(),
            first_sector,
            size_bytes,
            hash: e[168..200].try_into().expect("32 bytes"),
            is_dir,
        });
    }
    Ok(Store {
        entries,
        used_sectors: h.used,
        version: STORE_VERSION,
        seq: h.seq,
        from_b,
        table_first_sector: h.table_first,
    })
}

/// Read the store at `first_sector` (`available` sectors without the tail (header B, the settings record and the selection record); `header_b_sector` is the absolute sector of header
/// copy B, `StoreLayout::header_b`): `Ok(None)` when neither header has the magic (nothing was built into the area). A copy is valid when its magic,
/// version, header CRC, area and table CRC hold; the valid copy with the higher `seq` wins (equal: A).
pub fn read_store(
    img: &dyn RandomAccessRead,
    first_sector: u64,
    available: u64,
    header_b_sector: u64,
) -> Result<Option<Store>, StoreError> {
    let base = first_sector * SECTOR as u64;
    let a_sec = read_vec(img, base, SECTOR);
    let b_sec = read_vec(img, header_b_sector * SECTOR as u64, SECTOR);
    let a = parse_header(&a_sec).filter(|h| plausible(h, available));
    let b = parse_header(&b_sec).filter(|h| plausible(h, available));
    if a.is_none() && b.is_none() && a_sec[0..4] != STORE_MAGIC && b_sec[0..4] != STORE_MAGIC {
        return Ok(None);
    }
    let a_store = a.map(|h| load_store(img, base, available, &h, false));
    let b_store = b.map(|h| load_store(img, base, available, &h, true));
    let a_ok = a_store.as_ref().and_then(|r| r.as_ref().ok());
    let b_ok = b_store.as_ref().and_then(|r| r.as_ref().ok());
    let pick_b = match (a_ok, b_ok) {
        (Some(a), Some(b)) => b.seq > a.seq,
        (None, Some(_)) => true,
        _ => false,
    };
    if pick_b {
        return b_store.unwrap().map(Some);
    }
    match (a_store, b_store) {
        (Some(Ok(s)), _) => Ok(Some(s)),
        (Some(Err(e)), _) => Err(e),
        (None, Some(Err(e))) => Err(e),
        _ => Err(StoreError::Corrupt(
            "unknown version, area or header checksum".into(),
        )),
    }
}

/// The bytes of one entry, checked against its hash slot.
pub fn read_store_file(
    img: &dyn RandomAccessRead,
    first_sector: u64,
    entry: &StoreEntry,
) -> Result<Vec<u8>, StoreError> {
    if entry.size_bytes == 0 {
        return Ok(Vec::new());
    }
    let at = (first_sector + u64::from(entry.first_sector)) * SECTOR as u64;
    let bytes = read_vec(img, at, entry.size_bytes as usize);
    if dksample::file_hash(&bytes) != entry.hash {
        return Err(StoreError::Corrupt(format!(
            "digikit/{}: hash differs",
            entry.path
        )));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SparseImage;

    fn file(path: &str, len: usize) -> StoreFile {
        StoreFile::file(path, (0..len).map(|i| (i * 13 + 5) as u8).collect())
    }

    /// An area of 0x6000 sectors at 0x1000: header B at 0x1000 + 0x5fdd.
    const FIRST: u64 = 0x1000;
    const SECTORS: u64 = 0x6000;
    const AVAIL: u64 = SECTORS - STORE_TAIL;
    const B: u64 = FIRST + SECTORS - STORE_TAIL;

    /// An overwritable card for the torn-write cases (`SparseImage` refuses a second write).
    struct Mem(Vec<u8>);
    impl Mem {
        fn new() -> Self {
            Mem(vec![0; (FIRST + SECTORS) as usize * 512])
        }
        fn write_at(&mut self, at: u64, b: &[u8]) {
            self.0[at as usize..at as usize + b.len()].copy_from_slice(b);
        }
    }
    impl RandomAccessRead for Mem {
        fn len(&self) -> u64 {
            self.0.len() as u64
        }
        fn read_at(&self, off: u64, dst: &mut [u8]) -> usize {
            dst.copy_from_slice(&self.0[off as usize..off as usize + dst.len()]);
            dst.len()
        }
    }

    /// A card with `files` built, both header copies.
    fn card(files: Vec<StoreFile>) -> (Mem, Vec<u8>) {
        let bytes = encode_store(files, AVAIL).unwrap();
        let mut img = Mem::new();
        img.write_at(FIRST * 512, &bytes);
        img.write_at(B * 512, &bytes[..SECTOR]);
        (img, bytes)
    }

    #[test]
    fn crc32_matches_the_zlib_check_value() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    }

    #[test]
    fn round_trips_in_table_order_and_sector_aligned() {
        let files = vec![
            file("b/c.dkm", 1000),
            file("a.dkm", 512),
            file("empty", 0),
            StoreFile::folder("samples/new"),
        ];
        let bytes = encode_store(files.clone(), AVAIL).unwrap();
        assert_eq!(bytes.len() % SECTOR, 0);
        let mut img = SparseImage::new((FIRST + SECTORS) * 512);
        img.write_at(FIRST * 512, &bytes).unwrap();
        let store = read_store(&img, FIRST, AVAIL, B).unwrap().unwrap();
        // no data first (by path), then by first sector: header + 2 table sectors, a.dkm at 3, b/c.dkm at 4
        let names: Vec<_> = store.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(names, ["empty", "samples/new", "a.dkm", "b/c.dkm"]);
        assert_eq!(store.used_sectors, 1 + 2 + 1 + 2);
        assert_eq!(store.entries[2].first_sector, 3);
        assert_eq!(store.entries[3].first_sector, 4);
        assert!(store.entries[1].is_dir && !store.entries[0].is_dir);
        assert_eq!(store.files().count(), 3);
        for e in store.entries.iter().filter(|e| !e.is_dir) {
            let want = files.iter().find(|f| f.path == e.path).unwrap();
            assert_eq!(read_store_file(&img, FIRST, e).unwrap(), want.bytes);
        }
        assert_eq!(&bytes[0..4], b"DKST");
        assert_eq!(&bytes[4..8], &4u32.to_be_bytes());
    }

    #[test]
    fn many_entries_and_long_paths() {
        let files: Vec<_> = (0..700)
            .map(|i| file(&format!("samples/{}/s{i:04}.bin", "d".repeat(100)), 600 + i))
            .collect();
        let bytes = encode_store(files.clone(), AVAIL).unwrap();
        let mut img = Mem::new();
        img.write_at(FIRST * 512, &bytes);
        let store = read_store(&img, FIRST, AVAIL, B).unwrap().unwrap();
        assert_eq!(store.entries.len(), 700);
        assert_eq!(store.table_first_sector, 1);
        let last = store
            .entries
            .iter()
            .find(|e| e.path.ends_with("s0699.bin"))
            .unwrap();
        assert_eq!(
            read_store_file(&img, FIRST, last).unwrap(),
            files[699].bytes
        );
        assert!(encode_store(vec![file(&"x".repeat(159), 1)], AVAIL).is_ok());
        assert!(matches!(
            encode_store(vec![file(&"x".repeat(160), 1)], AVAIL),
            Err(StoreError::InvalidPath(_))
        ));
    }

    #[test]
    fn refuses_what_does_not_fit_or_is_malformed() {
        assert!(matches!(
            encode_store(vec![file("big", 10 * 512)], 5),
            Err(StoreError::DoesNotFit { .. })
        ));
        for bad in ["", "/a", "a//b", "a/../b", "./a", "a\tb", &"x".repeat(160)] {
            assert!(
                matches!(
                    encode_store(vec![file(bad, 1)], 100),
                    Err(StoreError::InvalidPath(_))
                ),
                "{bad:?}"
            );
        }
        assert!(matches!(
            encode_store(vec![file("a", 1), file("a", 2)], 100),
            Err(StoreError::DuplicatePath(_))
        ));
        // a file and a folder of other entries at one path
        assert!(matches!(
            encode_store(vec![file("a", 1), file("a/b", 2)], 100),
            Err(StoreError::PathConflict(_))
        ));
        // a folder entry that has files is not stored
        let b = encode_store(vec![StoreFile::folder("a"), file("a/b", 2)], 100).unwrap();
        assert_eq!(be32(&b, 8), 1);
    }

    #[test]
    fn an_empty_area_has_no_store_and_damage_is_found() {
        let mut img = Mem::new();
        assert_eq!(read_store(&img, FIRST, AVAIL, B).unwrap(), None);
        let mut bytes = encode_store(vec![file("a", 700)], AVAIL).unwrap();
        bytes[SECTOR + 3] ^= 1;
        img.write_at(FIRST * 512, &bytes);
        assert!(matches!(
            read_store(&img, FIRST, AVAIL, B),
            Err(StoreError::Corrupt(_))
        ));
    }

    /// Header copy with a higher seq that points at the table at sector `at` (a table copy written there).
    fn commit_b(img: &mut Mem, bytes: &[u8], seq: u32, at: u64) {
        let mut h = parse_header(&bytes[..SECTOR]).unwrap();
        let table = &bytes[SECTOR..SECTOR + h.table_sectors as usize * SECTOR];
        img.write_at((FIRST + at) * 512, table);
        h.seq = seq;
        h.table_first = at as u32;
        h.used = at as u32 + h.table_sectors + 1;
        img.write_at(B * 512, &encode_header(&h));
    }

    #[test]
    fn equal_seq_takes_a_and_the_higher_seq_takes_b() {
        let (mut img, bytes) = card(vec![file("a", 700)]);
        let s = read_store(&img, FIRST, AVAIL, B).unwrap().unwrap();
        assert_eq!((s.version, s.seq, s.from_b), (4, 1, false));
        commit_b(&mut img, &bytes, 2, 0x100);
        let s = read_store(&img, FIRST, AVAIL, B).unwrap().unwrap();
        assert_eq!((s.seq, s.from_b, s.table_first_sector), (2, true, 0x100));
        assert_eq!(s.entries[0].path, "a");
    }

    #[test]
    fn a_torn_b_leaves_the_old_store_and_a_torn_a_leaves_b() {
        let (mut img, bytes) = card(vec![file("a", 700)]);
        commit_b(&mut img, &bytes, 2, 0x100);
        // B torn (bit flip): header CRC fails, A (seq 1) is used
        let mut b = read_vec(&img, B * 512, 512);
        b[40] ^= 1;
        img.write_at(B * 512, &b);
        let s = read_store(&img, FIRST, AVAIL, B).unwrap().unwrap();
        assert_eq!((s.seq, s.from_b), (1, false));
        // B good, A torn: B is used
        commit_b(&mut img, &bytes, 2, 0x100);
        let mut a = read_vec(&img, FIRST * 512, 512);
        a[28] ^= 1;
        img.write_at(FIRST * 512, &a);
        let s = read_store(&img, FIRST, AVAIL, B).unwrap().unwrap();
        assert_eq!((s.seq, s.from_b), (2, true));
        // B points at a table that was not written: the table CRC fails, A is used
        let (mut img, bytes) = card(vec![file("a", 700)]);
        commit_b(&mut img, &bytes, 2, 0x100);
        img.write_at((FIRST + 0x100) * 512, &[0u8; 512]);
        let s = read_store(&img, FIRST, AVAIL, B).unwrap().unwrap();
        assert_eq!((s.seq, s.from_b), (1, false));
    }

    #[test]
    fn a_store_for_another_area_or_version_is_refused() {
        let (mut img, bytes) = card(vec![file("a", 700)]);
        // read with another area size: the header's area does not match
        assert!(read_store(&img, FIRST, AVAIL - 1, B - 1).is_err());
        // a version 2 header is not a store of this version
        let mut h = bytes[..SECTOR].to_vec();
        h[4..8].copy_from_slice(&2u32.to_be_bytes());
        img.write_at(FIRST * 512, &h);
        img.write_at(B * 512, &[0u8; 512]);
        assert!(read_store(&img, FIRST, AVAIL, B).is_err());
    }
}
