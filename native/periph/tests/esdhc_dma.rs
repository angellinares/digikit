//! Pure channel-59 eSDHC transfer effects, ported from `emu.esdhc`.

use periph::{
    edma::{ATTR, CSR_D_REQ, CSR_INT_MAJOR, NBYTES, SADDR, TCD_BASE, TcdSnapshot, TcdView},
    esdhc::{DMA_CHANNEL, DmaBuffers, DmaDirection, DmaError, MAX_DMA_BYTES, transfer_dma59},
    regfile::RegFile,
};

fn tcd(citer: u16, nbytes: u32) -> TcdSnapshot {
    TcdSnapshot {
        saddr: 0x8000_1000,
        attr: 0,
        soff: nbytes as i16,
        nbytes,
        slast: 0,
        daddr: 0x8000_2000,
        citer,
        doff: nbytes as i16,
        dlast: 0,
        biter: citer,
        csr: CSR_INT_MAJOR | CSR_D_REQ,
    }
}

#[test]
fn writeback_only_mutates_oracle_moving_address_citer_and_csr() {
    let mut regs = RegFile::new();
    let tcd_addr = TCD_BASE + (DMA_CHANNEL * 0x20) as u32;
    regs.write(tcd_addr + ATTR as u32, 2, 0x5a5a);
    regs.write(tcd_addr + NBYTES as u32, 4, 0x1234);
    regs.write(tcd_addr + SADDR as u32, 4, 0x8000_1000);
    let mut view = TcdView::new(&mut regs, DMA_CHANNEL);
    let mut completed = tcd(2, 512);
    completed.daddr += 1024;
    completed.citer = 3;
    completed.csr |= 0x80;
    view.apply_dma59_writeback(completed, true);
    let after = view.snapshot();
    assert_eq!(after.daddr, completed.daddr);
    assert_eq!(after.citer, completed.citer);
    assert_eq!(after.csr, completed.csr);
    assert_eq!(after.saddr, 0x8000_1000);
    assert_eq!(after.attr, 0x5a5a);
    assert_eq!(after.nbytes, 0x1234);

    completed.saddr = 0x8000_1200;
    view.apply_dma59_writeback(completed, false);
    let after_write = view.snapshot();
    assert_eq!(after_write.saddr, 0x8000_1200);
    assert_eq!(after_write.daddr, completed.daddr);
    assert_eq!(after_write.attr, 0x5a5a);
}

#[test]
fn cmd8_ext_csd_copies_512_bytes_and_matches_dma_out_writeback() {
    assert_eq!(DMA_CHANNEL, 59);
    let mut ext_csd = [0u8; 512];
    for (i, byte) in ext_csd.iter_mut().enumerate() {
        *byte = i as u8;
    }
    let mut guest = [0u8; 1024];
    let mut buffers = DmaBuffers {
        guest_base: 0x8000_2000,
        guest: &mut guest,
        card: &mut ext_csd,
    };

    let effect = transfer_dma59(8, tcd(1, 512), &mut buffers).unwrap();

    assert_eq!(effect.direction, DmaDirection::CardToGuest);
    assert_eq!(effect.bytes, 512);
    assert_eq!(&buffers.guest[..512], &buffers.card[..]);
    assert_eq!(effect.tcd.daddr, 0x8000_2200);
    assert_eq!(effect.tcd.citer, 1);
    assert_ne!(effect.tcd.csr & 0x80, 0);
    assert!(effect.completion.done);
    assert!(effect.completion.major_interrupt);
    assert!(effect.completion.disable_request);
}

