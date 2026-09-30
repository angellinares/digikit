//! Bounded read-only inspection of a formatted sparse +Drive image.

use crate::{
    CONTENT_AREA_SECTOR, HEADER_SECTOR, PAGE, RECORD_AREA_SECTOR, RECORD_SIZE, ROOT_ID, SECTOR,
    SUPERBLOCK_SECTOR,
};
use emmc_card::RandomAccessRead;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImageEntry {
    pub id: u32,
    pub name: Vec<u8>,
    pub kind: u8,
    pub size: u32,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ListError {
    ShortRead,
    InvalidHeader,
    InvalidRoot,
    InvalidDirectory,
}

fn offset_sector(sector: u64) -> Option<u64> {
    sector.checked_mul(SECTOR as u64)
}
fn record_offset(id: u32) -> Option<u64> {
    offset_sector(RECORD_AREA_SECTOR + u64::from(id / 256) * (PAGE / SECTOR) as u64)?
        .checked_add(u64::from(id % 256) * RECORD_SIZE as u64)
}
fn read_exact(image: &dyn RandomAccessRead, offset: u64, out: &mut [u8]) -> Result<(), ListError> {
    if image.read_at(offset, out) == out.len() {
        Ok(())
    } else {
        Err(ListError::ShortRead)
    }
}
fn be32(data: &[u8], at: usize) -> Option<u32> {
    data.get(at..at + 4)
        .and_then(|v| v.try_into().ok())
        .map(u32::from_be_bytes)
}

pub fn list_image(image: &dyn RandomAccessRead) -> Result<Vec<ImageEntry>, ListError> {
    if image.len() % SECTOR as u64 != 0 {
        return Err(ListError::InvalidHeader);
    }
    let mut header = [0; SECTOR];
    read_exact(
        image,
        offset_sector(HEADER_SECTOR).ok_or(ListError::InvalidHeader)?,
        &mut header,
    )?;
    if be32(&header, 0) != Some(0xbeef_bace) {
        return Err(ListError::InvalidHeader);
    }
    let mut superblock = [0; SECTOR];
    read_exact(
        image,
        offset_sector(u64::from(SUPERBLOCK_SECTOR)).ok_or(ListError::InvalidHeader)?,
        &mut superblock,
    )?;
    if be32(&superblock, 0) != Some(0x656b_4653) {
        return Err(ListError::InvalidHeader);
    }
    let mut root = [0; RECORD_SIZE];
    read_exact(
        image,
        record_offset(ROOT_ID).ok_or(ListError::InvalidRoot)?,
        &mut root,
    )?;
    if root[0] != 1 || be32(&root, 4) != Some(PAGE as u32) || root[0x1e..0x20] != [0, 2] {
        return Err(ListError::InvalidRoot);
    }
    let page = be32(&root, 0x28).ok_or(ListError::InvalidRoot)?;
    let page_offset = offset_sector(CONTENT_AREA_SECTOR + u64::from(page) * (PAGE / SECTOR) as u64)
        .ok_or(ListError::InvalidRoot)?;
    let mut directory = vec![0; PAGE];
    read_exact(image, page_offset, &mut directory)?;
    let mut out = Vec::new();
    let mut at = 0usize;
    while at < directory.len() {
        let id = be32(&directory, at).ok_or(ListError::InvalidDirectory)?;
        let slot = u16::from_be_bytes(
            directory
                .get(at + 4..at + 6)
                .ok_or(ListError::InvalidDirectory)?
                .try_into()
                .unwrap(),
        ) as usize;
        let name_len = *directory.get(at + 6).ok_or(ListError::InvalidDirectory)? as usize;
        let kind = *directory.get(at + 7).ok_or(ListError::InvalidDirectory)?;
        if slot < 8
            || at.checked_add(slot).is_none_or(|end| end > directory.len())
            || name_len > slot - 8
        {
            return Err(ListError::InvalidDirectory);
        }
        let name = directory[at + 8..at + 8 + name_len].to_vec();
        if name != b"." && name != b".." {
            let mut record = [0; RECORD_SIZE];
            read_exact(
                image,
                record_offset(id).ok_or(ListError::InvalidDirectory)?,
                &mut record,
            )?;
            out.push(ImageEntry {
                id,
                name,
                kind,
                size: be32(&record, 4).ok_or(ListError::InvalidDirectory)?,
            });
        }
        at += slot;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NamedSample, NativeInfo, NativeSample, build_native_sample, build_sample_image};
    #[test]
    fn lists_and_rejects_corrupt_root() {
        let bytes = build_native_sample(&[0, 1], false).unwrap();
        let built = build_sample_image(vec![NamedSample {
            file_name: "a.wav".into(),
            sample: NativeSample {
                bytes,
                info: NativeInfo {
                    src_rate: 48000,
                    channels: 1,
                    src_frames: 1,
                    frames: 1,
                    data_len: 2,
                },
            },
        }])
        .unwrap();
        assert_eq!(list_image(&built.image).unwrap()[0].name, b"a");
        let mut broken = built.image.clone();
        broken
            .replace_at(record_offset(ROOT_ID).unwrap(), &[0; RECORD_SIZE])
            .unwrap();
        assert_eq!(list_image(&broken), Err(ListError::InvalidRoot));
    }
}
