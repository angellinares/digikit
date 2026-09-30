//! Portable MSTATE version-1/2 snapshot parsing, without machine application.
//!
//! Limits are deliberately browser-safe: headers are at most 1 MiB and at
//! most 512 mapped (and therefore decoded) 1 MiB pages are accepted.  This
//! covers the current 134-page DT2 and 110-page DN2 checkpoints.

use std::collections::BTreeMap;

pub use emmc_card::CardOverlaySector as OverlaySectorRecord;
use flate2::{Decompress, FlushDecompress, Status};
use serde::Deserialize;
use serde_json::Value;

pub const MAGIC: &[u8; 8] = b"MSTATE\0\x01";
pub const MAGIC_V2: &[u8; 8] = b"MSTATE\0\x02";
pub const PAGE_SIZE: usize = 1024 * 1024;
pub const MAX_HEADER_SIZE: usize = 1024 * 1024;
pub const MAX_MAPPED_PAGES: usize = 512;
pub const MAX_COMPRESSED_PAGE_SIZE: usize = 2 * 1024 * 1024;
pub const OVERLAY_RECORD_SIZE: usize = 8 + 64 + 512;
pub const MAX_OVERLAY_SECTORS: usize = 131_072;
pub const MAX_COMPRESSED_OVERLAY_SIZE: usize = MAX_OVERLAY_SECTORS * OVERLAY_RECORD_SIZE;

