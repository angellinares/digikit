//! Pure, deliberately minimal eMMC identity and media state.
//!
//! This is the card side of the protocol only: it neither performs I/O nor
//! owns an image file. A host supplies a [`RandomAccessRead`] implementation
//! when it wants base media; card writes remain in the sparse in-memory
//! overlay. The command responses and synthetic identity match `emu/esdhc.py`
//! `Card`, rather than attempting to implement unobserved eMMC features.

use std::collections::BTreeMap;

use periph::esdhc::CardPort;

pub mod dma;

/// Bytes in one sector for the sector-addressed commands modelled here.
pub const SECTOR_SIZE: usize = 512;
/// Largest data transfer accepted by one card call.
///
/// Use [`Card::read_into`] repeatedly with a reusable buffer for larger media.
pub const MAX_TRANSFER_BYTES: usize = 1024 * 1024;
/// Default capacity accepted by the observed firmware identity check.
pub const DEFAULT_CAPACITY_BLOCKS: u32 = 0x0076_0000;
/// The other capacity accepted by that identity check.
pub const SMALL_CAPACITY_BLOCKS: u32 = 0x003b_0000;

const OCR: u32 = 0xc0ff_8080;
const R1_TRANSFER_READY: u32 = 0x0000_0900;
const CSD_RSP1: u32 = 0xafc0_0380;
const CSD_RSP2: u32 = 0x0000_0a03;

/// A caller-owned, random-access source for the immutable base media.
///
/// `read_at` returns the number of bytes copied, which may be less than the
/// destination length at end of media. Missing bytes are read as zeroes, as
/// in the Python oracle. Implementations must not write beyond `destination`.
pub trait RandomAccessRead {
    /// Length of the available base media in bytes.
    fn len(&self) -> u64;

    /// Whether the base media has no bytes.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Copy base-media bytes beginning at `offset` into `destination`.
    fn read_at(&self, offset: u64, destination: &mut [u8]) -> usize;
}

/// Why a card operation was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CardError {
    /// Only the two firmware-whitelisted capacity values are synthetic IDs.
    UnsupportedCapacity(u32),
    /// A caller requested a transfer larger than [`MAX_TRANSFER_BYTES`].
    TransferTooLarge { requested: usize, maximum: usize },
    /// Saved card state belongs to a different reported capacity.
    CheckpointCapacityMismatch { expected: u32, actual: u32 },
    /// A sector-overlay restore must not also contain the v1 byte overlay.
    CheckpointOverlayNotEmpty,
    /// A sector-overlay record is not ordered strictly after its predecessor.
    OverlaySectorNotStrictlyIncreasing { previous: u64, sector: u64 },
    /// A sector-overlay record names a sector outside the card capacity.
    OverlaySectorOutOfRange { sector: u64, blocks: u32 },
    /// A sector-overlay record does not mask any bytes.
    OverlaySectorEmpty { sector: u64 },
    /// Data for a byte absent from a sector-overlay mask must be zero.
    OverlaySectorNonCanonicalData { sector: u64, byte: usize },
    /// The total number of masked bytes cannot fit in the card's counter.
    OverlayWrittenCountOverflow,
}

/// Host-only card state from Python's `Esdhc` v1 checkpoint. The sparse
/// overlay maps absolute byte offsets, not whole sectors or backing bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CardCheckpoint {
    pub blocks: u32,
    pub rca: u16,
    pub selected: bool,
    pub overlay: BTreeMap<u64, u8>,
}

/// A compact sparse write overlay for one 512-byte card sector.
///
/// `written` is a 512-bit mask in byte order: bit 0 of `written[0]` names
/// `data[0]`, and bit 7 of `written[63]` names `data[511]`. Bytes absent from
/// the mask must have zero data so that this representation is canonical.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CardOverlaySector {
    pub sector: u64,
    pub written: [u8; 64],
    pub data: [u8; 512],
}

/// One sparse card sector. Unwritten bytes still come from the backing;
/// writes of zero are distinct from bytes that have never been written.
struct OverlaySector {
    data: [u8; SECTOR_SIZE],
    written: [u64; SECTOR_SIZE / 64],
    count: usize,
}

impl OverlaySector {
    fn new() -> Self {
        Self {
            data: [0; SECTOR_SIZE],
            written: [0; SECTOR_SIZE / 64],
            count: 0,
        }
    }

