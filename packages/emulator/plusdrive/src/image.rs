//! The sparse image and the flat (root-only, samples-only) builders; the general builder is in `tree.rs`.

use std::collections::BTreeMap;

use emmc_card::RandomAccessRead;

use crate::{
    CONTENT_AREA_SECTOR, DEFAULT_CAPACITY_BLOCKS, NativeInfo, NativeSample, PAGE,
    RECORD_AREA_SECTOR, RECORD_SIZE, SECTOR,
};

pub const MAX_SAMPLES: usize = 2048;
pub const MAX_NATIVE_BYTES: usize = 256 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SparseImageError {
    RangeOverflow,
    OutOfBounds {
        offset: u64,
        length: usize,
        capacity: u64,
    },
    Overlap {
        offset: u64,
        length: usize,
    },
    ReplacementMissing {
        offset: u64,
    },
}

/// A finite sparse byte-addressable image. Unwritten bytes read as zero.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SparseImage {
    logical_len: u64,
    ranges: BTreeMap<u64, Vec<u8>>,
}

impl SparseImage {
    pub fn new(logical_len: u64) -> Self {
        Self {
            logical_len,
            ranges: BTreeMap::new(),
        }
    }

    pub fn logical_len(&self) -> u64 {
        self.logical_len
    }
    pub fn ranges(&self) -> &BTreeMap<u64, Vec<u8>> {
        &self.ranges
    }

    /// Insert a distinct written range. Adjacent ranges stay distinct; overlap is an error.
    pub fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<(), SparseImageError> {
        let length = data.len();
        let end = offset
            .checked_add(length as u64)
            .ok_or(SparseImageError::RangeOverflow)?;
        if end > self.logical_len {
            return Err(SparseImageError::OutOfBounds {
                offset,
                length,
                capacity: self.logical_len,
            });
        }
        if length == 0 {
            return Ok(());
        }
        if let Some((&start, previous)) = self.ranges.range(..=offset).next_back() {
            let previous_end = start
                .checked_add(previous.len() as u64)
                .ok_or(SparseImageError::RangeOverflow)?;
            if previous_end > offset {
                return Err(SparseImageError::Overlap { offset, length });
            }
        }
        if let Some((&start, _)) = self.ranges.range(offset..).next()
            && start < end
        {
            return Err(SparseImageError::Overlap { offset, length });
        }
        self.ranges.insert(offset, data.to_vec());
        Ok(())
    }

    /// Replace a range written at exactly `offset`; no unrelated range may be overwritten.
    pub fn replace_at(&mut self, offset: u64, data: &[u8]) -> Result<(), SparseImageError> {
        let old = self
            .ranges
            .remove(&offset)
            .ok_or(SparseImageError::ReplacementMissing { offset })?;
        match self.write_at(offset, data) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.ranges.insert(offset, old);
                Err(error)
            }
        }
    }

    pub fn read_at(&self, offset: u64, destination: &mut [u8]) -> usize {
        if offset >= self.logical_len {
            return 0;
        }
        let count = usize::try_from((self.logical_len - offset).min(destination.len() as u64))
            .expect("bounded by destination length");
        destination[..count].fill(0);
        let end = offset + count as u64;
        for (&start, data) in self.ranges.range(..end) {
            let range_end = start + data.len() as u64;
            if range_end <= offset {
                continue;
            }
            let from = start.max(offset);
            let to = range_end.min(end);
            let source = (from - start) as usize;
            let target = (from - offset) as usize;
            destination[target..target + (to - from) as usize]
                .copy_from_slice(&data[source..source + (to - from) as usize]);
        }
        count
    }
}

