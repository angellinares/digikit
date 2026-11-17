//! The general +Drive image builder: nested directories, native samples and
//! plain files in the path filesystem, and raw `MmcFs` slot import.
//!
//! The folder contract (`plus_drive/<device>/`) is mapped onto [`Tree`] by the
//! `plusdrive` CLI; this module only sees paths and bytes.

use std::collections::BTreeMap;

use crate::image::{
    FileReport, ImageError, MAX_NATIVE_BYTES, SparseImage, bitmap, bytes_at_sector, page_offset,
    put16, put32, record_offset, validate_native,
};
use crate::{
    ATTR_DIR, ATTR_FILE, DEFAULT_CAPACITY_BLOCKS, DEFAULT_OFFSET01, DirEntry, FACTORY_TABLE_SECTOR,
    HASH_TABLE_SECTOR, ID_BITMAP_SECTOR, NativeInfo, NativeSample, PAGE, PAGE_BITMAP_SECTOR,
    RECORD_SIZE, RESERVED_PAGES, ROOT_ID, ROOT_PARENT, SECTOR, SUPERBLOCK_SECTOR,
    build_boot_config_record, build_directory, build_factory_table_record, build_superblock,
    content_hash,
};
pub use device_profile::{CardLayout, Pool, RegionRole};

pub(crate) const HEADER_MAGIC: u32 = 0xbeef_bace;
/// Logical page of the name-hash index (0x10001: order, 0x10002: id).
const LOGICAL_HASH_PAGE: u32 = 0x10000;
/// Extents per indirect extent-index page (32768 / 12, as the firmware divides).
pub const EXTENTS_PER_INDEX_PAGE: usize = 0xaaa;
/// Inline extents in a record; more than this and the record holds pointers
/// to extent-index pages instead (at most 24).
pub const INLINE_EXTENTS: usize = 8;
const MAX_INDEX_PAGES: usize = 24;
/// Largest total of file bytes in one tree.
pub const MAX_TREE_BYTES: usize = 1 << 30;
/// Highest record id the firmware accepts (`FUN_4015b64e`).
const MAX_ID: u32 = 0x57fff;
pub const POOL_TABLE_BYTES: usize = 400;

/// The card layout of a device by its short name (`dt2`, `dn2`), from the device profiles (`devices/*.toml`,
/// `[card]`; `$DIGI_DEVICES_DIR` and the embedded copy as for the other profile lookups).
pub fn card_layout(device: &str) -> Result<CardLayout, String> {
    let registry = device_profile::Registry::load().map_err(|e| e.to_string())?;
    registry
        .card_layout(device)
        .cloned()
        .ok_or_else(|| format!("no card layout for device {device}"))
}

/// Same, for the DT2 (the default of the builder and of the tests).
pub fn dt2_layout() -> CardLayout {
    card_layout("dt2").expect("the DT2 profile has a [card] section")
}

