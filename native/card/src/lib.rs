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
}

/// Minimal eMMC card identity, selection state, base reader, and write overlay.
pub struct Card {
    backing: Option<Box<dyn RandomAccessRead>>,
    blocks: u32,
    ext_csd: [u8; SECTOR_SIZE],
    rca: u16,
    selected: bool,
    overlay: BTreeMap<u64, u8>,
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
        self.overlay.len()
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
        for (&offset, &value) in self.overlay.range(start..) {
            let relative = offset - start;
            if relative >= destination.len() as u64 {
                break;
            }
            destination[relative as usize] = value;
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
        for (offset, &value) in payload.iter().enumerate() {
            self.overlay.insert(start + offset as u64, value);
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
