//! A [`ShortWords`] implementation over the loader-image segments of a
//! `tools/sharc_transpile_run.py::pack_image` blob ("SHIM" v1) -- what a
//! run-time JIT built on this crate feeds `Decoder::decode_at`.
//!
//! `pack_image` writes, in order: magic `b"SHIM"`, version `u32` (`1`), a
//! segment count `u32` then that many `(addr: u32, len: u32, bytes)`
//! records (`data.read(lo, hi-lo)` over `LoadedMemory`'s own resolved,
//! disjoint `[lo, hi)` runs -- so every byte the loader ever wrote is here
//! exactly once, at its final value, with "last write in the boot stream
//! wins" already applied), then the `sharcimm.name_address` addresses (exact
//! and `[lo, hi)` ranges) and `encoding.CORE_MMR_RESET_VALUES`' addresses.
//! Those last two sections name fixed-width MMRs for `memory._dm_read` and
//! are not needed to answer `ShortWords::read_sw`, so [`parse_pack_image`]
//! reads past them without keeping them.
//!
//! `addr` here is a *loader byte address* (`sharcldr.LoadedMemory`'s own
//! canonical addressing) -- not a short-word PC. [`ShortWords::read_sw`]'s
//! contract is exactly `sharcldr.LoadedMemory.read_sw`'s mapping from a
//! short-word PC to that byte address space; see `decode.rs`'s trait
//! docstring for the two-step primary/fallback rule this module implements.

use crate::decode::ShortWords;

const SW_ALIAS_BASE: u32 = 0x2800_0000;
const L2_BYTE_BASE: u32 = 0x2000_0000;
const L2_BYTE_LIMIT: u32 = 0x2010_0000;
const L2_SW_BASE: u32 = 0x00B8_0000;

/// The loader-image byte segments of one `pack_image` blob: disjoint,
/// sorted by address, each `(start, bytes)`.
pub struct SegmentImage {
    segments: Vec<(u32, Vec<u8>)>,
}

impl SegmentImage {
    /// Build directly from `(address, bytes)` segments (as `pack_image`
    /// writes them, or from any other source of the same shape). Segments
    /// need not be pre-sorted; they must not overlap.
    pub fn from_segments(mut segments: Vec<(u32, Vec<u8>)>) -> Result<SegmentImage, String> {
        segments.sort_by_key(|(a, _)| *a);
        for w in segments.windows(2) {
            let (a0, b0) = &w[0];
            let (a1, _) = &w[1];
            let end0 = (*a0 as u64) + b0.len() as u64;
            if end0 > *a1 as u64 {
                return Err(format!(
                    "overlapping segments at {a0:#x}..{end0:#x} and {a1:#x}"
                ));
            }
        }
        Ok(SegmentImage { segments })
    }

    /// The segments, sorted by address.
    pub fn segments(&self) -> &[(u32, Vec<u8>)] {
        &self.segments
    }

    /// Every short-word PC range `[lo, hi)` whose `read_sw` can find bytes
    /// (the primary alias and the L2 window), sorted.
    pub fn pc_ranges(&self) -> Vec<(u32, u32)> {
        let mut out = Vec::new();
        for (a, b) in &self.segments {
            let (a, e) = (*a as u64, *a as u64 + b.len() as u64);
            if a >= SW_ALIAS_BASE as u64 {
                out.push((((a - SW_ALIAS_BASE as u64) / 2) as u32, (e - SW_ALIAS_BASE as u64).div_ceil(2) as u32));
            }
            let (lo, hi) = (a.max(L2_BYTE_BASE as u64), e.min(L2_BYTE_LIMIT as u64));
            if lo < hi {
                let base = L2_SW_BASE as u64;
                out.push(((base + (lo - L2_BYTE_BASE as u64) / 2) as u32, (base + (hi - L2_BYTE_BASE as u64).div_ceil(2)) as u32));
            }
        }
        out.sort_unstable();
        out
    }

    /// The index of the segment covering `addr`, if any (binary search over
    /// the sorted, disjoint segment list).
    fn segment_containing(&self, addr: u32) -> Option<usize> {
        let i = self.segments.partition_point(|(start, _)| *start <= addr);
        if i == 0 {
            return None;
        }
        let (start, bytes) = &self.segments[i - 1];
        if (addr as u64) < (*start as u64) + bytes.len() as u64 {
            Some(i - 1)
        } else {
            None
        }
    }

    /// `sharcldr.LoadedMemory.read(address, size)`: exactly `size` final
    /// loaded bytes if every one of them is covered (possibly spanning more
    /// than one segment, when they sit back to back with no gap), else
    /// `None` for any gap.
    pub fn read(&self, address: u32, size: u32) -> Option<Vec<u8>> {
        if size == 0 {
            return Some(Vec::new());
        }
        let end = (address as u64).checked_add(size as u64)?;
        if end > u32::MAX as u64 + 1 {
            return None;
        }
        let mut out = Vec::with_capacity(size as usize);
        let mut addr = address;
        while (addr as u64) < end {
            let idx = self.segment_containing(addr)?;
            let (start, bytes) = &self.segments[idx];
            let seg_end = (*start as u64) + bytes.len() as u64;
            let off = (addr - start) as usize;
            let take = (seg_end.min(end) - addr as u64) as usize;
            out.extend_from_slice(&bytes[off..off + take]);
            addr += take as u32;
        }
        Some(out)
    }

    fn read_words(&self, address: u32, size_bytes: u32) -> Option<[u16; 3]> {
        let bytes = self.read(address, size_bytes)?;
        let mut out = [0u16; 3];
        for i in 0..(size_bytes / 2) as usize {
            out[i] = u16::from_le_bytes([bytes[2 * i], bytes[2 * i + 1]]);
        }
        Some(out)
    }
}