/// Raw slot contents by pool and slot index.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MmcSlots {
    pub slots: BTreeMap<(Pool, u32), Vec<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Content {
    /// A native sample (the converted WAVs and any file that passes the
    /// native-format check). The name loses a `.wav` suffix.
    Native(NativeSample),
    /// Any other file, stored byte for byte.
    Plain(Vec<u8>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeFile {
    /// Relative, `/`-separated, no leading slash.
    pub path: String,
    pub content: Content,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Tree {
    pub files: Vec<TreeFile>,
    /// Directories to create even when empty (relative paths).
    pub dirs: Vec<String>,
    pub slots: MmcSlots,
    /// Raw 16 MiB regions outside the pools by first sector (the boot and working projects of the layout). A
    /// region given here replaces the generated boot project at that sector.
    pub regions: BTreeMap<u64, Vec<u8>>,
    /// Files of `digikit/` for the module store.
    pub store: Vec<crate::StoreFile>,
    /// The runtime's selection (`digikit/selection.txt`): store paths in load order. `Some` writes both copies of the record
    /// (docs/design/digikit-store.md); `None` leaves the two sectors zero (no record).
    pub selection: Option<Vec<String>>,
}

#[derive(Clone, Debug)]
pub struct TreeOptions {
    pub capacity_blocks: u32,
    pub layout: CardLayout,
    /// Longest extent in pages. Files and directories longer than this get
    /// several (adjacent) extents. Nothing in the layout needs it; it
    /// exercises multi-extent and indirect-extent records.
    pub max_extent_pages: u32,
}
impl Default for TreeOptions {
    fn default() -> Self {
        Self {
            capacity_blocks: DEFAULT_CAPACITY_BLOCKS,
            layout: dt2_layout(),
            max_extent_pages: u32::MAX,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    Dir,
    Native,
    Plain,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EntryReport {
    /// The path as stored (names normalised), `/`-separated, no leading slash.
    pub path: String,
    pub id: u32,
    pub kind: EntryKind,
    pub size: u32,
    pub first_page: u32,
    /// Content pages (data pages; index pages not counted).
    pub page_count: u32,
    pub extents: usize,
}

/// Read a native sample from raw bytes: `Some` when they pass the same check
/// the builder applies to every sample.
pub fn parse_native(bytes: Vec<u8>) -> Option<NativeSample> {
    if bytes.len() < 0x50 || bytes[1] > 1 {
        return None;
    }
    let declared = u32::from_be_bytes(bytes[4..8].try_into().ok()?) as usize;
    let channels = usize::from(bytes[1]) + 1;
    let sample = NativeSample {
        info: NativeInfo {
            src_rate: 48_000,
            channels: channels as u16,
            src_frames: declared / (channels * 2),
            frames: declared / (channels * 2),
            data_len: declared,
        },
        bytes,
    };
    validate_native(&sample).then_some(sample)
}

/// A stored name: non-ASCII becomes `?`, 255 bytes at most. A `.wav` suffix
/// goes only from a native sample (the converted WAVs).
pub fn stored_name(component: &str, native: bool) -> Result<Vec<u8>, ImageError> {
    let stem = if native
        && component.len() >= 4
        && component.as_bytes()[component.len() - 4..].eq_ignore_ascii_case(b".wav")
    {
        &component[..component.len() - 4]
    } else {
        component
    };
    let mut result = Vec::with_capacity(stem.len().min(255));
    for character in stem.chars() {
        result.push(if character.is_ascii() {
            character as u8
        } else {
            b'?'
        });
        if result.len() == 255 {
            break;
        }
    }
    if result.is_empty() || result == b"." || result == b".." || result.contains(&b'/') {
        return Err(ImageError::InvalidName {
            file_name: component.to_owned(),
        });
    }
    Ok(result)
}

/// `stored_name` of a converted WAV (tests).
#[cfg(test)]
pub(crate) fn stored_name_native(component: &str) -> Result<Vec<u8>, ImageError> {
    stored_name(component, true)
}

enum Node {
    File(Content),
    Dir(BTreeMap<String, Node>),
}

fn insert(
    dir: &mut BTreeMap<String, Node>,
    parts: &[&str],
    leaf: Option<Content>,
    whole: &str,
) -> Result<(), ImageError> {
    let invalid = || ImageError::InvalidName {
        file_name: whole.to_owned(),
    };
    let (first, rest) = parts.split_first().ok_or_else(invalid)?;
    if rest.is_empty() {
        let node = match leaf {
            Some(content) => Node::File(content),
            None => Node::Dir(BTreeMap::new()),
        };
        return match dir.get(*first) {
            None => {
                dir.insert((*first).to_owned(), node);
                Ok(())
            }
            // An explicit empty directory that already exists is fine.
            Some(Node::Dir(_)) if matches!(node, Node::Dir(_)) => Ok(()),
            Some(_) => Err(ImageError::DuplicateName {
                name: whole.as_bytes().to_vec(),
            }),
        };
    }
    match dir
        .entry((*first).to_owned())
        .or_insert_with(|| Node::Dir(BTreeMap::new()))
    {
        Node::Dir(children) => insert(children, rest, leaf, whole),
        Node::File(_) => Err(ImageError::DuplicateName {
            name: whole.as_bytes().to_vec(),
        }),
    }
}

/// Where an allocated record's data lives.
struct Alloc {
    next_id: u32,
    next_page: u32,
}
impl Alloc {
    fn id(&mut self) -> Result<u32, ImageError> {
        let id = self.next_id;
        if id > MAX_ID {
            return Err(ImageError::CapacityOverflow);
        }
        self.next_id += 1;
        Ok(id)
    }
    fn pages(&mut self, count: u32) -> Result<u32, ImageError> {
        let first = self.next_page;
        self.next_page = first
            .checked_add(count)
            .ok_or(ImageError::CapacityOverflow)?;
        Ok(first)
    }
}

/// Split `[first, first + count)` into extents of at most `max` pages,
/// logical pages starting at `logical`.
fn extents(logical: u32, first: u32, count: u32, max: u32) -> Vec<(u32, u32, u32)> {
    let max = max.max(1);
    let mut out = Vec::new();
    let mut done = 0;
    while done < count {
        let length = (count - done).min(max);
        out.push((logical + done, length, first + done));
        done += length;
    }
    out
}

/// A record with inline extents (at most 8) or pointers to extent-index pages.
#[allow(clippy::too_many_arguments)]
fn make_record(
    attr: u8,
    size: u32,
    parent: u32,
    hash: u32,
    seq: u32,
    links: u16,
    extent_count: usize,
    table: &[u8],
) -> [u8; RECORD_SIZE] {
    let mut out = [0; RECORD_SIZE];
    out[0] = attr;
    out[1] = DEFAULT_OFFSET01;
    put16(&mut out, 2, links);
    put32(&mut out, 4, size);
    put32(&mut out, 8, parent);
    put32(&mut out, 12, hash);
    put32(&mut out, 16, seq);
    put16(&mut out, 0x1e, extent_count as u16);
    out[0x20..0x20 + table.len()].copy_from_slice(table);
    out
}

struct Writer<'a> {
    image: &'a mut SparseImage,
    alloc: Alloc,
    hash_pages: BTreeMap<u32, Vec<u8>>,
    id_bits: Vec<u32>,
    reports: Vec<EntryReport>,
    natives: Vec<FileReport>,
    max_extent_pages: u32,
}

impl Writer<'_> {
    /// Write the extent list of a record: inline up to 8, else through
    /// extent-index pages allocated now. Returns the 96-byte table.
    fn extent_table(&mut self, list: &[(u32, u32, u32)]) -> Result<Vec<u8>, ImageError> {
        let mut table = vec![0; 96];
        if list.len() <= INLINE_EXTENTS {
            for (i, &(logical, pages, physical)) in list.iter().enumerate() {
                put32(&mut table, i * 12, logical);
                put32(&mut table, i * 12 + 4, pages);
                put32(&mut table, i * 12 + 8, physical);
            }
            return Ok(table);
        }
        let index_pages = list.len().div_ceil(EXTENTS_PER_INDEX_PAGE);
        if index_pages > MAX_INDEX_PAGES {
            return Err(ImageError::CapacityOverflow);
        }
        for (k, chunk) in list.chunks(EXTENTS_PER_INDEX_PAGE).enumerate() {
            let page = self.alloc.pages(1)?;
            let mut data = vec![0; PAGE];
            for (i, &(logical, pages, physical)) in chunk.iter().enumerate() {
                put32(&mut data, i * 12, logical);
                put32(&mut data, i * 12 + 4, pages);
                put32(&mut data, i * 12 + 8, physical);
            }
            self.image.write_at(page_offset(page)?, &data)?;
            put32(&mut table, k * 4, page);
        }
        Ok(table)
    }

    fn write_pages(&mut self, first: u32, bytes: &[u8], count: u32) -> Result<(), ImageError> {
        for page in 0..count {
            let start = page as usize * PAGE;
            let end = bytes.len().min(start + PAGE);
            if start < end {
                self.image
                    .write_at(page_offset(first + page)?, &bytes[start..end])?;
            }
        }
        Ok(())
    }

    fn set_hash(&mut self, id: u32, hash: u32) {
        let page = self
            .hash_pages
            .entry(id >> 13)
            .or_insert_with(|| vec![0; PAGE]);
        let at = (id as usize & 0x1fff) * 4;
        page[at..at + 4].copy_from_slice(&hash.to_be_bytes());
    }

    fn dir(
        &mut self,
        id: u32,
        parent: u32,
        path: &str,
        source: &str,
        children: BTreeMap<String, Node>,
    ) -> Result<(), ImageError> {
        let mut seen: BTreeMap<Vec<u8>, ()> = BTreeMap::new();
        // Pass 1: names, ids, entries.
        struct Child {
            id: u32,
            name: String,
            stored: Vec<u8>,
            node: Node,
        }
        let mut list = Vec::with_capacity(children.len());
        let mut subdirs = 0u16;
        for (name, node) in children {
            let native = matches!(node, Node::File(Content::Native(_)));
            let stored = stored_name(&name, native)?;
            if parent == ROOT_PARENT && id == ROOT_ID && stored == b"factory" {
                return Err(ImageError::InvalidName { file_name: name });
            }
            if seen.insert(stored.clone(), ()).is_some() {
                return Err(ImageError::DuplicateName { name: stored });
            }
            if matches!(node, Node::Dir(_)) {
                subdirs += 1;
            }
            list.push(Child {
                id: self.alloc.id()?,
                name,
                stored,
                node,
            });
        }
        let entries: Vec<DirEntry> = list
            .iter()
            .map(|c| DirEntry {
                id: c.id,
                name: c.stored.clone(),
                kind: if matches!(c.node, Node::Dir(_)) {
                    ATTR_DIR
                } else {
                    ATTR_FILE
                },
            })
            .collect();
        let pages = build_directory(id, parent, &entries)?;
        let content_pages = (pages.contents.len() / PAGE) as u32;
        let content_first = self.alloc.pages(content_pages)?;
        let index_first = self.alloc.pages(3)?;
        self.write_pages(content_first, &pages.contents, content_pages)?;
        self.image
            .write_at(page_offset(index_first)?, &pages.by_hash)?;
        self.image
            .write_at(page_offset(index_first + 1)?, &pages.listing)?;
        self.image
            .write_at(page_offset(index_first + 2)?, &pages.by_id)?;
        let mut list_ext = extents(0, content_first, content_pages, self.max_extent_pages);
        list_ext.push((LOGICAL_HASH_PAGE, 3, index_first));
        let table = self.extent_table(&list_ext)?;
        let size = content_pages * PAGE as u32;
        self.image.write_at(
            record_offset(id)?,
            &make_record(
                ATTR_DIR,
                size,
                parent,
                0,
                id,
                2 + subdirs,
                list_ext.len(),
                &table,
            ),
        )?;
        self.id_bits.push(id);
        self.reports.push(EntryReport {
            path: path.to_owned(),
            id,
            kind: EntryKind::Dir,
            size,
            first_page: content_first,
            page_count: content_pages,
            extents: list_ext.len(),
        });
        // Pass 2: files in this directory, then the subdirectories.
        let mut dirs = Vec::new();
        for child in list {
            let child_path = if path.is_empty() {
                String::from_utf8_lossy(&child.stored).into_owned()
            } else {
                format!("{path}/{}", String::from_utf8_lossy(&child.stored))
            };
            let child_source = if source.is_empty() {
                child.name.clone()
            } else {
                format!("{source}/{}", child.name)
            };
            match child.node {
                Node::Dir(grand) => dirs.push((child.id, child_path, child_source, grand)),
                Node::File(content) => self.file(
                    child.id,
                    id,
                    (&child_path, &child_source),
                    &child.stored,
                    content,
                )?,
            }
        }
        for (child_id, child_path, child_source, grand) in dirs {
            self.dir(child_id, id, &child_path, &child_source, grand)?;
        }
        Ok(())
    }

    fn file(
        &mut self,
        id: u32,
        parent: u32,
        (path, source): (&str, &str),
        stored: &[u8],
        content: Content,
    ) -> Result<(), ImageError> {
        let (bytes, native) = match &content {
            Content::Native(sample) => (&sample.bytes, Some(sample)),
            Content::Plain(bytes) => (bytes, None),
        };
        let size = u32::try_from(bytes.len()).map_err(|_| ImageError::CapacityOverflow)?;
        let page_count = u32::try_from(bytes.len().div_ceil(PAGE).max(1))
            .map_err(|_| ImageError::CapacityOverflow)?;
        let first_page = self.alloc.pages(page_count)?;
        self.write_pages(first_page, bytes, page_count)?;
        let list = extents(0, first_page, page_count, self.max_extent_pages);
        let table = self.extent_table(&list)?;
        let hash = content_hash(bytes) | 1;
        self.image.write_at(
            record_offset(id)?,
            &make_record(ATTR_FILE, size, parent, hash, id, 1, list.len(), &table),
        )?;
        self.set_hash(id, hash);
        self.id_bits.push(id);
        self.reports.push(EntryReport {
            path: path.to_owned(),
            id,
            kind: if native.is_some() {
                EntryKind::Native
            } else {
                EntryKind::Plain
            },
            size,
            first_page,
            page_count,
            extents: list.len(),
        });
        if let Some(sample) = native {
            let mut reference = [0; 16];
            put32(&mut reference, 0, id);
            put32(&mut reference, 4, hash);
            put32(&mut reference, 8, size);
            put32(&mut reference, 12, id);
            self.natives.push(FileReport {
                file_name: source.to_owned(),
                stored_name: stored.to_vec(),
                id,
                seq: id,
                hash,
                reference,
                first_page,
                page_count,
                info: sample.info.clone(),
            });
        }
        Ok(())
    }
}

/// Build the image of a [`Tree`] for the card layout of `options`. A boot project (`project_seed`) is written
/// only when the tree holds a native sample for its references.
///
/// Boot regions: the generated boot record (and, with a seed and a native sample, the boot project) goes to every
/// `boot-project` region of the layout; a region in `tree.regions` replaces the generated bytes at its sector
/// (the generated project is then not written there). Working-project regions are written only from `tree.regions`.
pub fn build_tree_image(
    tree: Tree,
    options: TreeOptions,
    project_seed: Option<crate::ProjectSeed<'_>>,
) -> Result<crate::BuiltImage, ImageError> {
    let layout = &options.layout;
    // The store lies at the end of the card (above the +Drive): an image that holds store files or a selection reaches the end of its area.
    let store_end = match &layout.store {
        Some(s) if !tree.store.is_empty() || tree.selection.is_some() => s.end_sector,
        _ => 0,
    };
    let capacity = u64::from(options.capacity_blocks)
        .max(store_end)
        .checked_mul(SECTOR as u64)
        .ok_or(ImageError::CapacityOverflow)?;
    let path_fs = match &layout.path_fs {
        Some(fs) if fs.superblock_sector == u64::from(SUPERBLOCK_SECTOR) => true,
        Some(_) => return Err(ImageError::UnsupportedLayout("path_fs.superblock_sector")),
        None => false,
    };
    if !path_fs && !(tree.files.is_empty() && tree.dirs.is_empty()) {
        return Err(ImageError::NoPathFilesystem);
    }
    if path_fs {
        let factory_end = bytes_at_sector(FACTORY_TABLE_SECTOR)?
            .checked_add(PAGE as u64)
            .ok_or(ImageError::CapacityOverflow)?;
        if capacity < factory_end {
            return Err(ImageError::FixedRegionsDoNotFit {
                capacity_blocks: options.capacity_blocks,
            });
        }
    }

    // The tree of nodes, with the size limits.
    let mut root = BTreeMap::new();
    let (mut native_total, mut total) = (0usize, 0usize);
    for file in tree.files {
        let size = match &file.content {
            Content::Native(sample) => {
                if !validate_native(sample) {
                    return Err(ImageError::InvalidNativeSample {
                        file_name: file.path,
                    });
                }
                native_total = native_total.saturating_add(sample.bytes.len());
                sample.bytes.len()
            }
            Content::Plain(bytes) => bytes.len(),
        };
        total = total.saturating_add(size);
        if native_total > MAX_NATIVE_BYTES {
            return Err(ImageError::TooManyNativeBytes {
                maximum: MAX_NATIVE_BYTES,
            });
        }
        if total > MAX_TREE_BYTES {
            return Err(ImageError::TooManyNativeBytes {
                maximum: MAX_TREE_BYTES,
            });
        }
        let parts: Vec<&str> = file.path.split('/').collect();
        insert(&mut root, &parts, Some(file.content), &file.path)?;
    }
    for dir in &tree.dirs {
        let parts: Vec<&str> = dir.split('/').collect();
        insert(&mut root, &parts, None, dir)?;
    }

    let mut image = SparseImage::new(capacity);
    let mut header = [0; SECTOR];
    put32(&mut header, 0, HEADER_MAGIC);
    put32(&mut header, 4, 1);
    header[8] = 1;
    header[9] = 1;
    image.write_at(bytes_at_sector(layout.header_sector)?, &header)?;
    if path_fs {
        image.write_at(
            bytes_at_sector(u64::from(SUPERBLOCK_SECTOR))?,
            &build_superblock(),
        )?;
        image.write_at(
            bytes_at_sector(FACTORY_TABLE_SECTOR)?,
            &build_factory_table_record(),
        )?;
    }

    // Boot and working project regions.
    for sector in tree.regions.keys() {
        let region = layout
            .region_at(*sector)
            .ok_or(ImageError::UnknownRegion { sector: *sector })?;
        let data = &tree.regions[sector];
        let limit = (region.sectors as usize) * SECTOR;
        if data.is_empty() || data.len() > limit {
            return Err(ImageError::RegionSize {
                sector: *sector,
                bytes: data.len(),
                limit,
            });
        }
    }
    let boot = build_boot_config_record();
    let mut generated: Vec<u64> = Vec::new();
    for region in layout.regions_of(RegionRole::BootProject) {
        match tree.regions.get(&region.sector) {
            Some(given) => image.write_at(bytes_at_sector(region.sector)?, given)?,
            None => {
                image.write_at(bytes_at_sector(region.sector)?, &boot)?;
                generated.push(region.sector);
            }
        }
    }
    for region in layout.regions_of(RegionRole::WorkingProject) {
        if let Some(given) = tree.regions.get(&region.sector) {
            image.write_at(bytes_at_sector(region.sector)?, given)?;
        }
    }

    // Raw MmcFs slots. The table sector (400 bytes) holds the pools' WRITE-PROTECT bits (a set bit is a padlock: the
    // firmware refuses to save over, rename or delete the slot; finding 35), not occupancy, so an imported slot leaves
    // its bit clear. The table is written all zero: no slot protected, and the firmware reads it as valid.
    let pool_table = [0u8; SECTOR];
    for ((pool, slot), data) in &tree.slots.slots {
        if *slot >= layout.slot_count(*pool) {
            return Err(ImageError::SlotOutOfRange {
                pool: pool.dir_name(),
                slot: *slot,
            });
        }
        let limit = layout.slot_bytes(*pool);
        if data.is_empty() || data.len() > limit {
            return Err(ImageError::SlotSize {
                pool: pool.dir_name(),
                slot: *slot,
                bytes: data.len(),
                limit,
            });
        }
        image.write_at(bytes_at_sector(layout.slot_sector(*pool, *slot))?, data)?;
    }
    image.write_at(bytes_at_sector(layout.table_sector)?, &pool_table)?;

    // The module store.
    if !tree.store.is_empty() || tree.selection.is_some() {
        let range = layout.store.as_ref().ok_or(ImageError::NoStore)?;
        if range.selection_sectors < 2
            || range.end_sector - range.first_sector <= range.selection_sectors
        {
            return Err(ImageError::UnsupportedLayout(
                "the store range needs two selection record sectors at its end",
            ));
        }
        if !tree.store.is_empty() {
            // the files keep out of the selection record sectors at the end of the range
            // and out of header copy B, the sector before them
            let bytes =
                crate::encode_store(tree.store, range.store_limit()).map_err(ImageError::Store)?;
            image.write_at(bytes_at_sector(range.first_sector)?, &bytes)?;
            image.write_at(bytes_at_sector(range.header_b())?, &bytes[..crate::SECTOR])?;
        }
        if let Some(paths) = tree.selection {
            let record = crate::encode_selection(&crate::Selection {
                seq: 1,
                boot_counter: 0,
                flags: 0,
                paths,
            })
            .map_err(ImageError::Selection)?;
            // copy A at end - 2, copy B at end - 1
            image.write_at(bytes_at_sector(range.end_sector - 2)?, &record)?;
            image.write_at(bytes_at_sector(range.end_sector - 1)?, &record)?;
        }
    }

    if !path_fs {
        return Ok(crate::BuiltImage {
            image,
            files: Vec::new(),
            entries: Vec::new(),
            project: None,
        });
    }

    let mut writer = Writer {
        image: &mut image,
        alloc: Alloc {
            next_id: ROOT_ID + 1,
            next_page: RESERVED_PAGES,
        },
        hash_pages: BTreeMap::new(),
        id_bits: vec![0, 1],
        reports: Vec::new(),
        natives: Vec::new(),
        max_extent_pages: options.max_extent_pages,
    };
    writer
        .dir(ROOT_ID, ROOT_PARENT, "", "", root)
        .map_err(|error| match error {
            ImageError::Sparse(crate::SparseImageError::OutOfBounds { .. }) => {
                ImageError::ContentDoesNotFit {
                    capacity_blocks: options.capacity_blocks,
                }
            }
            other => other,
        })?;
    let Writer {
        alloc,
        hash_pages,
        id_bits,
        reports,
        natives,
        ..
    } = writer;
    if page_offset(alloc.next_page)? > capacity {
        return Err(ImageError::ContentDoesNotFit {
            capacity_blocks: options.capacity_blocks,
        });
    }
    for (page, data) in hash_pages {
        image.write_at(
            bytes_at_sector(HASH_TABLE_SECTOR + u64::from(page) * (PAGE / SECTOR) as u64)?,
            &data,
        )?;
    }
    image.write_at(bytes_at_sector(ID_BITMAP_SECTOR)?, &bitmap(id_bits))?;
    image.write_at(
        bytes_at_sector(PAGE_BITMAP_SECTOR)?,
        &bitmap(0..alloc.next_page),
    )?;
    let project = match project_seed {
        Some(seed) if !natives.is_empty() => {
            let (record, report) = crate::build_project_record(
                seed,
                &natives.iter().map(|f| f.reference).collect::<Vec<_>>(),
                alloc.next_id,
            )?;
            for sector in &generated {
                image.replace_at(bytes_at_sector(*sector)?, &record)?;
            }
            Some(report)
        }
        _ => None,
    };
    let mut entries = reports;
    entries.sort_by_key(|e| e.id);
    Ok(crate::BuiltImage {
        image,
        files: natives,
        entries,
        project,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BuiltImage, MAX_DIR_ENTRIES, build_native_sample, name_hash};

    // A minimal reader for the tests only: it follows the firmware's own
    // rules (record, extent lookup, directory pages, hash index search).
    struct View<'a>(&'a SparseImage);
    impl View<'_> {
        fn read(&self, offset: u64, len: usize) -> Vec<u8> {
            let mut out = vec![0; len];
            self.0.read_at(offset, &mut out);
            out
        }
        fn record(&self, id: u32) -> Vec<u8> {
            self.read(record_offset(id).unwrap(), RECORD_SIZE)
        }
        fn page(&self, physical: u32) -> Vec<u8> {
            self.read(page_offset(physical).unwrap(), PAGE)
        }
        fn u32(b: &[u8], at: usize) -> u32 {
            u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
        }
        fn extents(&self, id: u32) -> Vec<(u32, u32, u32)> {
            let r = self.record(id);
            let n = u16::from_be_bytes([r[0x1e], r[0x1f]]) as usize;
            (0..n)
                .map(|i| {
                    if n < 9 {
                        let o = 0x20 + i * 12;
                        (Self::u32(&r, o), Self::u32(&r, o + 4), Self::u32(&r, o + 8))
                    } else {
                        let index = self.page(Self::u32(&r, 0x20 + (i / 0xaaa) * 4));
                        let o = (i % 0xaaa) * 12;
                        (
                            Self::u32(&index, o),
                            Self::u32(&index, o + 4),
                            Self::u32(&index, o + 8),
                        )
                    }
                })
                .collect()
        }
        /// The firmware's `FUN_40157e60` + `FUN_40155c16`: a logical page.
        fn logical(&self, id: u32, logical: u32) -> Option<Vec<u8>> {
            self.extents(id)
                .into_iter()
                .find(|&(l, n, _)| (l..l + n).contains(&logical))
                .map(|(l, _, p)| self.page(p + logical - l))
        }
        fn size(&self, id: u32) -> usize {
            Self::u32(&self.record(id), 4) as usize
        }
        fn file(&self, id: u32) -> Vec<u8> {
            let size = self.size(id);
            let mut out = Vec::new();
            for page in 0..size.div_ceil(PAGE) as u32 {
                out.extend(self.logical(id, page).unwrap());
            }
            out.truncate(size);
            out
        }
        /// (id, name, kind) of every entry by walking the content pages.
        fn entries(&self, dir: u32) -> Vec<(u32, Vec<u8>, u8)> {
            let mut out = Vec::new();
            for page in 0..(self.size(dir) / PAGE) as u32 {
                let data = self.logical(dir, page).unwrap();
                let mut at = 0;
                while at < PAGE {
                    let id = Self::u32(&data, at);
                    let slot = u16::from_be_bytes([data[at + 4], data[at + 5]]) as usize;
                    let len = data[at + 6] as usize;
                    assert!(slot >= 8 + len && at + slot <= PAGE, "slot at {at}");
                    out.push((id, data[at + 8..at + 8 + len].to_vec(), data[at + 7]));
                    at += slot;
                }
                assert_eq!(at, PAGE, "slots of page {page} end on the page end");
            }
            out
        }
        /// An index page as (key, page, offset).
        fn index(&self, dir: u32, logical: u32) -> Vec<(u32, u32, usize)> {
            let p = self.logical(dir, logical).unwrap();
            let n = u16::from_be_bytes([p[0], p[1]]) as usize;
            (0..n)
                .map(|i| {
                    let v = Self::u32(&p, 12 + i * 8);
                    (Self::u32(&p, 8 + i * 8), v >> 15, (v & 0x7fff) as usize)
                })
                .collect()
        }
        /// `FUN_401572e0`: binary search on the hash index, then compare the name.
        fn lookup(&self, dir: u32, name: &[u8]) -> Option<u32> {
            let key = name_hash(name);
            let index = self.index(dir, 0x10000);
            let first = index.partition_point(|e| e.0 < key);
            for &(k, page, off) in &index[first..] {
                if k != key {
                    break;
                }
                let data = self.logical(dir, page).unwrap();
                let len = data[off + 6] as usize;
                if &data[off + 8..off + 8 + len] == name {
                    return Some(Self::u32(&data, off));
                }
            }
            None
        }
    }

    fn plain(path: &str, bytes: Vec<u8>) -> TreeFile {
        TreeFile {
            path: path.into(),
            content: Content::Plain(bytes),
        }
    }
    fn native(path: &str, frames: usize) -> TreeFile {
        let pcm = vec![7u8; frames * 2];
        let bytes = build_native_sample(&pcm, false).unwrap();
        TreeFile {
            path: path.into(),
            content: Content::Native(parse_native(bytes).unwrap()),
        }
    }
    fn build(files: Vec<TreeFile>, options: TreeOptions) -> BuiltImage {
        build_tree_image(
            Tree {
                files,
                ..Default::default()
            },
            options,
            None,
        )
        .unwrap()
    }
    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 7 + i / 251) as u8).collect()
    }
    fn id_of(built: &BuiltImage, path: &str) -> u32 {
        built.entries.iter().find(|e| e.path == path).unwrap().id
    }

    #[test]
    fn nested_directories_plain_files_and_indexes() {
        let module = pattern(5000);
        let built = build(
            vec![
                plain("digikit/modules/test.dkm", module.clone()),
                plain("digikit/readme.txt", b"hello".to_vec()),
                native("kicks/Kick 1.wav", 100),
                plain("top", vec![1; PAGE + 1]),
            ],
            TreeOptions::default(),
        );
        let v = View(&built.image);
        let dkm = id_of(&built, "digikit/modules/test.dkm");
        assert_eq!(v.file(dkm), module);
        assert_eq!(v.file(id_of(&built, "top")).len(), PAGE + 1);
        let root = v.entries(ROOT_ID);
        let names: Vec<_> = root.iter().map(|e| e.1.clone()).collect();
        assert_eq!(
            names,
            [&b"."[..], b"..", b"digikit", b"kicks", b"top"]
                .iter()
                .map(|n| n.to_vec())
                .collect::<Vec<_>>()
        );
        // Every directory: "." and ".." first, parent ids, the three index
        // pages sorted and pointing at entries with the right key.
        for (path, id) in built
            .entries
            .iter()
            .filter(|e| e.kind == EntryKind::Dir)
            .map(|e| (e.path.clone(), e.id))
        {
            let entries = v.entries(id);
            assert_eq!((entries[0].1.clone(), entries[0].0), (b".".to_vec(), id));
            assert_eq!(entries[1].1, b"..");
            assert_eq!(entries[1].0, View::u32(&v.record(id), 8), "{path}");
            let hash = v.index(id, 0x10000);
            assert!(hash.windows(2).all(|w| w[0].0 <= w[1].0));
            let by_id = v.index(id, 0x10002);
            assert!(by_id.windows(2).all(|w| w[0].0 <= w[1].0));
            assert_eq!(hash.len(), entries.len());
            assert_eq!(v.index(id, 0x10001).len(), entries.len());
            for (_, name, _) in &entries {
                let found = v.lookup(id, name).unwrap();
                assert!(entries.iter().any(|e| e.0 == found && &e.1 == name));
            }
            assert_eq!(v.lookup(id, b"missing"), None);
        }
        // Records carry the parent; children are found by path.
        let modules = v
            .lookup(v.lookup(ROOT_ID, b"digikit").unwrap(), b"modules")
            .unwrap();
        assert_eq!(v.lookup(modules, b"test.dkm"), Some(dkm));
        assert_eq!(View::u32(&v.record(dkm), 8), modules);
        assert_eq!(v.record(modules)[0], ATTR_DIR);
        assert_eq!(v.record(dkm)[0], ATTR_FILE);
        // The kind byte in the entry copies the record's.
        assert!(root.iter().find(|e| e.1 == b"kicks").unwrap().2 == ATTR_DIR);
        // The .wav suffix goes for the native sample only; its report exists.
        assert!(built.entries.iter().any(|e| e.path == "kicks/Kick 1"));
        assert_eq!(built.files.len(), 1);
        assert_eq!(built.files[0].file_name, "kicks/Kick 1.wav");
        // Hash table entry = record word +0x0c = content hash | 1.
        let rec = v.record(dkm);
        let hash = View::u32(&rec, 12);
        assert_eq!(hash, content_hash(&module) | 1);
        let table = v.read(
            bytes_at_sector(HASH_TABLE_SECTOR + (u64::from(dkm) >> 13) * 64).unwrap()
                + (u64::from(dkm) & 0x1fff) * 4,
            4,
        );
        assert_eq!(View::u32(&table, 0), hash);
        // Bitmaps (big-endian u32 words, bit n & 31 of word n >> 5).
        let bit = |map: &[u8], n: u32| {
            map[(n as usize >> 5) * 4 + 3 - ((n as usize >> 3) & 3)] >> (n & 7) & 1
        };
        let ids = v.read(bytes_at_sector(ID_BITMAP_SECTOR).unwrap(), PAGE);
        for id in built.entries.iter().map(|e| e.id).chain([0, 1, 2]) {
            assert_eq!(bit(&ids, id), 1, "id {id}");
        }
        assert_eq!(bit(&ids, built.entries.len() as u32 + 3), 0);
        let pages = v.read(bytes_at_sector(PAGE_BITMAP_SECTOR).unwrap(), PAGE);
        let last = built
            .entries
            .iter()
            .map(|e| e.first_page + e.page_count)
            .max()
            .unwrap();
        assert!((0..last).all(|p| bit(&pages, p) == 1));
        assert_eq!(bit(&pages, last + 40), 0);
    }

    #[test]
    fn directory_of_600_entries_spans_pages() {
        let files: Vec<_> = (0..600)
            .map(|i| {
                plain(
                    &format!("d/file-{i:04}-{}", "n".repeat(i % 90)),
                    vec![i as u8],
                )
            })
            .collect();
        let built = build(files, TreeOptions::default());
        let v = View(&built.image);
        let d = v.lookup(ROOT_ID, b"d").unwrap();
        let entries = v.entries(d);
        assert_eq!(entries.len(), 602);
        let pages = v.size(d) / PAGE;
        assert!(pages >= 2, "{pages} pages");
        // The three indexes and the content have separate extents; the
        // content extent covers all pages.
        let ext = v.extents(d);
        assert_eq!(
            ext,
            vec![(0, pages as u32, ext[0].2), (0x10000, 3, ext[1].2)]
        );
        for (_, name, _) in &entries {
            assert!(v.lookup(d, name).is_some());
        }
        // Pointers use the logical page: entries are found on later pages.
        let late = v.index(d, 0x10000).iter().filter(|e| e.1 > 0).count();
        assert!(late > 0);
        // Order index: directories first, then names; "." and ".." lead.
        let order = v.index(d, 0x10001);
        assert_eq!(order[0].0, u32::from_be_bytes([b'.', 0, 0, 0]));
        assert_eq!(order[1].0, u32::from_be_bytes([b'.', b'.', 0, 0]));
    }

    #[test]
    fn too_many_entries_in_one_directory_is_an_error() {
        let files: Vec<_> = (0..MAX_DIR_ENTRIES - 1)
            .map(|i| plain(&format!("f{i}"), vec![0]))
            .collect();
        assert!(matches!(
            build_tree_image(
                Tree {
                    files,
                    ..Default::default()
                },
                TreeOptions::default(),
                None
            ),
            Err(ImageError::Format(crate::FormatError::TooManyEntries))
        ));
    }

    #[test]
    fn files_with_several_extents_inline_and_indirect() {
        let options = TreeOptions {
            max_extent_pages: 4,
            ..Default::default()
        };
        let three = pattern(PAGE * 10 - 5);
        let ten = pattern(PAGE * 37 + 11);
        let built = build(
            vec![plain("a", three.clone()), plain("b", ten.clone())],
            options,
        );
        let v = View(&built.image);
        let a = id_of(&built, "a");
        let b = id_of(&built, "b");
        assert_eq!(v.extents(a).len(), 3);
        assert_eq!(v.file(a), three);
        // 38 pages in extents of 4: 10 extents, so indirect.
        let ext = v.extents(b);
        assert_eq!(ext.len(), 10);
        assert_eq!(
            u16::from_be_bytes([v.record(b)[0x1e], v.record(b)[0x1f]]),
            10
        );
        let mut next = 0;
        for &(logical, pages, _) in &ext {
            assert_eq!(logical, next);
            assert!(pages <= 4);
            next += pages;
        }
        assert_eq!(next, 38);
        assert_eq!(v.file(b), ten);
        // The record holds a pointer, not an extent, in the first slot.
        let pointer = View::u32(&v.record(b), 0x20);
        assert!(pointer >= RESERVED_PAGES);
        // The page bitmap covers the extent-index page too.
        let report = built.entries.iter().find(|e| e.path == "b").unwrap();
        assert_eq!(report.extents, 10);
    }

    #[test]
    fn more_than_2730_extents_use_a_second_index_page() {
        let options = TreeOptions {
            max_extent_pages: 1,
            ..Default::default()
        };
        let data = pattern(PAGE * 2735);
        let built = build(vec![plain("big", data.clone())], options);
        let v = View(&built.image);
        let id = id_of(&built, "big");
        let ext = v.extents(id);
        assert_eq!(ext.len(), 2735);
        assert_ne!(
            View::u32(&v.record(id), 0x20),
            View::u32(&v.record(id), 0x24)
        );
        for page in [0u32, 2729, 2730, 2734] {
            let (logical, _, physical) = ext[page as usize];
            assert_eq!(logical, page);
            let start = page as usize * PAGE;
            assert_eq!(v.page(physical), data[start..start + PAGE]);
        }
    }

    #[test]
    fn a_64_mib_file_is_one_extent() {
        let data = pattern(64 * 1024 * 1024 + 3);
        let built = build(vec![plain("big.bin", data.clone())], TreeOptions::default());
        let v = View(&built.image);
        let id = id_of(&built, "big.bin");
        assert_eq!(v.extents(id).len(), 1);
        assert_eq!(v.size(id), data.len());
        assert_eq!(v.file(id), data);
        assert_eq!(
            built
                .entries
                .iter()
                .find(|e| e.id == id)
                .unwrap()
                .page_count,
            2049
        );
    }

    #[test]
    fn mmcfs_slots_land_in_their_sectors_and_leave_the_write_protect_bits_clear() {
        for (device, sound_sector, kit_sector) in [
            ("dt2", (7 + 0x2400) * 8, 0x16000 + 3 * 0x80),
            ("dn2", (7 + 0x800) * 2, 0x2000 + 3 * 0x40),
        ] {
            let layout = card_layout(device).unwrap();
            let mut slots = MmcSlots::default();
            slots.slots.insert((Pool::Projects, 5), pattern(100_000));
            slots.slots.insert((Pool::Projects, 127), vec![9; 512]);
            slots.slots.insert((Pool::Sounds, 7), pattern(0x400));
            slots.slots.insert((Pool::Kits, 3), pattern(3000));
            let built = build_tree_image(
                Tree {
                    slots,
                    ..Default::default()
                },
                TreeOptions {
                    layout,
                    ..Default::default()
                },
                None,
            )
            .unwrap();
            let v = View(&built.image);
            assert_eq!(v.read((5 + 0xb) << 15 << 9, 100_000), pattern(100_000));
            assert_eq!(v.read((0x8a << 15) << 9, 512), vec![9; 512]);
            assert_eq!(v.read(sound_sector * 512, 0x400), pattern(0x400));
            assert_eq!(v.read(kit_sector * 512, 3000), pattern(3000));
            // The table holds write-protect bits (finding 35): an imported slot must not be a padlocked one, because the
            // firmware's project save refuses a protected slot ("N/A: Write-protected").
            let t = v.read(0x800 * 512, 512);
            assert!(t.iter().all(|&b| b == 0), "no slot is write-protected");
        }
    }

    fn tree_with(regions: Vec<(u64, Vec<u8>)>, store: Vec<crate::StoreFile>) -> Tree {
        Tree {
            regions: regions.into_iter().collect(),
            store,
            ..Default::default()
        }
    }

    #[test]
    fn a_folder_region_replaces_the_generated_boot_project() {
        let seed_free = |tree: Tree, device: &str| {
            build_tree_image(
                tree,
                TreeOptions {
                    layout: card_layout(device).unwrap(),
                    ..Default::default()
                },
                None,
            )
        };
        for device in ["dt2", "dn2"] {
            let built = seed_free(
                tree_with(
                    vec![(0x40000, pattern(3000)), (0x50000, vec![7; 600])],
                    Vec::new(),
                ),
                device,
            )
            .unwrap();
            let v = View(&built.image);
            assert_eq!(v.read(0x40000 * 512, 3000), pattern(3000));
            // the other boot copy keeps the generated record; the working project is only what the folder gave
            assert_eq!(
                v.read(0x48000 * 512, 256),
                build_boot_config_record().to_vec()
            );
            assert_eq!(v.read(0x50000 * 512, 600), vec![7; 600]);
        }
        let err = |sector, len| {
            seed_free(tree_with(vec![(sector, vec![1; len])], Vec::new()), "dt2").err()
        };
        assert!(matches!(
            err(0x41000, 10),
            Some(ImageError::UnknownRegion { .. })
        ));
        assert!(matches!(
            err(0x40000, 0x8000 * 512 + 1),
            Some(ImageError::RegionSize { .. })
        ));
        assert!(matches!(
            err(0x40000, 0),
            Some(ImageError::RegionSize { .. })
        ));
    }

    #[test]
    fn digikit_files_land_in_the_module_store_and_must_fit() {
        for device in ["dt2", "dn2"] {
            let files = vec![
                crate::StoreFile {
                    dir: false,
                    path: "b.dkm".into(),
                    bytes: pattern(1500),
                },
                crate::StoreFile {
                    dir: false,
                    path: "a/x.bin".into(),
                    bytes: vec![1],
                },
            ];
            let built = build_tree_image(
                tree_with(Vec::new(), files),
                TreeOptions {
                    layout: card_layout(device).unwrap(),
                    ..Default::default()
                },
                None,
            )
            .unwrap();
            let area = card_layout(device).unwrap().store.unwrap();
            let store = crate::read_store(
                &built.image,
                area.first_sector,
                area.store_limit(),
                area.header_b(),
            )
            .unwrap()
            .unwrap();
            // table order: by first sector (the data is in path order)
            let names: Vec<_> = store.entries.iter().map(|e| e.path.as_str()).collect();
            assert_eq!(names, ["a/x.bin", "b.dkm"]);
            let bytes =
                crate::read_store_file(&built.image, area.first_sector, &store.entries[1]).unwrap();
            assert_eq!(bytes, pattern(1500));
            // the image reaches the end of the area (the store is at the end of the card)
            assert!(built.image.logical_len() >= area.end_sector * 512);
        }
        let too_big = crate::StoreFile {
            dir: false,
            path: "big".into(),
            bytes: vec![0; 90 * 1024 * 1024],
        };
        // (82 MiB is the DT2's store)
        assert!(matches!(
            build_tree_image(
                tree_with(Vec::new(), vec![too_big]),
                TreeOptions::default(),
                None
            ),
            Err(ImageError::Store(crate::StoreError::DoesNotFit { .. }))
        ));
    }

    #[test]
    fn the_selection_is_written_as_two_copies_and_the_store_keeps_out_of_them() {
        for device in ["dt2", "dn2"] {
            let layout = card_layout(device).unwrap();
            let store = layout.store.clone().unwrap();
            let paths = vec!["modules/a.dkm".to_string(), "modules/b.dkm".to_string()];
            let built = build_tree_image(
                Tree {
                    store: vec![crate::StoreFile {
                        dir: false,
                        path: "modules/a.dkm".into(),
                        bytes: pattern(1500),
                    }],
                    selection: Some(paths.clone()),
                    ..Default::default()
                },
                TreeOptions {
                    layout: layout.clone(),
                    ..Default::default()
                },
                None,
            )
            .unwrap();
            let v = View(&built.image);
            for sector in [store.end_sector - 2, store.end_sector - 1] {
                let rec = crate::decode_selection(&v.read(sector * 512, 512)).unwrap();
                assert_eq!((rec.seq, rec.boot_counter, rec.flags), (1, 0, 0));
                assert_eq!(rec.paths, paths);
            }
            // the store itself reads back, and a store that would reach into the two sectors is refused
            let read = crate::read_store(
                &built.image,
                store.first_sector,
                store.store_limit(),
                store.header_b(),
            )
            .unwrap()
            .unwrap();
            assert_eq!(read.entries.len(), 1);
            assert_eq!(store.header_b(), store.end_sector - 35);
            let limit = store.store_limit() as usize * 512;
            let fits = |bytes: usize| {
                build_tree_image(
                    Tree {
                        store: vec![crate::StoreFile {
                            dir: false,
                            path: "big".into(),
                            bytes: vec![1; bytes],
                        }],
                        ..Default::default()
                    },
                    TreeOptions {
                        layout: layout.clone(),
                        ..Default::default()
                    },
                    None,
                )
            };
            // (the DN2's store is 22 GiB: its limit is not tried with a real file)
            if device == "dt2" {
                assert!(fits(limit - 2 * 512).is_ok());
                assert!(matches!(
                    fits(limit - 512),
                    Err(ImageError::Store(crate::StoreError::DoesNotFit { .. }))
                ));
            }
            // no selection: both sectors stay zero (no record)
            let none = build_tree_image(
                Tree::default(),
                TreeOptions {
                    layout,
                    ..Default::default()
                },
                None,
            )
            .unwrap();
            let v = View(&none.image);
            assert!(
                v.read((store.end_sector - 2) * 512, 1024)
                    .iter()
                    .all(|&b| b == 0)
            );
        }
        // a record that cannot be encoded is an error, not a silent truncation
        let long = Tree {
            selection: Some(vec!["x".repeat(56)]),
            ..Default::default()
        };
        assert!(matches!(
            build_tree_image(long, TreeOptions::default(), None),
            Err(ImageError::Selection(_))
        ));
    }

    #[test]
    fn dn2_takes_no_path_filesystem_files() {
        let dn2 = TreeOptions {
            layout: card_layout("dn2").unwrap(),
            ..Default::default()
        };
        let tree = Tree {
            files: vec![plain("a", vec![1])],
            ..Default::default()
        };
        assert!(matches!(
            build_tree_image(tree, dn2.clone(), None),
            Err(ImageError::NoPathFilesystem)
        ));
        let built = build_tree_image(Tree::default(), dn2, None).unwrap();
        // header and table only: no superblock where the DT2 keeps it
        let v = View(&built.image);
        assert_eq!(v.read(0, 4), HEADER_MAGIC.to_be_bytes());
        assert!(v.read(0x5d8000 * 512, 512).iter().all(|&b| b == 0));
    }

    #[test]
    fn slots_are_range_and_size_checked() {
        let one = |pool, slot, len| {
            let mut slots = MmcSlots::default();
            slots.slots.insert((pool, slot), vec![1; len]);
            build_tree_image(
                Tree {
                    slots,
                    ..Default::default()
                },
                TreeOptions::default(),
                None,
            )
            .err()
        };
        assert!(matches!(
            one(Pool::Projects, 128, 10),
            Some(ImageError::SlotOutOfRange { .. })
        ));
        assert!(matches!(
            one(Pool::Sounds, 2048, 10),
            Some(ImageError::SlotOutOfRange { .. })
        ));
        assert!(matches!(
            one(Pool::Sounds, 0, 4097),
            Some(ImageError::SlotSize { limit: 4096, .. })
        ));
        assert!(matches!(
            one(Pool::Kits, 0, 65537),
            Some(ImageError::SlotSize { .. })
        ));
        assert!(matches!(
            one(Pool::Kits, 0, 0),
            Some(ImageError::SlotSize { .. })
        ));
        assert!(one(Pool::Projects, 0, 16 * 1024 * 1024).is_none());
        // A project slot never reaches the boot project at 0x40000/0x48000.
        let top = dt2_layout().slot_sector(Pool::Projects, 0);
        assert!(top > 0x48000 + 0x6ecb);
    }

    #[test]
    fn names_follow_the_contract() {
        // Only a native sample loses ".wav"; plain files keep any name.
        assert_eq!(stored_name("a.WAV", true).unwrap(), b"a");
        assert_eq!(stored_name("a.wav", false).unwrap(), b"a.wav");
        assert_eq!(stored_name("é.dkm", false).unwrap(), b"?.dkm");
        assert!(stored_name("..", false).is_err());
        assert!(stored_name("", false).is_err());
        let err = |files| {
            build_tree_image(
                Tree {
                    files,
                    ..Default::default()
                },
                TreeOptions::default(),
                None,
            )
            .err()
        };
        // "x.wav" (native, stored "x") collides with a plain "x".
        assert!(matches!(
            err(vec![native("x.wav", 4), plain("x", vec![1])]),
            Some(ImageError::DuplicateName { .. })
        ));
        // A file where a directory is needed, and the firmware's /factory.
        assert!(matches!(
            err(vec![plain("a", vec![1]), plain("a/b", vec![1])]),
            Some(ImageError::DuplicateName { .. })
        ));
        assert!(matches!(
            err(vec![plain("factory", vec![1])]),
            Some(ImageError::InvalidName { .. })
        ));
        assert!(err(vec![plain("sub/factory", vec![1])]).is_none());
        assert!(matches!(
            err(vec![plain("a//b", vec![1])]),
            Some(ImageError::InvalidName { .. })
        ));
    }

    #[test]
    fn parse_native_accepts_only_valid_samples() {
        let bytes = build_native_sample(&[0, 1, 0, 2], false).unwrap();
        let sample = parse_native(bytes.clone()).unwrap();
        assert_eq!((sample.info.frames, sample.info.channels), (2, 1));
        let mut bad = bytes.clone();
        bad[0x14] = 0;
        assert!(parse_native(bad).is_none());
        assert!(parse_native(vec![0; 10]).is_none());
        assert!(parse_native(b"RIFF....WAVE".to_vec()).is_none());
        let mut short = bytes;
        short.pop();
        assert!(parse_native(short).is_none());
    }

    #[test]
    fn empty_directories_and_files_exist() {
        let built = build_tree_image(
            Tree {
                files: vec![plain("empty.bin", Vec::new())],
                dirs: vec!["a/b".into()],
                ..Default::default()
            },
            TreeOptions::default(),
            None,
        )
        .unwrap();
        let v = View(&built.image);
        let b = id_of(&built, "a/b");
        assert_eq!(v.entries(b).len(), 2);
        let e = id_of(&built, "empty.bin");
        assert_eq!(v.size(e), 0);
        assert_eq!(v.extents(e).len(), 1);
        // The link count of a directory is 2 plus its subdirectories.
        let a = id_of(&built, "a");
        assert_eq!(u16::from_be_bytes([v.record(a)[2], v.record(a)[3]]), 3);
    }
}
