#[path = "../src/state.rs"]
mod state;

use flate2::{Compression, write::ZlibEncoder};
use serde_json::json;
use state::{MAGIC, MAGIC_V2, OVERLAY_RECORD_SIZE, PAGE_SIZE, StateError, parse};
use std::io::Write;

fn compressed(data: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

fn image(records: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let header = json!({
        "format_version": 1,
        "clock": 0,
        "clock_basis": "checkpoint_relative_zero",
        "regs": {"d0": 0, "d1": 1, "d2": 2, "d3": 3, "d4": 4, "d5": 5, "d6": 6, "d7": 7, "a0": 8, "a1": 9, "a2": 10, "a3": 11, "a4": 12, "a5": 13, "a6": 14, "a7": 15, "pc": 16, "sr": 17},
        "ctlregs": {"1": 2},
        "mapped_bases": [0, PAGE_SIZE as u32],
        "page_count": records.len(),
        "mmio_forced": {"3": 4},
        "ff1_count": 5,
        "movec_count": 6,
        "components": {},
        "manifest": null,
    });
    let header = serde_json::to_vec(&header).unwrap();
    let mut result = MAGIC.to_vec();
    result.extend_from_slice(&(header.len() as u32).to_le_bytes());
    result.extend_from_slice(&header);
    for (base, compressed) in records {
        result.extend_from_slice(&base.to_le_bytes());
        result.extend_from_slice(&(PAGE_SIZE as u32).to_le_bytes());
        result.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
        result.extend_from_slice(compressed);
    }
    result
}

fn page() -> Vec<u8> {
    let mut page = vec![0; PAGE_SIZE];
    page[0] = 42;
    page
}

fn record_offset(image: &[u8]) -> usize {
    MAGIC.len() + 4 + u32::from_le_bytes(image[8..12].try_into().unwrap()) as usize
}

fn image_v2(raw: &[u8], count: u32, written_bytes: u64) -> Vec<u8> {
    let source = image(&[]);
    let mut header: serde_json::Value =
        serde_json::from_slice(&source[12..record_offset(&source)]).unwrap();
    let compressed = compressed(raw);
    header["format_version"] = json!(2);
    header["components"] = json!({"esdhc": {"card_overlay": {}}});
    header["overlay_encoding"] = json!("sector-bitmap-v1");
    header["overlay_sector_count"] = json!(count);
    header["overlay_written_bytes"] = json!(written_bytes);
    header["overlay_compressed_len"] = json!(compressed.len());
    let header = serde_json::to_vec(&header).unwrap();
    let mut result = MAGIC_V2.to_vec();
    result.extend_from_slice(&(header.len() as u32).to_le_bytes());
    result.extend_from_slice(&header);
    result.extend_from_slice(&compressed);
    result
}

fn overlay_record(sector: u64, written: u8, byte: u8) -> Vec<u8> {
    let mut record = vec![0; OVERLAY_RECORD_SIZE];
    record[..8].copy_from_slice(&sector.to_le_bytes());
    record[8] = written;
    record[72] = byte;
    record
}

#[test]
fn parses_nonzero_page_and_zero_mapped_base() {
    assert_eq!(MAGIC.len(), 8);
    assert_eq!(MAGIC, b"MSTATE\0\x01");
    let bytes = image(&[(PAGE_SIZE as u32, compressed(&page()))]);
    let state = parse(&bytes).unwrap();
    assert_eq!(state.mapped_bases, vec![0, PAGE_SIZE as u32]);
    assert_eq!(state.pages.len(), 1);
    assert_eq!(state.pages[0].data[0], 42);
    assert_eq!(state.regs.pc, 16);
    assert_eq!(state.overlay_sectors, None);
}

#[test]
fn compact_v2_preserves_written_zero_and_sorted_sector_masks() {
    let mut raw = overlay_record(3, 0b11, 0);
    raw[73] = 42;
    raw.extend(overlay_record(7, 1, 255));
    let state = parse(&image_v2(&raw, 2, 3)).unwrap();
    let sectors = state.overlay_sectors.unwrap();
    assert_eq!(sectors.len(), 2);
    assert_eq!(sectors[0].sector, 3);
    assert_eq!(sectors[0].written[0], 0b11);
    assert_eq!(sectors[0].data[0], 0);
    assert_eq!(sectors[0].data[1], 42);
    assert_eq!(sectors[1].sector, 7);
}

#[test]
fn compact_v2_rejects_malformed_records_and_extra_bytes() {
    let first = overlay_record(3, 1, 0);
    let mut duplicate = first.clone();
    duplicate.extend_from_slice(&first);
    assert_eq!(
        parse(&image_v2(&duplicate, 2, 2)),
        Err(StateError::InvalidOverlay)
    );
    let mut noncanonical = first.clone();
    noncanonical[73] = 1; // absent byte cannot carry an unmasked value
    assert_eq!(
        parse(&image_v2(&noncanonical, 1, 1)),
        Err(StateError::InvalidOverlay)
    );
    assert_eq!(
        parse(&image_v2(&first, 1, 2)),
        Err(StateError::InvalidOverlay)
    );
    assert_eq!(
        parse(&image_v2(&first, 2, 1)),
        Err(StateError::InvalidOverlay)
    );
    let mut trailing = image_v2(&first, 1, 1);
    trailing.push(0);
    assert_eq!(parse(&trailing), Err(StateError::InvalidLayout));
    let mut bad_magic = image_v2(&first, 1, 1);
    bad_magic[7] = 3;
    assert_eq!(parse(&bad_magic), Err(StateError::InvalidMagic));
}

#[test]
fn rejects_truncated_and_zlib_bomb() {
    assert_eq!(parse(&MAGIC[..]).unwrap_err(), StateError::Truncated);
    let bomb = compressed(&vec![0; PAGE_SIZE + 1]);
    assert_eq!(
        parse(&image(&[(0, bomb)])).unwrap_err(),
        StateError::InvalidPage
    );
}

#[test]
fn rejects_record_bytes_after_zlib_stream_and_zero_pages() {
    let mut trailing = compressed(&page());
    trailing.extend_from_slice(b"corrupt");
    assert_eq!(
        parse(&image(&[(0, trailing)])).unwrap_err(),
        StateError::InvalidPage
    );

    assert_eq!(
        parse(&image(&[(0, compressed(&vec![0; PAGE_SIZE]))])).unwrap_err(),
        StateError::InvalidPage
    );
}

#[test]
fn rejects_duplicate_unmapped_and_unaligned_records() {
    let record = compressed(&page());
    let duplicate = image(&[(0, record.clone()), (0, record.clone())]);
    assert_eq!(parse(&duplicate).unwrap_err(), StateError::InvalidPage);

    let unmapped = image(&[(2 * PAGE_SIZE as u32, record.clone())]);
    assert_eq!(parse(&unmapped).unwrap_err(), StateError::InvalidPage);

    let mut unaligned = image(&[(0, record)]);
    let offset = record_offset(&unaligned);
    unaligned[offset..offset + 4].copy_from_slice(&1u32.to_le_bytes());
    assert_eq!(parse(&unaligned).unwrap_err(), StateError::InvalidPage);
}