#[test]
fn cmd18_and_cmd25_move_32k_blocks_with_oracle_tcd_transforms() {
    let mut card_read = [0u8; 32 * 1024];
    for (i, byte) in card_read.iter_mut().enumerate() {
        *byte = (i.wrapping_mul(13) >> 3) as u8;
    }
    let mut guest_read = [0u8; 40 * 1024];
    let read_tcd = TcdSnapshot {
        daddr: 0x8000_4000,
        ..tcd(64, 512)
    };
    let read = transfer_dma59(
        18,
        read_tcd,
        &mut DmaBuffers {
            guest_base: 0x8000_4000,
            guest: &mut guest_read,
            card: &mut card_read,
        },
    )
    .unwrap();
    assert_eq!(read.bytes, 32 * 1024);
    assert_eq!(&guest_read[..32 * 1024], &card_read[..]);
    assert_eq!(read.tcd.daddr, 0x8000_C000);

    let mut card_write = [0u8; 32 * 1024];
    let write_tcd = TcdSnapshot {
        saddr: 0x8000_4000,
        soff: 512,
        slast: -(32 * 1024),
        ..tcd(64, 512)
    };
    let write = transfer_dma59(
        25,
        write_tcd,
        &mut DmaBuffers {
            guest_base: 0x8000_4000,
            guest: &mut guest_read,
            card: &mut card_write,
        },
    )
    .unwrap();
    assert_eq!(write.direction, DmaDirection::GuestToCard);
    assert_eq!(&card_write[..], &guest_read[..32 * 1024]);
    // `_dma_in` walks SOFF once per minor loop, then applies SLAST.
    assert_eq!(write.tcd.saddr, 0x8000_4000);
    assert_eq!(write.tcd.citer, 64);
}

#[test]
fn rejects_bounded_and_out_of_range_requests_without_mutating_buffers() {
    let mut guest = [0xA5; 32];
    let mut card = [0x5A; 32];
    let too_large = transfer_dma59(
        18,
        tcd(2, (MAX_DMA_BYTES / 2 + 1) as u32),
        &mut DmaBuffers {
            guest_base: 0x8000_2000,
            guest: &mut guest,
            card: &mut card,
        },
    );
    assert_eq!(
        too_large,
        Err(DmaError::TransferTooLarge {
            bytes: MAX_DMA_BYTES as u64 + 2
        })
    );
    assert_eq!(guest, [0xA5; 32]);

    let out_of_range = transfer_dma59(
        18,
        tcd(1, 32),
        &mut DmaBuffers {
            guest_base: 0x8000_2004,
            guest: &mut guest,
            card: &mut card,
        },
    );
    assert_eq!(
        out_of_range,
        Err(DmaError::GuestOutOfRange {
            address: 0x8000_2000,
            bytes: 32,
        })
    );
    assert_eq!(guest, [0xA5; 32]);
    assert_eq!(card, [0x5A; 32]);

    let mut last_byte = [0xA5];
    let mut card_byte = [0x5A];
    let overflow = transfer_dma59(
        18,
        TcdSnapshot {
            daddr: u32::MAX,
            ..tcd(1, 1)
        },
        &mut DmaBuffers {
            guest_base: u32::MAX,
            guest: &mut last_byte,
            card: &mut card_byte,
        },
    );
    assert_eq!(
        overflow,
        Err(DmaError::AddressOverflow {
            address: u32::MAX,
            bytes: 1
        })
    );
    assert_eq!(last_byte, [0xA5]);
    assert_eq!(card_byte, [0x5A]);

    let mut crossing_guest = [0xA5; 32];
    let mut crossing_card = [0x5A; 32];
    let crossing = transfer_dma59(
        25,
        TcdSnapshot {
            saddr: 0xffff_fff0,
            ..tcd(1, 32)
        },
        &mut DmaBuffers {
            guest_base: 0xffff_fff0,
            guest: &mut crossing_guest,
            card: &mut crossing_card,
        },
    );
    assert_eq!(
        crossing,
        Err(DmaError::AddressOverflow {
            address: 0xffff_fff0,
            bytes: 32
        })
    );
    assert_eq!(crossing_guest, [0xA5; 32]);
    assert_eq!(crossing_card, [0x5A; 32]);
}