    fn write(&mut self, start: usize, bytes: &[u8]) -> usize {
        self.data[start..start + bytes.len()].copy_from_slice(bytes);
        if start == 0 && bytes.len() == SECTOR_SIZE {
            let newly_written = SECTOR_SIZE - self.count;
            self.written.fill(u64::MAX);
            self.count = SECTOR_SIZE;
            return newly_written;
        }
        let mut newly_written = 0;
        for position in start..start + bytes.len() {
            let mask = 1u64 << (position & 63);
            let word = &mut self.written[position >> 6];
            if *word & mask == 0 {
                *word |= mask;
                newly_written += 1;
            }
        }
        self.count += newly_written;
        newly_written
    }

    fn read_into(&self, start: usize, destination: &mut [u8]) {
        if self.count == SECTOR_SIZE {
            destination.copy_from_slice(&self.data[start..start + destination.len()]);
        } else {
            for (offset, out) in destination.iter_mut().enumerate() {
                let position = start + offset;
                if self.written[position >> 6] & (1u64 << (position & 63)) != 0 {
                    *out = self.data[position];
                }
            }
        }
    }
}

/// Minimal eMMC card identity, selection state, base reader, and write overlay.
pub struct Card {
    backing: Option<Box<dyn RandomAccessRead>>,
    blocks: u32,
    ext_csd: [u8; SECTOR_SIZE],
    rca: u16,
    selected: bool,
    overlay: BTreeMap<u64, Box<OverlaySector>>,
    overlay_bytes: usize,
    cid: [u32; 4],
    csd: [u32; 4],
}

impl Default for Card {
    fn default() -> Self {
        match Self::new(DEFAULT_CAPACITY_BLOCKS) {
            Ok(card) => card,
            Err(_) => unreachable!("default capacity is supported"),
        }
    }
}

impl Card {
    /// Make a card with zero-filled base media and the supplied reported capacity.
    pub fn new(capacity_blocks: u32) -> Result<Self, CardError> {
        Self::with_backing(capacity_blocks, None)
    }

    /// Make a card whose immutable base media comes from `backing`.
    ///
    /// The backing length is independent of the reported identity capacity,
    /// matching the oracle: short reads are padded with zeroes and no image is
    /// copied into the card.
    pub fn with_backing(
        capacity_blocks: u32,
        backing: Option<Box<dyn RandomAccessRead>>,
    ) -> Result<Self, CardError> {
        Ok(Self {
            backing,
            blocks: capacity_blocks,
            ext_csd: make_ext_csd(capacity_blocks)?,
            rca: 0,
            selected: false,
            overlay: BTreeMap::new(),
            overlay_bytes: 0,
            cid: [0, 0x4530_0000, 0x3030_3447, 0x0011_0000],
            csd: [0, CSD_RSP1, CSD_RSP2, 0],
        })
    }

    /// Reported capacity in 512-byte sectors.
    pub const fn blocks(&self) -> u32 {
        self.blocks
    }
    /// Current relative card address assigned by CMD3.
    pub const fn rca(&self) -> u16 {
        self.rca
    }
    /// Whether CMD7 currently selects this card.
    pub const fn selected(&self) -> bool {
        self.selected
    }
    /// Number of bytes retained in the sparse write overlay.
    pub fn overlay_len(&self) -> usize {
        self.overlay_bytes
    }

    /// Restore host card identity and sparse writes without modifying media.
    /// Capacity is checked before touching the current card. In particular a
    /// written zero byte must mask a nonzero backing byte after restoration.
    pub fn restore_checkpoint(&mut self, state: &CardCheckpoint) -> Result<(), CardError> {
        if self.blocks != state.blocks {
            return Err(CardError::CheckpointCapacityMismatch {
                expected: self.blocks,
                actual: state.blocks,
            });
        }
        let mut overlay = BTreeMap::<u64, Box<OverlaySector>>::new();
        for (&offset, &byte) in &state.overlay {
            let sector = offset / SECTOR_SIZE as u64;
            let position = (offset % SECTOR_SIZE as u64) as usize;
            overlay
                .entry(sector)
                .or_insert_with(|| Box::new(OverlaySector::new()))
                .write(position, &[byte]);
        }
        self.overlay = overlay;
        self.overlay_bytes = state.overlay.len();
        self.rca = state.rca;
        self.selected = state.selected;
        Ok(())
    }

