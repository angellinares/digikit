//! Read a +Drive back out of a card image plus the guest's write overlay.
//!
//! Two independent regions (docs/findings/14-plus-drive-format.md):
//!
//! - region 2, the path filesystem: superblock at sector 0x5D8000 (magic, version 3 or 4, lookup3 checksum), the id
//!   and page bitmaps, 128-byte records (inline extents, or indirect extent pages above 8 extents), directory
//!   content pages;
//! - region 1, MmcFs: the header at sector 0, three write-protect bitmaps at sector 0x800 and the project, sound and
//!   kit slots at fixed sector addresses.
//!
//! Nothing here panics on a damaged card: inconsistencies become `issues` and the walk goes on where it can.

use std::collections::{BTreeMap, BTreeSet};

use emmc_card::{CardOverlaySector, RandomAccessRead};

use crate::tree::{CardLayout, Pool, RegionRole};
use crate::{
    CONTENT_AREA_SECTOR, ID_BITMAP_SECTOR, PAGE, PAGE_BITMAP_SECTOR, RECORD_AREA_SECTOR,
    RECORD_SIZE, ROOT_ID, SECTOR, SUPERBLOCK_SECTOR, hashlittle,
};

/// First id of the RAM-resident records (the read-only "factory" tree); not on the card.
pub const RAM_ID_BASE: u32 = 0x100_0000;
/// Ids on the card are `2..0x58000`.
pub const MAX_CARD_ID: u32 = 0x57fff;
/// Extents per indirect page (`PAGE / 12`).
const EXTENTS_PER_PAGE: usize = PAGE / 12;
/// A file larger than this is reported and skipped (the card is 4 GB; samples are far smaller).
pub const MAX_FILE_BYTES: u32 = 1 << 30;
const MAX_DEPTH: usize = 64;
const MAX_NODES: usize = 1 << 20;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReadError {
    /// The superblock is missing or damaged: this card has no path filesystem.
    NoFilesystem(String),
    /// The overlay file is damaged.
    BadOverlay(String),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoFilesystem(m) => write!(f, "no path filesystem: {m}"),
            Self::BadOverlay(m) => write!(f, "overlay: {m}"),
        }
    }
}

// ----- the sector view: base image plus overlay ---------------------------------------------------------------

/// A base image seen through the guest's write overlay (the card the guest sees). Bytes that neither the base nor
/// the overlay holds read as zero.
#[allow(clippy::type_complexity, reason = "one map field")]
pub struct OverlayImage<'a> {
    base: &'a dyn RandomAccessRead,
    sectors: BTreeMap<u64, (Box<[u8; 64]>, Box<[u8; SECTOR]>)>,
    len: u64,
}

impl<'a> OverlayImage<'a> {
    pub fn new(base: &'a dyn RandomAccessRead, overlay: &[CardOverlaySector]) -> Self {
        let mut len = base.len();
        let mut sectors = BTreeMap::new();
        for s in overlay {
            len = len.max((s.sector + 1) * SECTOR as u64);
            sectors.insert(s.sector, (Box::new(s.written), Box::new(s.data)));
        }
        Self { base, sectors, len }
    }
    /// The sectors the overlay touched.
    pub fn touched(&self) -> impl Iterator<Item = u64> + '_ {
        self.sectors.keys().copied()
    }
    pub fn is_touched(&self, sector: u64) -> bool {
        self.sectors.contains_key(&sector)
    }
    /// Whether the overlay touched any sector in `[first, first + count)`.
    pub fn touched_in(&self, first: u64, count: u64) -> bool {
        self.sectors
            .range(first..first.saturating_add(count))
            .next()
            .is_some()
    }
}