#[derive(Debug, Clone, PartialEq)]
pub struct MachineState {
    pub clock: u64,
    pub regs: Registers,
    pub ctlregs: BTreeMap<u32, u32>,
    pub mapped_bases: Vec<u32>,
    pub mmio_forced: BTreeMap<u32, u32>,
    pub ff1_count: u32,
    pub movec_count: u32,
    pub components: Value,
    pub manifest: Value,
    /// Only nonzero mapped pages are stored on disk; absent mapped pages are zero.
    pub pages: Vec<Page>,
    /// Only v2 snapshots have a compact overlay; a v1 empty overlay is None.
    pub overlay_sectors: Option<Vec<OverlaySectorRecord>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Registers {
    pub d: [u32; 8],
    pub a: [u32; 8],
    pub pc: u32,
    pub sr: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    pub base: u32,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateError {
    Truncated,
    InvalidMagic,
    HeaderTooLarge,
    InvalidHeader,
    InvalidLayout,
    LimitExceeded,
    InvalidPage,
    InvalidOverlay,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    format_version: u32,
    clock: u64,
    clock_basis: String,
    regs: BTreeMap<String, u32>,
    ctlregs: BTreeMap<String, u32>,
    mapped_bases: Vec<u32>,
    page_count: u32,
    mmio_forced: BTreeMap<String, u32>,
    ff1_count: u32,
    movec_count: u32,
    components: Value,
    manifest: Value,
    #[serde(default)]
    overlay_encoding: Option<String>,
    #[serde(default)]
    overlay_sector_count: Option<u32>,
    #[serde(default)]
    overlay_written_bytes: Option<u64>,
    #[serde(default)]
    overlay_compressed_len: Option<u32>,
}

/// Parse an MSTATE v1/v2 byte stream. This does not restore CPU or Board state.
pub fn parse(input: &[u8]) -> Result<MachineState, StateError> {
    if input.len() < MAGIC.len() + 4 {
        return Err(StateError::Truncated);
    }
    let format_version = match &input[..MAGIC.len()] {
        bytes if bytes == MAGIC => 1,
        bytes if bytes == MAGIC_V2 => 2,
        _ => return Err(StateError::InvalidMagic),
    };
    let header_len = le_u32(&input[MAGIC.len()..MAGIC.len() + 4])? as usize;
    if header_len > MAX_HEADER_SIZE {
        return Err(StateError::HeaderTooLarge);
    }
    let header_start = MAGIC.len() + 4;
    let header_end = header_start
        .checked_add(header_len)
        .ok_or(StateError::InvalidLayout)?;
    let header_bytes = input
        .get(header_start..header_end)
        .ok_or(StateError::Truncated)?;
    let header: Header =
        serde_json::from_slice(header_bytes).map_err(|_| StateError::InvalidHeader)?;
    validate_header(&header, format_version)?;
    let regs = parse_regs(&header.regs)?;
    let ctlregs = parse_address_map(&header.ctlregs)?;
    let mmio_forced = parse_address_map(&header.mmio_forced)?;
    let mut offset = header_end;
    let mut pages = Vec::with_capacity(header.page_count as usize);
    let mut previous = None;
    for _ in 0..header.page_count {
        let record_end = offset.checked_add(12).ok_or(StateError::InvalidLayout)?;
        let record = input.get(offset..record_end).ok_or(StateError::Truncated)?;
        let base = le_u32(&record[..4])?;
        let uncompressed_len = le_u32(&record[4..8])? as usize;
        let compressed_len = le_u32(&record[8..12])? as usize;
        offset = offset.checked_add(12).ok_or(StateError::InvalidLayout)?;
        if !(base as usize).is_multiple_of(PAGE_SIZE)
            || uncompressed_len != PAGE_SIZE
            || compressed_len > MAX_COMPRESSED_PAGE_SIZE
            || previous.is_some_and(|old| base <= old)
            || header.mapped_bases.binary_search(&base).is_err()
        {
            return Err(StateError::InvalidPage);
        }
        let compressed = input
            .get(
                offset
                    ..offset
                        .checked_add(compressed_len)
                        .ok_or(StateError::InvalidLayout)?,
            )
            .ok_or(StateError::Truncated)?;
        offset = offset
            .checked_add(compressed_len)
            .ok_or(StateError::InvalidLayout)?;
        let data = decompress_page(compressed)?;
        if !data.iter().any(|byte| *byte != 0) {
            return Err(StateError::InvalidPage);
        }
        previous = Some(base);
        pages.push(Page { base, data });
    }
    let overlay_sectors = if format_version == 2 {
        let count = header.overlay_sector_count.unwrap() as usize;
        let compressed_len = header.overlay_compressed_len.unwrap() as usize;
        let end = offset
            .checked_add(compressed_len)
            .ok_or(StateError::InvalidLayout)?;
        let compressed = input.get(offset..end).ok_or(StateError::Truncated)?;
        offset = end;
        let raw = decompress_exact(
            compressed,
            count * OVERLAY_RECORD_SIZE,
            StateError::InvalidOverlay,
        )?;
        let mut sectors = Vec::with_capacity(count);
        let mut last = None;
        let mut written_bytes = 0u64;
        for record in raw.chunks_exact(OVERLAY_RECORD_SIZE) {
            let sector = u64::from_le_bytes(record[..8].try_into().unwrap());
            let written: [u8; 64] = record[8..72].try_into().unwrap();
            let data: [u8; 512] = record[72..].try_into().unwrap();
            if last.is_some_and(|previous| sector <= previous)
                || !written.iter().any(|byte| *byte != 0)
                || (0..512).any(|position| {
                    written[position >> 3] & (1 << (position & 7)) == 0 && data[position] != 0
                })
            {
                return Err(StateError::InvalidOverlay);
            }
            written_bytes += written
                .iter()
                .map(|bits| u64::from(bits.count_ones()))
                .sum::<u64>();
            last = Some(sector);
            sectors.push(OverlaySectorRecord {
                sector,
                written,
                data,
            });
        }
        if written_bytes != header.overlay_written_bytes.unwrap() {
            return Err(StateError::InvalidOverlay);
        }
        Some(sectors)
    } else {
        None
    };
    if offset != input.len() {
        return Err(StateError::InvalidLayout);
    }
    Ok(MachineState {
        clock: header.clock,
        regs,
        ctlregs,
        mapped_bases: header.mapped_bases,
        mmio_forced,
        ff1_count: header.ff1_count,
        movec_count: header.movec_count,
        components: header.components,
        manifest: header.manifest,
        pages,
        overlay_sectors,
    })
}

fn le_u32(bytes: &[u8]) -> Result<u32, StateError> {
    Ok(u32::from_le_bytes(
        bytes.try_into().map_err(|_| StateError::Truncated)?,
    ))
}

fn validate_header(header: &Header, format_version: u32) -> Result<(), StateError> {
    if header.format_version != format_version || header.clock_basis != "checkpoint_relative_zero" {
        return Err(StateError::InvalidHeader);
    }
    let overlay_fields = (
        header.overlay_encoding.as_deref(),
        header.overlay_sector_count,
        header.overlay_written_bytes,
        header.overlay_compressed_len,
    );
    match (format_version, overlay_fields) {
        (1, (None, None, None, None)) => {}
        (2, (Some("sector-bitmap-v1"), Some(count), Some(written), Some(compressed)))
            if count as usize <= MAX_OVERLAY_SECTORS
                && written <= u64::from(count) * 512
                && compressed as usize <= MAX_COMPRESSED_OVERLAY_SIZE
                && header
                    .components
                    .get("esdhc")
                    .and_then(|card| card.get("card_overlay"))
                    .and_then(Value::as_object)
                    .is_some_and(serde_json::Map::is_empty) => {}
        (2, (_, Some(count), _, _)) if count as usize > MAX_OVERLAY_SECTORS => {
            return Err(StateError::LimitExceeded);
        }
        _ => return Err(StateError::InvalidHeader),
    }
    if header.mapped_bases.len() > MAX_MAPPED_PAGES
        || header.page_count as usize > MAX_MAPPED_PAGES
        || header.page_count as usize > header.mapped_bases.len()
    {
        return Err(StateError::LimitExceeded);
    }
    if header
        .mapped_bases
        .windows(2)
        .any(|pair| pair[0] >= pair[1])
        || header
            .mapped_bases
            .iter()
            .any(|base| !(*base as usize).is_multiple_of(PAGE_SIZE))
    {
        return Err(StateError::InvalidHeader);
    }
    Ok(())
}

fn parse_regs(values: &BTreeMap<String, u32>) -> Result<Registers, StateError> {
    if values.len() != 18 {
        return Err(StateError::InvalidHeader);
    }
    let get = |name: &str| values.get(name).copied().ok_or(StateError::InvalidHeader);
    let mut d = [0; 8];
    let mut a = [0; 8];
    for index in 0..8 {
        d[index] = get(&format!("d{index}"))?;
        a[index] = get(&format!("a{index}"))?;
    }
    Ok(Registers {
        d,
        a,
        pc: get("pc")?,
        sr: get("sr")?,
    })
}

fn parse_address_map(values: &BTreeMap<String, u32>) -> Result<BTreeMap<u32, u32>, StateError> {
    values
        .iter()
        .map(|(key, value)| {
            let address = key.parse::<u32>().map_err(|_| StateError::InvalidHeader)?;
            if address.to_string() != *key {
                return Err(StateError::InvalidHeader);
            }
            Ok((address, *value))
        })
        .collect()
}

fn decompress_page(compressed: &[u8]) -> Result<Vec<u8>, StateError> {
    decompress_exact(compressed, PAGE_SIZE, StateError::InvalidPage)
}

fn decompress_exact(
    compressed: &[u8],
    size: usize,
    error: StateError,
) -> Result<Vec<u8>, StateError> {
    let mut decoder = Decompress::new(true);
    let mut data = vec![0; size + 1];
    let status = decoder
        .decompress(compressed, &mut data, FlushDecompress::Finish)
        .map_err(|_| error.clone())?;
    if status != Status::StreamEnd
        || decoder.total_in() != compressed.len() as u64
        || decoder.total_out() != size as u64
    {
        return Err(error);
    }
    data.truncate(size);
    Ok(data)
}