impl RandomAccessRead for SparseImage {
    fn len(&self) -> u64 {
        self.logical_len
    }
    fn read_at(&self, offset: u64, destination: &mut [u8]) -> usize {
        SparseImage::read_at(self, offset, destination)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NamedSample {
    pub file_name: String,
    pub sample: NativeSample,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileReport {
    pub file_name: String,
    pub stored_name: Vec<u8>,
    pub id: u32,
    pub seq: u32,
    pub hash: u32,
    pub reference: [u8; 16],
    pub first_page: u32,
    pub page_count: u32,
    pub info: NativeInfo,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuiltImage {
    pub image: SparseImage,
    pub files: Vec<FileReport>,
    pub project: Option<crate::ProjectReport>,
    /// Every directory, native sample and plain file, in id order.
    pub entries: Vec<crate::EntryReport>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ImageError {
    ProjectRequiresSample,
    TooManySamples {
        maximum: usize,
    },
    TooManyNativeBytes {
        maximum: usize,
    },
    InvalidName {
        file_name: String,
    },
    DuplicateName {
        name: Vec<u8>,
    },
    InvalidNativeSample {
        file_name: String,
    },
    CapacityOverflow,
    FixedRegionsDoNotFit {
        capacity_blocks: u32,
    },
    ContentDoesNotFit {
        capacity_blocks: u32,
    },
    SlotOutOfRange {
        pool: &'static str,
        slot: u32,
    },
    SlotSize {
        pool: &'static str,
        slot: u32,
        bytes: usize,
        limit: usize,
    },
    /// A layout the builder cannot write (its path filesystem constants are fixed to one superblock sector).
    UnsupportedLayout(&'static str),
    /// Files were given for a device without a path filesystem.
    NoPathFilesystem,
    /// A region sector that is not a boot or working project region of the layout.
    UnknownRegion {
        sector: u64,
    },
    RegionSize {
        sector: u64,
        bytes: usize,
        limit: usize,
    },
    /// `digikit/` files for a device with no module store range.
    NoStore,
    Store(crate::StoreError),
    /// `digikit/selection.txt` cannot be turned into a record.
    Selection(String),
    Sparse(SparseImageError),
    Format(crate::FormatError),
    Project(crate::ProjectError),
}

impl From<SparseImageError> for ImageError {
    fn from(error: SparseImageError) -> Self {
        Self::Sparse(error)
    }
}
impl From<crate::FormatError> for ImageError {
    fn from(error: crate::FormatError) -> Self {
        Self::Format(error)
    }
}
impl From<crate::ProjectError> for ImageError {
    fn from(error: crate::ProjectError) -> Self {
        Self::Project(error)
    }
}

pub(crate) fn bytes_at_sector(sector: u64) -> Result<u64, ImageError> {
    sector
        .checked_mul(SECTOR as u64)
        .ok_or(ImageError::CapacityOverflow)
}
pub(crate) fn page_offset(page: u32) -> Result<u64, ImageError> {
    bytes_at_sector(CONTENT_AREA_SECTOR + u64::from(page) * (PAGE / SECTOR) as u64)
}
pub(crate) fn record_offset(id: u32) -> Result<u64, ImageError> {
    let group = u64::from(id) / 256;
    let slot = u64::from(id) % 256;
    bytes_at_sector(RECORD_AREA_SECTOR + group * (PAGE / SECTOR) as u64).and_then(|base| {
        base.checked_add(slot * RECORD_SIZE as u64)
            .ok_or(ImageError::CapacityOverflow)
    })
}
pub(crate) fn put32(out: &mut [u8], offset: usize, value: u32) {
    out[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
}
pub(crate) fn put16(out: &mut [u8], offset: usize, value: u16) {
    out[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
}

pub(crate) fn validate_native(sample: &NativeSample) -> bool {
    let bytes = &sample.bytes;
    if bytes.len() < 0x50 || bytes[0] != 0 || !matches!(bytes[1], 0 | 1) || bytes[0x14] != 0x7f {
        return false;
    }
    let declared = u32::from_be_bytes(bytes[4..8].try_into().unwrap()) as usize;
    let rate = u32::from_be_bytes(bytes[8..12].try_into().unwrap());
    let channels = usize::from(bytes[1]) + 1;
    declared.checked_add(0x50) == Some(bytes.len())
        && declared == sample.info.data_len
        && rate == 48_000
        && usize::from(sample.info.channels) == channels
        && declared.is_multiple_of(channels * 2)
        && sample.info.frames == declared / (channels * 2)
}

pub(crate) fn bitmap(bits: impl IntoIterator<Item = u32>) -> Vec<u8> {
    let bits = bits.into_iter().collect::<Vec<_>>();
    let top = bits.iter().copied().max().unwrap_or(0) as usize;
    let mut output = vec![0; PAGE.max(((top >> 5) + 1) * 4)];
    for bit in bits {
        let offset = (bit as usize >> 5) * 4;
        let current = u32::from_be_bytes(output[offset..offset + 4].try_into().unwrap());
        put32(&mut output, offset, current | (1 << (bit & 31)));
    }
    output
}

pub fn build_sample_image(samples: Vec<NamedSample>) -> Result<BuiltImage, ImageError> {
    build_sample_image_with_options(samples, DEFAULT_CAPACITY_BLOCKS, None)
}

pub fn build_sample_image_with_capacity(
    samples: Vec<NamedSample>,
    capacity_blocks: u32,
) -> Result<BuiltImage, ImageError> {
    build_sample_image_with_options(samples, capacity_blocks, None)
}

pub fn build_sample_image_with_project(
    samples: Vec<NamedSample>,
    project: crate::ProjectSeed<'_>,
) -> Result<BuiltImage, ImageError> {
    build_sample_image_with_options(samples, DEFAULT_CAPACITY_BLOCKS, Some(project))
}

pub fn build_sample_image_with_options(
    samples: Vec<NamedSample>,
    capacity_blocks: u32,
    project_seed: Option<crate::ProjectSeed<'_>>,
) -> Result<BuiltImage, ImageError> {
    if samples.is_empty() && project_seed.is_some() {
        return Err(ImageError::ProjectRequiresSample);
    }
    if samples.len() > MAX_SAMPLES {
        return Err(ImageError::TooManySamples {
            maximum: MAX_SAMPLES,
        });
    }
    let tree = crate::Tree {
        files: samples
            .into_iter()
            .map(|entry| crate::TreeFile {
                path: entry.file_name,
                content: crate::Content::Native(entry.sample),
            })
            .collect(),
        ..Default::default()
    };
    crate::build_tree_image(
        tree,
        crate::TreeOptions {
            capacity_blocks,
            ..Default::default()
        },
        project_seed,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ATTR_DIR, ROOT_ID, build_native_sample};

    fn sample(file_name: &str, pcm: &[i16]) -> NamedSample {
        let mut raw = Vec::with_capacity(pcm.len() * 2);
        for value in pcm {
            raw.extend_from_slice(&value.to_be_bytes());
        }
        let bytes = build_native_sample(&raw, false).unwrap();
        NamedSample {
            file_name: file_name.into(),
            sample: NativeSample {
                info: NativeInfo {
                    src_rate: 48_000,
                    channels: 1,
                    src_frames: pcm.len(),
                    frames: pcm.len(),
                    data_len: raw.len(),
                },
                bytes,
            },
        }
    }

    #[test]
    fn sparse_read_zeros_holes_and_crosses_ranges() {
        let mut image = SparseImage::new(12);
        image.write_at(2, b"ab").unwrap();
        image.write_at(7, b"cd").unwrap();
        let mut out = [9; 10];
        assert_eq!(image.read_at(1, &mut out), 10);
        assert_eq!(&out, &[0, b'a', b'b', 0, 0, 0, b'c', b'd', 0, 0]);
        assert_eq!(
            image.write_at(3, b"x"),
            Err(SparseImageError::Overlap {
                offset: 3,
                length: 1
            })
        );
    }

    #[test]
    fn sparse_read_at_large_logical_end_and_utf8_names_are_safe() {
        let image = SparseImage::new(u64::from(u32::MAX) + 32);
        let mut end = [7; 4];
        assert_eq!(image.read_at(u64::from(u32::MAX) + 30, &mut end), 2);
        assert_eq!(&end[..2], &[0, 0]);
        assert_eq!(crate::tree::stored_name_native("aéé"), Ok(b"a??".to_vec()));
        assert_eq!(
            crate::tree::stored_name_native("aéé.wav"),
            Ok(b"a??".to_vec())
        );
    }

    #[test]
    fn build_records_extents_and_metadata() {
        let result =
            build_sample_image(vec![sample("z.wav", &[1, 2]), sample("a?.WAV", &[3])]).unwrap();
        assert_eq!(
            result
                .files
                .iter()
                .map(|file| file.file_name.as_str())
                .collect::<Vec<_>>(),
            ["a?.WAV", "z.wav"]
        );
        assert_eq!(result.files[0].stored_name, b"a?");
        assert_eq!(result.files[0].first_page, 0x7c);
        let mut header = [0; 10];
        result.image.read_at(0, &mut header);
        assert_eq!(&header[..4], &crate::tree::HEADER_MAGIC.to_be_bytes());
        assert_eq!(&header[8..], &[1, 1]);
        let mut root = [0; RECORD_SIZE];
        result
            .image
            .read_at(record_offset(ROOT_ID).unwrap(), &mut root);
        assert_eq!(root[0], ATTR_DIR);
        assert_eq!(u16::from_be_bytes(root[0x1e..0x20].try_into().unwrap()), 2);
        assert_eq!(u32::from_be_bytes(root[0x24..0x28].try_into().unwrap()), 1);
        assert_eq!(
            u32::from_be_bytes(root[0x28..0x2c].try_into().unwrap()),
            0x78
        );
    }

    #[test]
    fn builds_and_lists_an_empty_formatted_root() {
        let built = build_sample_image(Vec::new()).unwrap();
        assert!(built.files.is_empty());
        assert_eq!(crate::list_image(&built.image).unwrap(), Vec::new());
    }

    #[test]
    fn empty_image_rejects_project_before_first_file_reference() {
        assert_eq!(
            build_sample_image_with_options(
                Vec::new(),
                DEFAULT_CAPACITY_BLOCKS,
                Some(crate::ProjectSeed {
                    main: &[],
                    contract: device_profile::PlusdriveProjectContract::Dt2V3Default116,
                }),
            ),
            Err(ImageError::ProjectRequiresSample)
        );
    }

    #[test]
    fn rejects_small_capacity_and_normalized_duplicate() {
        assert_eq!(
            build_sample_image_with_capacity(vec![sample("a.wav", &[0])], 0x3b0000),
            Err(ImageError::FixedRegionsDoNotFit {
                capacity_blocks: 0x3b0000
            })
        );
        assert!(matches!(
            build_sample_image(vec![sample("x.wav", &[0]), sample("x.WAV", &[1])]),
            Err(ImageError::DuplicateName { .. })
        ));
    }
}