impl RandomAccessRead for OverlayImage<'_> {
    fn len(&self) -> u64 {
        self.len
    }
    fn read_at(&self, offset: u64, destination: &mut [u8]) -> usize {
        destination.fill(0);
        if destination.is_empty() || offset >= self.len {
            return 0;
        }
        let count = usize::try_from((self.len - offset).min(destination.len() as u64))
            .unwrap_or(destination.len());
        self.base.read_at(offset, &mut destination[..count]);
        let first = offset / SECTOR as u64;
        let last = (offset + count as u64 - 1) / SECTOR as u64;
        for (&sector, (mask, data)) in self.sectors.range(first..=last) {
            let start = sector * SECTOR as u64;
            for i in 0..SECTOR {
                let at = start + i as u64;
                if at < offset || at >= offset + count as u64 {
                    continue;
                }
                if mask[i / 8] & (1 << (i % 8)) != 0 {
                    destination[(at - offset) as usize] = data[i];
                }
            }
        }
        count
    }
}

/// Bytes per record of an overlay file (`card.overlay`): `u64 le sector, [u8; 64] mask, [u8; 512] data`.
pub const OVERLAY_RECORD: usize = 8 + 64 + SECTOR;

pub fn parse_overlay(bytes: &[u8]) -> Result<Vec<CardOverlaySector>, ReadError> {
    if !bytes.len().is_multiple_of(OVERLAY_RECORD) {
        return Err(ReadError::BadOverlay(format!(
            "{} bytes is not a multiple of {OVERLAY_RECORD}",
            bytes.len()
        )));
    }
    let mut out = Vec::with_capacity(bytes.len() / OVERLAY_RECORD);
    let mut previous = None;
    for record in bytes.as_chunks::<OVERLAY_RECORD>().0 {
        let sector = u64::from_le_bytes(record[..8].try_into().unwrap());
        if previous.is_some_and(|p| p >= sector) {
            return Err(ReadError::BadOverlay(format!(
                "sector {sector:#x} is out of order"
            )));
        }
        previous = Some(sector);
        let mut written = [0u8; 64];
        written.copy_from_slice(&record[8..72]);
        let mut data = [0u8; SECTOR];
        data.copy_from_slice(&record[72..]);
        out.push(CardOverlaySector {
            sector,
            written,
            data,
        });
    }
    Ok(out)
}

pub fn encode_overlay(sectors: &[CardOverlaySector]) -> Vec<u8> {
    let mut out = Vec::with_capacity(sectors.len() * OVERLAY_RECORD);
    for s in sectors {
        out.extend_from_slice(&s.sector.to_le_bytes());
        out.extend_from_slice(&s.written);
        out.extend_from_slice(&s.data);
    }
    out
}

fn read_vec(img: &dyn RandomAccessRead, offset: u64, len: usize) -> Vec<u8> {
    let mut v = vec![0; len];
    img.read_at(offset, &mut v);
    v
}
fn be32(d: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(d[at..at + 4].try_into().unwrap())
}
fn be16(d: &[u8], at: usize) -> u16 {
    u16::from_be_bytes(d[at..at + 2].try_into().unwrap())
}

// ----- region 2: the path filesystem ----------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Record {
    pub id: u32,
    pub attr: u8,
    pub links: u16,
    pub size: u32,
    pub parent: u32,
    pub hash: u32,
    pub seq: u32,
    pub extent_count: u16,
    raw: [u8; RECORD_SIZE],
}

impl Record {
    pub fn is_dir(&self) -> bool {
        self.attr & 1 != 0
    }
}

#[derive(Clone, Debug)]
pub struct Node {
    /// Path below the root, components joined by `/`, raw name bytes.
    pub path: Vec<u8>,
    pub record: Record,
}

#[derive(Debug, Default)]
pub struct Walk {
    pub nodes: Vec<Node>,
    pub issues: Vec<String>,
    /// Entries that point at RAM-resident ids (the firmware links "factory" into the root at every mount).
    pub ram_entries: Vec<Vec<u8>>,
}

pub struct Fs<'a> {
    img: &'a dyn RandomAccessRead,
}

/// Sector of a content page, or None for the RAM-resident ids.
pub fn page_sector(page: u32) -> Option<u64> {
    if page < RAM_ID_BASE {
        Some(CONTENT_AREA_SECTOR + u64::from(page) * (PAGE / SECTOR) as u64)
    } else if page >= 0x100_0800 {
        // FUN_401559f4: `page * 0x40 - 0x3fbc8000`, 32-bit wrap (0x1000800 is sector 0x458000)
        Some(u64::from(
            (page.wrapping_mul(0x40)).wrapping_sub(0x3fbc_8000),
        ))
    } else {
        None
    }
}