impl ShortWords for SegmentImage {
    /// `sharcldr.LoadedMemory.read_sw(pc_sw, size)`, exactly: primary at
    /// `2*pc_sw + SW_ALIAS_BASE`; on a miss, only when `pc_sw >=
    /// L2_SW_BASE`, the L2 fallback at `L2_BYTE_BASE + 2*(pc_sw -
    /// L2_SW_BASE)`, only when it and `size_bytes` fit under `L2_BYTE_LIMIT`.
    fn read_sw(&self, pc_sw: u32, size_bytes: u32) -> Option<[u16; 3]> {
        let primary = (pc_sw as u64) * 2 + SW_ALIAS_BASE as u64;
        if primary <= u32::MAX as u64
            && let Some(words) = self.read_words(primary as u32, size_bytes)
        {
            return Some(words);
        }
        if pc_sw < L2_SW_BASE {
            return None;
        }
        let fallback = L2_BYTE_BASE as u64 + 2 * (pc_sw - L2_SW_BASE) as u64;
        if fallback + size_bytes as u64 > L2_BYTE_LIMIT as u64 {
            return None;
        }
        self.read_words(fallback as u32, size_bytes)
    }
}

struct Rd<'a> {
    b: &'a [u8],
    off: usize,
}

impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self
            .off
            .checked_add(n)
            .filter(|&e| e <= self.b.len())
            .ok_or_else(|| "pack image truncated".to_string())?;
        let s = &self.b[self.off..end];
        self.off = end;
        Ok(s)
    }

    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
}

/// Parse a `tools/sharc_transpile_run.py::pack_image` "SHIM" v1 blob into a
/// [`SegmentImage`]. Everything past the segment list (the named-MMR
/// addresses, ranges, and `CORE_MMR_RESET_VALUES`) is read past but not
/// kept: `ShortWords::read_sw` never needs it.
pub fn parse_pack_image(blob: &[u8]) -> Result<SegmentImage, String> {
    let mut r = Rd { b: blob, off: 0 };
    if r.take(4)? != b"SHIM" {
        return Err("not a pack_image blob (bad magic)".to_string());
    }
    let version = r.u32()?;
    if version != 1 {
        return Err(format!("unsupported pack_image version {version}"));
    }
    let n = r.u32()?;
    let mut segments = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let addr = r.u32()?;
        let len = r.u32()? as usize;
        let bytes = r.take(len)?.to_vec();
        segments.push((addr, bytes));
    }
    SegmentImage::from_segments(segments)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_spans_two_adjacent_segments() {
        let img =
            SegmentImage::from_segments(vec![(0x1000, vec![1, 2, 3, 4]), (0x1004, vec![5, 6])])
                .unwrap();
        assert_eq!(img.read(0x1000, 6).unwrap(), vec![1, 2, 3, 4, 5, 6]);
        assert_eq!(img.read(0x1002, 3).unwrap(), vec![3, 4, 5]);
    }

    #[test]
    fn read_fails_on_any_gap() {
        let img =
            SegmentImage::from_segments(vec![(0x1000, vec![1, 2]), (0x1010, vec![3, 4])]).unwrap();
        assert!(img.read(0x1000, 4).is_none());
        assert!(img.read(0x1010, 2).is_some());
    }

    #[test]
    fn overlapping_segments_are_rejected() {
        let err = SegmentImage::from_segments(vec![(0x1000, vec![1, 2, 3]), (0x1001, vec![9])]);
        assert!(err.is_err());
    }

    #[test]
    fn read_sw_uses_the_primary_sw_alias() {
        let img = SegmentImage::from_segments(vec![(
            SW_ALIAS_BASE + 0x100,
            vec![0xAA, 0xBB, 0xCC, 0xDD],
        )])
        .unwrap();
        // pc_sw = 0x80 -> byte address SW_ALIAS_BASE + 0x100.
        let got = img.read_sw(0x80, 4).unwrap();
        assert_eq!(got[0], 0xBBAA);
        assert_eq!(got[1], 0xDDCC);
    }

    #[test]
    fn read_sw_falls_back_to_the_l2_window_below_l2_sw_base_never_does() {
        let img = SegmentImage::from_segments(vec![(L2_BYTE_BASE, vec![0x11, 0x22])]).unwrap();
        // Below L2_SW_BASE, a primary miss must not try the fallback.
        assert!(img.read_sw(0, 2).is_none());
        // At/after L2_SW_BASE, a primary miss tries the fallback.
        let got = img.read_sw(L2_SW_BASE, 2).unwrap();
        assert_eq!(got[0], 0x2211);
    }

    #[test]
    fn parse_pack_image_rejects_bad_magic() {
        assert!(parse_pack_image(b"NOPE").is_err());
    }

    #[test]
    fn parse_pack_image_round_trips_segments() {
        let mut blob = Vec::new();
        blob.extend_from_slice(b"SHIM");
        blob.extend_from_slice(&1u32.to_le_bytes());
        blob.extend_from_slice(&1u32.to_le_bytes()); // one segment
        blob.extend_from_slice(&0x1000u32.to_le_bytes());
        blob.extend_from_slice(&3u32.to_le_bytes());
        blob.extend_from_slice(&[7, 8, 9]);
        // trailing named/ranges/reset sections, all empty counts.
        blob.extend_from_slice(&0u32.to_le_bytes());
        blob.extend_from_slice(&0u32.to_le_bytes());
        blob.extend_from_slice(&0u32.to_le_bytes());
        let img = parse_pack_image(&blob).unwrap();
        assert_eq!(img.read(0x1000, 3).unwrap(), vec![7, 8, 9]);
    }
}
