//! Portable MSTATE version-1 snapshot parsing, without machine application.
//!
//! Limits are deliberately browser-safe: headers are at most 1 MiB and at
//! most 512 mapped (and therefore decoded) 1 MiB pages are accepted.  This
//! covers the current 134-page DT2 and 110-page DN2 checkpoints.

use std::collections::BTreeMap;

use flate2::{Decompress, FlushDecompress, Status};
use serde::Deserialize;
use serde_json::Value;

pub const MAGIC: &[u8; 8] = b"MSTATE\0\x01";
pub const PAGE_SIZE: usize = 1024 * 1024;
pub const MAX_HEADER_SIZE: usize = 1024 * 1024;
pub const MAX_MAPPED_PAGES: usize = 512;
pub const MAX_COMPRESSED_PAGE_SIZE: usize = 2 * 1024 * 1024;

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
}

/// Parse an MSTATE v1 byte stream.  This does not restore CPU or Board state.
pub fn parse(input: &[u8]) -> Result<MachineState, StateError> {
    if input.len() < MAGIC.len() + 4 {
        return Err(StateError::Truncated);
    }
    if &input[..MAGIC.len()] != MAGIC {
        return Err(StateError::InvalidMagic);
    }
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
    validate_header(&header)?;
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
    })
}

fn le_u32(bytes: &[u8]) -> Result<u32, StateError> {
    Ok(u32::from_le_bytes(
        bytes.try_into().map_err(|_| StateError::Truncated)?,
    ))
}

fn validate_header(header: &Header) -> Result<(), StateError> {
    if header.format_version != 1 || header.clock_basis != "checkpoint_relative_zero" {
        return Err(StateError::InvalidHeader);
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
    let mut decoder = Decompress::new(true);
    let mut data = vec![0; PAGE_SIZE + 1];
    let status = decoder
        .decompress(compressed, &mut data, FlushDecompress::Finish)
        .map_err(|_| StateError::InvalidPage)?;
    if status != Status::StreamEnd
        || decoder.total_in() != compressed.len() as u64
        || decoder.total_out() != PAGE_SIZE as u64
    {
        return Err(StateError::InvalidPage);
    }
    data.truncate(PAGE_SIZE);
    Ok(data)
}