fn is_bit(bitmap: &[u8], n: u32) -> bool {
    let word = (n as usize >> 5) * 4;
    word + 4 <= bitmap.len() && be32(bitmap, word) & (1 << (n & 31)) != 0
}

impl<'a> Fs<'a> {
    /// Check the superblock (magic, version 3 or 4, checksum) the way the firmware's mount does.
    pub fn open(img: &'a dyn RandomAccessRead) -> Result<Self, ReadError> {
        let sb = read_vec(img, u64::from(SUPERBLOCK_SECTOR) * SECTOR as u64, SECTOR);
        if be32(&sb, 0) != 0x656b_4653 {
            return Err(ReadError::NoFilesystem(format!(
                "superblock magic {:#010x}",
                be32(&sb, 0)
            )));
        }
        if !matches!(be32(&sb, 4), 3 | 4) {
            return Err(ReadError::NoFilesystem(format!(
                "superblock version {}",
                be32(&sb, 4)
            )));
        }
        let want = hashlittle(&sb[..0x1fc], 0x3132_3334);
        if want != be32(&sb, 0x1fc) {
            return Err(ReadError::NoFilesystem(format!(
                "superblock checksum {:#010x}, expected {:#010x}",
                be32(&sb, 0x1fc),
                want
            )));
        }
        if be32(&sb, 8) as usize != PAGE {
            return Err(ReadError::NoFilesystem(format!(
                "page size {:#x}",
                be32(&sb, 8)
            )));
        }
        Ok(Self { img })
    }

    pub fn record(&self, id: u32) -> Result<Record, String> {
        if !(ROOT_ID..=MAX_CARD_ID).contains(&id) {
            return Err(format!("id {id:#x} is not a card record"));
        }
        let at = (RECORD_AREA_SECTOR + u64::from(id / 256) * (PAGE / SECTOR) as u64)
            * SECTOR as u64
            + u64::from(id % 256) * RECORD_SIZE as u64;
        let raw: [u8; RECORD_SIZE] = read_vec(self.img, at, RECORD_SIZE).try_into().unwrap();
        Ok(Record {
            id,
            attr: raw[0],
            links: be16(&raw, 2),
            size: be32(&raw, 4),
            parent: be32(&raw, 8),
            hash: be32(&raw, 12),
            seq: be32(&raw, 16),
            extent_count: be16(&raw, 0x1e),
            raw,
        })
    }

    /// All extents `(logical page, pages, physical page)` of a record.
    pub fn extents(&self, rec: &Record) -> Result<Vec<(u32, u32, u32)>, String> {
        let n = usize::from(rec.extent_count);
        let mut out = Vec::with_capacity(n);
        if n <= 8 {
            for i in 0..n {
                let o = 0x20 + i * 12;
                out.push((
                    be32(&rec.raw, o),
                    be32(&rec.raw, o + 4),
                    be32(&rec.raw, o + 8),
                ));
            }
            return Ok(out);
        }
        let pages = n.div_ceil(EXTENTS_PER_PAGE);
        if pages > 24 {
            return Err(format!(
                "record {:#x}: {n} extents need {pages} index pages (at most 24)",
                rec.id
            ));
        }
        for p in 0..pages {
            let pointer = be32(&rec.raw, 0x20 + p * 4);
            let sector = page_sector(pointer).ok_or_else(|| {
                format!(
                    "record {:#x}: extent index page {pointer:#x} is not on the card",
                    rec.id
                )
            })?;
            let page = read_vec(self.img, sector * SECTOR as u64, PAGE);
            let count = (n - p * EXTENTS_PER_PAGE).min(EXTENTS_PER_PAGE);
            for i in 0..count {
                let o = i * 12;
                out.push((be32(&page, o), be32(&page, o + 4), be32(&page, o + 8)));
            }
        }
        Ok(out)
    }