    /// Restore host card identity and a compact sector-bitmap overlay.
    ///
    /// The v1 byte overlay in `state` must be empty: mixing encodings would
    /// otherwise discard writes. Every input is validated before this card is
    /// changed, including canonical data for mask-absent bytes.
    pub fn restore_checkpoint_sectors(
        &mut self,
        state: &CardCheckpoint,
        sectors: &[CardOverlaySector],
    ) -> Result<(), CardError> {
        if self.blocks != state.blocks {
            return Err(CardError::CheckpointCapacityMismatch {
                expected: self.blocks,
                actual: state.blocks,
            });
        }
        if !state.overlay.is_empty() {
            return Err(CardError::CheckpointOverlayNotEmpty);
        }

        let mut overlay = BTreeMap::<u64, Box<OverlaySector>>::new();
        let mut overlay_bytes = 0usize;
        let mut previous_sector = None;
        for record in sectors {
            if let Some(previous) = previous_sector
                && record.sector <= previous
            {
                return Err(CardError::OverlaySectorNotStrictlyIncreasing {
                    previous,
                    sector: record.sector,
                });
            }
            if record.sector >= u64::from(self.blocks) {
                return Err(CardError::OverlaySectorOutOfRange {
                    sector: record.sector,
                    blocks: self.blocks,
                });
            }
            let count = record
                .written
                .iter()
                .map(|byte| byte.count_ones() as usize)
                .sum::<usize>();
            if count == 0 {
                return Err(CardError::OverlaySectorEmpty {
                    sector: record.sector,
                });
            }
            for (byte, &value) in record.data.iter().enumerate() {
                if record.written[byte >> 3] & (1 << (byte & 7)) == 0 && value != 0 {
                    return Err(CardError::OverlaySectorNonCanonicalData {
                        sector: record.sector,
                        byte,
                    });
                }
            }
            overlay_bytes = overlay_bytes
                .checked_add(count)
                .ok_or(CardError::OverlayWrittenCountOverflow)?;
            let written = std::array::from_fn(|word| {
                let mut bytes = [0; 8];
                bytes.copy_from_slice(&record.written[word * 8..(word + 1) * 8]);
                u64::from_le_bytes(bytes)
            });
            overlay.insert(
                record.sector,
                Box::new(OverlaySector {
                    data: record.data,
                    written,
                    count,
                }),
            );
            previous_sector = Some(record.sector);
        }

        self.overlay = overlay;
        self.overlay_bytes = overlay_bytes;
        self.rca = state.rca;
        self.selected = state.selected;
        Ok(())
    }

    /// Execute the minimal identity/selection command set and return RSP0..3.
    pub fn command(&mut self, idx: u32, arg: u32) -> [u32; 4] {
        match idx {
            0 => {
                self.selected = false;
                [0; 4]
            }
            1 => [OCR, 0, 0, 0],
            2 | 10 => self.cid,
            9 => self.csd,
            3 => {
                self.rca = (arg >> 16) as u16;
                [R1_TRANSFER_READY, 0, 0, 0]
            }
            7 => {
                self.selected = (arg >> 16) as u16 == self.rca;
                [R1_TRANSFER_READY, 0, 0, 0]
            }
            _ => [R1_TRANSFER_READY, 0, 0, 0],
        }
    }

    /// Return the next DATPORT word for a bus-test read command.
    pub const fn read_word(&self, idx: u32, pattern: u32) -> u32 {
        if idx == 14 { !pattern } else { 0 }
    }

    /// Return data for a modelled data-read command.
    ///
    /// The explicit `length` must not exceed [`MAX_TRANSFER_BYTES`]. CMD8
    /// returns its fixed 512-byte EXT_CSD as in the Python oracle, regardless
    /// of that supplied length. CMD18 returns exactly `length` bytes from
    /// sector `arg`, zero-padding missing base media and applying sparse
    /// writes. Other commands return `Ok(None)`.
    pub fn data_for(
        &self,
        idx: u32,
        arg: u32,
        length: usize,
    ) -> Result<Option<Vec<u8>>, CardError> {
        check_transfer_length(length)?;
        match idx {
            8 => Ok(Some(self.ext_csd.to_vec())),
            18 => {
                let mut data = vec![0; length];
                self.read_into(arg, &mut data)?;
                Ok(Some(data))
            }
            _ => Ok(None),
        }
    }

