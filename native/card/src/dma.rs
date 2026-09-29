//! Bounded eMMC/eSDHC channel-59 adapter for a caller-owned guest RAM window.
//! The future machine decides when SERQ59 arms the channel and supplies the
//! physical RAM slice; this module never opens an image or delivers an IRQ.

use periph::{
    edma::{TcdSnapshot, TcdView},
    esdhc::{self, DmaBuffers, DmaDirection, DmaEffect, DmaError, transfer_dma59},
    regfile::RegFile,
};

use crate::{Card, CardError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StorageDmaError {
    Dma(DmaError),
    Card(CardError),
    ScratchTooSmall { needed: usize, available: usize },
}

impl From<DmaError> for StorageDmaError {
    fn from(value: DmaError) -> Self {
        Self::Dma(value)
    }
}

impl From<CardError> for StorageDmaError {
    fn from(value: CardError) -> Self {
        Self::Card(value)
    }
}

/// Service one *already armed* eDMA59 command using supplied, reusable RAM
/// and card staging windows. Only a successful transfer commits TCD writes;
/// a zero-CITER CMD8/CMD18 is a no-op as in the Python oracle. The returned
/// completion intent must be delivered by the machine owner, not this core.
pub fn service_dma59(
    card: &mut Card,
    edma: &mut RegFile,
    command: u8,
    argument: u32,
    guest_base: u32,
    guest: &mut [u8],
    scratch: &mut [u8],
) -> Result<DmaEffect, StorageDmaError> {
    if !matches!(command, 8 | 18 | 25) {
        return Err(DmaError::UnsupportedCommand(command).into());
    }
    let tcd: TcdSnapshot = TcdView::new(edma, esdhc::DMA_CHANNEL).snapshot();
    let byte_count = u64::from(tcd.citer) * u64::from(tcd.nbytes);
    if byte_count > esdhc::MAX_DMA_BYTES as u64 {
        return Err(DmaError::TransferTooLarge { bytes: byte_count }.into());
    }
    let bytes = byte_count as usize;
    if scratch.len() < bytes {
        return Err(StorageDmaError::ScratchTooSmall {
            needed: bytes,
            available: scratch.len(),
        });
    }
    let card_window = &mut scratch[..bytes];
    match command {
        8 if bytes != 0 => {
            // CMD8's fixed 512-byte EXT_CSD is zero-padded to the TCD's
            // requested byte count by the oracle's `_dma_out`.
            let ext = card.data_for(8, argument, bytes)?.expect("CMD8 data");
            card_window.fill(0);
            let n = bytes.min(ext.len());
            card_window[..n].copy_from_slice(&ext[..n]);
        }
        18 if bytes != 0 => card.read_into(argument, card_window)?,
        _ => {}
    }
    let effect = transfer_dma59(
        command,
        tcd,
        &mut DmaBuffers {
            guest_base,
            guest,
            card: card_window,
        },
    )?;
    if command == 25 {
        card.write_data(25, argument, card_window)?;
    }
    if effect.completion.done {
        TcdView::new(edma, esdhc::DMA_CHANNEL)
            .apply_dma59_writeback(effect.tcd, effect.direction == DmaDirection::CardToGuest);
    }
    Ok(effect)
}