    /// The first `rec.size` bytes of a record's content. Unmapped pages read as zero and are reported.
    pub fn read_content(&self, rec: &Record, issues: &mut Vec<String>) -> Result<Vec<u8>, String> {
        if rec.size > MAX_FILE_BYTES {
            return Err(format!(
                "record {:#x}: size {:#x} exceeds the {MAX_FILE_BYTES:#x} limit",
                rec.id, rec.size
            ));
        }
        let extents = self.extents(rec)?;
        let pages = (rec.size as usize).div_ceil(PAGE);
        let mut map: BTreeMap<u32, u32> = BTreeMap::new();
        for &(logical, count, physical) in &extents {
            for k in 0..count.min(pages as u32 + 1) {
                if logical.saturating_add(k) < pages as u32 {
                    map.entry(logical + k).or_insert(physical.wrapping_add(k));
                }
            }
        }
        let mut out = vec![0u8; pages * PAGE];
        let mut holes = 0;
        for lp in 0..pages {
            match map.get(&(lp as u32)).and_then(|&p| page_sector(p)) {
                Some(sector) => {
                    self.img
                        .read_at(sector * SECTOR as u64, &mut out[lp * PAGE..(lp + 1) * PAGE]);
                }
                None => holes += 1,
            }
        }
        if holes > 0 {
            issues.push(format!(
                "record {:#x}: {holes} of {pages} pages are not mapped (read as zero)",
                rec.id
            ));
        }
        out.truncate(rec.size as usize);
        Ok(out)
    }

    /// Walk the tree from the root.
    pub fn walk(&self) -> Walk {
        let mut walk = Walk::default();
        let id_bitmap = read_vec(self.img, ID_BITMAP_SECTOR * SECTOR as u64, 0x58000 / 8 + 4);
        // pages 0 .. 0xc0 sectors * 4096 bits
        let page_bitmap = read_vec(self.img, PAGE_BITMAP_SECTOR * SECTOR as u64, 0xc0 * SECTOR);
        let root = match self.record(ROOT_ID) {
            Ok(r) if r.is_dir() => r,
            Ok(_) => {
                walk.issues
                    .push("root record (id 2) is not a directory".into());
                return walk;
            }
            Err(e) => {
                walk.issues.push(e);
                return walk;
            }
        };
        let mut seen = BTreeSet::from([ROOT_ID]);
        let mut used_pages = BTreeSet::new();
        let mut stack = vec![(Vec::<u8>::new(), root, 0usize)];
        while let Some((dir_path, dir, depth)) = stack.pop() {
            let entries = match self.read_dir(&dir, &mut walk.issues) {
                Ok(e) => e,
                Err(e) => {
                    walk.issues.push(e);
                    continue;
                }
            };
            for (id, name, kind) in entries {
                let mut path = dir_path.clone();
                if !path.is_empty() {
                    path.push(b'/');
                }
                path.extend_from_slice(&name);
                if id >= RAM_ID_BASE {
                    walk.ram_entries.push(path);
                    continue;
                }
                if !seen.insert(id) {
                    walk.issues.push(format!(
                        "{}: id {id:#x} is listed twice",
                        String::from_utf8_lossy(&path)
                    ));
                    continue;
                }
                if walk.nodes.len() >= MAX_NODES {
                    walk.issues
                        .push("too many entries; the walk stopped".into());
                    return walk;
                }
                let rec = match self.record(id) {
                    Ok(r) => r,
                    Err(e) => {
                        walk.issues
                            .push(format!("{}: {e}", String::from_utf8_lossy(&path)));
                        continue;
                    }
                };
                if rec.is_dir() != (kind & 1 != 0) {
                    walk.issues.push(format!(
                        "{}: directory entry kind {kind} but record attribute {}",
                        String::from_utf8_lossy(&path),
                        rec.attr
                    ));
                }
                if rec.parent != dir.id {
                    walk.issues.push(format!(
                        "{}: record parent {:#x}, listed in {:#x}",
                        String::from_utf8_lossy(&path),
                        rec.parent,
                        dir.id
                    ));
                }
                if !is_bit(&id_bitmap, id) {
                    walk.issues.push(format!(
                        "{}: id {id:#x} is not set in the id bitmap",
                        String::from_utf8_lossy(&path)
                    ));
                }
                if let Ok(extents) = self.extents(&rec) {
                    for (_, count, physical) in extents {
                        for k in 0..count.min(0x10_0000) {
                            used_pages.insert(physical.wrapping_add(k));
                        }
                    }
                }
                if rec.is_dir() {
                    if depth + 1 >= MAX_DEPTH {
                        walk.issues.push(format!(
                            "{}: deeper than {MAX_DEPTH}",
                            String::from_utf8_lossy(&path)
                        ));
                    } else {
                        stack.push((path.clone(), rec.clone(), depth + 1));
                    }
                }
                walk.nodes.push(Node { path, record: rec });
            }
        }
        for page in used_pages {
            if page < RAM_ID_BASE
                && !is_bit(&page_bitmap, page)
                && (page as usize >> 5) * 4 + 4 <= page_bitmap.len()
            {
                walk.issues.push(format!(
                    "content page {page:#x} is used but not set in the page bitmap"
                ));
            }
        }
        let orphans = (ROOT_ID..=MAX_CARD_ID.min(0x58000 - 1))
            .filter(|&id| is_bit(&id_bitmap, id) && !seen.contains(&id))
            .count();
        if orphans > 0 {
            walk.issues.push(format!(
                "{orphans} record ids are set in the id bitmap but not reachable from the root"
            ));
        }
        walk.nodes.sort_by(|a, b| a.path.cmp(&b.path));
        walk
    }