    /// Stream CMD18 media from sector `arg` into a caller-owned reusable buffer.
    ///
    /// `destination.len()` is the explicit transfer length and is bounded by
    /// [`MAX_TRANSFER_BYTES`]. Repeated calls with consecutive sector arguments
    /// avoid allocating a whole backing image. The byte results match CMD18
    /// `data_for` for supported lengths.
    pub fn read_into(&self, arg: u32, destination: &mut [u8]) -> Result<(), CardError> {
        check_transfer_length(destination.len())?;
        let start = u64::from(arg) * SECTOR_SIZE as u64;
        // CMD18 constructs a zero-filled block before applying base media;
        // reset reusable caller storage to preserve that byte semantics.
        destination.fill(0);
        if let Some(backing) = &self.backing {
            let available = backing
                .len()
                .saturating_sub(start)
                .min(destination.len() as u64) as usize;
            let _ = backing.read_at(start, &mut destination[..available]);
        }
        if destination.is_empty() {
            return Ok(());
        }
        let end = start + destination.len() as u64;
        for (&sector, overlay) in self
            .overlay
            .range(start / SECTOR_SIZE as u64..=(end - 1) / SECTOR_SIZE as u64)
        {
            let sector_start = sector * SECTOR_SIZE as u64;
            let copied_start = sector_start.max(start);
            let copied_end = (sector_start + SECTOR_SIZE as u64).min(end);
            let out_offset = (copied_start - start) as usize;
            let sector_offset = (copied_start - sector_start) as usize;
            overlay.read_into(
                sector_offset,
                &mut destination[out_offset..out_offset + (copied_end - copied_start) as usize],
            );
        }
        Ok(())
    }

    /// Retain bounded CMD25 payload bytes in the sparse overlay.
    ///
    /// Other command indices ignore the payload after validating its bounded
    /// transfer size, as the Python oracle ignores non-CMD25 writes.
    pub fn write_data(&mut self, idx: u32, arg: u32, payload: &[u8]) -> Result<(), CardError> {
        check_transfer_length(payload.len())?;
        if idx != 25 {
            return Ok(());
        }
        let start = u64::from(arg) * SECTOR_SIZE as u64;
        let mut consumed = 0;
        while consumed < payload.len() {
            let position = start + consumed as u64;
            let sector = position / SECTOR_SIZE as u64;
            let sector_offset = (position % SECTOR_SIZE as u64) as usize;
            let count = (SECTOR_SIZE - sector_offset).min(payload.len() - consumed);
            let page = self
                .overlay
                .entry(sector)
                .or_insert_with(|| Box::new(OverlaySector::new()));
            self.overlay_bytes += page.write(sector_offset, &payload[consumed..consumed + count]);
            consumed += count;
        }
        Ok(())
    }
}

impl CardPort for Card {
    fn command(&mut self, idx: u8, arg: u32) -> [u32; 4] {
        Card::command(self, u32::from(idx), arg)
    }

    fn read_word(&mut self, idx: u8, pattern: u32) -> u32 {
        Card::read_word(self, u32::from(idx), pattern)
    }
}

fn check_transfer_length(length: usize) -> Result<(), CardError> {
    if length > MAX_TRANSFER_BYTES {
        return Err(CardError::TransferTooLarge {
            requested: length,
            maximum: MAX_TRANSFER_BYTES,
        });
    }
    Ok(())
}

#[cfg(test)]
mod overlay_tests {
    use super::*;

    #[test]
    fn megabyte_write_uses_one_entry_per_sector() {
        let mut card = Card::default();
        let payload = vec![0x5a; MAX_TRANSFER_BYTES];
        card.write_data(25, 0, &payload).unwrap();
        assert_eq!(card.overlay_len(), MAX_TRANSFER_BYTES);
        assert_eq!(card.overlay.len(), MAX_TRANSFER_BYTES / SECTOR_SIZE);
        let mut out = vec![0; MAX_TRANSFER_BYTES];
        card.read_into(0, &mut out).unwrap();
        assert_eq!(out, payload);
    }
}

fn make_ext_csd(sectors: u32) -> Result<[u8; SECTOR_SIZE], CardError> {
    let (sec_count, slc_ok) = match sectors {
        DEFAULT_CAPACITY_BLOCKS => (DEFAULT_CAPACITY_BLOCKS, 0),
        SMALL_CAPACITY_BLOCKS => (0, 1),
        other => return Err(CardError::UnsupportedCapacity(other)),
    };
    let mut bytes = [0; SECTOR_SIZE];
    bytes[0x98] = slc_ok;
    bytes[0xd4..0xd8].copy_from_slice(&sec_count.to_be_bytes());
    bytes[0xaf] = 1;
    bytes[0xb7] = 1;
    bytes[0xb9] = 1;
    bytes[0x9c..0x9f].copy_from_slice(&[0, 1, 0xd8]);
    bytes[0xde] = 1;
    bytes[0xe3] = 8;
    Ok(bytes)
}