    /// The entries of a directory except "." and "..": `(id, name, kind)`.
    fn read_dir(
        &self,
        dir: &Record,
        issues: &mut Vec<String>,
    ) -> Result<Vec<(u32, Vec<u8>, u8)>, String> {
        let content = self.read_content(dir, issues)?;
        let mut out = Vec::new();
        for (page_number, page) in content.chunks(PAGE).enumerate() {
            let mut at = 0usize;
            while at + 8 <= page.len() {
                let id = be32(page, at);
                let slot = usize::from(be16(page, at + 4));
                let name_len = usize::from(page[at + 6]);
                let kind = page[at + 7];
                if slot < 8 || at + slot > page.len() || slot % 4 != 0 {
                    if slot != 0 || id != 0 {
                        issues.push(format!(
                            "directory {:#x} page {page_number}: bad entry at {at:#x} (slot {slot:#x})",
                            dir.id
                        ));
                    }
                    break;
                }
                if name_len > slot - 8 {
                    issues.push(format!(
                        "directory {:#x} page {page_number}: name overruns its slot at {at:#x}",
                        dir.id
                    ));
                    at += slot;
                    continue;
                }
                let name = &page[at + 8..at + 8 + name_len];
                if id != 0 && name != b"." && name != b".." {
                    out.push((id, name.to_vec(), kind));
                }
                at += slot;
            }
        }
        Ok(out)
    }
}

// ----- region 1: MmcFs slots ---------------------------------------------------------------------------------------

pub const MMCFS_HEADER_MAGIC: u32 = 0xbeef_bace;
const SOUND_MAGICS: [u32; 2] = [0x4454_3153, 0x444e_3153]; // "DT1S", "DN1S"

#[derive(Clone, Debug)]
pub struct Slot {
    pub pool: Pool,
    pub index: u32,
    pub sector: u64,
    /// The slot's bytes up to the last non-zero sector (a stored slot is zero-padded to its size, and an image
    /// reads unwritten bytes as zero, so the trimmed form restores to the same card).
    pub bytes: Vec<u8>,
}

#[derive(Debug, Default)]
pub struct MmcFsRead {
    pub slots: Vec<Slot>,
    /// Boot project regions the overlay changed (sector, trimmed bytes).
    pub boot: Vec<(u64, Vec<u8>)>,
    pub issues: Vec<String>,
    /// The header was found (without it the folder has no say about `.mmcfs`).
    pub present: bool,
}

/// Bytes of `sectors` sectors at `first`, trimmed after the last non-zero sector.
fn read_trimmed(img: &dyn RandomAccessRead, first: u64, sectors: u64) -> Vec<u8> {
    const CHUNK: u64 = 256;
    let mut end = 0u64;
    let mut at = 0u64;
    let mut buf = vec![0u8; CHUNK as usize * SECTOR];
    while at < sectors {
        let n = CHUNK.min(sectors - at);
        let slice = &mut buf[..n as usize * SECTOR];
        slice.fill(0);
        img.read_at((first + at) * SECTOR as u64, slice);
        for s in 0..n {
            if slice[s as usize * SECTOR..(s as usize + 1) * SECTOR]
                .iter()
                .any(|&b| b != 0)
            {
                end = at + s + 1;
            }
        }
        at += n;
    }
    read_vec(img, first * SECTOR as u64, end as usize * SECTOR)
}

/// Read the occupied MmcFs slots.
///
/// A slot is occupied when its first sector holds data: not all zero (never written) and not all 0xFF (erased).
/// The bitmaps at sector 0x800 are not used for that: they are the slots' write-protect bits (a padlock; finding 35), and
/// a project saved from the panel (FUNC+SETTINGS, YES) writes the slot and leaves them untouched (measured, DT2 1.16).
/// A write-protect bit that is set for an empty slot is reported (the firmware only sets it for a slot that holds data). `overlay` (when known) selects the boot regions the guest wrote.
/// The project regions outside the pools are the layout's `boot-project` and `working-project` regions (the two boot
/// copies are COKi containers the builder seeds and the firmware reads at start; the working project is rewritten
/// from the boot copy at every start, FUN_4012eef2); they are exported only when the guest wrote them.
pub fn read_mmcfs(
    img: &dyn RandomAccessRead,
    layout: &CardLayout,
    overlay: Option<&OverlayImage<'_>>,
) -> MmcFsRead {
    let mut out = MmcFsRead::default();
    let header = read_vec(img, layout.header_sector * SECTOR as u64, SECTOR);
    if be32(&header, 0) != MMCFS_HEADER_MAGIC {
        out.issues.push(format!(
            "MmcFs header magic {:#010x}: no slot pools on this card",
            be32(&header, 0)
        ));
        return out;
    }
    out.present = true;
    let table = read_vec(img, layout.table_sector * SECTOR as u64, SECTOR);
    for pool in Pool::ALL {
        let (offset, length) = (
            layout.pool(pool).bitmap_offset,
            layout.pool(pool).bitmap_bytes,
        );
        let bits = &table[offset..offset + length];
        for slot in 0..layout.slot_count(pool) {
            // First sector of a slot (FUN_4012eef2, FUN_4012f596, FUN_401324cc).
            let sector = layout.slot_sector(pool, slot);
            let first = read_vec(img, sector * SECTOR as u64, SECTOR);
            let used = first.iter().any(|&b| b != 0) && first.iter().any(|&b| b != 0xff);
            let bit = bits[slot as usize >> 3] & (1 << (slot & 7)) != 0;
            if !used {
                if bit {
                    out.issues.push(format!(
                        "{} slot {slot}: write-protect bit set but the slot is empty",
                        pool.dir_name()
                    ));
                }
                continue;
            }
            let magic = be32(&first, 0);
            let known = match pool {
                Pool::Projects => magic == MMCFS_HEADER_MAGIC,
                Pool::Sounds => SOUND_MAGICS.contains(&magic),
                Pool::Kits => true,
            };
            if !known {
                out.issues.push(format!(
                    "{} slot {slot}: unexpected magic {magic:#010x}; exported as is",
                    pool.dir_name()
                ));
            }
            let bytes = read_trimmed(img, sector, (layout.slot_bytes(pool) / 512) as u64);
            out.slots.push(Slot {
                pool,
                index: slot,
                sector,
                bytes,
            });
        }
    }
    if let Some(overlay) = overlay {
        for region in layout
            .regions_of(RegionRole::BootProject)
            .chain(layout.regions_of(RegionRole::WorkingProject))
        {
            if overlay.touched_in(region.sector, region.sectors) {
                out.boot.push((
                    region.sector,
                    read_trimmed(img, region.sector, region.sectors),
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        NamedSample, NativeInfo, NativeSample, SparseImage, build_native_sample, build_sample_image,
    };

    fn sample(name: &str, pcm: &[i16]) -> (NamedSample, Vec<u8>) {
        let raw: Vec<u8> = pcm.iter().flat_map(|v| v.to_be_bytes()).collect();
        let bytes = build_native_sample(&raw, false).unwrap();
        (
            NamedSample {
                file_name: name.into(),
                sample: NativeSample {
                    info: NativeInfo {
                        src_rate: 48_000,
                        channels: 1,
                        src_frames: pcm.len(),
                        frames: pcm.len(),
                        data_len: raw.len(),
                    },
                    bytes: bytes.clone(),
                },
            },
            bytes,
        )
    }

    #[test]
    fn walks_a_built_image_and_returns_the_native_bytes() {
        let (a, ab) = sample("a.wav", &[1, 2, 3]);
        let (b, bb) = sample("b.wav", &[4; 40_000]);
        let built = build_sample_image(vec![a, b]).unwrap();
        let fs = Fs::open(&built.image).unwrap();
        let walk = fs.walk();
        assert!(walk.issues.is_empty(), "{:?}", walk.issues);
        assert_eq!(walk.nodes.len(), 2);
        let mut issues = Vec::new();
        assert_eq!(
            fs.read_content(&walk.nodes[0].record, &mut issues).unwrap(),
            ab
        );
        assert_eq!(
            fs.read_content(&walk.nodes[1].record, &mut issues).unwrap(),
            bb
        );
        assert_eq!(walk.nodes[1].path, b"b");
    }

    #[test]
    fn a_damaged_superblock_is_an_error_not_a_panic() {
        let img = SparseImage::new(0x100_0000);
        assert!(matches!(Fs::open(&img), Err(ReadError::NoFilesystem(_))));
        let built = build_sample_image(Vec::new()).unwrap();
        let mut bad = built.image.clone();
        let mut sb = [0u8; SECTOR];
        built
            .image
            .read_at(u64::from(SUPERBLOCK_SECTOR) * SECTOR as u64, &mut sb);
        sb[0x20] ^= 1;
        bad.replace_at(u64::from(SUPERBLOCK_SECTOR) * SECTOR as u64, &sb)
            .unwrap();
        assert!(Fs::open(&bad).is_err());
    }

    #[test]
    fn overlay_bytes_replace_base_bytes_by_mask() {
        let mut base = SparseImage::new(4096);
        base.write_at(0, &[7; 1024]).unwrap();
        let mut written = [0u8; 64];
        let mut data = [0u8; SECTOR];
        written[0] = 0b101;
        data[0] = 1;
        data[2] = 3;
        let overlay = [CardOverlaySector {
            sector: 1,
            written,
            data,
        }];
        let view = OverlayImage::new(&base, &overlay);
        let mut out = [9u8; 8];
        view.read_at(SECTOR as u64, &mut out);
        assert_eq!(out, [1, 7, 3, 7, 7, 7, 7, 7]);
        let again = parse_overlay(&encode_overlay(&overlay)).unwrap();
        assert_eq!(again, overlay);
        assert!(parse_overlay(&[0; 5]).is_err());
    }

    #[test]
    fn slot_addresses_match_the_firmware_formulas() {
        let dt2 = crate::dt2_layout();
        assert_eq!(dt2.slot_sector(Pool::Projects, 0), 0x58000);
        assert_eq!(dt2.slot_sector(Pool::Projects, 127), 0x8a << 15);
        assert_eq!(dt2.slot_sector(Pool::Sounds, 0), 0x12000);
        assert_eq!(dt2.slot_sector(Pool::Kits, 1), (0x2c0_0000 + 0x10000) >> 9);
        assert_eq!(page_sector(0x100_0800), Some(0x458000));
        assert_eq!(page_sector(0x100_0000), None);
    }
}
